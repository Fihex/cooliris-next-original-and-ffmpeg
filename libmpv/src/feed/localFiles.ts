import type { Feed, MediaItem, ProgressFn } from "./types";
import { parseCover } from "./coverArt";

const IMAGE_RE = /\.(jpe?g|png|gif|webp|avif|bmp|svg)$/i;
// mkv/avi are listed so they appear on the wall; Chromium can't decode/poster
// most of them, so they show as error ("broken") tiles. Real playback would need
// an ffmpeg remux/transcode layer (desktop-only).
const VIDEO_RE = /\.(mp4|webm|ogv|mov|m4v|mkv|avi)$/i;
const AUDIO_RE = /\.(mp3|wav|ogg|oga|flac|m4a|aac|opus|weba)$/i;

export const isImage = (name: string) => IMAGE_RE.test(name);
export const isVideo = (name: string) => VIDEO_RE.test(name);
export const isAudio = (name: string) => AUDIO_RE.test(name);
export const isMedia = (name: string) => isImage(name) || isVideo(name) || isAudio(name);

let localId = 0;

/** Object URLs created from local files; revoked when no longer referenced. */
const liveUrls = new Set<string>();

function track(url: string): string {
  liveUrls.add(url);
  return url;
}

/** Snapshot the currently-tracked URLs (call before loading a new feed). */
export function currentLocalUrls(): string[] {
  return Array.from(liveUrls);
}

/** Revoke a specific set of object URLs (the previous feed's). */
export function revokeUrls(urls: string[]): void {
  for (const url of urls) {
    URL.revokeObjectURL(url);
    liveUrls.delete(url);
  }
}

/** Revoke everything (used on teardown). */
export function revokeLocalUrls(): void {
  for (const url of liveUrls) URL.revokeObjectURL(url);
  liveUrls.clear();
}

/** Run async work with bounded concurrency so big folders don't stampede. */
export async function mapPool<T, R>(
  items: T[],
  limit: number,
  fn: (item: T, index: number) => Promise<R>,
  onProgress?: (done: number, total: number) => void
): Promise<R[]> {
  const results = new Array<R>(items.length);
  let cursor = 0;
  let done = 0;
  const total = items.length;
  const workers = Array.from({ length: Math.min(limit, total) }, async () => {
    while (cursor < total) {
      const i = cursor++;
      results[i] = await fn(items[i], i);
      done++;
      if (onProgress && (done % 64 === 0 || done === total)) onProgress(done, total);
    }
  });
  await Promise.all(workers);
  return results;
}


/** Capture the first frame of a video File as a poster thumbnail (object URL). */
export async function makeVideoThumb(fullUrl: string): Promise<string | null> {
  return new Promise((resolve) => {
    const video = document.createElement("video");
    video.muted = true;
    video.preload = "metadata";
    // Needed so drawing the frame to a canvas doesn't taint it (the file is served
    // cross-origin from coolmedia://, which now sends Access-Control-Allow-Origin).
    video.crossOrigin = "anonymous";
    video.src = fullUrl;
    const done = (result: string | null) => {
      video.removeAttribute("src");
      video.load();
      resolve(result);
    };
    video.addEventListener(
      "loadeddata",
      () => {
        try {
          const maxEdge = 512;
          const scale = Math.min(
            1,
            maxEdge / Math.max(video.videoWidth, video.videoHeight)
          );
          const canvas = document.createElement("canvas");
          canvas.width = Math.max(1, Math.round(video.videoWidth * scale));
          canvas.height = Math.max(1, Math.round(video.videoHeight * scale));
          const ctx = canvas.getContext("2d");
          if (!ctx) return done(null);
          ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
          canvas.toBlob(
            (blob) => done(blob ? track(URL.createObjectURL(blob)) : null),
            "image/jpeg",
            0.82
          );
        } catch {
          done(null);
        }
      },
      { once: true }
    );
    video.addEventListener("error", () => done(null));
    video.currentTime = 0.1;
  });
}

