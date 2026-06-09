/** A single media item shown on the wall. */
export interface MediaItem {
  /** Stable unique id (guid). */
  id: string;
  /** "image", "video", or "audio". */
  type: "image" | "video" | "audio";
  /** True for animated images (GIF) — played as an <img> when opened. */
  animated?: boolean;
  /** Thumbnail URL used for the wall tile. */
  thumb: string;
  /** Full-resolution image or video URL used in the detail view. */
  full: string;
  /** Display title. */
  title?: string;
  /** File path / location (relative path within an opened folder, or a URL). */
  path?: string;
  /** Timestamp (epoch ms) — file modified time, or a feed-provided date. */
  date?: number;
  /** Creation timestamp (epoch ms). Electron only (Node birthtime); not in browsers. */
  created?: number;
  /** Optional click-through link. */
  link?: string;
  /** Native aspect ratio (width / height), if known. Defaults to 1.5. */
  aspect?: number;
  /** Sidecar subtitle tracks (.vtt / .srt found next to a video file). */
  subs?: VideoSub[];
}

/** A sidecar subtitle track for a video. */
export interface VideoSub {
  /** URL to fetch the subtitle text (coolmedia:// for local files). */
  url: string;
  /** Label shown in the captions chooser (e.g. "en", "Subtitles"). */
  label: string;
  /** True if the source is SubRip (.srt) and needs WebVTT conversion. */
  srt: boolean;
}

/** The shape of a JSON manifest file/URL. Either a bare array or `{ items: [...] }`. */
export type JsonFeed = MediaItem[] | { title?: string; items: MediaItem[] };

export interface Feed {
  title?: string;
  items: MediaItem[];
}

/** Progress reporter passed to feed loaders: (done, total). */
export type ProgressFn = (done: number, total: number) => void;
