# Cooliris Next — libmpv edition (Option C)

Goal: play **every** format **instantly** (mkv/avi/HEVC/AC-3/DTS, all audio + subtitle
tracks) with **no transcoding**, fully integrated into the 3D wall — by decoding with
**libmpv** and rendering frames into a texture, instead of Chromium's `<video>`.

Based on the `original` edition (no ffmpeg layer). Replaces the `<video>`-based player
with a libmpv-backed one.

## Why libmpv
Chromium only decodes H.264/VP9/AV1 + a few audio codecs and can't demux mkv/avi. libmpv
(bundles ffmpeg) decodes/renders everything itself — no convert step, hardware-accelerated,
and it even renders subtitles into the frame for us.

## Architecture
- **Native addon** (`native/`, N-API + libmpv) — owns the mpv instance and exposes
  control (command / get / set property) and, via the render API, decoded RGBA frames.
- **Renderer** — uploads frames to a GL/canvas texture (the lightbox first, then wall
  tiles) and drives playback/seek/volume/track selection through the addon.
- Frame path uses the **software render API** (`MPV_RENDER_API_TYPE_SW`): mpv renders the
  finished frame (video + subs) into a CPU buffer we hand to a texture — cross-platform,
  no GL-context interop required. (Decode stays hardware-accelerated.)

## Roadmap
- [x] **M1 — native binding**: create/initialise mpv, `command()`, `get/setProperty()`.
      Verified: loads libmpv 2.5 / mpv 0.41, controls a player from Node.
- [x] **M2 — frames**: SW render API → `renderFrame(w,h)` returns the composited RGBA
      frame, `videoSize()` the dimensions, `hwdec=auto-safe`. Verified: decoded an mkv
      to a real (non-black) frame with no transcode.
- [x] **M3 — renderer integration**: mpv runs in a forked **system-Node** process
      (`electron/mpvHost.cjs`) so it never touches Electron's cut-down libffmpeg and uses
      the full system ffmpeg (all codecs). `electron/mpv.ts` proxies over Node IPC;
      `MpvPlayer.tsx` paints frames into a <canvas> (~30fps, capped 1280w) with play/pause/
      seek/volume + keyboard. Verified on desktop: mkv + avi play with video + audio.
- [x] **M4 — tracks**: audio-language + subtitle menus from mpv's `track-list`; switching
      sets `aid`/`sid` live (mpv composites subs into the frame). Subs off by default,
      transparent background; ±10s buttons.
- [x] **M5 — packaging (Linux, self-contained)**: `scripts/bundle-linux.sh` collects a
      Node binary + libmpv and its shared-lib deps (166 libs) into `vendor/`;
      electron-builder ships it as extraResources + asarUnpacks the addon/host. At runtime
      the host runs under `vendor/node` with `LD_LIBRARY_PATH=vendor/lib`, so **no system
      Node or mpv is required**. Linux AppImage builds (~232 MB). Build order:
      `bash scripts/bundle-linux.sh && ELECTRON=1 npm run build && npx electron-builder --linux AppImage`.
      _Needs a runtime test on a clean machine to confirm the bundled lib set is complete._
- [ ] **M5 — Windows / macOS**: same idea per OS — bundle Node + a self-contained libmpv
      (Windows `libmpv-2.dll` from shinchiro builds is self-contained → easiest; macOS
      needs a `libmpv.dylib` + deps via @rpath, built on a Mac). Each must build the addon
      against the bundled Node's ABI on that platform.

## Build notes
- Native build needs: a C/C++ toolchain, `python`, `node-gyp`, and **libmpv + headers**
  (`pkg-config --exists mpv`). On Arch/CachyOS: `pacman -S mpv`.
- Build the addon: `cd native && npx node-gyp rebuild`.
- The addon is currently built against system Node for development; for the packaged app
  it must be rebuilt against Electron's headers (`electron-rebuild`).
