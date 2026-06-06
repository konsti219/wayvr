# WiVRn Network Metrics Plan

This note captures the medium-effort implementation path for exposing WiVRn transport timing in WayVR using the existing Monado metrics FD stream.

It is intentionally scoped to frame timing, not full transport telemetry like packet loss or Wi-Fi stats.

## Status: not implemented, and one assumption below is wrong

The `NET` graph in the watch is a placeholder. It reads
`SystemPresentInfo.present_margin_ns`, so it shows `--` and an empty graph on
every setup — that value is compositor present margin, not transport timing, and
more importantly the record never arrives at all.

The plan below assumes `SystemPresentInfo` already flows and only needs extra
fields. It does not, as of WiVRn 26.6.2:

- `u_metrics_write_system_present_info` is called from exactly one place in
  Monado, `u_pacing_compositor.c`.
- WiVRn 26.6 replaced Monado's compositor with its own (`server/compositor/`,
  including its own `pacer.cpp`) and never creates a `u_pacing_compositor`.
- WiVRn's server tree contains **no** calls to any `u_metrics_write_*` function.

So the only records that reach WayVR are `SessionFrame`, emitted by our patched
copy of WiVRn's `app_pacer.cpp`, plus the version header `u_metrics` writes when
a sink attaches.

That means step 1 is not "extend `SystemPresentInfo`" but "make WiVRn emit a
system-frame record at all" — a new writer on the WiVRn side, next to whatever
carries the headset feedback packet. Everything below stays useful as the schema
and join design; treat the producer half as unwritten.

## Goal

Expose a `NET` timing graph in WayVR by sending WiVRn transport/decode timing through the same Monado metrics FD mechanism already used for Monado timing data.

## Recommended Scope

Implement transport timing on compositor/system-frame records, not on `SessionFrame`.

Why:

- WiVRn headset feedback is keyed by streamed compositor frame id, not app/session frame id.
- Monado `SessionFrame` is keyed by app/session frame id.
- Forcing network timing into `SessionFrame` would require a bad or fragile join.
- The existing metrics schema already has system-frame keyed records.

## Existing Data Sources

WiVRn already has the needed timing points for a useful network/decode graph.

### Headset feedback packet

File:
- `/tmp/wivrn-26.2.3-inspect/common/wivrn_packets.h`

Relevant struct:
- `from_headset::feedback`

Relevant fields:
- `send_begin`
- `send_end`
- `received_first_packet`
- `received_last_packet`
- `sent_to_decoder`
- `received_from_decoder`
- `blitted`
- `displayed`

These are visible in the upstream source around:
- `common/wivrn_packets.h:472`

### Server feedback handling

File:
- `/tmp/wivrn-26.2.3-inspect/server/driver/wivrn_session.cpp`

Relevant function:
- `wivrn_session::operator()(from_headset::feedback && feedback)`

This already receives feedback centrally and converts headset timestamps back into server time via `clock_offset`.

Relevant lines:
- `server/driver/wivrn_session.cpp:696`

### Compositor frame id mapping

Files:
- `/tmp/wivrn-26.2.3-inspect/server/driver/wivrn_comp_target.cpp`
- `/tmp/wivrn-26.2.3-inspect/server/driver/wivrn_pacer.cpp`

Relevant behavior:
- `wivrn_comp_target` stores the streamed compositor frame id in `psc.frame_index`
- feedback uses `feedback.frame_index`
- `wivrn_pacer::on_feedback` already correlates feedback using that frame id

Relevant lines:
- `server/driver/wivrn_comp_target.cpp:639`
- `server/driver/wivrn_pacer.cpp:125`

### Existing send-side timing

File:
- `/tmp/wivrn-26.2.3-inspect/server/encoder/video_encoder.cpp`

Relevant function:
- `video_encoder::SendData`

This already records send-side timing and is the natural place if per-frame send byte counts are later desired.

Relevant lines:
- `server/encoder/video_encoder.cpp:300`

## Current Monado Metrics Schema Constraint

WayVR currently mirrors the Monado metrics protobuf generated into:
- [proto.rs](/home/konsti/programs/wayvr/wayvr/src/subsystem/monado_metrics/proto.rs)

Current record types:
- `Version`
- `SessionFrame`
- `Used`
- `SystemFrame`
- `SystemGpuInfo`
- `SystemPresentInfo`

`SessionFrame` is app/session keyed.

`SystemFrame` and `SystemPresentInfo` are compositor/system-frame keyed and are the correct place for WiVRn transport timing.

## Recommended Schema Change

Extend `SystemPresentInfo` with WiVRn transport/decode timing fields.

