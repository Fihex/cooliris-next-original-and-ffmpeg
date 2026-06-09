# Building Cooliris Next (ffmpeg edition)

This is the **ffmpeg** edition: it bundles ffmpeg/ffprobe so it can play formats
Chromium can't (mkv, avi, HEVC/H.265, AC-3/DTS audio), switch audio tracks, show
embedded subtitles, and optionally use the GPU for encoding. The sibling
`cooliris-next-original` folder is the lighter edition without any of that.

## Prerequisites

- **Node.js 22+** and **npm**.
- For building a **Windows** installer **on Linux/macOS**: **Wine** (electron-builder
  uses it to assemble the NSIS installer).
  - Arch/CachyOS: `sudo pacman -S wine`
  - Debian/Ubuntu: `sudo apt install wine`
  - Building Windows *on Windows* needs no Wine.

## 1. Install dependencies

```bash
npm install
```

ffmpeg binaries are bundled (not taken from the system):

- `ffmpeg-static` ships the **newest** ffmpeg (7.x) for whatever OS you run
  `npm install` on. This is the binary the app prefers at runtime.
- `ffprobe-static` already contains every platform's `ffprobe`.
- `@ffmpeg-installer/<platform>` (optional deps) provide ffmpeg for the **other**
  platforms, used only when cross-building.

### Cross-building (e.g. a Windows build from Linux)

A plain `npm install` only fetches binaries for your current OS. To bundle **all**
platforms in one install (so a Windows package built on Linux contains
`ffmpeg.exe`), force the optional platform packages in:

```bash
npm install --force
```

> Alternatively, build each target on its own OS — `ffmpeg-static` then gives the
> newest binary for that platform automatically and `--force` isn't needed.

## 2. Build

The renderer + Electron main are built first (`npm run build`), then packaged.

```bash
# Linux only  → release/Cooliris Next-<version>.AppImage
npm run electron:build:linux

# Windows only → release/Cooliris Next Setup <version>.exe   (needs Wine on Linux)
npm run electron:build:win

# Both at once
ELECTRON=1 npm run build && npx electron-builder --linux AppImage --win nsis
```

Other handy targets:

```bash
# Unpacked folder (no installer) — fastest, just run the binary inside:
ELECTRON=1 npm run build && npx electron-builder --linux dir   # release/linux-unpacked/
ELECTRON=1 npm run build && npx electron-builder --win dir     # release/win-unpacked/
```

> **Note:** if a rebuild doesn't seem to pick up changes, clear the Vite/Electron
> caches first: `rm -rf node_modules/.vite dist-electron dist release`.

## 3. Output

Everything lands in `release/`:

| Platform | File | Approx size |
|----------|------|-------------|
| Linux | `Cooliris Next-<version>.AppImage` | ~290 MB |
| Windows | `Cooliris Next Setup <version>.exe` | ~260 MB |

These are large because the bundle currently ships ffmpeg for *all* platforms
(works fine; the app picks the right one at runtime). To slim a build, install
without `--force` and build on the target OS so only that platform's binary is
included.

- **Linux:** `chmod +x "Cooliris Next-<version>.AppImage"` then run it.
- **Windows:** run the `Setup` exe (per-user install, no admin needed).

## How playback works (ffmpeg edition)

When you open a non-native video (mkv/avi/HEVC/…), the app **remuxes or transcodes it
to a real, seekable temporary `.mp4`** (in the OS temp dir) and plays that file, rather
than live-streaming. This is what keeps video, audio, and subtitles in sync, gives
native seeking and a correct duration, and lets subtitles use their true timestamps.
A short "Preparing video…" notice shows while it works:

- **Remux** (codecs already mp4-friendly, e.g. most mkv) is near-instant.
- **Transcode** (e.g. HEVC) takes time proportional to the clip.

Switching the audio track re-prepares with the chosen track and resumes from the same
position. Prepared files are cached for the session and deleted on quit.

## Runtime notes (ffmpeg edition)

- A config file is created on first run at
  `<userData>/cooliris.config.json` (Linux: `~/.config/Cooliris Next/`,
  Windows: `%APPDATA%\Cooliris Next\`):

  ```json
  { "ffmpeg": { "enabled": true, "hwAccel": false } }
  ```

  - `enabled` — turn the whole ffmpeg layer on/off (also toggleable in **Settings**;
    takes effect on restart).
  - `hwAccel` — CPU (false, most compatible) vs GPU (true) encoding. The app
    auto-detects a working hardware H.264 encoder (NVENC / QSV / VAAPI / AMF /
    VideoToolbox) and falls back to software if none works.
- Binary resolution order at runtime: `ffmpeg-static` → `@ffmpeg-installer/<platform>`
  → system `PATH`.
