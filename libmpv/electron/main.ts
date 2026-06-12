import { app, BrowserWindow, dialog, ipcMain, Menu, nativeImage, net, protocol } from "electron";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promises as fs, createReadStream, type Stats } from "node:fs";
import { Readable } from "node:stream";
import { createHash } from "node:crypto";
import v8 from "node:v8";
import { runInNewContext } from "node:vm";
import path from "node:path";
import { extractCoverArt } from "./coverArt";
import {
  mpvAvailable,
  mpvWarm,
  setEmbedWid,
  mpvFit,
  mpvLoad,
  mpvCommand,
  mpvSet,
  mpvSetFast,
  mpvGet,
  mpvVideoSize,
  mpvFrame,
  mpvStop,
  mpvDestroy,
} from "./mpv";

// Content-Type for the local-media protocol — needed for correct playback/decoding.
const MIME: Record<string, string> = {
  ".mp4": "video/mp4", ".webm": "video/webm", ".ogv": "video/ogg", ".mov": "video/quicktime",
  ".m4v": "video/x-m4v", ".mkv": "video/x-matroska", ".avi": "video/x-msvideo",
  ".mp3": "audio/mpeg", ".wav": "audio/wav", ".ogg": "audio/ogg", ".oga": "audio/ogg",
  ".flac": "audio/flac", ".m4a": "audio/mp4", ".aac": "audio/aac", ".opus": "audio/opus",
  ".weba": "audio/webm",
  ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".png": "image/png", ".gif": "image/gif",
  ".webp": "image/webp", ".avif": "image/avif", ".bmp": "image/bmp", ".svg": "image/svg+xml",
};
const mimeFor = (p: string): string =>
  MIME[p.slice(p.lastIndexOf(".")).toLowerCase()] || "application/octet-stream";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// dist-electron/main.js  →  project root is one level up.
process.env.APP_ROOT = path.join(__dirname, "..");
const VITE_DEV_SERVER_URL = process.env.VITE_DEV_SERVER_URL;
const RENDERER_DIST = path.join(process.env.APP_ROOT, "dist");

let win: BrowserWindow | null = null;
let videoWin: BrowserWindow | null = null; // two-window embed: child window mpv renders into
let openT0 = 0; // timing: when a video open was requested (→ "[open]" log on first frame)

// Experimental Option 1: render hardware-decoded mpv into the window (native fps). The
// window is transparent + frameless so the mpv surface underneath shows through where the
// web layer is cleared. Windows-only, opt-in. Off → normal canvas frame-pump.
const EMBED_MODE = process.env.COOLIRIS_MPV_EMBED === "1" && process.platform === "win32";

/* --------------------------- local-media protocol --------------------------- */
// A privileged scheme so the renderer (http:// in dev, file:// in prod) can load
// files from anywhere on disk *by streaming* — we never read whole files into JS.
// URL shape: coolmedia://f/<encodeURIComponent(absolutePath)>
protocol.registerSchemesAsPrivileged([
  {
    scheme: "coolmedia",
    privileges: {
      standard: true,
      secure: true,
      supportFetchAPI: true,
      stream: true,
      bypassCSP: true,
      corsEnabled: true,
    },
  },
  // Serve the bundled renderer from a real origin (app://bundle/) so the SPA
  // router and absolute fetches like /sample-feed.json work — file:// gives the
  // document a disk path as its pathname, which matches no route (blank screen).
  {
    scheme: "app",
    privileges: { standard: true, secure: true, supportFetchAPI: true, corsEnabled: true },
  },
]);

/* ------------------------------- folder scan -------------------------------- */
const IMAGE_RE = /\.(jpe?g|png|gif|webp|avif|bmp|svg)$/i;
const VIDEO_RE = /\.(mp4|webm|ogv|mov|m4v|mkv|avi)$/i;
const AUDIO_RE = /\.(mp3|wav|ogg|oga|flac|m4a|aac|opus|weba)$/i;
const isMedia = (n: string) => IMAGE_RE.test(n) || VIDEO_RE.test(n) || AUDIO_RE.test(n);

