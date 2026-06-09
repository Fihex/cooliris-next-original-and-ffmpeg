// Self-contained ffmpeg layer for extended formats (mkv/avi/HEVC/AC-3/DTS) and
// embedded subtitles. Spawns the bundled ffmpeg/ffprobe (asarUnpack'd) as separate
// processes — nothing is baked into the renderer. Remove this file + its one hook in
// main.ts + the deps to drop the feature entirely.

import { app } from "electron";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

const EXE = process.platform === "win32" ? ".exe" : "";

function nodeModules(): string {
  return app.isPackaged
    ? path.join(process.resourcesPath, "app.asar.unpacked", "node_modules")
    : path.join(app.getAppPath(), "node_modules");
}

let _ffmpeg: string | null = null;
let _ffprobe: string | null = null;
function ffmpegBin(): string {
  if (_ffmpeg) return _ffmpeg;
  const nm = nodeModules();
  // ffmpeg-static is the newest build (7.x) and is correct on a native build — the EXE
  // suffix self-disambiguates, so a Linux-built Windows package (no ffmpeg.exe here)
  // falls through to the per-platform @ffmpeg-installer binary. Then PATH.
  const candidates = [
    path.join(nm, "ffmpeg-static", "ffmpeg" + EXE),
    path.join(nm, "@ffmpeg-installer", `${process.platform}-${process.arch}`, "ffmpeg" + EXE),
  ];
  return (_ffmpeg = candidates.find((c) => existsSync(c)) ?? "ffmpeg" + EXE);
}
function ffprobeBin(): string {
  if (_ffprobe) return _ffprobe;
  const bundled = path.join(
    nodeModules(), "ffprobe-static", "bin", process.platform, process.arch, "ffprobe" + EXE
  );
  return (_ffprobe = existsSync(bundled) ? bundled : "ffprobe" + EXE);
}

/* --------------------------------- probing ---------------------------------- */

// Codecs Chromium can play directly → no transcode needed.
const VIDEO_OK = new Set(["h264", "avc1", "vp8", "vp9", "av1"]);
const AUDIO_OK = new Set(["aac", "mp3", "opus", "vorbis", "flac"]);

export interface SubStream {
  index: number;
  codec: string;
  lang?: string;
  title?: string;
  text: boolean; // convertible to WebVTT (text-based, not bitmap)
}
export interface AudioStream {
  index: number;
  codec: string;
  lang?: string;
  title?: string;
}
export interface ProbeInfo {
  durationSec: number;
  video?: { codec: string; width: number; height: number };
  audio?: { codec: string };
  audios: AudioStream[];
  subtitles: SubStream[];
  mode: "native" | "remux" | "transcode";
}

const TEXT_SUBS = new Set(["subrip", "srt", "ass", "ssa", "webvtt", "mov_text", "text"]);

export async function probe(abs: string, container: string): Promise<ProbeInfo | null> {
  let data: any;
  try {
    data = JSON.parse(
      await run(ffprobeBin(), [
        "-v", "error", "-print_format", "json", "-show_format", "-show_streams", abs,
      ])
    );
  } catch {
    return null;
  }
  const streams: any[] = data.streams ?? [];
  const v = streams.find((s) => s.codec_type === "video" && s.disposition?.attached_pic !== 1);
  const a = streams.find((s) => s.codec_type === "audio");
  const audios: AudioStream[] = streams
    .filter((s) => s.codec_type === "audio")
    .map((s) => ({
      index: s.index as number,
      codec: s.codec_name as string,
      lang: s.tags?.language,
      title: s.tags?.title,
    }));
  const subtitles: SubStream[] = streams
    .filter((s) => s.codec_type === "subtitle")
    .map((s) => ({
      index: s.index as number,
      codec: s.codec_name as string,
      lang: s.tags?.language,
      title: s.tags?.title,
      text: TEXT_SUBS.has(s.codec_name),
    }));

  // mp4/webm with friendly codecs play natively; otherwise remux (codecs ok, bad
  // container) or transcode (unfriendly video/audio codec).
  const vOk = !v || VIDEO_OK.has(v.codec_name);
  const aOk = !a || AUDIO_OK.has(a.codec_name);
  const nativeContainer = container === "mp4" || container === "webm" || container === "m4v";
  let mode: ProbeInfo["mode"];
  if (vOk && aOk) mode = nativeContainer ? "native" : "remux";
  else mode = "transcode";

  return {
    durationSec: parseFloat(data.format?.duration ?? "0") || 0,
    video: v ? { codec: v.codec_name, width: v.width, height: v.height } : undefined,
    audio: a ? { codec: a.codec_name } : undefined,
    audios,
    subtitles,
    mode,
  };
}

