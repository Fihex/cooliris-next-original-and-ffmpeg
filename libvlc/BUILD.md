# Building Cooliris Next — libVLC edition

Like the `libmpv` edition, this plays **every format instantly** (no transcoding) via a
native engine painting frames into a `<canvas>` — but the engine is **libVLC 3**. See
`VLC.md` for the architecture and the differences (notably: subtitle styling is
selection-only on VLC).

There is a **native C++ addon** (`native/vlc.node`), so building involves a compile step.

## What to install before building

### Linux

Arch / CachyOS:
```bash
sudo pacman -S --needed base-devel python nodejs npm vlc pkgconf
```
Debian / Ubuntu:
```bash
sudo apt install build-essential python3 nodejs npm libvlc-dev vlc pkg-config
```

| Package | Why it's needed |
|---|---|
| `base-devel` / `build-essential` | gcc + make — node-gyp compiles `vlc_addon.cc` |
| `python` | node-gyp is a Python tool |
| `nodejs` + `npm` | build tooling + the binary bundled into `vendor/` to run the vlc host |
| `vlc` / `libvlc-dev` | **libvlc + headers** (`/usr/include/vlc/vlc.h`) and the **plugins tree** (`/usr/lib/vlc/plugins`) that gets bundled |
| `pkgconf` / `pkg-config` | how `binding.gyp` locates libvlc (`pkg-config --libs libvlc`) |

Sanity check: `pkg-config --modversion libvlc` should print a version (e.g. `3.0.23`).

### Windows  (the Windows build must be made *on* Windows)

1. **Node.js LTS** — <https://nodejs.org>.
2. **Visual Studio Build Tools 2022** — workload **“Desktop development with C++”**
   (MSVC + Windows SDK for node-gyp).
3. **Python 3** — <https://python.org> (node-gyp needs it).
4. **The official VLC Windows zip** — `vlc-3.x.x-win64.zip` from
   <https://get.videolan.org/vlc/> (pick a version → `win64/`). Extract it anywhere.
   It already contains the **SDK** (`sdk\include`, `sdk\lib` with import libraries),
   the DLLs, and the plugin tree — no extra packages, no import-lib generation.

## 1. Install dependencies
```bash
npm install
```

## 2. Build for Linux  (self-contained AppImage)
```bash
cd native && npx node-gyp rebuild && cd ..   # native addon
bash scripts/bundle-linux.sh                 # vendor/: node + libvlc + plugins + deps
ELECTRON=1 npm run build
npx electron-builder --linux AppImage        # → release/Cooliris Next-<ver>.AppImage
```
At runtime the vlc host runs under `vendor/node` with `LD_LIBRARY_PATH=vendor/lib` and
`VLC_PLUGIN_PATH=vendor/vlc-plugins` — **no system Node or VLC required**.

> Stale rebuild? Clear caches: `rm -rf node_modules/.vite dist dist-electron`.

## 3. Build for Windows  (Developer PowerShell for VS 2022)
```powershell
pwsh scripts/setup-windows.ps1 -VlcDir C:\path\to\vlc-3.0.21   # SDK → native\vlc-sdk
cd native; npx node-gyp rebuild; cd ..                          # vlc.node
pwsh scripts/bundle-windows.ps1 -VlcDir C:\path\to\vlc-3.0.21  # vendor\: node + dlls + plugins
$env:ELECTRON=1; npm run build
npx electron-builder --win nsis              # → release\Cooliris Next Setup <ver>.exe
```

## Unpacked builds (no installer)
Same steps, but use the `dir` target: `npx electron-builder --linux dir` →
`release/linux-unpacked/`, or `--win dir` (on Windows) → `release\win-unpacked\`.
The folder is fully self-contained (vendor runtime + unpacked addon) — just copy it.

## Why the Windows build can't be cross-built from Linux
Same reason as the libmpv edition: the native addon must be compiled with **MSVC**
against the Windows Node ABI — Wine runs Windows executables but is not a compiler, and
MSVC/node-gyp don't work under it. Build on a Windows PC or Windows CI
(e.g. GitHub Actions `windows-latest`).

## Runtime notes
- Subtitles default **off**; pick a track from the **CC** menu (selection-only on VLC —
  no font/color/background styling). Audio language switches live from the **🌐** menu.
- VLC warnings/errors surface in the terminal prefixed `[vlc-host]`.
- Dev (`npm run electron:preview`) uses the **system** Node + VLC; the `vendor/` bundle
  is only used in the packaged app.
