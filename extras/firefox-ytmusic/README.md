# WayVR YouTube Music bridge

Shows the current YouTube Music track on the WayVR watch and lets the watch's
play/pause and next buttons control that one Firefox tab — sidestepping
playerctl/MPRIS, which lump every Firefox tab into a single player.

## Pieces

```
content.js (music.youtube.com tab)
   │  reads title/artist/play-state from the player DOM, clicks its buttons
background.js (extension)
   │  browser.runtime.connectNative("dev.wayvr.ytmusic")
wayvr-media-bridge (native messaging host)
   │  native-messaging stdio  <->  wayvr IPC socket  (/tmp/wayvr_ipc.sock, abstract ns)
wayvr (existing IPC server -> watch overlay)
```

The bridge connects to wayvr's existing IPC socket (the same one the dashboard
uses) rather than a dedicated socket: it does the wayvr-ipc handshake, then
sends `PacketClient::WatchMediaState` and receives `PacketServer::WatchMediaCommand`.
Playback state lands in `app.watch_data.media`; the watch falls back to "No media"
if no update arrives for 3 s.

- `extension/` — the Firefox add-on (manifest v2 + background + content script).
- `native-host/dev.wayvr.ytmusic.json` — native-messaging host manifest template
  (`@BRIDGE_PATH@` is replaced with the absolute path to the built bridge binary).
- The bridge binary itself is the `wayvr-media-bridge` workspace crate
  (`cargo build -p wayvr-media-bridge`, output at `target/<profile>/wayvr-media-bridge`).

## Install (manual)

1. Build the bridge:

   ```sh
   cargo build --release -p wayvr-media-bridge
   ```

2. Install the native-messaging host manifest with the binary's absolute path:

   ```sh
   mkdir -p ~/.mozilla/native-messaging-hosts
   sed "s#@BRIDGE_PATH@#$PWD/target/release/wayvr-media-bridge#" \
     extras/firefox-ytmusic/native-host/dev.wayvr.ytmusic.json \
     > ~/.mozilla/native-messaging-hosts/dev.wayvr.ytmusic.json
   ```

3. Load the extension. For development: `about:debugging#/runtime/this-firefox`
   → "Load Temporary Add-on" → pick `extension/manifest.json`. (Temporary add-ons
   are removed on Firefox restart; for a permanent install, sign/package it or use
   an unbranded/Developer/Nightly build with `xpinstall.signatures.required=false`.)

4. Start wayvr, open <https://music.youtube.com>, play something. The watch should
   show the track and the buttons should control it.

The native host's name (`dev.wayvr.ytmusic`) and the extension id
(`wayvr-ytmusic@konsti`) must stay in sync across `background.js`, the host
manifest's `allowed_extensions`, and `manifest.json`'s `gecko.id`.

## Install (NixOS)

The wayvr flake exposes two packages for this:

- `media-bridge` — the bridge binary plus the native-messaging host manifest
  (installed under `lib/mozilla/native-messaging-hosts/`), with `@BRIDGE_PATH@`
  already substituted to the built binary.
- `ytmusic-extension` — the add-on packed as an **unsigned** `.xpi`.

Register the host (this part works on stock branded Firefox):

```nix
programs.firefox.nativeMessagingHosts.packages = [
  inputs.wayvr.packages.${pkgs.system}.media-bridge
];
```

Branded Firefox refuses unsigned extensions, so the XPI must be self-signed once
via your Mozilla AMO account, then force-installed:

```sh
nix build <wayvr-flake>#ytmusic-extension      # produces result/wayvr-ytmusic@konsti.xpi
# AMO API credentials from https://addons.mozilla.org/developers/addon/api/key/
web-ext sign --channel=unlisted \
  --source-dir <extension-dir> \
  --api-key "$AMO_JWT_ISSUER" --api-secret "$AMO_JWT_SECRET"
# drop the signed .xpi into your nix-config, e.g. nixos/firefox/wayvr-ytmusic.xpi
```

```nix
# force-install the signed xpi; guarded so the config still evaluates before it exists
programs.firefox.policies.ExtensionSettings =
  lib.optionalAttrs (builtins.pathExists ./firefox/wayvr-ytmusic.xpi) {
    "wayvr-ytmusic@konsti" = {
      installation_mode = "force_installed";
      install_url = "file://${./firefox/wayvr-ytmusic.xpi}";
    };
  };
```

Re-sign and replace the vendored `.xpi` whenever the extension changes (bump
`version` in `manifest.json` so Firefox picks up the update).

## Troubleshooting

- **Watch shows "No media" with a tab playing** — the bridge isn't connected.
  Check that wayvr is running (its IPC socket is the abstract-namespace
  `/tmp/wayvr_ipc.sock`, shown as `@/tmp/wayvr_ipc.sock` in `ss -x` — it is *not*
  a file on disk) and that the host manifest path points at the real binary. The
  bridge logs to stderr, visible in Firefox's `about:debugging` → extension →
  "Inspect" console via `port.onDisconnect`.
- **Buttons do nothing** — YT Music likely changed its DOM; update the selectors
  in `content.js` (`#play-pause-button`, `.next-button`, `.title`, `.byline`).
- **Wrong/!empty artist** — the byline format changed; adjust the `split("•")`
  in `content.js`.