/* --------------------------- streaming & encoders ---------------------------- */

export interface PrepareOpts {
  videoCopy: boolean; // true = -c:v copy (codec already mp4-friendly), false = encode
  audioCopy: boolean; // true = -c:a copy, false = transcode to aac
  encoder?: string | null; // hardware H.264 encoder (GPU); null/undefined = software
  audioIndex?: number; // pick a specific audio stream (multi-track mkv); -1 = default
  durationSec?: number; // total duration, for computing prepare progress
}

/** Decide per-stream whether we can copy or must re-encode, for the SELECTED audio
 *  track (switching to an AC-3/DTS track means that track must be transcoded even if
 *  the default one was fine). */
export function planCodecs(
  info: ProbeInfo | null,
  audioIndex: number
): { videoCopy: boolean; audioCopy: boolean } {
  if (!info) return { videoCopy: false, audioCopy: false };
  const sel = audioIndex >= 0 ? info.audios.find((a) => a.index === audioIndex) : info.audios[0];
  return {
    videoCopy: info.video ? VIDEO_OK.has(info.video.codec) : true,
    audioCopy: sel ? AUDIO_OK.has(sel.codec) : true,
  };
}

interface EncCfg {
  pre: string[]; // args before -i (decode hwaccel / device init)
  vf?: string; // video filter (e.g. VAAPI upload)
  codec: string[]; // -c:v … encode args
}

// Default VAAPI render node (Linux). Overridable for unusual setups.
const VAAPI_DEVICE = process.env.COOLIRIS_VAAPI_DEVICE || "/dev/dri/renderD128";

// Realtime-friendly software baseline — keeps up with playback on any machine.
const SOFTWARE: EncCfg = {
  pre: [],
  codec: ["-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-crf", "23", "-pix_fmt", "yuv420p"],
};

function encCfg(encoder: string | null): EncCfg {
  switch (encoder) {
    case "h264_nvenc": // NVIDIA
      return { pre: ["-hwaccel", "auto"], codec: ["-c:v", "h264_nvenc", "-preset", "p1", "-tune", "ll", "-rc", "vbr", "-cq", "23", "-pix_fmt", "yuv420p"] };
    case "h264_qsv": // Intel QuickSync
      return { pre: [], vf: "format=nv12", codec: ["-c:v", "h264_qsv", "-preset", "veryfast", "-global_quality", "23"] };
    case "h264_vaapi": // AMD/Intel on Linux
      return { pre: ["-vaapi_device", VAAPI_DEVICE], vf: "format=nv12,hwupload", codec: ["-c:v", "h264_vaapi", "-qp", "23"] };
    case "h264_amf": // AMD on Windows
      return { pre: ["-hwaccel", "auto"], codec: ["-c:v", "h264_amf", "-quality", "speed", "-rc", "cqp", "-qp_i", "23", "-qp_p", "23", "-pix_fmt", "yuv420p"] };
    case "h264_videotoolbox": // macOS
      return { pre: ["-hwaccel", "auto"], codec: ["-c:v", "h264_videotoolbox", "-b:v", "6M", "-pix_fmt", "yuv420p"] };
    default:
      return SOFTWARE;
  }
}