Suggested additions:
- `send_begin_ns`
- `send_end_ns`
- `receive_begin_ns`
- `receive_end_ns`
- `decode_begin_ns`
- `decode_end_ns`

Reasoning:

- These timings are properties of a streamed compositor frame.
- They align naturally with `feedback.frame_index`.
- Keeping them on `SystemPresentInfo` avoids introducing a second parallel frame record unless the upstream Monado schema owner strongly prefers a dedicated record.

Alternative:

- Add a new protobuf record such as `SystemTransportInfo`

That is also valid, but it creates more schema churn on both producer and consumer sides. For the medium-effort path, extending `SystemPresentInfo` is simpler.

## Producer Changes

### 1. Extend the Monado metrics protobuf/schema

Patch the Monado metrics source used by MR 2484 so the generated protobuf includes the new timing fields on `SystemPresentInfo`.

This implies regenerating:
- Monado-side protobuf code
- WayVR-side `proto.rs`

### 2. Add a WiVRn metrics write point on feedback

Patch WiVRn in:
- `server/driver/wivrn_session.cpp`

Inside:
- `wivrn_session::operator()(from_headset::feedback && feedback)`

Do this:
- convert the headset timestamps into server time with the existing `clock_offset`
- populate the new transport timing fields
- write a Monado metrics record keyed by `feedback.frame_index`

This is the best hook because:
- it already has the full feedback packet
- it already has clock conversion
- it already runs once per feedback frame

### 3. Keep app/session metrics in app_pacer unchanged

Do not try to merge transport timing into:
- `server/driver/app_pacer.cpp`

That code emits session-frame metrics and uses a different frame-id space.

The app pacer should continue to emit `SessionFrame`.

The feedback path should emit system-frame transport timing.

## Consumer Changes in WayVR

### 1. Update protobuf

Regenerate:
- [proto.rs](/home/konsti/programs/wayvr/wayvr/src/subsystem/monado_metrics/proto.rs)

The existing note about regeneration is in:
- [README.md](/home/konsti/programs/wayvr/wayvr/src/subsystem/monado_metrics/README.md)

### 2. Preserve new records in the metrics queue

The queueing/FD read path in:
- [metrics_fd.rs](/home/konsti/programs/wayvr/wayvr/src/subsystem/monado_metrics/metrics_fd.rs)

already accepts arbitrary decoded records, so no special transport work should be needed there beyond updated protobuf definitions.

### 3. Stop discarding non-session records at the dashboard boundary

Current code in:
- [dashboard.rs](/home/konsti/programs/wayvr/wayvr/src/overlays/dashboard.rs)

only maps `SessionFrame` out of the queue and drops:
- `Used`
- `SystemFrame`
- `SystemGpuInfo`
- `SystemPresentInfo`

Relevant section:
- `dashboard.rs:594`

This needs a new path that exposes system-frame transport timing to the frontend.

### 4. Frontend graphing

The frontend should graph one or more of:
- send duration: `send_end_ns - send_begin_ns`
- receive duration: `receive_end_ns - receive_begin_ns`
- decode duration: `decode_end_ns - decode_begin_ns`
- optionally total transport-to-decode path: `decode_end_ns - send_begin_ns`

For the watch mock, a single compact `NET` graph is probably enough. That graph can be driven by one chosen derived metric, with deeper breakdown reserved for the Monado debug page if desired.

## Correlation Model

If the watch only needs a global `NET` graph:

- no session join is required
- consume the newest system-frame transport records directly

If a future UI wants per-session correlation:

- join `SystemPresentInfo.frame_id` to `Used.system_frame_id`
- then map to `Used.session_id` and `Used.session_frame_id`

That is the proper way to connect compositor-frame transport data back to session frames.

## Explicitly Out of Scope

These are not part of the medium-effort plan:

- packet loss
- retransmit count
- Wi-Fi RSSI
- true client-side throughput counters
- server socket queue depth
- jitter summaries

Those would require either:
- new headset protocol fields
- new counters in WiVRn socket code
- or both

## Concrete Implementation Steps

1. Extend the Monado metrics protobuf/schema for `SystemPresentInfo`.
2. Regenerate Monado protobuf output.
3. Regenerate WayVR `proto.rs`.
4. Patch WiVRn `wivrn_session::operator()(from_headset::feedback)` to emit the new system-frame transport timing record.
5. Extend WayVR dashboard/backend interfaces to surface system-frame transport records instead of dropping them.
6. Add frontend graphing for a derived `NET` metric.

## Expected Result

After implementation, WayVR should be able to display a real network/decode timing graph sourced from WiVRn headset feedback, using the same Monado metrics FD stream already used for timing records today.
