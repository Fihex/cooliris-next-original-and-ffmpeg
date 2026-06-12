import type { Feed, MediaItem, ProgressFn } from "@/feed/types";
import {
  currentLocalUrls,
  feedFromDataTransfer,
  feedFromFiles,
  isAudio,
  isVideo,
  makeAudioThumb,
  makeVideoThumb,
  mapPool,
  revokeLocalUrls,
  revokeUrls,
} from "@/feed/localFiles";
import type { Platform } from "./Platform";

/* ----- bridge contract (must match electron/preload.ts + main.ts) ----- */
interface ScanFile {
  abs: string;
  rel: string;
  name: string;
  mtime: number;
  btime: number;
  cover?: string;
}
interface ScanResult {
  rootName: string;
  files: ScanFile[];
}
interface ElectronBridge {
  pickFolder(): Promise<ScanResult | null>;
  pickFiles(): Promise<ScanResult | null>;
  fetchText(url: string): Promise<string>;
  getCover(abs: string): Promise<string | null>;
  getPathForFile(file: File): string;
  readFileBytes(abs: string): Promise<ArrayBuffer>;
  statFile(abs: string): Promise<{ mtime: number; btime: number } | null>;
  scanPaths(paths: string[]): Promise<ScanResult | null>;
  // libVLC all-format player (Option C).
  vlcAvailable(): Promise<boolean>;
  vlcLoad(abs: string): Promise<unknown>;
  vlcCmd(args: string[]): Promise<boolean>;
  vlcSet(name: string, value: string): Promise<boolean>;
  vlcGet(name: string): Promise<string | null>;
  vlcSize(): Promise<{ w: number; h: number }>;
  vlcFrame(w: number, h: number): Promise<Uint8Array | null>;
  vlcStop(): Promise<unknown>;
  vlcStyle(args: string[]): Promise<boolean>;
}

declare global {
  interface Window {
    electron?: ElectronBridge;
  }
}

let eid = 0;

/** Stream a local file to the renderer via the privileged custom protocol. */
function mediaUrl(abs: string): string {
  return `coolmedia://f/${encodeURIComponent(abs)}`;
}

/** Match the web picker's cancel semantics so WallView ignores it. */
function aborted(): never {
  throw new DOMException("Picker cancelled", "AbortError");
}

async function feedFromScan(res: ScanResult, onProgress?: ProgressFn): Promise<Feed> {
  const files = [...res.files].sort((a, b) =>
    a.rel.localeCompare(b.rel, undefined, { numeric: true })
  );
  const items = await mapPool(
    files,
    6,
    async (f): Promise<MediaItem> => {
      const url = mediaUrl(f.abs);
      const title = f.name.replace(/\.[^.]+$/, "");
      const date = f.mtime || undefined;
      const created = f.btime || undefined;
      if (isVideo(f.name)) {
        const poster = await makeVideoThumb(url);
        return {
          id: `el-${eid++}`,
          type: "video",
          thumb: poster ?? "",
          full: url,
          title,
          path: f.rel,
          date,
          created,
        };
      }
      if (isAudio(f.name)) {
        // Prefer embedded cover art, then a sidecar cover image, then a generated tile.
        const art =
          (await window.electron!.getCover(f.abs)) ??
          (f.cover ? mediaUrl(f.cover) : null) ??
          (await makeAudioThumb(title));
        return {
          id: `el-${eid++}`,
          type: "audio",
          thumb: art ?? "",
          full: url,
          title,
          path: f.rel,
          date,
          created,
          aspect: 512 / 384,
        };
      }
      return {
        id: `el-${eid++}`,
        type: "image",
        animated: /\.gif$/i.test(f.name) || undefined,
        thumb: url,
        full: url,
        title,
        path: f.rel,
        date,
        created,
      };
    },
    onProgress
  );
  return { title: `${res.rootName} — ${items.length} item(s)`, items };
}

/** Electron implementation: native dialogs + Node fs scan, streamed via coolmedia://. */
export const electronPlatform: Platform = {
  name: "electron",
  supportsFolderPicker: true,

  // Fetch remote feeds via the main process — no CORS, no third-party proxy.
  fetchRemoteText: (url: string): Promise<string> => window.electron!.fetchText(url),

  pickFiles: async (onProgress?: ProgressFn): Promise<Feed> => {
    const r = await window.electron!.pickFiles();
    if (!r) aborted();
    return feedFromScan(r, onProgress);
  },
  pickFolder: async (onProgress?: ProgressFn): Promise<Feed> => {
    const r = await window.electron!.pickFolder();
    if (!r) aborted();
    return feedFromScan(r, onProgress);
  },

  // Drag-and-drop / <input>: resolve the real paths and run the SAME native scan as
  // the pickers → full parity (covers, dates, seekable coolmedia URLs).
  // Falls back to the web object-URL path if a path can't be resolved.
  fromDataTransfer: async (dt: DataTransfer, onProgress?: ProgressFn): Promise<Feed> => {
    const paths = Array.from(dt.files)
      .map((f) => window.electron!.getPathForFile(f))
      .filter(Boolean);
    const r = paths.length ? await window.electron!.scanPaths(paths) : null;
    return r ? feedFromScan(r, onProgress) : feedFromDataTransfer(dt, onProgress);
  },
  fromFileList: async (files: File[], onProgress?: ProgressFn): Promise<Feed> => {
    const paths = files.map((f) => window.electron!.getPathForFile(f)).filter(Boolean);
    const r = paths.length ? await window.electron!.scanPaths(paths) : null;
    return r ? feedFromScan(r, onProgress) : feedFromFiles(files, onProgress);
  },

  // Only the DnD/input fallbacks create object URLs; coolmedia:// needs no revoke.
  snapshotResources: () => currentLocalUrls(),
  releaseResources: (snapshot: unknown) => revokeUrls((snapshot as string[]) ?? []),
  releaseAll: () => revokeLocalUrls(),
};
