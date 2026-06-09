// App config read at launch from <userData>/cooliris.config.json. The ffmpeg layer
// (extended formats: mkv/avi/HEVC/AC-3/DTS + embedded subs) is gated entirely on this,
// so when disabled there is zero ffmpeg cost. `hwAccel` (CPU↔GPU) is also adjustable
// at runtime from Settings and persisted back here.

import { app } from "electron";
import { promises as fs } from "node:fs";
import path from "node:path";

export interface AppConfig {
  ffmpeg: {
    enabled: boolean; // master switch for the extended-format layer
    hwAccel: boolean; // GPU (hardware) transcoding vs CPU (software)
  };
}

const DEFAULTS: AppConfig = {
  ffmpeg: { enabled: true, hwAccel: false },
};

let cached: AppConfig = DEFAULTS;

function configPath(): string {
  return path.join(app.getPath("userData"), "cooliris.config.json");
}

/** Load (and create-with-defaults on first run) the config. Call once after whenReady. */
export async function loadConfig(): Promise<AppConfig> {
  try {
    const raw = JSON.parse(await fs.readFile(configPath(), "utf8"));
    cached = { ffmpeg: { ...DEFAULTS.ffmpeg, ...(raw?.ffmpeg ?? {}) } };
  } catch {
    cached = { ffmpeg: { ...DEFAULTS.ffmpeg } };
    await saveConfig().catch(() => {});
  }
  return cached;
}

export function getConfig(): AppConfig {
  return cached;
}

export async function saveConfig(): Promise<void> {
  try {
    await fs.writeFile(configPath(), JSON.stringify(cached, null, 2));
  } catch {
    /* best effort */
  }
}

/** Runtime update from Settings (persisted). */
export async function updateFfmpeg(patch: Partial<AppConfig["ffmpeg"]>): Promise<AppConfig> {
  cached = { ffmpeg: { ...cached.ffmpeg, ...patch } };
  await saveConfig();
  return cached;
}
