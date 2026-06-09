// mpv host — runs under SYSTEM Node (forked by the main process), NOT the Electron
// binary. So it never loads Chromium's cut-down libffmpeg.so; libmpv uses the full
// system ffmpeg directly (all codecs/subtitles), with no LD_PRELOAD hacks. Talks to the
// main process over Node IPC (advanced serialization → Buffers pass as binary).
// Plain CommonJS — run directly, not bundled.
const path = require("node:path");

let player = null;
try {
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

process.on("message", (m) => {
  const { id, fn, args } = m || {};
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
  try {
    process.send({ id, result });
  } catch {
    /* channel closed */
  }
});
