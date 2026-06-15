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

A small toolbar shows an **Open** button (top-left) and the **item count** with a **loading bar**
while tiles decode; open a folder with the button, by **drag-and-drop**, or the **O** key. (No
folder dialog is forced at startup — the wall opens straight away with the Open hint.)

Controls — **wall:** **wheel** zoom (toward the cursor) · **left-drag** scroll · **right/middle-drag**
pan · **bottom bar** scrub · **edge arrows** / **←→** scroll · click a tile to focus. **Lightbox:**
**wheel** zoom the photo · **drag** pan when zoomed · **‹ ›** / **←→** prev/next · **Esc** back.
**Video** (needs `--features video`): an on-screen controls bar (play/pause · click/drag the
**seek bar**, hover it for a time tooltip · **Audio** / **Subs** track cycle · **Full**) that
auto-hides when idle; keys **Space** play/pause · **A** audio · **S** subtitles. **F** fullscreen ·
**O** / Open button / drag-and-drop to load a folder · **Esc** back / exit fullscreen / quit.

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
10. **✅ Video, integrated** — video files appear as play tiles; **focus one and libmpv plays it
    in place** over the tile (software render API → wgpu texture → a quad via the wall camera).
    **No second window, no GL/Vulkan interop** — the exact thing that was painful in the Electron
    embed. Opt-in behind `--features video`; the wall builds with no libmpv dependency otherwise.
    Standalone proof too: `cargo run --features video --bin video -- clip.mp4`.
11. **✅ Folder picker** (`rfd`) — runs with no CLI path; a native dialog chooses the folder.
    _(Per-OS installers via `cargo-bundle` are the remaining packaging step.)_

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
cargo run --release                          # folder picker; video plays on focus (libmpv, default)
cargo run --release -- /path/to/media        # …or pass a folder (skips the picker)
cargo run --release --no-default-features -- /media  # wall-only build for machines without libmpv

# A shippable optimized binary:
cargo build --release                        # → target/release/cooliris-rs
```

Video is a **default feature** now (libmpv linked via pkg-config); use `--no-default-features`
to build the wall-only variant where libmpv isn't installed.

Run with no folder and a native picker appears (cancel → placeholder tiles). Cross-platform:
Windows (DX12/Vulkan), macOS (Metal), Linux (Vulkan). First build compiles wgpu (~2–3 min);
afterwards it's incremental. The `video` feature links libmpv (via pkg-config); the default wall
build has no such dependency. (Per-OS installers via `cargo-bundle` are the remaining packaging
step.)

Logging: `RUST_LOG=cooliris_rs=info cargo run` (the wgpu backends are chatty at `info`; the
default filter keeps just our logs).

## Opening media

The toolbar **Open** button opens a dialog with four ways in:

- **Choose files…** — pick one or more images / videos / audio files.
- **Choose folder…** — pick a folder; it's scanned recursively (subfolders included).
- **Drag & drop** — with the dialog open, drop a folder or files onto its drop zone.
- **From JSON…** — load a JSON manifest that lists media paths (see below).

You can also pass a folder on the CLI (`cargo run --release -- /path/to/media`) or press **O**.

> Drag-and-drop delivery is handled by the OS/compositor. It works on Windows and X11. On some
> Wayland compositors winit doesn't deliver drop events — if a drop does nothing, run with
> `RUST_LOG=cooliris_rs=info` (you'll see `drag hover:` / `dropped:` lines if events arrive), and
> as a fallback launch under X11 with `WINIT_UNIX_BACKEND=x11 ./cooliris-rs`.

### From JSON… (manifest format)

"From JSON…" reads a `.json` file and loads **every string anywhere in it that resolves to an
existing local media file**. The parser is deliberately lenient, so all of these work:

```jsonc
// 1) a bare array of paths
["a/cat.gif", "b/clip.mp4", "/abs/photo.jpg"]

// 2) an array of objects — the key name doesn't matter (path, src, file, url, …)
[ { "path": "a/cat.gif" }, { "src": "b/clip.mp4", "caption": "ignored" } ]

// 3) a nested feed — strings are collected from anywhere in the tree
{ "title": "My album", "items": [ { "src": "a/cat.gif" } ], "extras": ["b/clip.mp4"] }
```

Rules:

- **Relative paths** resolve against the JSON file's own folder; absolute paths are used as-is.
- Only **existing files with a known media extension** are kept (non-media strings like titles,
  dates or captions are ignored), de-duplicated, in first-seen order.
- **`http(s)://` URLs are skipped** — this is a local-file wall, not a web fetcher.

Ready-to-run examples live in [`examples/`](examples/) and load the bundled sample tiles
(`libmpv/public/samples/*.svg`) via relative paths, so you can try the feature immediately:

```sh
cargo run --release        # then: Open → From JSON… → pick rust/examples/manifest-paths.json
```

- [`examples/manifest-paths.json`](examples/manifest-paths.json) — bare array of paths (all 18 tiles).
- [`examples/manifest-objects.json`](examples/manifest-objects.json) — array of objects (mixed keys).
- [`examples/manifest-feed.json`](examples/manifest-feed.json) — nested feed (note the remote URL is skipped).
