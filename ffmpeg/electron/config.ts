// App config, read at launch. Stored as a plain JSON file *next to the app* so it's
// easy to find and edit before launching:
//   - AppImage: alongside the .AppImage file
//   - Windows/unpacked: next to the executable
//   - dev: the project root
// If that location isn't writable (e.g. a read-only install dir), it falls back to the
// per-user data dir. The ffmpeg layer (mkv/avi/HEVC/AC-3 + embedded subs) is gated
// entirely on this; `hwAccel` (CPU↔GPU) is also adjustable from Settings and persisted.

import { app } from "electron";
import { promises as fs } from "node:fs";
import path from "node:path";

export interface AppConfig {
  ffmpeg: {
    enabled: boolean; // master switch for the extended-format layer
    hwAccel: boolean; // GPU (hardware) transcoding vs CPU (software)
  };
}

const FILE = "cooliris.config.json";
const DEFAULTS: AppConfig = {
  ffmpeg: { enabled: true, hwAccel: false },
};

let cached: AppConfig = DEFAULTS;
let activePath: string | null = null; // the file we loaded from / write back to

/** The directory that holds the app's own files (portable config lives here). */
function portableDir(): string {
  if (!app.isPackaged) return app.getAppPath(); // dev → project root
  // An AppImage's execPath points inside the read-only mount; APPIMAGE is the real file.
  if (process.env.APPIMAGE) return path.dirname(process.env.APPIMAGE);
  return path.dirname(process.execPath); // win / mac / unpacked
}

// Candidate locations, most-preferred first: next to the app, then the per-user dir.
function candidatePaths(): string[] {
  return [path.join(portableDir(), FILE), path.join(app.getPath("userData"), FILE)];
}

function merge(raw: any): AppConfig {
  return { ffmpeg: { ...DEFAULTS.ffmpeg, ...(raw?.ffmpeg ?? {}) } };
}

/** Load the config, or create it with defaults on first run. Call once after whenReady. */
export async function loadConfig(): Promise<AppConfig> {
  const candidates = candidatePaths();
  // 1) Use the first existing config (prefer the portable one next to the app).
  for (const p of candidates) {
    try {
      cached = merge(JSON.parse(await fs.readFile(p, "utf8")));
      activePath = p;
      return cached;
    } catch {
      /* not here — try next */
    }
  }
  // 2) None exists → write defaults to the first writable location.
  cached = merge(null);
  for (const p of candidates) {
    try {
      await fs.mkdir(path.dirname(p), { recursive: true });
      await fs.writeFile(p, JSON.stringify(cached, null, 2));
      activePath = p;
      return cached;
    } catch {
      /* read-only — try the next location */
    }
  }
  activePath = candidates[candidates.length - 1]; // give up writing; keep defaults in memory
  return cached;
}

export function getConfig(): AppConfig {
  return cached;
}

export function configFilePath(): string {
  return activePath ?? candidatePaths()[0];
}

export async function saveConfig(): Promise<void> {
  const target = activePath ?? candidatePaths()[0];
  try {
    await fs.mkdir(path.dirname(target), { recursive: true });
    await fs.writeFile(target, JSON.stringify(cached, null, 2));
    activePath = target;
  } catch {
    // Target became unwritable — fall back to the per-user dir.
    const fallback = path.join(app.getPath("userData"), FILE);
    try {
      await fs.mkdir(path.dirname(fallback), { recursive: true });
      await fs.writeFile(fallback, JSON.stringify(cached, null, 2));
      activePath = fallback;
    } catch {
      /* best effort */
    }
  }
}

/** Runtime update from Settings (persisted). */
export async function updateFfmpeg(patch: Partial<AppConfig["ffmpeg"]>): Promise<AppConfig> {
  cached = { ffmpeg: { ...cached.ffmpeg, ...patch } };
  await saveConfig();
  return cached;
}
