import { useEffect, useRef, useState } from "react";

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
  /** Fill the viewport edge-to-edge (fullscreen) vs. contained with a bottom band. */
  fullscreen: boolean;
  /** Fade out the control bar when the pointer is idle. */
  chromeHidden: boolean;
  onFullscreen: () => void;
  /** Close the lightbox (used when clicking the empty letterbox around the frame). */
  onRequestClose: () => void;
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
  fullscreen,
  chromeHidden,
  onFullscreen,
  onRequestClose,
}: VideoPlayerProps) {
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
            src={src}
            autoPlay
            draggable={false}
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
}: {
  videoRef: React.RefObject<HTMLVideoElement>;
  itemId: string;
  onFullscreen: () => void;
  fullscreen: boolean;
  hidden: boolean;
}) {
  const [playing, setPlaying] = useState(true);
  const [cur, setCur] = useState(0);
  const [dur, setDur] = useState(0);
  const [buf, setBuf] = useState(0);
  const [vol, setVol] = useState(1);
  const [muted, setMuted] = useState(false);
  const seeking = useRef(false);
  const trackRef = useRef<HTMLDivElement>(null);

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

  const pct = dur ? (cur / dur) * 100 : 0;
  const bufPct = dur ? (buf / dur) * 100 : 0;

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
