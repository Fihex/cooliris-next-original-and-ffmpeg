# Building Cooliris Next — libmpv edition

This edition plays **every format instantly** (mkv/avi/HEVC/AC‑3/DTS, all audio +
subtitle tracks) with **no transcoding**, by decoding with **libmpv** and painting frames
into a `<canvas>`. mpv runs in its own Node process (never the Electron binary), so it
uses a full ffmpeg with no conflict. See `MPV.md` for the architecture.

Unlike the `ffmpeg` edition, this one has a **native C++ addon** (`native/mpv.node`) that
links libmpv — so building involves a compile step, and the runtime bundles libmpv.

## What to install before building

Because of the native addon, you need a compiler toolchain in addition to Node — this is
the one edition where `npm install` alone is not enough.

### Linux

Arch / CachyOS — one command covers everything:
```bash
sudo pacman -S --needed base-devel python nodejs npm mpv pkgconf
```
Debian / Ubuntu equivalent:
```bash
sudo apt install build-essential python3 nodejs npm libmpv-dev pkg-config
```

What each piece is for:

| Package | Why it's needed |
|---|---|
| `base-devel` / `build-essential` | gcc + make — node-gyp compiles `mpv_addon.cc` with these |
| `python` | node-gyp is a Python tool |
| `nodejs` + `npm` | build tooling, and the binary that gets bundled into `vendor/` to run the mpv host |
| `mpv` / `libmpv-dev` | **libmpv itself + its headers** (`/usr/include/mpv/*.h`) — what the addon links against, and what `bundle-linux.sh` copies into `vendor/lib` |
| `pkgconf` / `pkg-config` | how `binding.gyp` locates libmpv (`pkg-config --cflags --libs mpv`) |

**No Wine is needed for anything in this edition** (and it wouldn't help — see the
cross-build section below).

Sanity check before building: `pkg-config --modversion mpv` should print a version
(e.g. `2.5.0`). If it doesn't, the headers aren't installed.

### Windows  (the Windows build must be made *on* Windows)

Install, in order:

1. **Node.js LTS** — installer from <https://nodejs.org>. (Skip the installer's optional
   "tools for native modules" checkbox; install the build tools yourself in step 2 —
   it's more reliable.)
2. **Visual Studio Build Tools 2022** — from
   <https://visualstudio.microsoft.com/downloads/> → *Tools for Visual Studio*. In the
   installer select the **“Desktop development with C++”** workload. This provides MSVC
   (the compiler) and the Windows SDK — what node-gyp uses to compile the addon, and
   `lib.exe`, which `setup-windows.ps1` uses to generate the import library.
3. **Python 3** — from <https://python.org> (check “Add to PATH”). node-gyp requires it.
   (The VS installer can also add it as a component — either way works.)
4. **7‑Zip** — from <https://7-zip.org>, to extract the libmpv package.
5. **The libmpv dev package** — download `mpv-dev-x86_64-*.7z` from
   SourceForge → *mpv-player-windows / libmpv*
   (<https://sourceforge.net/projects/mpv-player-windows/files/libmpv/>) and extract it
   somewhere. It contains the headers (`include/mpv/*.h`), the self-contained
   `libmpv-2.dll` (ffmpeg baked in), and the `.def` file used to generate `mpv.lib`.

Then run the build steps below **from a “Developer PowerShell for VS 2022”** (or an
*x64 Native Tools* prompt) — that's what puts `lib.exe` and the MSVC environment on
PATH; a plain terminal will fail at `setup-windows.ps1`.

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
