# Building Cooliris Next — libmpv edition

This edition plays **every format instantly** (mkv/avi/HEVC/AC‑3/DTS, all audio +
subtitle tracks) with **no transcoding**, by decoding with **libmpv** and painting frames
into a `<canvas>`. mpv runs in its own Node process (never the Electron binary), so it
uses a full ffmpeg with no conflict. See `MPV.md` for the architecture.

Unlike the `ffmpeg` edition, this one has a **native C++ addon** (`native/mpv.node`) that
links libmpv — so building involves a compile step, and the runtime bundles libmpv.

## Prerequisites (all platforms)
- **Node.js 22+** and **npm**.
- A **C/C++ toolchain** + **python** + **node-gyp** (to build the native addon).

Per-platform, additionally:
- **Linux:** `libmpv` + headers + `pkg-config` (Arch/CachyOS: `sudo pacman -S mpv`;
  Debian/Ubuntu: `sudo apt install libmpv-dev pkg-config`).
- **Windows:** **Visual Studio Build Tools** (C++), a libmpv **dev** package
  (`mpv-dev-x86_64-*.7z` from sourceforge → *mpv-player-windows/libmpv*), and **7‑Zip**.

## 1. Install dependencies
```bash
npm install
```

## 2. Build for Linux  (self-contained AppImage)
```bash
# build the native addon for your Node
cd native && npx node-gyp rebuild && cd ..

# collect a self-contained runtime: a Node binary + libmpv and its deps → vendor/
bash scripts/bundle-linux.sh

# renderer + main, then package
ELECTRON=1 npm run build
npx electron-builder --linux AppImage          # → release/Cooliris Next-<ver>.AppImage
```
At runtime the app runs the mpv host under `vendor/node` with
`LD_LIBRARY_PATH=vendor/lib`, so **no system Node or mpv is required**.

> If a rebuild seems stale, clear caches: `rm -rf node_modules/.vite dist dist-electron`.

## 3. Build for Windows  (run **on Windows**, in a Developer prompt)
```powershell
# 1. download + extract a libmpv dev package (mpv-dev-x86_64-*.7z)
# 2. headers + import lib + dll:
pwsh scripts/setup-windows.ps1 -MpvDev C:\path\to\mpv-dev-x86_64-vXXXX
# 3. build the native addon (against your Node's ABI):
cd native; npx node-gyp rebuild; cd ..
# 4. self-contained runtime: vendor\{node.exe, libmpv-2.dll}
pwsh scripts/bundle-windows.ps1
# 5. renderer + main, then package
$env:ELECTRON=1; npm run build
npx electron-builder --win nsis                # → release\Cooliris Next Setup <ver>.exe
```
Windows `libmpv-2.dll` is self-contained (ffmpeg baked in), so the bundle is just
`node.exe` + that one DLL. At runtime `vendor\` is on `PATH` so the DLL loads.

## Unpacked builds (no installer — a folder you run directly)
Swap the installer target for `dir`. You still do the same addon build + `vendor/`
bundling first; only the final packaging differs (no AppImage/NSIS assembly, so it's
faster and Wine isn't involved at all):
```bash
# Linux  → release/linux-unpacked/  (run ./"cooliris-next")
ELECTRON=1 npm run build && npx electron-builder --linux dir
```
```powershell
# Windows (on Windows) → release\win-unpacked\  (run "Cooliris Next.exe")
$env:ELECTRON=1; npm run build; npx electron-builder --win dir
```
The unpacked folder contains everything the installer would (the `vendor/` runtime in
`resources/vendor`, the addon in `resources/app.asar.unpacked`), so it's fully
self-contained and portable — just copy the folder.

> The **Windows** unpacked build must still be produced **on Windows**: `--win dir` skips
> the installer/Wine step, but the native `mpv.node` inside it must be the Windows build
> (a Linux-made `--win dir` would contain the Linux addon and won't run on Windows).

## Why you can't cross-build the Windows version from Linux (even with Wine)

The **ffmpeg** edition *can* be cross-built from Linux with Wine — because that build has
**no compilation**: the renderer/main are plain JS, the ffmpeg binaries are pre-built and
just downloaded per OS, and electron-builder only uses Wine to **assemble the NSIS
installer** (zip the files into a `.exe`). Wine is enough to *run* that packager tool.

This **libmpv** edition is different: it contains a **native C++ addon** that must be
**compiled into a Windows `.node`** (linked against the Windows libmpv import library and
the **Windows Node ABI**). That needs a real **Windows C++ compiler (MSVC)** — and:

- **Wine is not a compiler.** Wine runs Windows *executables* on Linux; it does not
  produce Windows object code. It can't turn `mpv_addon.cc` into a Windows `.node`.
- Running MSVC itself *under* Wine is unsupported and breaks in practice (node-gyp +
  MSBuild + the Windows SDK do not work reliably under Wine).
- The realistic cross-compile alternative (MinGW) won't match the **MSVC Node ABI** that
  Electron's Node expects, and linking libmpv + N‑API that way is fragile/unsupported.

So a native addon has to be built with the target OS's own toolchain. The Windows build
must be produced **on Windows** (a Windows PC or Windows CI such as GitHub Actions
`windows-latest`); only the final installer assembly — not compilation — is what Wine
ever helped with.

## Runtime notes
- Subtitles default **off**; choose a track (and adjust size / text color / background +
  opacity) from the **CC** menu. Audio language switches live from the **🌐** menu.
- mpv logs surface in the terminal prefixed `[mpv-host]` (run from a terminal to see them).
- Dev (`npm run electron:preview`) uses the **system** Node + mpv; the `vendor/` bundle is
  only used in the packaged app.
