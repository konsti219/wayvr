#[cfg(feature = "feat-monado-metrics")]
use crate::subsystem::monado_metrics::{self, metrics_fd::MonadoMetricsFd};
#[cfg(feature = "feat-monado-metrics")]
use std::collections::VecDeque;

#[cfg(feature = "feat-monado-metrics")]
use crate::subsystem::monado_metrics::proto::{self, record};

pub struct MonadoState {
    pub ipc: libmonado::Monado,

    #[cfg(feature = "feat-monado-metrics")]
    pub metrics: Option<MonadoMetricsFd>,

    #[cfg(feature = "feat-monado-metrics")]
    pub watch_metrics: MonadoWatchMetrics,
}

#[cfg(feature = "feat-monado-metrics")]
const WATCH_METRICS_CAPACITY: usize = 256;

#[cfg(feature = "feat-monado-metrics")]
const FPS_TIMESTAMPS_CAPACITY: usize = 512;

#[cfg(feature = "feat-monado-metrics")]
#[derive(Default)]
pub struct MonadoWatchMetrics {
    pub cpu_ms: VecDeque<f32>,
    pub gpu_ms: VecDeque<f32>,
    pub net_ms: VecDeque<f32>,
    pub cpu_ms_count: usize,
    pub gpu_ms_count: usize,
    pub net_ms_count: usize,
    pub latest_cpu_ms: Option<f32>,
    pub latest_gpu_ms: Option<f32>,
    pub latest_net_ms: Option<f32>,
    pub fps_current: Option<f32>,
    /// Compositor display period, i.e. the frame budget the app is being paced
    /// to. Constant for a given refresh rate, unlike measured FPS.
    pub display_period_ms: Option<f32>,
    pub dropped_frames: u64,
    dashboard_records: VecDeque<proto::Record>,
    // (session_id, when_delivered_ns)
    frame_timestamps: VecDeque<(i64, u64)>,
    // WayVR always starts before the game is launched, so the first session_id
    // to appear in the metrics stream is WayVR's own XR session.  Any other
    // session_id is treated as the game session.
    own_session_id: Option<i64>,
}

#[cfg(feature = "feat-monado-metrics")]
impl MonadoWatchMetrics {
    fn push_sample(queue: &mut VecDeque<f32>, count: &mut usize, value: f32) {
        if queue.len() >= WATCH_METRICS_CAPACITY {
            queue.pop_front();
        }
        queue.push_back(value);
        *count += 1;
    }

    fn push_record(&mut self, record: proto::Record) {
        if self.dashboard_records.len() >= 500 {
            self.dashboard_records.pop_front();
        }
        self.dashboard_records.push_back(record);
    }

