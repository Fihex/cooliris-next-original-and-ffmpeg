import { useEffect, useMemo, useRef, useState } from "react";
import type { VideoSub } from "@/feed/types";

/**
 * Self-contained video viewer for the Lightbox. All video-specific behaviour
 * lives here so it's easy to change or remove:
 *   - the video *frame* is rendered inside a transform layer driven by the
 *     Lightbox's shared zoom/pan state (`t`), so it zooms/pans with everything else;
 *   - the control bar is rendered *outside* that transform, pinned to the bottom,
 *     so it never moves when the frame is zoomed or dragged.
 *
 * To drop these custom controls entirely, replace <VideoPlayer> in Lightbox with a
 * native `<video controls>` again — nothing else depends on this file.
 */

const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));

/** Recover the absolute file path from a cooltranscode:// URL. */
function decodeAbs(url: string): string {
  try {
    return decodeURIComponent(new URL(url).pathname.replace(/^\//, ""));
  } catch {
    return "";
  }
}

interface Transform {
  s: number;
  x: number;
  y: number;
}

interface VideoPlayerProps {
  src: string;
  itemId: string;
  /** Shared zoom/pan transform from the Lightbox. */
  t: Transform;
  /** Animate the transform (wheel) vs. track instantly (drag/pinch). */
  smooth: boolean;
  /** The Lightbox centers media (and computes zoom origin) against this stage. */
  stageRef: React.RefObject<HTMLDivElement>;
  /** Owned by the Lightbox so its keyboard shortcuts can seek/adjust volume. */
  videoRef: React.RefObject<HTMLVideoElement>;
  /** Sidecar subtitle tracks (.vtt / .srt) detected next to the video file. */
  subs?: VideoSub[];
  /** Fill the viewport edge-to-edge (fullscreen) vs. contained with a bottom band. */
  fullscreen: boolean;
  /** Fade out the control bar when the pointer is idle. */
  chromeHidden: boolean;
  onFullscreen: () => void;
  /** Close the lightbox (used when clicking the empty letterbox around the frame). */
  onRequestClose: () => void;
}

/** SubRip → WebVTT: add the header and use a dot (not comma) before milliseconds. */
function srtToVtt(srt: string): string {
  const body = srt.replace(/\r+/g, "").replace(/(\d{2}:\d{2}:\d{2}),(\d{3})/g, "$1.$2");
  return `WEBVTT\n\n${body}`;
}

interface Cue {
  start: number;
  end: number;
  text: string;
}

// Parse WebVTT into cues. We render subtitles ourselves (overlay div) rather than via
// <track>, because Chromium's out-of-band text tracks duplicate / linger when their src
// changes — doing it manually is fully deterministic.
function parseVtt(vtt: string): Cue[] {
  const cues: Cue[] = [];
  const ts = /(?:(\d{1,2}):)?(\d{1,2}):(\d{2}(?:\.\d{1,3})?)\s*-->\s*(?:(\d{1,2}):)?(\d{1,2}):(\d{2}(?:\.\d{1,3})?)/;
  for (const block of vtt.replace(/\r/g, "").split(/\n\n+/)) {
    const lines = block.split("\n");
    const i = lines.findIndex((l) => l.includes("-->"));
    if (i < 0) continue;
    const m = ts.exec(lines[i]);
    if (!m) continue;
    const start = (+m[1] || 0) * 3600 + +m[2] * 60 + parseFloat(m[3]);
    const end = (+m[4] || 0) * 3600 + +m[5] * 60 + parseFloat(m[6]);
    const text = lines.slice(i + 1).join("\n").replace(/<[^>]+>/g, "").trim();
    if (text && end > start) cues.push({ start, end, text });
  }
  return cues;
}

/** Big glyph for the transient play/pause/skip overlay. */
function FlashIcon({ kind }: { kind: string }) {
  const p = { width: 34, height: 34, viewBox: "0 0 24 24", fill: "currentColor" } as const;
  if (kind === "pause")
    return (
      <svg {...p}>
        <rect x="6" y="5" width="4" height="14" rx="1" />
        <rect x="14" y="5" width="4" height="14" rx="1" />
      </svg>
    );
  if (kind === "back")
    return (
      <svg {...p}>
        <path d="M11 6L5 12l6 6V6z" />
        <path d="M19 6l-6 6 6 6V6z" />
      </svg>
    );
  if (kind === "forward")
    return (
      <svg {...p}>
        <path d="M13 6l6 6-6 6V6z" />
        <path d="M5 6l6 6-6 6V6z" />
      </svg>
    );
  return (
    <svg {...p}>
      <path d="M8 5v14l11-7z" />
    </svg>
  );
}

export function VideoPlayer({
  src,
  itemId,
  t,
  smooth,
  stageRef,
  videoRef,
  subs,
  fullscreen,
  chromeHidden,
  onFullscreen,
  onRequestClose,
}: VideoPlayerProps) {
  // Fetch each sidecar subtitle and keep the raw WebVTT text (converted from .srt).
  // Blob URLs are built later, after any time-shift, in the shared track effect below.
  const [sidecarRaw, setSidecarRaw] = useState<{ vtt: string; label: string }[]>([]);
  useEffect(() => {
    let cancelled = false;
    (async () => {
      const out: { vtt: string; label: string }[] = [];
      for (const s of subs ?? []) {
        try {
          const text = await (await fetch(s.url)).text();
          out.push({ vtt: s.srt ? srtToVtt(text) : text, label: s.label });
        } catch {
          /* skip unreadable subtitle */
        }
      }
      if (!cancelled) setSidecarRaw(out);
    })();
    return () => {
      cancelled = true;
      setSidecarRaw([]);
    };
  }, [subs, itemId]);

  // Brief center overlay on play / pause / skip — covers the button, the click, and
  // the keyboard shortcuts (which dispatch a "uiskip" event on the video element).
  const [flash, setFlash] = useState<{ kind: string; id: number } | null>(null);
  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    let n = 0;
    let first = true; // skip the flash for the initial autoplay
    const fire = (kind: string) => setFlash({ kind, id: ++n });
    const onPlay = () => {
      if (first) {
        first = false;
        return;
      }
      fire("play");
    };
    const onPause = () => fire("pause");
    const onSkip = (e: Event) => fire((e as CustomEvent).detail === "back" ? "back" : "forward");
    v.addEventListener("play", onPlay);
    v.addEventListener("pause", onPause);
    v.addEventListener("uiskip", onSkip as EventListener);
    return () => {
      v.removeEventListener("play", onPlay);
      v.removeEventListener("pause", onPause);
      v.removeEventListener("uiskip", onSkip as EventListener);
    };
  }, [videoRef, itemId]);

  // Non-native videos (mkv/avi/HEVC/…) are remuxed/transcoded to a real, seekable temp
  // .mp4 on open (and again on audio switch). Playing a complete file gives the browser
  // native seeking, a real duration, and correct A/V sync; subtitles use their true
  // (absolute) timestamps — no streaming, no timeline offset, no cue shifting.
  const isTranscode = src.startsWith("cooltranscode:");
  const transcodeAbs = isTranscode ? decodeAbs(src) : "";
  const [playSrc, setPlaySrc] = useState(isTranscode ? "" : src);
  const [preparing, setPreparing] = useState(isTranscode);
  const [prepPct, setPrepPct] = useState(0);
  const [prepMode, setPrepMode] = useState("");
  const [prepError, setPrepError] = useState(false);
  const [audioIndex, setAudioIndex] = useState(-1); // -1 = default track
  const [audioTracks, setAudioTracks] = useState<{ index: number; label: string }[]>([]);
  const [preparedSubs, setPreparedSubs] = useState<{ vtt: string; label: string }[]>([]);
  const [activeSub, setActiveSub] = useState(-1); // index into subList; -1 = off
  const resumeAtRef = useRef(0); // play position to restore after an audio-switch re-prepare
  const wasPlayingRef = useRef(true); // whether to resume after a prepare / switch

  // Reset the source SYNCHRONOUSLY (during render) the instant the item changes — so the
  // new <video> never mounts with the previous item's prepared URL and plays the old
  // video for a beat. (Doing this in an effect runs too late: the element commits first.)
  const [shownItem, setShownItem] = useState(itemId);
  if (itemId !== shownItem) {
    setShownItem(itemId);
    setPlaySrc(isTranscode ? "" : src);
    setPreparing(isTranscode);
    setPrepPct(0);
    setPrepError(false);
    setActiveSub(-1);
    setAudioIndex(-1);
    resumeAtRef.current = 0;
    wasPlayingRef.current = true;
  }

  // Prepare (or re-prepare on audio switch) the temp file.
  useEffect(() => {
    if (!isTranscode) {
      setPlaySrc(src);
      return;
    }
    let cancelled = false;
    setPlaySrc(""); // drop the previous file so the old video can't keep playing while preparing
    setPreparing(true);
    setPrepPct(0);
    setPrepMode("");
    setPrepError(false);
    const offProgress = window.electron?.onFfProgress?.((p) => {
      if (!cancelled) setPrepPct(p);
    });
    const offMode = window.electron?.onFfPrepareMode?.((m) => {
      if (!cancelled) setPrepMode(m);
    });
    window.electron
      ?.ffPrepare?.(transcodeAbs, audioIndex)
      .then((res) => {
        if (cancelled) return;
        if (!res) {
          setPrepError(true);
          setPreparing(false);
          return;
        }
        setAudioTracks(res.audios);
        setPreparedSubs(res.subs);
        setPlaySrc(res.videoUrl);
        setPreparing(false);
      })
      .catch(() => {
        if (!cancelled) {
          setPrepError(true);
          setPreparing(false);
        }
      });
    return () => {
      cancelled = true;
      offProgress?.();
      offMode?.();
      // Switching away / re-preparing: kill the in-flight transcode so it doesn't keep
      // running in the background. (ff-cancel is sent before the next ff-prepare.)
      window.electron?.ffCancel?.();
    };
  }, [src, itemId, isTranscode, transcodeAbs, audioIndex]);

  // Switch audio track: pause and remember the position + play state, then re-prepare
  // with the new track. Playback resumes (at the same spot) once the new file loads.
  const selectAudio = (idx: number) => {
    const v = videoRef.current;
    wasPlayingRef.current = v ? !v.paused : true;
    resumeAtRef.current = v?.currentTime ?? 0;
    v?.pause();
    setAudioIndex(idx);
  };

  // Switch subtitle track — instant (the overlay just reads from a different cue list).
  const selectSub = (i: number) => setActiveSub(i);

  // All available subtitles (sidecar + embedded), and the parsed cues for the active one.
  const subList = useMemo(() => [...sidecarRaw, ...preparedSubs], [sidecarRaw, preparedSubs]);
  const cues = useMemo(
    () => (activeSub >= 0 && subList[activeSub] ? parseVtt(subList[activeSub].vtt) : []),
    [activeSub, subList]
  );

  // Render the active cue ourselves, driven by currentTime. Deterministic — no <track>,
  // so subtitles can't duplicate, linger, or get stuck across seeks / src reloads.
  const [cueText, setCueText] = useState("");
  useEffect(() => {
    const v = videoRef.current;
    if (!v || !cues.length) {
      setCueText("");
      return;
    }
    const update = () => {
      const t = v.currentTime;
      const c = cues.find((q) => t >= q.start && t < q.end);
      setCueText(c ? c.text : "");
    };
    update();
    v.addEventListener("timeupdate", update);
    v.addEventListener("seeking", update);
    v.addEventListener("seeked", update);
    return () => {
      v.removeEventListener("timeupdate", update);
      v.removeEventListener("seeking", update);
      v.removeEventListener("seeked", update);
      setCueText("");
    };
  }, [videoRef, itemId, cues, playSrc]);

  // Keyboard ±10s (Lightbox dispatches "uiskiprel") — the prepared file seeks natively.
  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    const onRel = (e: Event) => {
      const delta = (e as CustomEvent).detail as number;
      v.currentTime = clamp(v.currentTime + delta, 0, v.duration || Infinity);
      v.dispatchEvent(new CustomEvent("uiskip", { detail: delta < 0 ? "back" : "forward" }));
    };
    v.addEventListener("uiskiprel", onRel as EventListener);
    return () => v.removeEventListener("uiskiprel", onRel as EventListener);
  }, [videoRef, itemId]);

  return (
    <>
      {/* Centered in the full viewport (like images); the control bar overlays the
          bottom. Fullscreen fills edge-to-edge, windowed stays contained — only the
          size differs. Follows the shared zoom/pan transform; origin = stage center. */}
      <div ref={stageRef} className="absolute inset-0">
        <div
          className={`pointer-events-none absolute inset-0 flex items-center justify-center will-change-transform ${
            fullscreen ? "" : "p-6"
          }`}
          style={{
            transform: `translate(${t.x}px, ${t.y}px) scale(${t.s})`,
            transformOrigin: "center center",
            transition: smooth ? "transform 140ms ease-out" : "none",
          }}
        >
          <video
            data-media
            ref={videoRef}
            key={itemId}
            src={playSrc || undefined}
            // coolmedia is a different origin than the app:// renderer; Chromium won't
            // render out-of-band <track> cues on cross-origin media without this (the
            // handler sends Access-Control-Allow-Origin: *).
            crossOrigin="anonymous"
            autoPlay
            draggable={false}
            onLoadedData={() => {
              // After a (re-)prepare, restore the previous position and resume only if it
              // was playing before the switch.
              const v = videoRef.current;
              if (!v) return;
              if (resumeAtRef.current > 0) {
                v.currentTime = resumeAtRef.current;
                resumeAtRef.current = 0;
              }
              if (wasPlayingRef.current) v.play().catch(() => {});
              else v.pause();
            }}
            onClick={(e) => {
              const v = videoRef.current;
              if (!v) return;
              // object-contain letterboxes the frame inside the full-size element.
              // A click on the empty bars (outside the actual frame) closes; a click
              // on the frame toggles play.
              const r = v.getBoundingClientRect();
              const vw = v.videoWidth;
              const vh = v.videoHeight;
              if (vw && vh) {
                const scale = Math.min(r.width / vw, r.height / vh);
                const dw = vw * scale;
                const dh = vh * scale;
                const x0 = r.left + (r.width - dw) / 2;
                const y0 = r.top + (r.height - dh) / 2;
                const inFrame =
                  e.clientX >= x0 && e.clientX <= x0 + dw && e.clientY >= y0 && e.clientY <= y0 + dh;
                if (!inFrame) {
                  onRequestClose();
                  return;
                }
              }
              v.paused ? v.play() : v.pause();
            }}
            className={`pointer-events-auto h-full w-full object-contain ${
              chromeHidden ? "cursor-none" : "cursor-pointer"
            }`}
          />
        </div>

        {/* Subtitles: rendered by us (not a <track>), pinned above the control bar so they
            never duplicate or stick. */}
        {cueText && (
          <div
            className="pointer-events-none absolute inset-x-0 z-20 flex justify-center px-6"
            style={{ bottom: chromeHidden ? "6%" : "5.5rem" }}
          >
            <span
              className="max-w-[90%] whitespace-pre-line rounded bg-black/60 px-2 py-0.5 text-center text-lg font-medium leading-snug text-white"
              style={{ textShadow: "0 2px 4px rgba(0,0,0,0.95)" }}
            >
              {cueText}
            </span>
          </div>
        )}

        {/* Preparing (remux/transcode to a temp file) / failure notices. */}
        {(preparing || prepError) && (
          <div className="pointer-events-none absolute inset-0 z-10 flex items-center justify-center">
            <div className="min-w-52 rounded-lg bg-black/75 px-4 py-3 text-sm text-white shadow-lg">
              {prepError ? (
                "Couldn't prepare this video."
              ) : (
                <>
                  <div className="mb-2 flex items-center justify-between gap-4">
                    <span>Preparing video…</span>
                    <span className="tabular-nums text-white/80">{prepPct}%</span>
                  </div>
                  <div className="h-1.5 w-full overflow-hidden rounded-full bg-white/20">
                    <div
                      className="h-full bg-white transition-[width] duration-200"
                      style={{ width: `${prepPct}%` }}
                    />
                  </div>
                  {prepMode && (
                    <div className="mt-2 text-xs text-white/60">{prepMode}</div>
                  )}
                </>
              )}
            </div>
          </div>
        )}

        {/* Transient play/pause/skip indicator, centered over the frame. */}
        {flash && (
          <div
            key={flash.id}
            className="ui-flash pointer-events-none absolute inset-0 z-10 flex items-center justify-center"
          >
            <div className="flex h-20 w-20 items-center justify-center rounded-full bg-black/55 text-white">
              <FlashIcon kind={flash.kind} />
            </div>
          </div>
        )}
      </div>

      <VideoControls
        videoRef={videoRef}
        itemId={itemId}
        onFullscreen={onFullscreen}
        fullscreen={fullscreen}
        hidden={chromeHidden}
        audioTracks={audioTracks}
        activeAudio={audioIndex}
        onSelectAudio={selectAudio}
        subTracks={subList.map((s, i) => ({ i, label: s.label }))}
        activeSub={activeSub}
        onSelectSub={selectSub}
      />
    </>
  );
}