interface ScanFile {
  abs: string;
  rel: string;
  name: string;
  mtime: number;
  btime: number; // birthtime (created), falls back to mtime when unavailable
  cover?: string; // sidecar cover image (absolute path) for audio without embedded art
}

// Common "whole album" cover filenames, checked when an audio file has no sibling
// image of its own.
const COVER_RE = /^(cover|folder|front|albumart(?:small)?|album|thumb)\.(jpe?g|png|webp|avif|bmp)$/i;

/** Find a cover image next to an audio file: same-stem image first, then cover.jpg etc. */
function audioCover(audioName: string, dir: string, names: string[]): string | null {
  const stem = audioName.replace(/\.[^.]+$/, "").toLowerCase();
  // 1) An image sharing the track's name (song.mp3 → song.jpg).
  for (const n of names) {
    if (IMAGE_RE.test(n) && n.replace(/\.[^.]+$/, "").toLowerCase() === stem) {
      return path.join(dir, n);
    }
  }
  // 2) A generic album cover in the same folder.
  for (const n of names) if (COVER_RE.test(n)) return path.join(dir, n);
  return null;
}

async function statTimes(abs: string): Promise<{ mtime: number; btime: number }> {
  try {
    const s = await fs.stat(abs);
    // birthtimeMs can be 0 on filesystems that don't record it → fall back to mtime.
    return { mtime: s.mtimeMs, btime: s.birthtimeMs || s.mtimeMs };
  } catch {
    return { mtime: 0, btime: 0 };
  }
}

/** Recursively collect media files under a root directory (Node fs — fast, no upload). */
async function scanDir(root: string): Promise<ScanFile[]> {
  const out: ScanFile[] = [];
  async function walk(dir: string, prefix: string): Promise<void> {
    let entries: import("node:fs").Dirent[];
    try {
      entries = await fs.readdir(dir, { withFileTypes: true });
    } catch {
      return;
    }
    const names = entries.map((e) => e.name);
    for (const e of entries) {
      const abs = path.join(dir, e.name);
      const rel = prefix ? `${prefix}/${e.name}` : e.name;
      if (e.isDirectory()) {
        await walk(abs, rel);
      } else if (e.isFile() && isMedia(e.name)) {
        const cover = AUDIO_RE.test(e.name) ? audioCover(e.name, dir, names) : null;
        const { mtime, btime } = await statTimes(abs);
        out.push({
          abs,
          rel,
          name: e.name,
          mtime,
          btime,
          cover: cover ?? undefined,
        });
      }
    }
  }
  await walk(root, "");
  return out;
}

/* ----------------------------------- IPC ------------------------------------ */
ipcMain.handle("pick-folder", async () => {
  if (!win) return null;
  const r = await dialog.showOpenDialog(win, { properties: ["openDirectory"] });
  if (r.canceled || !r.filePaths[0]) return null;
  const root = r.filePaths[0];
  return { rootName: path.basename(root), files: await scanDir(root) };
});

ipcMain.handle("pick-files", async () => {
  if (!win) return null;
  const r = await dialog.showOpenDialog(win, {
    properties: ["openFile", "multiSelections"],
    filters: [
      {
        name: "Media",
        extensions: [
          "jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "svg",
          "mp4", "webm", "ogv", "mov", "m4v", "mkv", "avi",
          "mp3", "wav", "ogg", "oga", "flac", "m4a", "aac", "opus", "weba",
        ],
      },
    ],
  });
  if (r.canceled || !r.filePaths.length) return null;
  const files: ScanFile[] = await Promise.all(
    r.filePaths.map(async (abs) => {
      const name = path.basename(abs);
      let cover: string | undefined;
      if (AUDIO_RE.test(name)) {
        try {
          const dir = path.dirname(abs);
          const names = await fs.readdir(dir);
          cover = audioCover(name, dir, names) ?? undefined;
        } catch {
          /* ignore */
        }
      }
      const { mtime, btime } = await statTimes(abs);
      return { abs, rel: name, name, mtime, btime, cover };
    })
  );
  return { rootName: `${files.length} file(s)`, files };
});

