# Building Cooliris (Rust / wgpu)

The app is a native wgpu wall. The `video` feature (on by default) links **libmpv** for
video/audio playback; **ffmpeg** is used at runtime as a subprocess for thumbnails (video poster
frames, animated‑GIF first frame, audio cover art).

```
cd rust
cargo run --release -- /path/to/media/folder     # or no path → placeholder tiles
```

---

## Linux (native)

**Prerequisites**
- Rust (stable) — `rustc`/`cargo`
- `libmpv` + `pkg-config` (build links libmpv via pkg-config) — e.g. Arch: `pacman -S mpv pkgconf`
- `ffmpeg` on `PATH` (runtime, for thumbnails)

**Build**
```
cd rust
cargo build --release                      # wall + video
cargo build --release --no-default-features # wall only (no libmpv needed)
```
Binary: `target/release/cooliris-rs`.

---

## Windows (cross-compiled from Linux)

This is the exact setup used to produce `cooliris-rs.exe`. It targets `x86_64-pc-windows-gnu`
(MinGW) and links Windows libmpv via an import library — **no MSVC / no Windows machine needed.**

### 1. Rust target
```
rustup target add x86_64-pc-windows-gnu
```
(If `rustup` isn't installed: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal`)

### 2. MinGW cross-compiler (linker + C compiler for `mimalloc`)
- **Arch / CachyOS:** `sudo pacman -S --needed mingw-w64-gcc`
- **Fallback (no root):** download a self-contained toolchain and put its `bin/` on `PATH`:
  ```
  # llvm-mingw — Linux-hosted, targets Windows (clang-based, gcc-compatible wrappers)
  curl -L https://github.com/mstorsjo/llvm-mingw/releases/latest/download/<asset>-ubuntu-*-x86_64.tar.xz | tar xJ
  export PATH="$PWD/llvm-mingw-*/bin:$PATH"
  ```
Verify: `x86_64-w64-mingw32-gcc --version`. Cargo auto-uses it as the linker and as the C
compiler for `mimalloc` — no `.cargo/config` needed.

### 3. Windows libmpv (only for the `video` feature)
Grab the **mpv dev** package (import lib + dll + headers); shinchiro's monolithic build bundles
ffmpeg/codecs inside `libmpv-2.dll`:
```
# from https://github.com/shinchiro/mpv-winbuild-cmake/releases  (mpv-dev-x86_64-*.7z)
7z x mpv-dev-x86_64-*.7z -o mpv-dev
# mpv-dev/ now has: libmpv.dll.a  libmpv-2.dll  include/mpv/*.h
```
`build.rs` links it via the **`MPV_LIB_DIR`** env var (folder containing `libmpv.dll.a`).

### 4. Build
```
cd rust
MPV_LIB_DIR=/path/to/mpv-dev cargo build --release --target x86_64-pc-windows-gnu
```
Wall-only (no libmpv, skip step 3): add `--no-default-features` and drop `MPV_LIB_DIR`.

Binary: `target/x86_64-pc-windows-gnu/release/cooliris-rs.exe`.

### 5. Bundle for the user
Windows loads DLLs from the executable's own folder, so ship them together:
```
cooliris-rs.exe
libmpv-2.dll          # from mpv-dev/ — REQUIRED next to the exe for video/audio
ffmpeg.exe            # optional, on PATH or beside the exe — needed for thumbnails
```
Without `libmpv-2.dll` beside the exe you get *"libmpv-2.dll can't be found"*. Without `ffmpeg.exe`
thumbnails fall back to placeholders (images + playback still work).

---

## Runtime: GPU backend

The app logs the chosen GPU/backend at startup:
```
GPU: <name> | backend <Dx12/Vulkan/Gl> | type <DiscreteGpu/IntegratedGpu/Cpu> | driver ...
```
- **Windows defaults to DX12** (wgpu's Vulkan path is much slower on some NVIDIA setups; GL glitches
  array textures).
- Override to compare: `WGPU_BACKEND=dx12|vulkan|gl` (Windows: `set WGPU_BACKEND=dx12`).
- `type Cpu` / "Microsoft Basic Render Driver" → no usable GPU driver (that's the cause of low fps).