// Candidate hardware encoders per platform, in preference order.
function hwCandidates(): string[] {
  if (process.platform === "win32") return ["h264_nvenc", "h264_qsv", "h264_amf"];
  if (process.platform === "darwin") return ["h264_videotoolbox"];
  return ["h264_nvenc", "h264_qsv", "h264_vaapi"]; // linux
}

// Does this encoder actually work here? Encode 0.2s of test video to null.
function canEncode(encoder: string): Promise<boolean> {
  const cfg = encCfg(encoder);
  const args = [
    "-hide_banner", "-loglevel", "error", ...cfg.pre,
    "-f", "lavfi", "-i", "color=c=black:s=320x240:r=15:d=0.2",
    ...(cfg.vf ? ["-vf", cfg.vf] : []), ...cfg.codec, "-f", "null", "-",
  ];
  return run(ffmpegBin(), args).then(() => true).catch(() => false);
}

let _hwEnc: string | null | undefined;
/** First working GPU H.264 encoder (NVENC/QSV/VAAPI/AMF/VideoToolbox), or null for
 *  software. Probed once and cached — the result is hardware, it won't change. */
export async function detectHwEncoder(): Promise<string | null> {
  if (_hwEnc !== undefined) return _hwEnc;
  for (const enc of hwCandidates()) {
    if (await canEncode(enc)) {
      console.log("[ffmpeg] hardware encoder:", enc);
      return (_hwEnc = enc);
    }
  }
  console.log("[ffmpeg] no usable hardware encoder; using libx264 (CPU)");
  return (_hwEnc = null);
}

// Temp dir for prepared files; cleaned up on quit. Each prepared file is cached by
// (source, audio track, encoder) so re-opening / re-selecting is instant.
const TEMP_DIR = path.join(os.tmpdir(), "cooliris-ffmpeg");
const prepared = new Map<string, string>(); // cache key → temp .mp4 path

function keyFor(abs: string, opts: PrepareOpts): string {
  return createHash("sha1")
    .update(`${abs}|a=${opts.audioIndex ?? -1}|e=${opts.encoder ?? "sw"}`)
    .digest("hex");
}

/**
 * Remux/transcode `abs` into a normal, seekable temp .mp4 (moov at the front via
 * +faststart) and return its path. Playing a complete file — instead of a live
 * fragmented stream — gives the browser a real duration, native seeking, and correct
 * A/V sync, and lets sidecar/embedded subtitles use their true (absolute) timestamps.
 *
 * Remux (-c copy) is near-instant; a full transcode takes time proportional to the
 * clip. Results are cached, so it only happens once per (file, audio track).
 */
export async function prepareFile(
  abs: string,
  opts: PrepareOpts,
  onProgress?: (fraction: number) => void
): Promise<string> {
  const key = keyFor(abs, opts);
  const cached = prepared.get(key);
  if (cached && existsSync(cached)) return cached;

  await fs.mkdir(TEMP_DIR, { recursive: true });
  const out = path.join(TEMP_DIR, `${key}.mp4`);
  const enc = opts.videoCopy ? null : encCfg(opts.encoder ?? null);
  const args: string[] = ["-hide_banner", "-loglevel", "error", "-y"];
  if (enc) args.push(...enc.pre); // decode hwaccel / device init (before -i)
  args.push("-i", abs);
  // video + one audio track (the chosen one, or ffmpeg's default first audio).
  args.push("-map", "0:v:0?");
  args.push("-map", opts.audioIndex != null && opts.audioIndex >= 0 ? `0:${opts.audioIndex}` : "0:a:0?");
  if (enc) {
    if (enc.vf) args.push("-vf", enc.vf);
    args.push(...enc.codec);
  } else {
    args.push("-c:v", "copy");
  }
  args.push(...(opts.audioCopy ? ["-c:a", "copy"] : ["-c:a", "aac", "-b:a", "192k", "-ac", "2"]));
  // A complete file: faststart puts the moov atom up front so the browser can seek
  // immediately. No fragmentation / timestamp hacks needed — ffmpeg writes correct PTS.
  // -progress pipe:1 streams machine-readable progress to stdout (output is a file).
  args.push("-movflags", "+faststart", "-progress", "pipe:1", "-nostats", out);

  await runWithProgress(ffmpegBin(), args, opts.durationSec ?? 0, onProgress);
  prepared.set(key, out);
  return out;
}

