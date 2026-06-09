// Main-process proxy to the mpv host (Option C). mpv runs in a utilityProcess so it
// doesn't share Chromium's bundled libffmpeg.so — it uses the full system ffmpeg, so
// every codec/subtitle works (no DEEPBIND hacks). We forward load/control + frame pulls
// over parentPort and resolve replies by id.
import { app, utilityProcess, type UtilityProcess } from "electron";
import { execSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";

function pick(rel: string): string {
  const dev = path.join(app.getAppPath(), rel);
  const packed = path.join(process.resourcesPath, "app.asar.unpacked", rel);
  return existsSync(dev) ? dev : packed;
}
const hostScript = () => pick(path.join("electron", "mpvHost.cjs"));
const addonFile = () => pick(path.join("native", "build", "Release", "mpv.node"));

// The mpv host runs as the Electron binary, which loads Chromium's cut-down libffmpeg.so
// (no ac3/subtitle/etc. decoders). LD_PRELOAD the FULL system ffmpeg libs that libmpv
// links, so its symbols win in that process. Safe — the host does no Chromium media.
function ffmpegPreload(): string {
  if (process.platform !== "linux") return "";
  try {
    const out = execSync(`ldd "${addonFile()}"`, { encoding: "utf8" });
    const re = /=>\s*(\/\S+\/lib(?:avcodec|avformat|avutil|avfilter|swresample|swscale|avdevice)\.so[^\s]*)/g;
    const libs = [...out.matchAll(re)].map((m) => m[1]);
    return [...new Set(libs)].join(" ");
  } catch {
    return "";
  }
}

let child: UtilityProcess | null = null;
let nextId = 1;
const pending = new Map<number, (v: unknown) => void>();

function ensureChild(): UtilityProcess {
  if (child) return child;
  // Pipe the host's stdout/stderr through here so mpv's logs actually reach the terminal
  // (utilityProcess "inherit" often doesn't). Pass full env for PipeWire/Pulse audio.
  const preload = ffmpegPreload();
  const env = { ...process.env };
  if (preload) env.LD_PRELOAD = preload + (env.LD_PRELOAD ? " " + env.LD_PRELOAD : "");
  console.log("[mpv] starting host:", hostScript());
  console.log("[mpv] LD_PRELOAD:", preload || "(none)");
  child = utilityProcess.fork(hostScript(), [], { stdio: "pipe", env });
  child.stdout?.on("data", (d) => process.stdout.write(`[mpv-host] ${d}`));
  child.stderr?.on("data", (d) => process.stderr.write(`[mpv-host] ${d}`));
  child.on("message", (msg: { id: number; result: unknown }) => {
    const cb = pending.get(msg.id);
    if (cb) {
      pending.delete(msg.id);
      cb(msg.result);
    }
  });
  child.on("exit", (code) => {
    console.log("[mpv] host exited", code);
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
