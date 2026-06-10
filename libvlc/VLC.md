# Cooliris Next — libVLC edition

Same goal and architecture as the `libmpv` edition — play **every** format **instantly**
(no transcoding) by decoding natively and painting frames into a `<canvas>` — but with
**libVLC 3** as the engine instead of libmpv. The main practical difference: VideoLAN
ships **official, self-contained binaries for every OS**, which makes packaging
(especially Windows) much simpler than libmpv's hand-bundled runtime.

## Architecture
- **Native addon** (`native/vlc_addon.cc`, N-API): owns a `libvlc_media_player` and
  exposes the *same interface the mpv addon had* — `command()`, `set/getProperty()` with
  mpv property names (`time-pos`, `duration`, `pause`, `aid`, `sid`, `track-list/*`),
  `videoSize()`, `renderFrame()` — implemented as a shim over libVLC. Video goes through
  the **vmem callbacks**: VLC decodes + scales each frame (subtitles composited) into an
  RGBA buffer capped at 1280 wide; `renderFrame()` hands a copy to JS.
- **Process isolation** (`electron/vlcHost.cjs` + `electron/vlc.ts`): libVLC runs in a
  forked **system-Node** process — never the Electron binary — so it and its plugins
  load with no Chromium library conflicts. Same IPC proxy/crash-guard design as libmpv.
- **Renderer** (`src/components/VlcPlayer.tsx`): canvas frame pump (~30fps), controls,
  ±10s, audio-language menu, subtitle menu. Subs are forced **off** on load (VLC
  auto-enables them) until chosen.

## Differences vs the libmpv edition
- **Subtitle styling is selection-only.** libVLC 3 has no runtime equivalents of mpv's
  `sub-font-size` / `sub-color` / `sub-back-color`, so the size/color/background pickers
  don't exist here. Track selection (audio + subs) works the same.
- **Track labels** come from VLC's descriptions (a display name; no separate language
  field).
- **Volume** is 0–100 (mpv allowed up to 130).
- **Packaging needs VLC's plugin tree** (`vendor/vlc-plugins` + `VLC_PLUGIN_PATH`) — the
  plugins are the actual demuxers/decoders. On Windows the official VLC zip provides
  everything including import libs (no lib.exe step like libmpv needed).

## Status
- [x] Native addon builds against libVLC 3 (pkg-config) and renders frames + tracks +
      properties (verified under system Node).
- [x] Host/proxy/renderer wired (mpv-shim protocol unchanged).
- [ ] Desktop run-through (video/audio/tracks in the app) — needs an
      `npm run electron:preview` test.
- [ ] Packaged self-contained Linux build (bundle script written; needs a runtime test).
- [ ] Windows build (scripts ready; must be built on Windows — see BUILD.md).
