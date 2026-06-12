# Cooliris (Rust / wgpu)

A native rebuild of the Cooliris 3D media wall in Rust + [wgpu](https://wgpu.rs) — the same
scrolling 3D photo/video wall as the Electron editions, but without a browser anywhere in the
stack.

## Why a rewrite

Every memory problem in the Electron build came from embedding Chromium: the protocol layer
retaining image bytes in the browser process, opaque GC, a ~150 MB baseline before any content.
A native GPU app removes all of that:

- **We own every texture's lifetime** — upload, evict and free on our schedule; no protocol, no
  HTTP cache, no GC guessing.
- **Decode off-thread** in a worker pool straight into GPU uploads.
- **Idle in the tens of MB**, not hundreds.
- **mpv embeds natively** via its render API (no `--wid` child-window contortions).
- **One binary per OS** (Vulkan / Metal / DX12), a few MB instead of 100 MB+.

## Status

A **working, virtualized, streamed wall**: point it at a folder and it scrolls a 3-row grid of
your photos at their true aspect ratio with a perspective camera that banks as you scroll;
left-click a tile to fly in and focus it, Esc to return. Tiles decode on a worker-thread pool
and stream in around the camera; a fixed pool of texture-array layers is recycled as you scroll,
so **GPU/CPU stay flat no matter how large the library** (verified: 120 tiles → residency capped
at the ~99-tile window, layers recycled, never exhausted, zero panics). Next up is a touch more
polish (full-res focus, reflections) and the libmpv video layer.

Controls: **mouse wheel / ←→** scroll · **left-click** focus a tile · **Esc** back / quit.

## Roadmap (each step is a runnable milestone)

1. **✅ Window + GPU surface** — winit 0.30 + wgpu, sRGB swapchain, resize handling.
2. **✅ Textured quad / tile shader** — texture + sampler + WGSL.
3. **✅ Instanced tile grid** — one draw call, per-tile instance buffer, 3-row column-major layout.
4. **✅ Camera + scroll** — perspective camera panning the wall; wheel + ←/→ with accel/damp/inertia.
5. **✅ Virtualization + eviction** — only tiles in a window around the view are resident; layers
   freed + recycled on scroll-out. The bounded-memory guarantee, by construction.
6. **✅ Threaded streaming** — a worker pool reads files directly (no IPC), decodes + downscales,
   and uploads to free layers; throttled like the JS `MAX_INFLIGHT`. Handles 16k+ libraries.
7. **✅ Aspect-correct tiles** — each quad sized to its image, sampling the used sub-rect; the
   wall banks as you scroll.
8. **✅ Focus / lightbox (camera)** — left-click ray-picks a tile and the camera animates in to
   center on it; Esc / click returns. _(Full-resolution swap on focus is the next refinement —
   it currently shows the streamed thumbnail.)_
9. **Reflections, labels, scrubber** — the visual polish from the WebGL wall.
10. **✅ Video (proof)** — libmpv's **software render API** → a wgpu texture on a fullscreen quad.
    mpv decodes (HW-accelerated internally) and we composite it; **no second window, no GL/Vulkan
    interop** — the exact thing that was painful in the Electron embed. Verified frames flowing.
    Opt-in: `cargo run --features video --bin video -- clip.mp4`. Next: composite onto video
    tiles in the wall.
11. **Packaging** — `cargo-bundle` / per-OS installers; folder picker (`rfd`).

## Architecture (as it grows)

```
src/
  main.rs      event loop + App (winit ApplicationHandler), owns State   [now]
  state.rs     wgpu device/queue/surface, frame render                   [next: split out]
  wall.rs      tile layout, virtualization, eviction
  camera.rs    view/projection, scroll physics
  texture.rs   GPU texture upload + the resident-texture cache
  loader.rs    threaded file-read + decode + downscale pipeline
  video.rs     libmpv render-API integration
  shader.wgsl  tile shader (textured, instanced)
```

## Build & run

```sh
cd rust
cargo run --release -- /path/to/photos          # the wall (no libmpv needed)
cargo run --features video --bin video -- a.mp4  # video proof (needs libmpv installed)
```

Cross-platform: Windows (DX12/Vulkan), macOS (Metal), Linux (Vulkan). First build compiles wgpu
(~2–3 min); afterwards it's incremental. The `video` feature links libmpv (via pkg-config); the
default wall build has no such dependency.

Logging: `RUST_LOG=cooliris_rs=info cargo run` (the wgpu backends are chatty at `info`; the
default filter keeps just our logs).
