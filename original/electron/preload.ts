import { contextBridge, ipcRenderer, webUtils } from "electron";
import { promises as fs } from "node:fs";

/**
 * The only surface exposed to the renderer. Mirrors the shape consumed by
 * src/platform/electron.ts (ElectronBridge). Native dialogs run in the main
 * process and return plain file metadata; media bytes are streamed lazily via
 * the coolmedia:// protocol, so nothing large crosses this bridge.
 */
contextBridge.exposeInMainWorld("electron", {
  pickFolder: () => ipcRenderer.invoke("pick-folder"),
  pickFiles: () => ipcRenderer.invoke("pick-files"),
  fetchText: (url: string) => ipcRenderer.invoke("fetch-text", url),
  getCover: (abs: string) => ipcRenderer.invoke("get-cover", abs),
  // For drag-and-drop / <input> files: resolve the real path, then stat it for the
  // created date (renderer only sees File.lastModified = modified).
  getPathForFile: (file: File) => webUtils.getPathForFile(file),
  // Read a media file's bytes DIRECTLY in the renderer process (preload has Node fs; sandbox is
  // off). Keeps wall images out of the browser process: serving them over coolmedia:// makes
  // the main process retain a working set proportional to the bytes pushed through it.
  readFileBytes: async (abs: string): Promise<ArrayBuffer> => {
    const b = await fs.readFile(abs);
    // Copy out of any shared Buffer pool so the transferable ArrayBuffer is exactly this file.
    return b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength);
  },
  statFile: (abs: string) => ipcRenderer.invoke("stat-file", abs),
  scanPaths: (paths: string[]) => ipcRenderer.invoke("scan-paths", paths),
});
