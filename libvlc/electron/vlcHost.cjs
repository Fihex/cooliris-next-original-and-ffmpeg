// vlc host — runs under SYSTEM Node (forked by the main process), NOT the Electron
// binary, so libVLC and its plugins load cleanly with no Chromium conflicts. Talks to
// the main process over Node IPC (advanced serialization → Buffers pass as binary).
//
// libVLC 3 quirk: subtitle style (freetype options) is creation-time only. The "style"
// op therefore RECREATES the player with the new options and restores the current
// file, position, tracks, and pause state — a brief reload, the best VLC 3 allows.
// Plain CommonJS — run directly, not bundled.
const path = require("node:path");

let addon = null;
try {
  const candidates = [
    path.join(__dirname, "..", "native", "build", "Release", "vlc.node"),
    path.join(process.resourcesPath || "", "app.asar.unpacked", "native", "build", "Release", "vlc.node"),
  ];
  for (const c of candidates) {
    try {
      addon = require(c);
      break;
    } catch {
      /* try next */
    }
  }
  if (!addon) throw new Error("vlc.node not found");
} catch (e) {
  console.error("[vlc-host] init failed:", e && e.message);
}

let player = null;
let styleArgs = []; // VLC options applied at player creation (subtitle style)
let currentPath = null;
let renderWidth = ""; // sticky across recreates
let restoreTimer = null;

function makePlayer() {
  if (!addon) return null;
  try {
    player = new addon.VlcPlayer(styleArgs);
    if (renderWidth) player.setProperty("render-width", renderWidth);
  } catch (e) {
    console.error("[vlc-host] player create failed:", e && e.message);
    player = null;
  }
  return player;
}
if (addon) makePlayer();

// Recreate the player with new style args, reloading the current file at its position.
function applyStyle(args) {
  const next = Array.isArray(args) ? args.map(String) : [];
  // No-op if the style is unchanged — avoids a needless reload/freeze when a control
  // re-emits the same value (e.g. re-opening the menu or re-picking the same color).
  if (next.length === styleArgs.length && next.every((a, i) => a === styleArgs[i])) {
    return !!player;
  }
  styleArgs = next;
  const pos = player ? parseFloat(player.getProperty("time-pos") || "0") : 0;
  const aid = player ? player.getProperty("aid") : null;
  const sid = player ? player.getProperty("sid") : null;
  const paused = player ? player.getProperty("pause") : "no";
  if (restoreTimer) clearInterval(restoreTimer);
  try {
    if (player) player.destroy();
  } catch {
    /* ignore */
  }
  player = null;
  if (!makePlayer() || !currentPath) return !!player;

  player.command(["loadfile", currentPath]);
  const t0 = Date.now();
  restoreTimer = setInterval(() => {
    if (!player) return clearInterval(restoreTimer);
    const dur = parseFloat(player.getProperty("duration") || "0");
    if (dur > 0 || Date.now() - t0 > 5000) {
      clearInterval(restoreTimer);
      restoreTimer = null;
      if (pos > 0.5) player.command(["seek", String(pos), "absolute"]);
      if (aid) player.setProperty("aid", aid);
      player.setProperty("sid", sid && sid !== "no" ? sid : "no");
      if (paused === "yes") player.setProperty("pause", "yes");
    }
  }, 150);
  return true;
}

process.on("message", (m) => {
  const { id, fn, args } = m || {};
  let result = null;
  try {
    if (fn === "style") {
      result = applyStyle(args[0]);
    } else {
      if (!player) throw new Error("vlc unavailable");
      switch (fn) {
        case "load":
          currentPath = args[0];
          player.command(["loadfile", args[0]]);
          break;
        case "cmd": result = player.command(args[0]); break;
        case "set":
          if (args[0] === "render-width") renderWidth = String(args[1]);
          result = player.setProperty(args[0], String(args[1]));
          break;
        case "get": result = player.getProperty(args[0]); break;
        case "size": result = player.videoSize(); break;
        case "frame": result = player.renderFrame(args[0], args[1]); break;
        case "stop": player.command(["stop"]); break;
      }
    }
  } catch {
    result = null;
  }
  try {
    process.send({ id, result });
  } catch {
    /* channel closed */
  }
});