// Scan dropped / selected real paths (files or folders) exactly like the native
// pickers, so drag-and-drop has full parity: embedded + sidecar covers and
// modified/created dates.
ipcMain.handle("scan-paths", async (_e, paths: string[]) => {
  const files: ScanFile[] = [];
  for (const p of paths) {
    try {
      const st = await fs.stat(p);
      if (st.isDirectory()) {
        files.push(...(await scanDir(p)));
        continue;
      }
      const name = path.basename(p);
      if (!isMedia(name)) continue;
      const dir = path.dirname(p);
      let cover: string | undefined;
      try {
        if (AUDIO_RE.test(name)) {
          const names = await fs.readdir(dir);
          cover = audioCover(name, dir, names) ?? undefined;
        }
      } catch {
        /* ignore */
      }
      const { mtime, btime } = await statTimes(p);
      files.push({ abs: p, rel: name, name, mtime, btime, cover });
    } catch {
      /* skip unreadable */
    }
  }
  const rootName = paths.length === 1 ? path.basename(paths[0]) : `${files.length} item(s)`;
  return { rootName, files };
});

// Read embedded cover art (ID3/FLAC/MP4) from an audio file → data URL, or null.
// Read embedded cover art (ID3/FLAC/MP4/Ogg) with a dependency-free, bundled parser
// → no dynamic import / asar resolution, instant during the scan, and leak-free.
ipcMain.handle("get-cover", async (_e, abs: string): Promise<string | null> => {
  try {
    const c = await extractCoverArt(abs);
    return c ? `data:${c.mime};base64,${Buffer.from(c.data).toString("base64")}` : null;
  } catch {
    return null;
  }
});

// modified + created (birthtime) times for a path — used to give drag-and-drop files
// the same created date as the folder scan.
ipcMain.handle(
  "stat-file",
  async (_e, abs: string): Promise<{ mtime: number; btime: number } | null> => {
    try {
      const s = await fs.stat(abs);
      return { mtime: s.mtimeMs, btime: s.birthtimeMs || s.mtimeMs };
    } catch {
      return null;
    }
  }
);

// Fetch a remote URL from the main process — no browser CORS, so the desktop app
// loads remote JSON feeds directly (no third-party proxy).
ipcMain.handle("fetch-text", async (_e, url: string) => {
  const res = await net.fetch(url, { headers: { Accept: "application/json" } });
  if (!res.ok) throw new Error(`Failed to load feed (${res.status} ${res.statusText})`);
  return res.text();
});

/* ---------------------------------- libmpv ---------------------------------- */
// All-format playback: the renderer loads a file, pulls RGBA frames each animation
// frame, and drives playback through these. Frames are composited (video + subs) by mpv.
ipcMain.handle("mpv-available", () => mpvAvailable());
ipcMain.handle("mpv-load", (_e, abs: string) => mpvLoad(abs));
ipcMain.handle("mpv-cmd", (_e, args: string[]) => mpvCommand(args));
ipcMain.handle("mpv-set", (_e, name: string, value: string) => mpvSet(name, value));
// Fire-and-forget (no reply): hot-path batched property set for zoom/pan.
ipcMain.on("mpv-set-fast", (_e, props: Record<string, string>) => mpvSetFast(props));
ipcMain.handle("mpv-get", (_e, name: string) => mpvGet(name));
ipcMain.handle("mpv-size", () => mpvVideoSize());
ipcMain.handle("mpv-frame", (_e, w: number, h: number) => mpvFrame(w, h));
ipcMain.handle("mpv-stop", () => mpvStop());
// Toggle the OS window fullscreen (embed mode). setFullScreen also resizes the window,
// which makes the embedded mpv --wid surface re-fit to fill it. Returns the new state.
ipcMain.handle("win-fullscreen", (_e, on: boolean) => {
  if (!win) return false;
  win.setFullScreen(!!on);
  return win.isFullScreen();
});

