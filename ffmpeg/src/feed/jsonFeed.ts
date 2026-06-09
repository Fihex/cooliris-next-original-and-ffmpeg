import { platform } from "@/platform";
import type { Feed, JsonFeed, MediaItem } from "./types";

let autoId = 0;
const nextId = () => `item-${Date.now().toString(36)}-${autoId++}`;

/** Parse a date from a number (epoch ms) or a date string into epoch ms. */
function parseDate(raw: unknown): number | undefined {
  if (typeof raw === "number") return raw;
  if (typeof raw === "string") {
    const t = Date.parse(raw);
    if (!Number.isNaN(t)) return t;
  }
  return undefined;
}

/** Normalize a loosely-typed JSON record into a MediaItem. */
function normalizeItem(raw: unknown): MediaItem | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;

  // Accept a few common aliases so hand-written manifests are forgiving.
  const full =
    (r.full as string) ??
    (r.url as string) ??
    (r.src as string) ??
    (r.image as string) ??
    (r.content as string);
  const thumb =
    (r.thumb as string) ??
    (r.thumbnail as string) ??
    (r.preview as string) ??
    full;
  if (!full && !thumb) return null;

  const rawType = (r.type as string)?.toLowerCase();
  const looksVideo =
    rawType === "video" || /\.(mp4|webm|ogv|mov|m4v|mkv|avi)(\?|$)/i.test(full ?? "");
  const looksAudio =
    !looksVideo &&
    (rawType === "audio" || /\.(mp3|wav|ogg|oga|flac|m4a|aac|opus|weba)(\?|$)/i.test(full ?? ""));
  const type = looksVideo ? "video" : looksAudio ? "audio" : "image";

  return {
    id: (r.id as string) ?? (r.guid as string) ?? nextId(),
    type,
    animated: type === "image" && /\.gif(\?|$)/i.test(full ?? "") ? true : undefined,
    thumb: thumb ?? full,
    full: full ?? thumb,
    title: (r.title as string) ?? (r.name as string),
    path: (r.path as string) ?? undefined,
    date: parseDate(r.date ?? r.taken ?? r.timestamp),
    link: (r.link as string) ?? (r.href as string),
    aspect: typeof r.aspect === "number" ? (r.aspect as number) : undefined,
  };
}

/** Parse a parsed-JSON value (array or `{items}`) into a Feed. */
export function parseJsonFeed(data: JsonFeed): Feed {
  const arr = Array.isArray(data) ? data : data.items;
  const title = Array.isArray(data) ? undefined : data.title;
  const items = (arr ?? [])
    .map(normalizeItem)
    .filter((i): i is MediaItem => i !== null);
  return { title, items };
}

async function fetchJson(url: string): Promise<JsonFeed> {
  const res = await fetch(url, { headers: { Accept: "application/json" } });
  if (!res.ok) throw new Error(`Failed to load feed (${res.status} ${res.statusText})`);
  return (await res.json()) as JsonFeed;
}

/**
 * Load a JSON manifest from a URL. Tries a direct fetch first; if that fails
 * (typically a cross-site CORS block), it retries through a public CORS proxy so
 * feeds hosted on other websites still work.
 */
export async function loadJsonFeedFromUrl(url: string): Promise<Feed> {
  let data: JsonFeed;

  // Desktop (Electron): fetch through the main process — no CORS, no third party.
  if (platform.fetchRemoteText && /^https?:\/\//i.test(url)) {
    const text = await platform.fetchRemoteText(url);
    data = JSON.parse(text) as JsonFeed;
    const feed = parseJsonFeed(data);
    if (feed.items.length === 0) throw new Error("Feed contained no media items.");
    return feed;
  }

  try {
    data = await fetchJson(url);
  } catch (direct) {
    if (/^https?:\/\//i.test(url)) {
      // Browser only: a public CORS proxy as a last resort for cross-site feeds.
      try {
        data = await fetchJson(`https://corsproxy.io/?url=${encodeURIComponent(url)}`);
      } catch {
        throw direct instanceof Error ? direct : new Error(String(direct));
      }
    } else {
      throw direct instanceof Error ? direct : new Error(String(direct));
    }
  }
  const feed = parseJsonFeed(data);
  if (feed.items.length === 0) throw new Error("Feed contained no media items.");
  return feed;
}

/** Parse a JSON manifest from a pasted string. */
export function loadJsonFeedFromText(text: string): Feed {
  let data: JsonFeed;
  try {
    data = JSON.parse(text) as JsonFeed;
  } catch {
    throw new Error("That isn't valid JSON.");
  }
  const feed = parseJsonFeed(data);
  if (feed.items.length === 0) throw new Error("Feed contained no media items.");
  return feed;
}

/** Read and parse a local .json file (no network needed). */
export async function loadJsonFeedFromFile(file: File): Promise<Feed> {
  return loadJsonFeedFromText(await file.text());
}