/** Run ffmpeg, parsing its -progress stream (out_time=HH:MM:SS.us on stdout) into a
 *  0..1 fraction against the known total duration. */
function runWithProgress(
  bin: string,
  args: string[],
  durationSec: number,
  onProgress?: (fraction: number) => void
): Promise<void> {
  return new Promise((resolve, reject) => {
    const p = spawn(bin, args);
    let err = "";
    p.stderr.on("data", (d) => (err = (err + d).slice(-4000)));
    p.stdout.on("data", (d: Buffer) => {
      if (!onProgress || durationSec <= 0) return;
      const matches = d.toString().match(/out_time=(\d+):(\d+):(\d+(?:\.\d+)?)/g);
      const last = matches?.[matches.length - 1];
      const m = last && /(\d+):(\d+):(\d+(?:\.\d+)?)/.exec(last);
      if (m) {
        const sec = +m[1] * 3600 + +m[2] * 60 + parseFloat(m[3]);
        onProgress(Math.max(0, Math.min(1, sec / durationSec)));
      }
    });
    p.on("error", reject);
    p.on("close", (code) => (code === 0 ? resolve() : reject(new Error(err || `exit ${code}`))));
  });
}

/** Remove every prepared temp file (call on app quit). */
export async function cleanupPrepared(): Promise<void> {
  prepared.clear();
  try {
    await fs.rm(TEMP_DIR, { recursive: true, force: true });
  } catch {
    /* ignore */
  }
}

/* --------------------------- posters & subtitles ----------------------------- */

/** A single downscaled frame (~3s in) as a JPEG buffer — wall thumbnail for files
 *  Chromium can't decode. */
export async function makePoster(abs: string): Promise<Buffer | null> {
  const frame = (seekArgs: string[]) =>
    runBuffer(ffmpegBin(), [
      "-hide_banner", "-loglevel", "error", ...seekArgs, "-i", abs,
      "-frames:v", "1", "-vf", "scale=512:-1", "-f", "image2", "-c:v", "mjpeg", "pipe:1",
    ]);
  try {
    return await frame(["-ss", "3"]);
  } catch {
    try {
      return await frame([]); // very short clip → first frame
    } catch {
      return null;
    }
  }
}

/** Extract a text subtitle stream to WebVTT (bitmap subs fail → null). */
export async function extractSubtitleVtt(abs: string, streamIndex: number): Promise<string | null> {
  try {
    return await run(ffmpegBin(), [
      "-hide_banner", "-loglevel", "error", "-i", abs, "-map", `0:${streamIndex}`, "-f", "webvtt", "pipe:1",
    ]);
  } catch {
    return null;
  }
}

/* --------------------------------- helpers ---------------------------------- */

function run(bin: string, args: string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    const p = spawn(bin, args);
    let out = "";
    let err = "";
    p.stdout.on("data", (d) => (out += d));
    p.stderr.on("data", (d) => (err += d));
    p.on("error", reject);
    p.on("close", (code) => (code === 0 ? resolve(out) : reject(new Error(err || `exit ${code}`))));
  });
}

function runBuffer(bin: string, args: string[]): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const p = spawn(bin, args);
    const chunks: Buffer[] = [];
    let err = "";
    p.stdout.on("data", (d: Buffer) => chunks.push(d));
    p.stderr.on("data", (d) => (err += d));
    p.on("error", reject);
    p.on("close", (code) =>
      code === 0 && chunks.length ? resolve(Buffer.concat(chunks)) : reject(new Error(err || `exit ${code}`))
    );
  });
}