    fn ingest(&mut self, record: &proto::Record) {
        let Some(record) = &record.record else {
            return;
        };

        match record {
            record::Record::SessionFrame(frame) => {
                // The first session_id we ever see is WayVR's own XR session (WayVR
                // always starts before any game).  Every other session_id is the game.
                if self.own_session_id.is_none() {
                    self.own_session_id = Some(frame.session_id);
                }
                let own = self.own_session_id.unwrap();

                if self.frame_timestamps.len() >= FPS_TIMESTAMPS_CAPACITY {
                    self.frame_timestamps.pop_front();
                }
                self.frame_timestamps
                    .push_back((frame.session_id, frame.when_delivered_ns));

                // The active session is the first non-own session seen in the last second.
                // If no foreign session exists yet, fall back to own (no game running).
                let one_sec_ago = frame.when_delivered_ns.saturating_sub(1_000_000_000);
                let active_session = self
                    .frame_timestamps
                    .iter()
                    .rev()
                    .find(|&&(sid, ts)| sid != own && ts > one_sec_ago)
                    .map(|&(sid, _)| sid)
                    .unwrap_or(own);

                // FPS counter: frames from the active session in the last second.
                let fps_count = self
                    .frame_timestamps
                    .iter()
                    .filter(|&&(sid, ts)| sid == active_session && ts > one_sec_ago)
                    .count();
                self.fps_current = Some(fps_count as f32);

                // All metrics below are only for the active (game) session.
                if frame.session_id != active_session {
                    return;
                }

                if frame.predicted_display_period_ns > 0 {
                    self.display_period_ms =
                        Some(frame.predicted_display_period_ns as f32 / 1_000_000.0);
                }

                if frame.discarded {
                    // Push a sentinel (-1) into both graph queues so the drop appears
                    // as a distinct full-height red bar rather than a gap.
                    Self::push_sample(&mut self.cpu_ms, &mut self.cpu_ms_count, -1.0);
                    Self::push_sample(&mut self.gpu_ms, &mut self.gpu_ms_count, -1.0);
                    self.dropped_frames = self.dropped_frames.saturating_add(1);
                } else {
                    // CPU: xrWaitFrame return → xrEndFrame, i.e. the app's whole
                    // CPU frame, which is also what WiVRn's pacer itself uses.
                    // Measuring from when_begin_ns (xrBeginFrame) instead misses
                    // everything an engine does between waking and beginning the
                    // frame — for engines that simulate before xrBeginFrame that
                    // is most of the work, so the graph reads plausible but low.
                    //
                    // Both stamps are taken server-side as the IPC calls are
                    // handled, so this reads ~0.5ms above what an in-process
                    // profiler reports. That gap is the IPC hop, not the app.
                    if frame.when_wait_woke_ns > 0
                        && frame.when_delivered_ns > frame.when_wait_woke_ns
                    {
                        let cpu_ms = frame
                            .when_delivered_ns
                            .saturating_sub(frame.when_wait_woke_ns)
                            as f32
                            / 1_000_000.0;
                        self.latest_cpu_ms = Some(cpu_ms);
                        Self::push_sample(&mut self.cpu_ms, &mut self.cpu_ms_count, cpu_ms);
                    }
                    // GPU: xrEndFrame → GPU fence.
                    if frame.when_gpu_done_ns > frame.when_delivered_ns {
                        let gpu_ms = frame
                            .when_gpu_done_ns
                            .saturating_sub(frame.when_delivered_ns)
                            as f32
                            / 1_000_000.0;
                        self.latest_gpu_ms = Some(gpu_ms);
                        Self::push_sample(&mut self.gpu_ms, &mut self.gpu_ms_count, gpu_ms);
                    }
                }
            }
            record::Record::SystemPresentInfo(info) => {
                // Placeholder wiring only: WiVRn never emits this record (see
                // NETWORK_METRICS_PLAN.md), so the NET graph stays empty until a
                // WiVRn-side writer exists. present_margin_ns is compositor
                // present margin, not transport timing, even when it does arrive.
                if info.present_margin_ns > 0 {
                    let net_ms = info.present_margin_ns as f32 / 1_000_000.0;
                    self.latest_net_ms = Some(net_ms);
                    Self::push_sample(&mut self.net_ms, &mut self.net_ms_count, net_ms);
                }
            }
            _ => {}
        }
    }

    pub fn take_dashboard_records(&mut self) -> Vec<proto::Record> {
        self.dashboard_records.drain(..).collect()
    }
}

impl MonadoState {
    pub fn new() -> anyhow::Result<Self> {
        let ipc = libmonado::Monado::auto_connect().map_err(|s| anyhow::anyhow!("{s}"))?;
        let res = Self {
            ipc,
            #[cfg(feature = "feat-monado-metrics")]
            metrics: None,
            #[cfg(feature = "feat-monado-metrics")]
            watch_metrics: MonadoWatchMetrics::default(),
        };
        Ok(res)
    }

    #[allow(clippy::missing_const_for_fn)]
    #[allow(clippy::unused_self)]
    pub fn update(&mut self) {
        #[cfg(feature = "feat-monado-metrics")]
        if let Some(metrics) = &mut self.metrics {
            metrics.update();

            for record in metrics.dump_records() {
                self.watch_metrics.ingest(&record);
                self.watch_metrics.push_record(record);
            }
        }
    }

    #[allow(clippy::missing_const_for_fn)]
    #[allow(clippy::unused_self)]
    #[allow(clippy::unnecessary_wraps)]
    #[cfg(feature = "feat-monado-metrics")]
    pub fn set_metrics_enabled(&mut self, enabled: bool) -> anyhow::Result<()> {
        #[cfg(feature = "feat-monado-metrics")]
        {
            if enabled {
                if self.metrics.is_none() {
                    log::info!("Starting Monado metrics");
                    self.metrics = Some(monado_metrics::metrics_fd::MonadoMetricsFd::new(
                        &mut self.ipc,
                    )?);
                }
            } else {
                if self.metrics.is_some() {
                    log::info!("Stopping Monado metrics");
                }
                self.metrics = None;
            }
        }
        #[cfg(not(feature = "feat-monado-metrics"))]
        {
            #[allow(path_statements)]
            enabled;
        }

        Ok(())
    }
}
