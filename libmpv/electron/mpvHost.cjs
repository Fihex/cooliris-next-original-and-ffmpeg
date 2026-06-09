// mpv host — runs in an Electron utilityProcess (a plain Node process). Crucially it does
// NOT load Chromium's cut-down libffmpeg.so, so libmpv uses the full system ffmpeg (all
// codecs / subtitles). The main process talks to it over parentPort; frames come back as
// transferred ArrayBuffers. Keep this file plain CommonJS — it's run directly, not bundled.
const path = require("node:path");

let player = null;
try {
  // dev: <root>/native/...   packaged: app.asar.unpacked/native/...
  const candidates = [
    path.join(__dirname, "..", "native", "build", "Release", "mpv.node"),
    path.join(process.resourcesPath || "", "app.asar.unpacked", "native", "build", "Release", "mpv.node"),
  ];
  let addon = null;
  for (const c of candidates) {
    try {
      addon = require(c);
      break;
    } catch {
      /* try next */
    }
  }
  if (!addon) throw new Error("mpv.node not found");
  player = new addon.MpvPlayer();
} catch (e) {
  console.error("[mpv-host] init failed:", e && e.message);
}

process.parentPort.on("message", (e) => {
  const { id, fn, args } = e.data || {};
  let result = null;
  try {
    if (!player) throw new Error("mpv unavailable");
    switch (fn) {
      case "load": player.command(["loadfile", args[0]]); break;
      case "cmd": result = player.command(args[0]); break;
      case "set": result = player.setProperty(args[0], String(args[1])); break;
      case "get": result = player.getProperty(args[0]); break;
      case "size": result = player.videoSize(); break;
      case "frame": result = player.renderFrame(args[0], args[1]); break;
      case "stop": player.command(["stop"]); break;
    }
  } catch {
    result = null;
  }
  // Transfer the frame's backing buffer (large, dedicated) to avoid a copy.
  const transfer = result && result.buffer instanceof ArrayBuffer ? [result.buffer] : [];
  process.parentPort.postMessage({ id, result }, transfer);
});
