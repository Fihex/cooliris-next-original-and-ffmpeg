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
  // libVLC all-format player (Option C).
  vlcAvailable: () => ipcRenderer.invoke("vlc-available"),
  vlcLoad: (abs: string) => ipcRenderer.invoke("vlc-load", abs),
  vlcCmd: (args: string[]) => ipcRenderer.invoke("vlc-cmd", args),
  vlcSet: (name: string, value: string) => ipcRenderer.invoke("vlc-set", name, value),
  vlcGet: (name: string) => ipcRenderer.invoke("vlc-get", name),
  vlcSize: () => ipcRenderer.invoke("vlc-size"),
  vlcFrame: (w: number, h: number) => ipcRenderer.invoke("vlc-frame", w, h),
  vlcStop: () => ipcRenderer.invoke("vlc-stop"),
  vlcStyle: (args: string[]) => ipcRenderer.invoke("vlc-style", args),
});