/** Draw a generic cover-art thumbnail for an audio file (no embedded artwork). */
export async function makeAudioThumb(title: string): Promise<string | null> {
  try {
    const w = 512;
    const h = 384;
    const canvas = document.createElement("canvas");
    canvas.width = w;
    canvas.height = h;
    const ctx = canvas.getContext("2d");
    if (!ctx) return null;
    const grad = ctx.createLinearGradient(0, 0, w, h);
    grad.addColorStop(0, "#334155");
    grad.addColorStop(1, "#0f172a");
    ctx.fillStyle = grad;
    ctx.fillRect(0, 0, w, h);
    ctx.fillStyle = "rgba(255,255,255,0.92)";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.font = "160px system-ui, 'Segoe UI Symbol', sans-serif";
    ctx.fillText("♪", w / 2, h / 2 - 24);
    ctx.fillStyle = "rgba(255,255,255,0.7)";
    ctx.font = "22px system-ui, sans-serif";
    const label = title.length > 30 ? `${title.slice(0, 29)}…` : title;
    ctx.fillText(label, w / 2, h - 52);
    return await new Promise((resolve) =>
      canvas.toBlob(
        (blob) => resolve(blob ? track(URL.createObjectURL(blob)) : null),
        "image/png"
      )
    );
  } catch {
    return null;
  }
}

/**
 * Build a MediaItem from a File. Images are *instant* (just an object URL —
 * the wall decodes/loads them lazily). Videos get a poster frame; audio gets a
 * generated cover-art tile.
 */
/** Extract embedded cover art from an audio File → a tracked blob URL, or null. */
async function fileCover(file: File): Promise<string | null> {
  try {
    const bytes = new Uint8Array(await file.arrayBuffer());
    const ext = file.name.slice(file.name.lastIndexOf(".") + 1);
    const c = parseCover(bytes, ext);
    if (!c) return null;
    // Copy into a standalone ArrayBuffer-backed array (detaches the view + satisfies BlobPart).
    const img = new Uint8Array(c.data.length);
    img.set(c.data);
    return track(URL.createObjectURL(new Blob([img], { type: c.mime })));
  } catch {
    return null;
  }
}

async function fileToItem(file: File, pathHint?: string): Promise<MediaItem> {
  const fullUrl = track(URL.createObjectURL(file));
  const path = pathHint ?? file.name; // relative path within the opened folder
  const title = file.name.replace(/\.[^.]+$/, ""); // bare filename (no folders)
  let date = file.lastModified || undefined;
  let created: number | undefined;
  // In Electron, resolve the real path to also get the created (birthtime) date, so
  // drag-and-drop matches the folder scan (the renderer alone only has lastModified).
  const realPath = window.electron?.getPathForFile?.(file);
  if (realPath) {
    const meta = await window.electron!.statFile(realPath);
    if (meta) {
      date = meta.mtime || date;
      created = meta.btime || undefined;
    }
  }
  if (isVideo(file.name)) {
    const poster = await makeVideoThumb(fullUrl);
    return {
      id: `local-${localId++}`,
      type: "video",
      thumb: poster ?? "",
      full: fullUrl,
      title,
      path,
      date,
      created,
    };
  }
  if (isAudio(file.name)) {
    // Embedded cover art (parsed from the file's bytes in the renderer) → generated tile.
    const art = (await fileCover(file)) ?? (await makeAudioThumb(title));
    return {
      id: `local-${localId++}`,
      type: "audio",
      thumb: art ?? "",
      full: fullUrl,
      title,
      path,
      date,
      created,
      aspect: 512 / 384,
    };
  }
  return {
    id: `local-${localId++}`,
    type: "image",
    animated: /\.gif$/i.test(file.name) || undefined,
    thumb: fullUrl,
    full: fullUrl,
    title,
    path,
    date,
    created,
  };
}

/** Build a Feed from a flat list of File objects (input[multiple] / drag-drop). */
export async function feedFromFiles(files: File[], onProgress?: ProgressFn): Promise<Feed> {
  const media = files.filter((f) => isMedia(f.name));
  // Stable, human-friendly order.
  media.sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }));
  const items = await mapPool(
    media,
    6,
    (f) => {
      const rel = (f as unknown as { _path?: string; webkitRelativePath?: string });
      return fileToItem(f, rel._path || rel.webkitRelativePath || f.name);
    },
    onProgress
  );
  return { title: `${items.length} local file(s)`, items };
}