// Two-window embed: the main (wall) window asks to play a video → show + align the child
// video window over the content area and tell it which file. Close → hide it, stop mpv,
// and let the wall return.
ipcMain.handle("play-video", (_e, abs: string) => {
  if (!win || !videoWin) return false;
  // Start decoding RIGHT NOW (at click), in parallel with the child rendering its UI, so
  // the first frame is ready as early as possible. Load while HIDDEN — the child calls
  // "video-ready" once the frame is decoded, and only then do we reveal the window (no
  // blank window during decode).
  openT0 = Date.now();
  mpvLoad(abs);
  videoWin.webContents.send("video-play", abs);
  return true;
});
ipcMain.handle("video-ready", () => {
  if (!win || !videoWin) return;
  if (openT0) console.log(`[open] click → first frame ready: ${Date.now() - openT0} ms`);
  videoWin.setBounds(win.getContentBounds());
  videoWin.show();
  videoWin.focus(); // so Space / arrows / Esc reach the player
});
ipcMain.handle("close-video", () => {
  videoWin?.hide();
  mpvStop(); // free the file/decoder while browsing
  win?.webContents.send("video-closed");
});
// Prev/next: the child player asks; the wall window (which has the feed) picks the
// adjacent video and calls play-video.
ipcMain.handle("video-nav", (_e, dir: "prev" | "next") => win?.webContents.send("video-nav-main", dir));

/* --------------------------------- window ----------------------------------- */
// Always-on memory readout: every 2s print each process's *current* working set (resident
// RAM) — the headline numbers to watch climb/settle while loading and scrolling. mpv runs in a
// forked Node child so it isn't in getAppMetrics(); "renderer" sums all windows (wall + video).
// Electron's main-process V8 has already booted by the time app.commandLine runs, so
// appendSwitch("js-flags","--expose-gc") only reaches renderer processes — global.gc stays
// undefined in main. That left the main process with NO manual GC at all: serving thousands of
// coolmedia:// image requests churns short-lived Buffers/Response/stream objects, nothing runs
// a render loop to pressure V8, and the working set ratcheted up and never fell at idle. Grab a
// real collect handle at runtime instead (standard setFlagsFromString trick) so main can
// actually reclaim them. Returns null only if the V8 API is unavailable.
function makeGc(): (() => void) | null {
  try {
    v8.setFlagsFromString("--expose-gc");
    const fn = runInNewContext("gc") as unknown;
    v8.setFlagsFromString("--no-expose-gc"); // leave the flag as we found it
    return typeof fn === "function" ? (fn as () => void) : null;
  } catch {
    return null;
  }
}
const forceGc = makeGc();
let imageServeCount = 0; // nudge a collect every N served images (see coolmedia handler)

// On-disk thumbnail cache. The wall pulls hundreds of tiles; serving the multi-MB originals
// through the main process is what kept the browser working set high (Electron retains a chunk
// of every served body, beyond GC's reach) AND made loads slow. Instead we decode+downscale
// once with nativeImage, cache a small JPEG to disk, and serve that (~40KB) — so the wall moves
// ~100x less data through main and re-visits are an instant tiny read. Full-res (focus) and
// formats nativeImage can't decode fall back to the original.
let THUMB_DIR = "";
async function makeThumb(abs: string, st: Stats, w: number): Promise<Buffer | null> {
  const key = createHash("sha1").update(`${abs}|${st.mtimeMs}|${st.size}|${w}`).digest("hex");
  const cacheFile = path.join(THUMB_DIR, `${key}.jpg`);
  try {
    return await fs.readFile(cacheFile); // hit — tiny read, no decode
  } catch {
    /* miss → generate below */
  }
  try {
    const img = nativeImage.createFromPath(abs);
    if (img.isEmpty()) return null; // unsupported format (HEIC/RAW/…) → caller serves original
    const jpeg = img.resize({ width: w, quality: "good" }).toJPEG(72);
    if (jpeg.length) fs.writeFile(cacheFile, jpeg).catch(() => {}); // fire-and-forget cache write
    return jpeg.length ? jpeg : null;
  } catch {
    return null;
  }
}

