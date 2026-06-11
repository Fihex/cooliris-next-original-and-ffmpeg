import { app, BrowserWindow, dialog, ipcMain, Menu, net, protocol } from "electron";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promises as fs, createReadStream } from "node:fs";
import { Readable } from "node:stream";
import path from "node:path";
import { extractCoverArt } from "./coverArt";
import {
  vlcAvailable,
  vlcWarm,
  vlcLoad,
  vlcCommand,
  vlcSet,
  vlcGet,
  vlcVideoSize,
  vlcFrame,
  vlcStop,
  vlcStyle,
  vlcDestroy,
} from "./vlc";

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

/* ---------------------------------- libVLC ---------------------------------- */
// All-format playback: the renderer loads a file, pulls RGBA frames each animation
// frame, and drives playback through these. Frames are composited (video + subs) by VLC.
ipcMain.handle("vlc-available", () => vlcAvailable());
ipcMain.handle("vlc-load", (_e, abs: string) => vlcLoad(abs));
ipcMain.handle("vlc-cmd", (_e, args: string[]) => vlcCommand(args));
ipcMain.handle("vlc-set", (_e, name: string, value: string) => vlcSet(name, value));
ipcMain.handle("vlc-get", (_e, name: string) => vlcGet(name));
ipcMain.handle("vlc-size", () => vlcVideoSize());
ipcMain.handle("vlc-frame", (_e, w: number, h: number) => vlcFrame(w, h));
ipcMain.handle("vlc-stop", () => vlcStop());
ipcMain.handle("vlc-style", (_e, args: string[]) => vlcStyle(args));

/* --------------------------------- window ----------------------------------- */
function createWindow() {
  win = new BrowserWindow({
    width: 1440,
    height: 900,
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

  if (VITE_DEV_SERVER_URL) {
    win.loadURL(VITE_DEV_SERVER_URL);
    win.webContents.openDevTools({ mode: "detach" });
  } else {
    win.loadURL("app://bundle/");
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

app.whenReady().then(() => {
  protocol.handle("coolmedia", async (request) => {
    const abs = decodeURIComponent(new URL(request.url).pathname.replace(/^\//, ""));
    let size: number;
    try {
      size = (await fs.stat(abs)).size;
    } catch {
      return new Response(null, { status: 404 });
    }
    // ACAO so the app://bundle renderer can use these as WebGL textures / canvas
    // posters; Accept-Ranges so <video>/<audio> can seek. Streamed (never read whole
    // files into JS); the read stream closes when the response is consumed/cancelled.
    const base: Record<string, string> = {
      "Access-Control-Allow-Origin": "*",
      "Accept-Ranges": "bytes",
      "Content-Type": mimeFor(abs),
    };
    const stream = (start?: number, end?: number) => {
      const rs = createReadStream(abs, start === undefined ? {} : { start, end });
      rs.on("error", () => rs.destroy());
      return Readable.toWeb(rs) as unknown as ReadableStream;
    };

    const m = /bytes=(\d*)-(\d*)/.exec(request.headers.get("Range") ?? "");
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
  // Warm the engine as soon as the renderer has loaded (the window is already shown via
  // ready-to-show, and the warm runs in a child process), so libVLC is ready by the time
  // the user opens a file rather than paying the engine init on first open.
  win?.webContents.once("did-finish-load", () => vlcWarm());

  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on("will-quit", () => vlcDestroy());

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});