/* ----------------------- File System Access API ----------------------- */

interface FSDirHandle {
  kind: "directory";
  name: string;
  values(): AsyncIterableIterator<FSDirHandle | FSFileHandle>;
}
interface FSFileHandle {
  kind: "file";
  name: string;
  getFile(): Promise<File>;
}

declare global {
  interface Window {
    showOpenFilePicker?: (opts?: unknown) => Promise<FSFileHandle[]>;
    showDirectoryPicker?: (opts?: unknown) => Promise<FSDirHandle>;
  }
}

export const supportsFsAccess = () =>
  typeof window !== "undefined" && typeof window.showDirectoryPicker === "function";

async function collectDir(
  dir: FSDirHandle,
  prefix: string,
  out: { file: File; path: string }[],
  onCount?: (n: number) => void
): Promise<void> {
  for await (const entry of dir.values()) {
    const path = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.kind === "file") {
      if (isMedia(entry.name)) {
        out.push({ file: await entry.getFile(), path });
        if (onCount && out.length % 50 === 0) onCount(out.length);
      }
    } else {
      await collectDir(entry, path, out, onCount);
    }
  }
}

/** Open a folder via the File System Access API and recurse into subfolders. */
export async function feedFromDirectoryPicker(onProgress?: ProgressFn): Promise<Feed> {
  if (!window.showDirectoryPicker) throw new Error("Directory picker unsupported.");
  const dir = await window.showDirectoryPicker();
  const collected: { file: File; path: string }[] = [];
  // total is unknown while scanning → report (n, 0) so the UI can show "Scanning N".
  await collectDir(dir, "", collected, (n) => onProgress?.(n, 0));
  collected.sort((a, b) =>
    a.path.localeCompare(b.path, undefined, { numeric: true })
  );
  const items = await mapPool(
    collected,
    6,
    ({ file, path }) => fileToItem(file, path),
    onProgress
  );
  return { title: `${dir.name} — ${items.length} item(s)`, items };
}

/** Open one or more files via the File System Access API. */
export async function feedFromFilePicker(onProgress?: ProgressFn): Promise<Feed> {
  if (!window.showOpenFilePicker) throw new Error("File picker unsupported.");
  const handles = await window.showOpenFilePicker({
    multiple: true,
    types: [
      {
        description: "Media",
        accept: {
          "image/*": [".jpg", ".jpeg", ".png", ".gif", ".webp", ".avif", ".bmp", ".svg"],
          "video/*": [".mp4", ".webm", ".ogv", ".mov", ".m4v"],
        },
      },
    ],
  });
  const files = await Promise.all(handles.map((h) => h.getFile()));
  return feedFromFiles(files, onProgress);
}

/** Extract Files from a drag-and-drop DataTransfer, recursing into folders. */
export async function feedFromDataTransfer(dt: DataTransfer, onProgress?: ProgressFn): Promise<Feed> {
  const files: File[] = [];

  const readEntry = async (entry: any, prefix = ""): Promise<void> => {
    if (!entry) return;
    if (entry.isFile) {
      const file: File = await new Promise((res, rej) => entry.file(res, rej));
      if (isMedia(file.name)) {
        Object.defineProperty(file, "_path", { value: `${prefix}${file.name}` });
        files.push(file);
      }
    } else if (entry.isDirectory) {
      const reader = entry.createReader();
      // readEntries returns in batches; loop until empty.
      for (;;) {
        const batch: any[] = await new Promise((res) =>
          reader.readEntries((e: any[]) => res(e))
        );
        if (!batch.length) break;
        for (const child of batch) await readEntry(child, `${prefix}${entry.name}/`);
      }
    }
  };

  const items = Array.from(dt.items).filter((i) => i.kind === "file");
  const canRecurse = items.some(
    (i) => typeof (i as any).webkitGetAsEntry === "function"
  );

  if (canRecurse) {
    const entries = items.map((i) => (i as any).webkitGetAsEntry());
    for (const entry of entries) await readEntry(entry);
    return feedFromFiles(files, onProgress);
  }
  return feedFromFiles(Array.from(dt.files), onProgress);
}
