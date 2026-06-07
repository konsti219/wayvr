// Maintains the native-messaging connection to the wayvr bridge and relays:
//   content script  -> native:  {type:"state", ...}   (playback state)
//   native -> content script:   {type:"cmd", ...}      (control commands)
//
// The native host exits whenever wayvr isn't reachable, so we reconnect on
// disconnect with a fixed backoff.

const HOST = "dev.wayvr.ytmusic";
const RECONNECT_MS = 3000;

let port = null;
let reconnectTimer = null;

function scheduleReconnect() {
  if (reconnectTimer !== null) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connect();
  }, RECONNECT_MS);
}

function connect() {
  try {
    port = browser.runtime.connectNative(HOST);
  } catch (err) {
    console.warn("wayvr: connectNative failed:", err);
    port = null;
    scheduleReconnect();
    return;
  }

  port.onMessage.addListener((msg) => {
    // Forward the command to the YT Music tab(s).
    browser.tabs
      .query({ url: "*://music.youtube.com/*" })
      .then((tabs) => {
        for (const tab of tabs) {
          browser.tabs.sendMessage(tab.id, msg).catch(() => { });
        }
      });
  });

  port.onDisconnect.addListener(() => {
    if (port && port.error) console.warn("wayvr: native port error:", port.error);
    port = null;
    scheduleReconnect();
  });
}

// Playback state coming from the content script -> native host.
browser.runtime.onMessage.addListener((msg) => {
  if (!port) return;
  try {
    port.postMessage(msg);
  } catch (err) {
    console.warn("wayvr: postMessage failed:", err);
    port = null;
    scheduleReconnect();
  }
});

connect();
