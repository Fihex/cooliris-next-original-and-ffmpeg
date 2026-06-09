# Cooliris Next

A modern, **Flash-free** reimplementation of the [Cooliris Embed Wall](https://github.com/cooliris/embed-wall) — the classic 3D media wall — built with **TanStack Router + Vite + React + TypeScript + Tailwind** and rendered with **Three.js** (WebGL). No Flash, no plugins, runs in any modern browser, including mobile.

## Features

- **3D media wall** — a flat-at-rest grid of aspect-preserving tiles that **banks in the scroll direction**, with mirrored under-photo reflections.
- **Inertial scrolling** — drag, hold arrow keys / edge arrows (smooth accelerate + momentum stop), or the bottom scrubber.
- **Camera** — mouse wheel zooms toward the cursor; middle-mouse drag pans freely (vertical + horizontal).
- **Zoom-to-photo + slideshow** — click a photo to zoom it in with a caption underneath; `←/→` or the caption arrows glide to the next/prev; `Esc` / Back / click-background to exit.
- **Open local media** — pick files or a whole folder (recursed). Uses the File System Access API where available, with an `<input webkitdirectory>` fallback. Images load instantly (lazily per tile); video poster frames are generated on-device; nothing is uploaded.
- **JSON manifest feeds** — load a manifest from a URL or serve your own.
- **Drop-in API shim** — `window.cooliris.embed.*` mirrors the original API surface.

## Run

```bash
npm install
npm run dev      # start the dev server
npm run build    # typecheck + production build
npm run preview  # preview the production build
```

Open the printed local URL. The sample feed (`public/sample-feed.json`) loads automatically.

## JSON manifest format

Either a bare array or `{ "title": "...", "items": [...] }`:

```json
{
  "title": "My gallery",
  "items": [
    {
      "id": "unique-id",
      "type": "image",
      "thumb": "https://example.com/thumb.jpg",
      "full": "https://example.com/full.jpg",
      "title": "Caption",
      "link": "https://example.com/source"
    }
  ]
}
```

Field aliases are accepted to keep hand-written manifests forgiving: `url`/`src`/`image` → `full`; `thumbnail`/`preview` → `thumb`; `guid` → `id`. `type` is inferred from the file extension when omitted.

## Cooliris-compatible JavaScript API

The original `cooliris-embed.js` surface is mirrored at `window.cooliris.embed`:

```js
cooliris.embed.setCallbacks({
  select: (index, item) => {},
  deselect: (index) => {},
  feedload: (count) => {},
  feederror: (message) => {},
});

cooliris.embed.setFeedURL("/sample-feed.json");
cooliris.embed.getItems();
cooliris.embed.selectItemByIndex(0);
cooliris.embed.selectItemByGUID("p1");
cooliris.embed.getSelectedItem();
```

## Project structure

```
src/
  wall/WallScene.ts        Three.js scene: tiles, reflective floor, scroll, focus
  feed/jsonFeed.ts         JSON manifest loading + normalization
  feed/localFiles.ts       File / folder / drag-drop loading + thumbnailing
  embed/cooliris-embed.ts  window.cooliris.embed.* compatibility shim
  components/              WallView, Toolbar, DetailOverlay, Toast
  routes/                  TanStack Router (SPA, no SSR)
```

## Notes vs. the original

- The legacy provider integrations (Flickr / YouTube / Picasa / Facebook with hard-coded keys) are intentionally **not** reimplemented — most of those APIs are dead or now require OAuth. Use a JSON manifest or local files instead. A Media RSS adapter can be added on top of the same `Feed` model.
