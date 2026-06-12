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
  // libmpv all-format player (Option C).
  mpvAvailable: () => ipcRenderer.invoke("mpv-available"),
  mpvLoad: (abs: string) => ipcRenderer.invoke("mpv-load", abs),
  mpvCmd: (args: string[]) => ipcRenderer.invoke("mpv-cmd", args),
  mpvSet: (name: string, value: string) => ipcRenderer.invoke("mpv-set", name, value),
  // Fire-and-forget batched set (zoom/pan): no invoke/await, so no round-trip latency.
  mpvSetFast: (props: Record<string, string>) => ipcRenderer.send("mpv-set-fast", props),
  mpvGet: (name: string) => ipcRenderer.invoke("mpv-get", name),
  mpvSize: () => ipcRenderer.invoke("mpv-size"),
  mpvFrame: (w: number, h: number) => ipcRenderer.invoke("mpv-frame", w, h),
  mpvStop: () => ipcRenderer.invoke("mpv-stop"),
  // Toggle the OS window fullscreen (embed mode uses this instead of the browser's
  // requestFullscreen, which would paint over the mpv video surface). Returns the
  // resulting fullscreen state.
  winFullscreen: (on: boolean) => ipcRenderer.invoke("win-fullscreen", on),
  // Two-window embed: main wall asks to play a video in the child window; the child
  // listens for the path; either side can close.
  playVideo: (abs: string) => ipcRenderer.invoke("play-video", abs),
  videoReady: () => ipcRenderer.invoke("video-ready"),
  closeVideo: () => ipcRenderer.invoke("close-video"),
  onVideoPlay: (cb: (abs: string) => void) => {
    const h = (_e: unknown, abs: string) => cb(abs);
    ipcRenderer.on("video-play", h);
    return () => ipcRenderer.removeListener("video-play", h);
  },
  onVideoClosed: (cb: () => void) => {
    const h = () => cb();
    ipcRenderer.on("video-closed", h);
    return () => ipcRenderer.removeListener("video-closed", h);
  },
  // Prev/next from inside the player: the child asks, main relays to the wall window which
  // finds the adjacent video and plays it.
  videoNav: (dir: "prev" | "next") => ipcRenderer.invoke("video-nav", dir),
  onVideoNav: (cb: (dir: "prev" | "next") => void) => {
    const h = (_e: unknown, dir: "prev" | "next") => cb(dir);
    ipcRenderer.on("video-nav-main", h);
    return () => ipcRenderer.removeListener("video-nav-main", h);
  },
});
