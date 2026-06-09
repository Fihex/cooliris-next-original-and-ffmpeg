// Main-process libmpv bridge (Option C). Loads the native addon, owns one mpv player,
// and exposes load/control + frame pulling. Frames are RGBA buffers (video + subtitles
// composited by mpv) handed to the renderer over IPC for upload into a <canvas>.
import { app } from "electron";
import { createRequire } from "node:module";
import { existsSync } from "node:fs";
import path from "node:path";

const require = createRequire(import.meta.url);

function addonPath(): string {
  const rel = path.join("native", "build", "Release", "mpv.node");
  const dev = path.join(app.getAppPath(), rel);
  const packed = path.join(process.resourcesPath, "app.asar.unpacked", rel);
  return existsSync(dev) ? dev : packed;
}

/* eslint-disable @typescript-eslint/no-explicit-any */
let addon: any;
let player: any;

export function mpvAvailable(): boolean {
  try {
    if (!addon) addon = require(addonPath());
    return !!addon?.MpvPlayer;
  } catch (e) {
    console.error("[mpv] addon load failed:", (e as Error).message);
    return false;
  }
}

function ensure(): any {
  if (!addon) addon = require(addonPath());
  if (!player) player = new addon.MpvPlayer();
  return player;
}

export function mpvLoad(abs: string): void {
  ensure().command(["loadfile", abs]);
}
export function mpvCommand(args: string[]): boolean {
  return ensure().command(args);
}
export function mpvSet(name: string, value: string): boolean {
  return ensure().setProperty(name, String(value));
}
export function mpvGet(name: string): string | null {
  return ensure().getProperty(name);
}
export function mpvVideoSize(): { w: number; h: number } {
  return ensure().videoSize();
}
export function mpvFrame(w: number, h: number): Buffer | null {
  return ensure().renderFrame(w, h);
}
export function mpvStop(): void {
  if (player) player.command(["stop"]);
}
export function mpvDestroy(): void {
  if (player) {
    try {
      player.destroy();
    } catch {
      /* ignore */
    }
    player = null;
  }
}
