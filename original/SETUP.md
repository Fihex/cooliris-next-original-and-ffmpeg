# Cooliris Next — Setup & Usage Guide

A modern, Flash-free reimplementation of the Cooliris 3D media wall.
**Stack:** Vite + React + TypeScript + TailwindCSS, rendered with Three.js (WebGL). Pure client-side SPA (no server, no backend).

---

## 1. Requirements

- **Node.js 20+** (and npm). Check with `node -v`.
- A modern Chromium-based browser (Chrome/Edge) is recommended — the **Open folder** picker and **GIF wall animation** use APIs that Firefox/Safari may not fully support.

## 2. Install

```bash
cd coolirsNext
npm install
```

## 3. Run (development)

```bash
npm run dev
```
Open the printed URL (default <http://localhost:5173>). A sample wall loads automatically.

### Open it on your phone (same Wi‑Fi)
```bash
npm run dev -- --host
```
It prints a **Network:** line, e.g. `http://192.168.1.162:5173/`. Open that exact `http://` URL on the phone.
- Phone and computer must be on the **same network** (not guest Wi‑Fi / cellular).
- If it won't load, the network is blocking device-to-device ("client/AP isolation"). Use a tunnel instead:
  ```bash
  npx localtunnel --port 5173      # opens a public https URL you can open anywhere
  ```

## 4. Build (production)

```bash
npm run build      # type-checks + bundles into dist/
npm run preview    # serve the production build locally
npm run preview -- --host   # ...and expose it to your phone
```
Deploy by serving the static `dist/` folder on any static host.

---

## 4b. Desktop app (Electron)

The same UI also runs as a native desktop app via Electron — bundled Chromium for
consistent, hardware-accelerated WebGL, plus **real folder access** (absolute
paths, file dates, no upload prompt) through native OS dialogs.

```bash
npm run electron:dev       # Vite dev server + Electron window, hot-reload
npm run electron:preview   # build, then run the production bundle in Electron
npm run electron:build     # build + package a distributable (AppImage/nsis/dmg) into release/
```

How it fits together (kept isolated so it's easy to reason about / remove):

- `electron/main.ts` — main process: GPU window, native file/folder dialogs
  (Node `fs` scan → paths + dates), and a `coolmedia://` protocol that **streams**
  local files to the renderer (no loading thousands of images into memory).
- `electron/preload.ts` — `contextBridge` exposing a tiny `window.electron` API.
- `src/platform/electron.ts` — implements the shared `Platform` interface; the
  React/Three.js UI is unchanged and auto-selects this when `window.electron` exists
  (see `src/platform/index.ts`).
- Electron is opt-in: the `electron:*` scripts set `ELECTRON=1`, which enables
  `vite-plugin-electron` in `vite.config.ts`. Plain `npm run dev`/`build` stay
  pure-web and untouched.

> Note: the main/preload bundles are emitted as CommonJS (`dist-electron/*.cjs`)
> so `require("electron")` resolves to Electron's built-in API even though the
> project is `"type": "module"`.

---

## 5. Using it

### Loading media
Press **Open** → a modal with:
- **Choose files…** — pick individual images/videos.
- **Choose folder…** — pick a whole folder (recurses subfolders). *(Desktop Chrome/Edge only.)*
- **From JSON…** — load a feed by URL, local `.json` file, or pasted text (see format below).
- **Drag & drop** files or a folder onto the modal's drop zone.

### Browsing the wall
- **Drag** (left mouse) or **right‑drag / middle‑drag** to move around; **wheel** = zoom; **two‑finger pinch** = zoom (touch).
- **Arrow keys** or the on‑screen **edge arrows** scroll; the **white bottom bar** is a scrubber (drag it, any mouse button).
- **Hover** a photo to see its name; **click** to open it.

### Viewing an item (lightbox)
- **Wheel / pinch** to zoom, **drag** to pan when zoomed.
- **‹ / ›**, the **arrow keys**, or swipe to go prev/next.
- **Back**, **Esc**, or **tap the dark area** to close (it returns you to the item you were last viewing).

### Toolbar
- **Slideshow** — auto‑advance.
- **⚙ Settings** — *Show titles* (labels on every tile) and *Animate GIFs on the wall* (both **off by default**).
- **Dates** — filter by file date; type `YYYY‑MM‑DD` or use the 📅 picker; **Clear dates**.
- **Search** — filter by name.
- **Fullscreen**.

---

## 6. JSON feed format

A bare array, or `{ "title": "...", "items": [...] }`:
```json
{
  "title": "My wall",
  "items": [
    {
      "id": "a1",
      "type": "image",
      "thumb": "https://example.com/thumb.jpg",
      "full": "https://example.com/full.jpg",
      "title": "Caption",
      "path": "folder/file.jpg",
      "date": "2023-05-01",
      "link": "https://example.com"
    }
  ]
}
```
- Only `full` (or `thumb`) is required per item. Aliases accepted: `url`/`src`/`image` → `full`; `thumbnail`/`preview` → `thumb`; `guid` → `id`; `name` → `title`; `href` → `link`; `taken`/`timestamp` → `date`.
- `type` is auto‑detected as video from the extension if omitted; `.gif` is auto‑flagged as animated.
- Cross‑site URLs are retried via a CORS proxy if needed; **remote images must allow CORS** to render in WebGL.
- See `public/sample-feed.json` (offline) and `public/sample-100.json` (100 remote images).

---

## 7. Tuning / performance knobs

- **GIF playback rate (wall):** `src/gifsAnimation/GifController.ts` → `GIF_BASE_FPS` (24) and `GIF_SKIP` (fps to subtract; 4→20fps, 8→16…). Also `MAX_ACTIVE_GIFS`, `MAX_GIF_DECODES`.
- **Texture streaming:** `src/wall/WallScene.ts` → `MAX_INFLIGHT`, `MAX_NEW_LOADS_PER_FRAME`, `KEEP_BUFFER_COLS`.
- **Wall layout / motion:** same file — `ROWS`, `MAX_W`, camera/zoom/bank constants.

### Removing GIF wall‑animation entirely
Delete `src/gifsAnimation/`, remove the `GifController` field + its 4 call sites in `WallScene.ts`, and the *Animate GIFs* toggle in `SettingsDialog.tsx`/`WallView.tsx`. Nothing else depends on it.

---

## 8. Known browser limits (and the fix)

The browser sandbox prevents some things — these would work in a desktop (Electron/Tauri) or Android (Capacitor) build:
- **No absolute disk paths** — only the path within the folder you opened.
- **No file creation date** — only last‑modified.
- **No folder picker on mobile** — use *Choose files* or *Load JSON* on phones.

---

## 9. Recover / revert with git

History:
- `e24c0be` — recovery point: virtualized 3D wall.
- later commits — streaming load, JSON dialog, Open modal, date filter, GIF module, lightbox viewer, etc.

```bash
git log --oneline                 # list commits
git status                        # see uncommitted changes
git stash                         # set aside current changes (restore with: git stash pop)
git restore <file>                # discard changes to one file
git reset --hard <commit-hash>    # ⚠️ revert EVERYTHING to that commit (discards changes)
git reset --hard e24c0be          # back to the first recovery point
```
To save your current work as a snapshot:
```bash
git add -A && git commit -m "snapshot"
```

---

## 10. Project structure

```
src/
  wall/WallScene.ts        Three.js scene: virtualized tiles, scroll/zoom/bank, reflections, worker decode pool
  wall/decodeWorker.ts     Off-main-thread image decode + downscale
  gifsAnimation/           Self-contained wall GIF animation (easy to remove)
  feed/                    jsonFeed.ts, localFiles.ts, types.ts  (loading + normalization)
  platform/                File-access abstraction: Platform.ts (interface) + web.ts + electron.ts + index.ts (selector)
  components/              WallView, Toolbar, OpenDialog, JsonDialog, SettingsDialog, Lightbox, Scrubber, Toast, ErrorBoundary
  embed/cooliris-embed.ts  window.cooliris.embed.* compatibility shim
  routes/                  TanStack Router (SPA, no SSR)
electron/                  Desktop target: main.ts, preload.ts, tsconfig.json  (opt-in via ELECTRON=1)
electron-builder.yml       Packaging config (AppImage / nsis / dmg → release/)
public/                    sample-feed.json, sample-100.json, samples/*.svg
```
