// Main-process proxy to the mpv host (Option C). mpv runs in a utilityProcess so it
// doesn't share Chromium's bundled libffmpeg.so — it uses the full system ffmpeg, so
// every codec/subtitle works (no DEEPBIND hacks). We forward load/control + frame pulls
// over parentPort and resolve replies by id.
import { app, utilityProcess, type UtilityProcess } from "electron";
import { existsSync } from "node:fs";
import path from "node:path";

function pick(rel: string): string {
  const dev = path.join(app.getAppPath(), rel);
  const packed = path.join(process.resourcesPath, "app.asar.unpacked", rel);
  return existsSync(dev) ? dev : packed;
}
const hostScript = () => pick(path.join("electron", "mpvHost.cjs"));
const addonFile = () => pick(path.join("native", "build", "Release", "mpv.node"));

let child: UtilityProcess | null = null;
let nextId = 1;
const pending = new Map<number, (v: unknown) => void>();

function ensureChild(): UtilityProcess {
  if (child) return child;
  // Pass the full env so mpv's audio output can reach PipeWire/Pulse (XDG_RUNTIME_DIR…).
  child = utilityProcess.fork(hostScript(), [], { stdio: "inherit", env: process.env });
  child.on("message", (msg: { id: number; result: unknown }) => {
    const cb = pending.get(msg.id);
    if (cb) {
      pending.delete(msg.id);
      cb(msg.result);
    }
  });
  child.on("exit", () => {
    child = null;
    pending.forEach((cb) => cb(null));
    pending.clear();
  });
  return child;
}

function call<T = unknown>(fn: string, args: unknown[]): Promise<T> {
  return new Promise((resolve) => {
    const id = nextId++;
    pending.set(id, resolve as (v: unknown) => void);
    ensureChild().postMessage({ id, fn, args });
  });
}

export function mpvAvailable(): boolean {
  return existsSync(addonFile()) && existsSync(hostScript());
}
export const mpvLoad = (abs: string) => call("load", [abs]);
export const mpvCommand = (args: string[]) => call<boolean>("cmd", [args]);
export const mpvSet = (name: string, value: string) => call<boolean>("set", [name, value]);
export const mpvGet = (name: string) => call<string | null>("get", [name]);
export const mpvVideoSize = () => call<{ w: number; h: number }>("size", []);
export const mpvFrame = (w: number, h: number) => call<Uint8Array | null>("frame", [w, h]);
export const mpvStop = () => call("stop", []);
export function mpvDestroy(): void {
  if (child) {
    try {
      child.kill();
    } catch {
      /* already gone */
    }
    child = null;
  }
}
