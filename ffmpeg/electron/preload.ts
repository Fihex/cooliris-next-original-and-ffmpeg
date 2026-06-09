import { contextBridge, ipcRenderer, webUtils } from "electron";

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
  statFile: (abs: string) => ipcRenderer.invoke("stat-file", abs),
  scanPaths: (paths: string[]) => ipcRenderer.invoke("scan-paths", paths),
  // ffmpeg layer (extended formats) — no-ops when disabled in config.
  getConfig: () => ipcRenderer.invoke("get-config"),
  setHwAccel: (on: boolean) => ipcRenderer.invoke("set-hwaccel", on),
  setFfmpegEnabled: (on: boolean) => ipcRenderer.invoke("set-ffmpeg-enabled", on),
  ffProbe: (abs: string, container: string) => ipcRenderer.invoke("ff-probe", abs, container),
  ffPoster: (abs: string) => ipcRenderer.invoke("ff-poster", abs),
  ffSubtitle: (abs: string, index: number) => ipcRenderer.invoke("ff-subtitle", abs, index),
  ffPrepare: (abs: string, audioIndex: number) => ipcRenderer.invoke("ff-prepare", abs, audioIndex),
  // Subscribe to prepare progress (0–100); returns an unsubscribe fn.
  onFfProgress: (cb: (pct: number) => void) => {
    const h = (_e: unknown, pct: number) => cb(pct);
    ipcRenderer.on("ff-progress", h);
    return () => ipcRenderer.removeListener("ff-progress", h);
  },
  // The encode path chosen for the current prepare (e.g. "GPU · h264_nvenc", "CPU · libx264").
  onFfPrepareMode: (cb: (mode: string) => void) => {
    const h = (_e: unknown, mode: string) => cb(mode);
    ipcRenderer.on("ff-prepare-mode", h);
    return () => ipcRenderer.removeListener("ff-prepare-mode", h);
  },
});
