// Main-process proxy to the mpv host (Option C). mpv runs in a forked SYSTEM Node
// process — not the Electron binary — so it never touches Chromium's cut-down
// libffmpeg.so and uses the full system ffmpeg directly (all codecs, no LD_PRELOAD).
// We forward load/control + frame pulls over Node IPC and resolve replies by id.
import { app } from "electron";
import { fork, execSync, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";

function pick(rel: string): string {
  const dev = path.join(app.getAppPath(), rel);
  const packed = path.join(process.resourcesPath, "app.asar.unpacked", rel);
  return existsSync(dev) ? dev : packed;
}
const hostScript = () => pick(path.join("electron", "mpvHost.cjs"));
const addonFile = () => pick(path.join("native", "build", "Release", "mpv.node"));

// Self-contained runtime bundled by scripts/bundle-*.sh (vendor/node + vendor/lib),
// shipped as extraResources. Only used in the packaged app — in dev we use the system
// Node + system mpv (avoids the bundle interfering with development).
const vendorDir = () => (app.isPackaged ? path.join(process.resourcesPath, "vendor") : "");

// A Node binary to run the host under (must NOT be the Electron binary): bundled if
// present, else system Node.
function nodePath(): string {
  const bundled = path.join(vendorDir(), process.platform === "win32" ? "node.exe" : "node");
  if (existsSync(bundled)) return bundled;
  for (const c of ["/usr/bin/node", "/usr/local/bin/node", "/opt/homebrew/bin/node"]) {
    if (existsSync(c)) return c;
  }
  try {
    const p = execSync(process.platform === "win32" ? "where node" : "command -v node", {
      encoding: "utf8",
    })
      .split("\n")[0]
      .trim();
    if (p) return p;
  } catch {
    /* fall through */
  }
  return "node";
}

let child: ChildProcess | null = null;
let nextId = 1;
const pending = new Map<number, (v: unknown) => void>();

function ensureChild(): ChildProcess {
  if (child) return child;
  const env = { ...process.env };
  // Point the addon at the bundled libmpv (+ deps) so no system mpv is needed. On Windows
  // libmpv-2.dll sits next to vendor/node.exe and is found via PATH; on Linux the deps
  // live in vendor/lib via LD_LIBRARY_PATH.
  const isWin = process.platform === "win32";
  const libDir = isWin ? vendorDir() : path.join(vendorDir(), "lib");
  if (vendorDir() && existsSync(libDir)) {
    const v = isWin ? "PATH" : "LD_LIBRARY_PATH";
    const sep = isWin ? ";" : ":";
    env[v] = libDir + sep + (env[v] ?? "");
  }
  console.log("[mpv] starting host under node:", nodePath());
  child = fork(hostScript(), [], {
    execPath: nodePath(),
    serialization: "advanced", // Buffers (frames) cross IPC as binary
    stdio: ["ignore", "pipe", "pipe", "ipc"],
    env,
  });
  child.stdout?.on("data", (d) => process.stdout.write(`[mpv-host] ${d}`));
  child.stderr?.on("data", (d) => process.stderr.write(`[mpv-host] ${d}`));
  child.on("message", (msg: { id: number; result: unknown }) => {
    const cb = pending.get(msg.id);
    if (cb) {
      pending.delete(msg.id);
      cb(msg.result);
    }
  });
  const dead = (why: string) => {
    if (child) console.error("[mpv] host", why);
    child = null;
    pending.forEach((cb) => cb(null));
    pending.clear();
  };
  child.on("exit", (code) => dead(`exited ${code}`));
  child.on("error", (e) => dead(`error: ${e.message}`));
  return child;
}

function call<T = unknown>(fn: string, args: unknown[]): Promise<T> {
  return new Promise((resolve) => {
    const id = nextId++;
    pending.set(id, resolve as (v: unknown) => void);
    try {
      // send can throw EPIPE if the host already died — fail soft instead of crashing.
      ensureChild().send({ id, fn, args }, (err) => {
        if (err) {
          pending.delete(id);
          resolve(null as T);
        }
      });
    } catch {
      pending.delete(id);
      resolve(null as T);
    }
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
