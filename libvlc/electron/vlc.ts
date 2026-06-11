// Main-process proxy to the vlc host. libVLC runs in a forked SYSTEM Node process —
// not the Electron binary — so it and its plugin set load cleanly with no Chromium
// library conflicts. We forward load/control + frame pulls over Node IPC and resolve
// replies by id.
import { app } from "electron";
import { fork, execSync, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";

// These files (host script + addon) are asarUnpacked and must be read by the EXTERNAL
// node process, which can't see inside the asar. So when packaged, point at the real
// on-disk app.asar.unpacked path — NOT app.getAppPath() (that's the virtual asar path,
// which Electron's patched fs reports as existing but plain node can't load).
function pick(rel: string): string {
  return app.isPackaged
    ? path.join(process.resourcesPath, "app.asar.unpacked", rel)
    : path.join(app.getAppPath(), rel);
}
const hostScript = () => pick(path.join("electron", "vlcHost.cjs"));
const addonFile = () => pick(path.join("native", "build", "Release", "vlc.node"));

// Self-contained runtime bundled by scripts/bundle-*.sh (vendor/node + vendor/lib),
// shipped as extraResources. Only used in the packaged app — in dev we use the system
// Node + system vlc (avoids the bundle interfering with development).
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

// Point libVLC at the bundled plugin dir, which ships a pre-built plugins.dat (the
// afterPack hook runs vlc-cache-gen against these exact files, and NSIS preserves their
// timestamps on install, so the cache validates → no ~30s rescan on first open). The
// per-user install dir is writable, so libVLC can also refresh the cache there if ever
// needed. (Earlier we copied the tree into userData first, but that ~150MB copy froze
// the first launch for no benefit — the bundled cache is already valid.)
function pluginDir(): string | undefined {
  const isWin = process.platform === "win32";
  const bundled = path.join(vendorDir(), isWin ? "plugins" : "vlc-plugins");
  if (!vendorDir() || !existsSync(bundled)) return undefined;
  return bundled;
}

let child: ChildProcess | null = null;
let nextId = 1;
const pending = new Map<number, (v: unknown) => void>();

function ensureChild(): ChildProcess {
  if (child) return child;
  const env = { ...process.env };
  // Point the addon at the bundled libVLC so no system VLC is needed. libVLC is the
  // library (libvlc + libvlccore) PLUS a plugins directory it loads at runtime — the
  // plugins are what actually demux/decode, so VLC_PLUGIN_PATH must point at them.
  // Windows: DLLs sit next to vendor/node.exe (found via PATH), plugins in vendor/plugins.
  // Linux: libs in vendor/lib (LD_LIBRARY_PATH), plugins in vendor/vlc-plugins.
  const isWin = process.platform === "win32";
  const libDir = isWin ? vendorDir() : path.join(vendorDir(), "lib");
  if (vendorDir() && existsSync(libDir)) {
    const v = isWin ? "PATH" : "LD_LIBRARY_PATH";
    const sep = isWin ? ";" : ":";
    env[v] = libDir + sep + (env[v] ?? "");
    const plugins = pluginDir();
    if (plugins) env.VLC_PLUGIN_PATH = plugins;
  }
  console.log("[vlc] starting host under node:", nodePath());
  child = fork(hostScript(), [], {
    execPath: nodePath(),
    serialization: "advanced", // Buffers (frames) cross IPC as binary
    stdio: ["ignore", "pipe", "pipe", "ipc"],
    env,
  });
  child.stdout?.on("data", (d) => process.stdout.write(`[vlc-host] ${d}`));
  child.stderr?.on("data", (d) => process.stderr.write(`[vlc-host] ${d}`));
  child.on("message", (msg: { id: number; result: unknown }) => {
    const cb = pending.get(msg.id);
    if (cb) {
      pending.delete(msg.id);
      cb(msg.result);
    }
  });
  const dead = (why: string) => {
    if (child) console.error("[vlc] host", why);
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

export function vlcAvailable(): boolean {
  return existsSync(addonFile()) && existsSync(hostScript());
}
// Pre-warm: fork the host process now so libVLC initialises and scans its plugin tree
// ahead of the first open. Without this, the first video pays that one-time cost and
// shows "Loading…" longer; afterwards opens reuse the running player. Best-effort —
// if it fails the lazy fork on first load still works.
export function vlcWarm(): void {
  if (!vlcAvailable()) return;
  try {
    ensureChild();
  } catch {
    /* ignore — first real call will spawn it */
  }
}
export const vlcLoad = (abs: string) => call("load", [abs]);
export const vlcCommand = (args: string[]) => call<boolean>("cmd", [args]);
export const vlcSet = (name: string, value: string) => call<boolean>("set", [name, value]);
export const vlcGet = (name: string) => call<string | null>("get", [name]);
export const vlcVideoSize = () => call<{ w: number; h: number }>("size", []);
export const vlcFrame = (w: number, h: number) => call<Uint8Array | null>("frame", [w, h]);
export const vlcStop = () => call("stop", []);
// Subtitle style on libVLC 3 is creation-time only: the host recreates the player with
// these VLC options and restores file/position/tracks (a brief reload).
export const vlcStyle = (args: string[]) => call<boolean>("style", [args]);
export function vlcDestroy(): void {
  if (child) {
    try {
      child.kill();
    } catch {
      /* already gone */
    }
    child = null;
  }
}