/** mm:ss (or h:mm:ss for long videos). */
function fmtTime(s: number): string {
  if (!isFinite(s) || s < 0) s = 0;
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = Math.floor(s % 60);
  const mm = h ? String(m).padStart(2, "0") : String(m);
  return `${h ? `${h}:` : ""}${mm}:${String(sec).padStart(2, "0")}`;
}

/**
 * Custom control bar rendered outside the zoom/pan transform, so it stays pinned at
 * the bottom while the video frame moves. Drives the underlying <video> via ref.
 */
function VideoControls({
  videoRef,
  itemId,
  onFullscreen,
  fullscreen,
  hidden,
  audioTracks = [],
  activeAudio = -1,
  onSelectAudio,
  subTracks = [],
  activeSub = -1,
  onSelectSub,
}: {
  videoRef: React.RefObject<HTMLVideoElement>;
  itemId: string;
  onFullscreen: () => void;
  fullscreen: boolean;
  hidden: boolean;
  // Multi-track audio (mkv): switching re-prepares the temp file with the chosen track.
  audioTracks?: { index: number; label: string }[];
  activeAudio?: number;
  onSelectAudio?: (index: number) => void;
  // Subtitles, controlled by the player (single <track> remounted per selection).
  subTracks?: { i: number; label: string }[];
  activeSub?: number;
  onSelectSub?: (index: number) => void;
}) {
  const [playing, setPlaying] = useState(true);
  const [cur, setCur] = useState(0);
  const [dur, setDur] = useState(0);
  const [buf, setBuf] = useState(0);
  const [vol, setVol] = useState(1);
  const [muted, setMuted] = useState(false);
  const [capsMenu, setCapsMenu] = useState(false);
  const [audioMenu, setAudioMenu] = useState(false);
  const seeking = useRef(false);
  const trackRef = useRef<HTMLDivElement>(null);

  // Close the pop-up menus when the chrome hides.
  useEffect(() => {
    if (hidden) {
      setCapsMenu(false);
      setAudioMenu(false);
    }
  }, [hidden]);

  // (Re)subscribe to the current <video> whenever the shown item changes.
  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    const onTime = () => {
      if (!seeking.current) setCur(v.currentTime);
    };
    const onDur = () => setDur(v.duration || 0);
    const onPlay = () => setPlaying(true);
    const onPause = () => setPlaying(false);
    const onProg = () => {
      if (v.buffered.length) setBuf(v.buffered.end(v.buffered.length - 1));
    };
    const onVol = () => {
      setVol(v.volume);
      setMuted(v.muted);
    };
    v.addEventListener("timeupdate", onTime);
    v.addEventListener("durationchange", onDur);
    v.addEventListener("loadedmetadata", onDur);
    v.addEventListener("play", onPlay);
    v.addEventListener("pause", onPause);
    v.addEventListener("progress", onProg);
    v.addEventListener("volumechange", onVol);
    setDur(v.duration || 0);
    setCur(v.currentTime);
    setPlaying(!v.paused);
    setVol(v.volume);
    setMuted(v.muted);
    return () => {
      v.removeEventListener("timeupdate", onTime);
      v.removeEventListener("durationchange", onDur);
      v.removeEventListener("loadedmetadata", onDur);
      v.removeEventListener("play", onPlay);
      v.removeEventListener("pause", onPause);
      v.removeEventListener("progress", onProg);
      v.removeEventListener("volumechange", onVol);
    };
  }, [videoRef, itemId]);

  const pct = dur ? clamp(cur / dur, 0, 1) * 100 : 0;
  const bufPct = dur ? clamp(buf / dur, 0, 1) * 100 : 0;

  const seekTo = (clientX: number) => {
    const el = trackRef.current;
    const v = videoRef.current;
    if (!el || !v || !dur) return;
    const r = el.getBoundingClientRect();
    const frac = clamp((clientX - r.left) / r.width, 0, 1);
    setCur(frac * dur);
    v.currentTime = frac * dur;
  };

  const togglePlay = () => {
    const v = videoRef.current;
    if (v) (v.paused ? v.play() : v.pause());
  };
  const skip = (delta: number) => {
    const v = videoRef.current;
    if (!v) return;
    v.currentTime = clamp(v.currentTime + delta, 0, v.duration || Infinity);
    v.dispatchEvent(new CustomEvent("uiskip", { detail: delta < 0 ? "back" : "forward" }));
  };
  const toggleMute = () => {
    const v = videoRef.current;
    if (v) v.muted = !v.muted;
  };
  const setVolume = (value: number) => {
    const v = videoRef.current;
    if (!v) return;
    v.volume = value;
    v.muted = value === 0;
  };
  const selectTrack = (i: number) => {
    onSelectSub?.(i);
    setCapsMenu(false);
  };

  const btn = "rounded p-1.5 text-white/85 transition hover:bg-white/15 hover:text-white";

  return (
    <div
      data-control
      className={`absolute inset-x-0 bottom-0 z-20 flex items-center gap-3 bg-gradient-to-t from-black/90 via-black/70 to-transparent px-4 pb-3 pt-8 text-white transition-opacity duration-300 ${
        hidden ? "pointer-events-none opacity-0" : "pointer-events-auto opacity-100"
      }`}
    >
      <button onClick={togglePlay} className={btn} aria-label={playing ? "Pause" : "Play"}>
        {playing ? (
          <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor">
            <rect x="6" y="5" width="4" height="14" rx="1" />
            <rect x="14" y="5" width="4" height="14" rx="1" />
          </svg>
        ) : (
          <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor">
            <path d="M8 5v14l11-7z" />
          </svg>
        )}
      </button>

      <button onClick={toggleMute} className={btn} aria-label={muted ? "Unmute" : "Mute"}>
        {muted || vol === 0 ? (
          <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor">
            <path d="M5 9v6h4l5 5V4L9 9H5z" />
            <path d="M16 9l4 4m0-4l-4 4" stroke="currentColor" strokeWidth="2" fill="none" />
          </svg>
        ) : (
          <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor">
            <path d="M5 9v6h4l5 5V4L9 9H5z" />
            <path d="M16.5 8.5a5 5 0 0 1 0 7" stroke="currentColor" strokeWidth="2" fill="none" />
          </svg>
        )}
      </button>
      <input
        type="range"
        min={0}
        max={1}
        step={0.02}
        value={muted ? 0 : vol}
        onChange={(e) => setVolume(Number(e.target.value))}
        className="w-20 accent-white"
        aria-label="Volume"
      />

      <span className="shrink-0 text-xs tabular-nums text-white/80">
        {fmtTime(cur)} / {fmtTime(dur)}
      </span>

      {/* Seek bar with buffered + played progress; click or drag to scrub. */}
      <div
        ref={trackRef}
        onPointerDown={(e) => {
          seeking.current = true;
          try {
            (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
          } catch {
            /* ignore */
          }
          seekTo(e.clientX);
        }}
        onPointerMove={(e) => {
          if (seeking.current) seekTo(e.clientX);
        }}
        onPointerUp={(e) => {
          seeking.current = false;
          try {
            (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
          } catch {
            /* ignore */
          }
        }}
        className="group relative h-4 flex-1 cursor-pointer"
      >
        <div className="absolute top-1/2 h-1.5 w-full -translate-y-1/2 overflow-hidden rounded-full bg-white/25">
          <div className="absolute inset-y-0 left-0 bg-white/35" style={{ width: `${bufPct}%` }} />
          <div className="absolute inset-y-0 left-0 bg-white" style={{ width: `${pct}%` }} />
        </div>
        <div
          className="pointer-events-none absolute top-1/2 h-3 w-3 -translate-x-1/2 -translate-y-1/2 rounded-full bg-white opacity-0 shadow transition group-hover:opacity-100"
          style={{ left: `${pct}%` }}
        />
      </div>

      <button onClick={() => skip(-10)} className={btn} aria-label="Back 10 seconds" title="Back 10s">
        <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor">
          <path d="M11 6L5 12l6 6V6z" />
          <path d="M19 6l-6 6 6 6V6z" />
        </svg>
      </button>
      <button onClick={() => skip(10)} className={btn} aria-label="Forward 10 seconds" title="Forward 10s">
        <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor">
          <path d="M13 6l6 6-6 6V6z" />
          <path d="M5 6l6 6-6 6V6z" />
        </svg>
      </button>

      {audioTracks.length > 1 && (
        <div className="relative">
          <button
            onClick={() => setAudioMenu((o) => !o)}
            aria-label="Audio language"
            title="Audio language"
            aria-pressed={audioMenu}
            className={`${btn} ${audioMenu ? "bg-white/20 text-white" : ""}`}
          >
            {/* Globe = language (audio tracks are usually different languages). */}
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <circle cx="12" cy="12" r="9" />
              <line x1="3" y1="12" x2="21" y2="12" strokeLinecap="round" />
              <path d="M12 3c2.6 2.6 2.6 15.4 0 18M12 3c-2.6 2.6-2.6 15.4 0 18" strokeLinecap="round" />
            </svg>
          </button>
          {audioMenu && (
            <div className="absolute bottom-full right-0 mb-2 min-w-32 overflow-hidden rounded-lg bg-black/90 py-1 text-sm ring-1 ring-white/10">
              {audioTracks.map((tr, i) => {
                const on = activeAudio === tr.index || (activeAudio < 0 && i === 0);
                return (
                  <button
                    key={tr.index}
                    onClick={() => {
                      onSelectAudio?.(tr.index);
                      setAudioMenu(false);
                    }}
                    className={`block w-full truncate px-3 py-1.5 text-left hover:bg-white/10 ${
                      on ? "text-white" : "text-white/70"
                    }`}
                  >
                    {tr.label}
                  </button>
                );
              })}
            </div>
          )}
        </div>
      )}

      {subTracks.length > 0 && (
        <div className="relative">
          <button
            onClick={() => setCapsMenu((o) => !o)}
            aria-label="Captions"
            title="Captions (CC)"
            aria-pressed={activeSub >= 0}
            className={`${btn} ${activeSub >= 0 ? "bg-white/20 text-white" : ""}`}
          >
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <rect x="3" y="5" width="18" height="14" rx="2" />
              <path d="M8 11h2M8 14h3M14 11h2M14 14h3" strokeLinecap="round" />
            </svg>
          </button>
          {capsMenu && (
            <div className="absolute bottom-full right-0 mb-2 min-w-32 overflow-hidden rounded-lg bg-black/90 py-1 text-sm ring-1 ring-white/10">
              <button
                onClick={() => selectTrack(-1)}
                className={`block w-full px-3 py-1.5 text-left hover:bg-white/10 ${
                  activeSub < 0 ? "text-white" : "text-white/70"
                }`}
              >
                Off
              </button>
              {subTracks.map((tr) => (
                <button
                  key={tr.i}
                  onClick={() => selectTrack(tr.i)}
                  className={`block w-full truncate px-3 py-1.5 text-left hover:bg-white/10 ${
                    activeSub === tr.i ? "text-white" : "text-white/70"
                  }`}
                >
                  {tr.label}
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      <button
        onClick={onFullscreen}
        className={btn}
        aria-label={fullscreen ? "Exit fullscreen" : "Fullscreen"}
        title={fullscreen ? "Exit fullscreen" : "Fullscreen"}
      >
        {fullscreen ? (
          // Inward corners = currently fullscreen (click to exit).
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" />
          </svg>
        ) : (
          // Outward corners = enter fullscreen.
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" />
          </svg>
        )}
      </button>
    </div>
  );
}