let memTimer: ReturnType<typeof setInterval> | null = null;
function startMemLog(): void {
  if (memTimer) return;
  console.log(`[mem] main gc ${forceGc ? "active" : "unavailable"}`);
  const mb = (kb: number) => String(Math.round(kb / 1024)).padStart(4);
  memTimer = setInterval(() => {
    // Main is idle (no render loop), so a periodic collect is cheap and keeps the working set
    // flat instead of letting per-request image buffers pile up.
    forceGc?.();
    try {
      const m = app.getAppMetrics();
      const sum = (type: string) =>
        m.filter((x) => x.type === type).reduce((s, x) => s + x.memory.workingSetSize, 0);
      console.log(
        `[mem] browser ${mb(sum("Browser"))}MB · renderer ${mb(sum("Tab"))}MB · gpu ${mb(sum("GPU"))}MB`,
      );
    } catch {
      /* ignore */
    }
  }, 2000);
}

function createWindow() {
  win = new BrowserWindow({
    width: 1440,
    height: 900,
    // Real OS-framed window (title bar + native min/max/close + edge resize). Embed mode
    // renders mpv ON TOP of the web (raised in the addon's FitWindow), so no transparency
    // is needed — the window stays a normal opaque framed window.
    backgroundColor: "#000000",
    autoHideMenuBar: true,
    // Don't show the window until the renderer has painted its first frame (the black
    // boot splash) — otherwise Windows briefly shows an empty white window first.
    show: false,
    webPreferences: {
      preload: path.join(__dirname, "preload.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: false,
      // Keep WebGL hardware-accelerated; let pages persist GPU resources.
      backgroundThrottling: false,
    },
  });
  win.once("ready-to-show", () => win?.show());
  // Surface the wall's [wall mem] diagnostics (load/scroll/gc) in the terminal too, not just
  // DevTools, so they show alongside the periodic [mem] line under run-embed.bat.
  win.webContents.on("console-message", (e) => {
    if (e.message.startsWith("[wall")) console.log(e.message);
  });
  startMemLog();
  // Two-window mode: keep the child video window aligned to the main window's content,
  // and re-fit mpv inside it, as the user moves/resizes/maximizes.
  if (EMBED_MODE) {
    win.on("resize", () => {
      positionVideoWin();
      mpvFit();
    });
    win.on("move", positionVideoWin);
    win.on("maximize", positionVideoWin);
    win.on("unmaximize", positionVideoWin);
    const reFit = () => {
      positionVideoWin();
      mpvFit();
    };
    win.on("enter-full-screen", reFit);
    win.on("leave-full-screen", reFit);
  }

  // Two-window: the main window is a normal framed wall; videos play in a child window.
  const q = EMBED_MODE ? "?twowin=1" : "";
  if (VITE_DEV_SERVER_URL) {
    win.loadURL(VITE_DEV_SERVER_URL + q);
    win.webContents.openDevTools({ mode: "detach" });
  } else {
    win.loadURL("app://bundle/" + q);
    // No menu in production → removes the default Reload/DevTools accelerators.
    Menu.setApplicationMenu(null);
    // Belt-and-suspenders: also swallow Chromium's built-in reload keys. A reload
    // tears down the WebGL wall and the in-memory feed → black screen, so block it.
    win.webContents.on("before-input-event", (event, input) => {
      if (input.type !== "keyDown") return;
      const key = input.key.toLowerCase();
      const reload = (input.control || input.meta) && key === "r";
      if (reload || key === "f5") {
        event.preventDefault();
        return;
      }
      // F12 still toggles DevTools (the default menu accelerator is gone).
      if (key === "f12") {
        event.preventDefault();
        win?.webContents.toggleDevTools();
      }
    });
  }

  win.on("closed", () => {
    win = null;
  });
}

// Two-window embed: a transparent, frameless CHILD window stacked on the main window. mpv
// renders hardware-decoded video into THIS window (under its web layer, with the controls
// overlaid — the proven embed layering), giving native fps while the MAIN window stays a
// normal framed wall. Child windows always sit above their parent, so there's no z-order
// fight. It's hidden until a video plays, and tracks the main window's content area.
function createVideoWindow() {
  if (!win) return;
  videoWin = new BrowserWindow({
    parent: win,
    transparent: true,
    frame: false,
    show: false,
    resizable: false,
    skipTaskbar: true,
    hasShadow: false,
    backgroundColor: "#00000000",
    webPreferences: {
      preload: path.join(__dirname, "preload.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: false,
      backgroundThrottling: false,
    },
  });
  const u = "?videochild=1&embed=1"; // slim player view + embed (transparent) styling
  if (VITE_DEV_SERVER_URL) videoWin.loadURL(VITE_DEV_SERVER_URL + u);
  else videoWin.loadURL("app://bundle/" + u);
  videoWin.on("closed", () => {
    videoWin = null;
  });
}

// Align the child video window exactly over the main window's content area (screen coords).
function positionVideoWin() {
  if (!win || !videoWin || !videoWin.isVisible()) return;
  try {
    videoWin.setBounds(win.getContentBounds());
  } catch {
    /* ignore */
  }
}

// Expose globalThis.gc() in the renderer so the wall can hand the JS heap back to the OS when
// it goes idle after a large tile-eviction burst (see WallScene.scheduleGc). Must be set before
// the app is ready. Harmless if unused.
app.commandLine.appendSwitch("js-flags", "--expose-gc");

app.whenReady().then(() => {
  THUMB_DIR = path.join(app.getPath("userData"), "thumb-cache");
  fs.mkdir(THUMB_DIR, { recursive: true }).catch(() => {});

  protocol.handle("coolmedia", async (request) => {
    const reqUrl = new URL(request.url);
    const abs = decodeURIComponent(reqUrl.pathname.replace(/^\//, ""));
    let st: Stats;
    try {
      st = await fs.stat(abs);
    } catch {
      return new Response(null, { status: 404 });
    }
    const size = st.size;
    // ACAO so the app://bundle renderer can use these as WebGL textures / canvas
    // posters; Accept-Ranges so <video>/<audio> can seek. Streamed (never read whole
    // files into JS); the read stream closes when the response is consumed/cancelled.
    const mime = mimeFor(abs);
    // Do NOT cache. Electron runs the network service IN the browser process, so its in-memory
    // HTTP cache lives there too — caching the original image bytes (multi-MB each) made the
    // browser process retain hundreds of MB that never dropped at idle. Re-fetching on
    // scroll-back is cheap now that images are served by a single fs.readFile (below), so we
    // keep nothing cached and let the working set fall back to baseline when scrolling stops.
    const base: Record<string, string> = {
      "Access-Control-Allow-Origin": "*",
      "Accept-Ranges": "bytes",
      "Content-Type": mime,
      "Cache-Control": "no-store",
    };
    const stream = (start?: number, end?: number) => {
      const rs = createReadStream(abs, start === undefined ? {} : { start, end });
      rs.on("error", () => rs.destroy());
      return Readable.toWeb(rs) as unknown as ReadableStream;
    };

    const m = /bytes=(\d*)-(\d*)/.exec(request.headers.get("Range") ?? "");

    // Wall thumbnail: the wall requests images with ?w=N. Serve a small cached JPEG instead of
    // the original so the main process never moves multi-MB bodies for the hundreds of tiles a
    // wall loads (that was the browser-process memory floor) and tiles load fast. Falls through
    // to the original on failure (unsupported format) or for full-res focus loads (no ?w).
    const w = Number(reqUrl.searchParams.get("w"));
    if (!m && w > 0 && mime.startsWith("image/")) {
      const thumb = await makeThumb(abs, st, w);
      if (thumb) {
        if (forceGc && ++imageServeCount % 48 === 0) setImmediate(forceGc);
        return new Response(new Uint8Array(thumb), {
          status: 200,
          headers: { ...base, "Content-Type": "image/jpeg", "Content-Length": String(thumb.length) },
        });
      }
    }

    // Full-res image (focus) or a format we couldn't thumbnail: serve the original whole.
    // fs.readFile opens, reads and closes the fd in a single call, leaving nothing behind.
    // Large media keeps streaming below (never read a whole video into memory).
    if (!m && mime.startsWith("image/")) {
      try {
        const buf = await fs.readFile(abs);
        const res = new Response(new Uint8Array(buf), {
          status: 200,
          headers: { ...base, "Content-Length": String(buf.length) },
        });
        // Each served image left a whole-file Buffer for V8 to reclaim. The 2s timer alone lets
        // a fast scroll pile up a big transient peak between collects, so also nudge a collect
        // every N images — keeps the peak down, not just the idle floor. (Deferred so it never
        // blocks the response.) Cheap: main has no render loop.
        if (forceGc && ++imageServeCount % 48 === 0) setImmediate(forceGc);
        return res;
      } catch {
        return new Response(null, { status: 404 });
      }
    }

    if (m) {
      let start = m[1] ? parseInt(m[1], 10) : 0;
      let end = m[2] ? parseInt(m[2], 10) : size - 1;
      if (!Number.isFinite(start) || start < 0 || start >= size) {
        return new Response(null, { status: 416, headers: { ...base, "Content-Range": `bytes */${size}` } });
      }
      end = Math.min(end, size - 1);
      return new Response(stream(start, end), {
        status: 206,
        headers: { ...base, "Content-Range": `bytes ${start}-${end}/${size}`, "Content-Length": String(end - start + 1) },
      });
    }
    return new Response(stream(), { status: 200, headers: { ...base, "Content-Length": String(size) } });
  });

  // Serve the renderer bundle from dist/, with SPA fallback to index.html.
  protocol.handle("app", async (request) => {
    const { pathname } = new URL(request.url);
    const rel = pathname === "/" ? "/index.html" : decodeURIComponent(pathname);
    const filePath = path.join(RENDERER_DIST, rel);
    const indexFile = path.join(RENDERER_DIST, "index.html");
    // Stay inside dist/; anything else (or a missing deep route) falls back to index.html.
    const target = filePath.startsWith(RENDERER_DIST) ? filePath : indexFile;
    try {
      return await net.fetch(pathToFileURL(target).toString());
    } catch {
      return net.fetch(pathToFileURL(indexFile).toString());
    }
  });

  createWindow();
  // Two-window embed (opt-in COOLIRIS_MPV_EMBED=1, Windows): create the child video window,
  // attach mpv to ITS handle, then warm. The child stays hidden until a video plays.
  if (EMBED_MODE && win) {
    createVideoWindow();
    const attachAndWarm = () => {
      try {
        const handle = videoWin!.getNativeWindowHandle();
        const wid = handle.readBigUInt64LE(0).toString();
        setEmbedWid(wid);
        console.log("[mpv] two-window embed ON, video-window wid =", wid);
      } catch (e) {
        console.error("[mpv] getNativeWindowHandle (video window) failed:", e);
      }
      mpvWarm();
    };
    if (videoWin) videoWin.webContents.once("did-finish-load", attachAndWarm);
    else mpvWarm();
  } else {
    // Non-embed: warm immediately (fork is non-blocking, runs in a child process).
    mpvWarm();
  }

  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on("will-quit", () => mpvDestroy());

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});
