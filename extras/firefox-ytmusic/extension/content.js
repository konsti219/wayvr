// Runs in the music.youtube.com tab. Reads the current track + play state from
// the player DOM and reports changes to the background script; executes
// play/pause and next commands by clicking the player-bar buttons.
//
// Selectors target YouTube Music's <ytmusic-player-bar>. If YT Music changes
// its markup, these are the lines to update.

const SEL = {
  playPause: "#play-pause-button",
  next: "ytmusic-player-bar .next-button",
};

let lastSent = "";
function pushState() {
  let mediaSession = window.wrappedJSObject.navigator.mediaSession;
  const key = {
    type: "state",
    mediastate: {
      title: mediaSession.metadata.title,
      artist: mediaSession.metadata.artist,
      album: mediaSession.metadata.album,
      artwork: mediaSession.metadata.artwork[0]?.src,
      playing: mediaSession.playbackState === "playing",
    }
  };
  browser.runtime.sendMessage(key).catch(() => { });
}

// React quickly to play/pause; the interval covers track changes and is a
// cheap catch-all (pushState dedupes, so it only sends on real changes).
function hookVideo() {
  const video = document.querySelector("video");
  if (!video || video.dataset.wayvrHooked) return;
  console.log("wayvr: hooking video element");
  video.dataset.wayvrHooked = "1";
  video.addEventListener("play", pushState);
  video.addEventListener("pause", pushState);
}

setInterval(() => {
  hookVideo();
  pushState();
}, 1000);

// Control commands from the watch (via background -> native -> here).
browser.runtime.onMessage.addListener((msg) => {
  console.log("wayvr: got cmd", msg);
  if (!msg || msg.type !== "cmd") return;
  if (msg.action === "play_pause") {
    document.querySelector(SEL.playPause)?.click();
  } else if (msg.action === "next") {
    document.querySelector(SEL.next)?.click();
  }

  setTimeout(pushState, 100);
});
