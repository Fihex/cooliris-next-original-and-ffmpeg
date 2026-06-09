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
- [ ] **M3 — renderer integration**: paint frames in the lightbox; wire play/pause/seek/
      volume; replace the `<video>` player.
- [ ] **M4 — tracks**: audio + subtitle track lists and switching (mpv properties; subs
      are burned into the frame by mpv — no overlay needed).
- [ ] **M5 — packaging**: bundle `libmpv` per OS (Win `libmpv-2.dll`, Linux `libmpv.so`,
      mac `libmpv.dylib`); rebuild the addon against Electron's ABI (electron-rebuild);
      build installers.

## Build notes
- Native build needs: a C/C++ toolchain, `python`, `node-gyp`, and **libmpv + headers**
  (`pkg-config --exists mpv`). On Arch/CachyOS: `pacman -S mpv`.
- Build the addon: `cd native && npx node-gyp rebuild`.
- The addon is currently built against system Node for development; for the packaged app
  it must be rebuilt against Electron's headers (`electron-rebuild`).
