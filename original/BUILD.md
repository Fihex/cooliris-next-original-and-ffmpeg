# Building Cooliris Next (original edition)

This is the **original** edition — the lightweight build with **no ffmpeg** and
**no subtitle/caption** support. It plays whatever the browser engine (Chromium)
can play natively (mp4/webm, jpg/png/gif, mp3/flac, …). For mkv/avi/HEVC, embedded
subtitles, audio-track switching, etc., use the sibling `cooliris-next-ffmpeg`
folder instead.

## Prerequisites

- **Node.js 22+** and **npm**.
- For building a **Windows** installer **on Linux/macOS**: **Wine**
  (electron-builder uses it to assemble the NSIS installer).
  - Arch/CachyOS: `sudo pacman -S wine`
  - Debian/Ubuntu: `sudo apt install wine`
  - Building Windows *on Windows* needs no Wine.

There are **no native/binary dependencies** — cover-art extraction is pure JS — so
`npm install` is all that's needed, no special flags.

## 1. Install dependencies

```bash
npm install
```

## 2. Build

```bash
# Linux only  → release/Cooliris Next-<version>.AppImage
npm run electron:build:linux

# Windows only → release/Cooliris Next Setup <version>.exe   (needs Wine on Linux)
npm run electron:build:win

# Both at once
ELECTRON=1 npm run build && npx electron-builder --linux AppImage --win nsis
```

Unpacked (no installer), for quick testing:

```bash
ELECTRON=1 npm run build && npx electron-builder --linux dir   # release/linux-unpacked/
ELECTRON=1 npm run build && npx electron-builder --win dir     # release/win-unpacked/
```

> **Note:** if a rebuild doesn't seem to pick up changes, clear the Vite/Electron
> caches first: `rm -rf node_modules/.vite dist-electron dist release`.

## 3. Output

Everything lands in `release/`:

| Platform | File | Approx size |
|----------|------|-------------|
| Linux | `Cooliris Next-<version>.AppImage` | ~125 MB |
| Windows | `Cooliris Next Setup <version>.exe` | ~100 MB |

- **Linux:** `chmod +x "Cooliris Next-<version>.AppImage"` then run it.
- **Windows:** run the `Setup` exe (per-user install, no admin needed).
