import { useCallback, useEffect, useRef, useState } from "react";
import type { MediaItem } from "@/feed/types";
import { MpvPlayer } from "./MpvPlayer";
import { EMBED } from "@/embedMode";

// All videos play through libmpv (every format, no transcode). Recover the file path
// from the coolmedia:// URL the scan produced.
function decodeAbs(url: string): string {
  try {
    return decodeURIComponent(new URL(url).pathname.replace(/^\//, ""));
  } catch {
    return url;
  }
}

interface LightboxProps {
  item: MediaItem;
  index: number;
  total: number;
  /** Which timestamp the info panel shows (matches the toolbar's date filter). */
  dateField?: "modified" | "created";
  closing?: boolean; // fading out (driven by the parent)
  onClose: () => void;
  onPrev: () => void;
  onNext: () => void;
}

const MIN_SCALE = 1;
const MAX_SCALE = 8;
const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));

/**
 * Full-screen media viewer (opaque, so the wall behind never shows through).
 * Wheel/pinch zoom, drag/2-finger pan. Clicking the media or controls never
 * closes — only an empty-area tap, Back, or Esc do. Media can't be selected/dragged.
 */
export function Lightbox({
  item,
  index,
  total,
  dateField = "modified",
  closing,
  onClose,
  onPrev,
  onNext,
}: LightboxProps) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const [t, setT] = useState({ s: 1, x: 0, y: 0 });
  const tRef = useRef(t);
  tRef.current = t;

  const [shown, setShown] = useState(false); // for fade-in
  const [smooth, setSmooth] = useState(false); // transition on for wheel, off for drag/pinch
  // Info-panel toggle persists across items + sessions (off stays off until re-enabled).
  const [showInfo, setShowInfo] = useState(
    () => localStorage.getItem("lightbox.showInfo") !== "0"
  );
  const toggleInfo = () =>
    setShowInfo((v) => {
      const next = !v;
      localStorage.setItem("lightbox.showInfo", next ? "1" : "0");
      return next;
    });
  const [isFullscreen, setIsFullscreen] = useState(false); // gestures off in fullscreen
  const [idle, setIdle] = useState(false); // pointer inactive → hide chrome
  const [videoPaused, setVideoPaused] = useState(false);
  const pointers = useRef(new Map<number, { x: number; y: number }>());
  const pinchDist = useRef(0);
  const moved = useRef(false);
  const downOnEmpty = useRef(false);
  const idleRef = useRef(false);
  const idleTimer = useRef<number | undefined>(undefined);

  // Auto-hide chrome (controls/info/cursor) when idle: for a *playing* video in any
  // mode, and for images/audio only in fullscreen. Paused video keeps its controls.
  const hideChrome =
    idle && (item.type === "video" ? !videoPaused : isFullscreen);

  // Any pointer activity shows the chrome and restarts the idle countdown.
  const bumpActivity = () => {
    if (idleRef.current) {
      idleRef.current = false;
      setIdle(false);
    }
    window.clearTimeout(idleTimer.current);
    idleTimer.current = window.setTimeout(() => {
      idleRef.current = true;
      setIdle(true);
    }, 2500);
  };

  useEffect(() => {
    const id = requestAnimationFrame(() => setShown(true));
    return () => cancelAnimationFrame(id);
  }, []);

  // Start the idle countdown on open; clear the timer on close.
  useEffect(() => {
    bumpActivity();
    return () => window.clearTimeout(idleTimer.current);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Reset zoom whenever the shown item changes.
  useEffect(() => setT({ s: 1, x: 0, y: 0 }), [item.id]);

  // Track fullscreen; entering/leaving resets zoom so the frame fits cleanly.
  useEffect(() => {
    const onFsChange = () => {
      setIsFullscreen(!!document.fullscreenElement);
      setT({ s: 1, x: 0, y: 0 });
    };
    document.addEventListener("fullscreenchange", onFsChange);
    return () => document.removeEventListener("fullscreenchange", onFsChange);
  }, []);

  // Track play/pause so chrome only auto-hides while a video is actually playing.
  useEffect(() => {
    if (item.type !== "video") return;
    const v = videoRef.current;
    if (!v) return;
    const sync = () => setVideoPaused(v.paused);
    sync();
    v.addEventListener("play", sync);
    v.addEventListener("pause", sync);
    return () => {
      v.removeEventListener("play", sync);
      v.removeEventListener("pause", sync);
    };
  }, [item.id, item.type]);

  // Keyboard: Esc closes; arrows navigate (images/audio) or seek/volume (video).
  useEffect(() => {
    const isVideo = item.type === "video";
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        // Esc leaves fullscreen first; only closes the lightbox when not fullscreen.
        if (EMBED) {
          if (isFullscreen) window.electron?.winFullscreen(false).then(() => setIsFullscreen(false));
          else onClose();
        } else if (document.fullscreenElement) {
          document.exitFullscreen();
        } else {
          onClose();
        }
        return;
      }
      // Videos play through MpvPlayer, which owns the arrow keys (seek / volume) —
      // don't let the lightbox navigate items while watching one.
      if (isVideo) return;
      if (e.key === "ArrowRight") onNext();
      else if (e.key === "ArrowLeft") onPrev();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, onPrev, onNext, item.type, isFullscreen]);

  const zoomAt = useCallback((factor: number, clientX: number, clientY: number) => {
    setT((p) => {
      const s2 = clamp(p.s * factor, MIN_SCALE, MAX_SCALE);
      if (s2 === 1) return { s: 1, x: 0, y: 0 };
      const r = stageRef.current!.getBoundingClientRect();
      const px = clientX - (r.left + r.width / 2);
      const py = clientY - (r.top + r.height / 2);
      return { s: s2, x: px - (px - p.x) * (s2 / p.s), y: py - (py - p.y) * (s2 / p.s) };
    });
  }, []);

  // Wheel-zoom via a non-passive native listener: React attaches `onWheel` as
  // passive, so e.preventDefault() there is ignored (and warns). Attach it ourselves.
  useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const onWheelNative = (e: WheelEvent) => {
      if (isFullscreen) return; // no zoom in fullscreen — behave like a normal player
      e.preventDefault();
      setSmooth(true); // animate between wheel steps
      zoomAt(e.deltaY < 0 ? 1.2 : 1 / 1.2, e.clientX, e.clientY);
    };
    el.addEventListener("wheel", onWheelNative, { passive: false });
    return () => el.removeEventListener("wheel", onWheelNative);
  }, [isFullscreen, zoomAt]);

  const onPointerDown = (e: React.PointerEvent) => {
    bumpActivity();
    moved.current = false;
    const isMouse = e.pointerType === "mouse";
    // A left-click / touch on empty space can become a close-tap.
    const onEmpty = !(e.target as HTMLElement).closest("[data-media],[data-control]");
    downOnEmpty.current = onEmpty && (!isMouse || e.button === 0);

    // Pan/pinch is driven by the MIDDLE or RIGHT mouse button (left stays free for
    // taps and the native media controls), or by touch/pen. Disabled in fullscreen.
    const canPan = !isFullscreen && (!isMouse || e.button === 1 || e.button === 2);
    if (!canPan) return;
    e.preventDefault(); // suppress middle-click autoscroll / right-click quirks

    pointers.current.set(e.pointerId, { x: e.clientX, y: e.clientY });
    // Capture mouse pointers so the release is always delivered here — otherwise a
    // button let go off-window leaves a stale pointer that "pans" on plain moves.
    if (isMouse) {
      try {
        (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
      } catch {
        /* pointer already gone */
      }
    }
    if (pointers.current.size === 2) {
      const [a, b] = [...pointers.current.values()];
      pinchDist.current = Math.hypot(a.x - b.x, a.y - b.y);
    }
  };

  const onPointerMove = (e: React.PointerEvent) => {
    bumpActivity();
    // Safety net: a mouse move with no button held means a pointerup was missed —
    // drop stale pointers so the image never pans while nothing is pressed.
    if (e.pointerType === "mouse" && e.buttons === 0 && pointers.current.size > 0) {
      pointers.current.clear();
      pinchDist.current = 0;
      return;
    }
    const prev = pointers.current.get(e.pointerId);
    if (!prev) return;
    pointers.current.set(e.pointerId, { x: e.clientX, y: e.clientY });

    if (pointers.current.size === 2) {
      const [a, b] = [...pointers.current.values()];
      const dist = Math.hypot(a.x - b.x, a.y - b.y);
      setSmooth(false); // pinch must track the fingers instantly
      if (pinchDist.current > 0) zoomAt(dist / pinchDist.current, (a.x + b.x) / 2, (a.y + b.y) / 2);
      pinchDist.current = dist;
      moved.current = true;
      return;
    }
    const dx = e.clientX - prev.x;
    const dy = e.clientY - prev.y;
    if (Math.abs(dx) > 3 || Math.abs(dy) > 3) moved.current = true;
    if (tRef.current.s > 1) {
      setSmooth(false); // pan must track the cursor instantly
      setT((p) => ({ ...p, x: p.x + dx, y: p.y + dy }));
    }
  };

  const onPointerUp = (e: React.PointerEvent) => {
    pointers.current.delete(e.pointerId);
    if (pointers.current.size < 2) pinchDist.current = 0;
    if (!moved.current && downOnEmpty.current) onClose(); // tap empty area = close
  };

  // Fullscreen the whole lightbox (so our custom control bar stays visible too).
  const toggleFullscreen = useCallback(() => {
    // Embed mode: toggle the OS WINDOW fullscreen (the browser's requestFullscreen would
    // paint over the mpv video surface, hiding it). Also re-fits the mpv --wid surface.
    if (EMBED) {
      const next = !isFullscreen;
      window.electron?.winFullscreen(next).then((on) => {
        setIsFullscreen(!!on);
        setT({ s: 1, x: 0, y: 0 });
      });
      return;
    }
    const el = wrapRef.current;
    if (!el) return;
    if (document.fullscreenElement) document.exitFullscreen();
    else el.requestFullscreen?.();
  }, [isFullscreen]);

  const isVideo = item.type === "video";
  const isAudio = item.type === "audio";

  // Info-panel date: follow the chosen field, labeled when a created date exists.
  const labeledDate = item.created != null; // Electron item (has both dates)
  const showCreated = dateField === "created" && item.created != null;
  const dateTs = showCreated ? item.created : item.date;
  const dateLabel = labeledDate ? (showCreated ? "Created" : "Modified") : "";

  return (
    <div
      ref={wrapRef}
      className={`absolute inset-0 z-40 select-none touch-none overflow-hidden transition-opacity duration-200 ${
        EMBED ? "" : "bg-black"
      } ${shown && !closing ? "opacity-100" : "opacity-0"} ${
        closing ? "pointer-events-none" : ""
      } ${hideChrome ? "cursor-none" : ""}`}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      onContextMenu={(e) => e.preventDefault()}
    >
      {/* Video gets a dedicated player (frame zooms/pans, controls stay pinned).
          Images/audio use the shared stage: a reserved bottom band keeps the media
          centered above the info panel; zoom origin is the stage center. */}
      {isVideo ? (
        <MpvPlayer
          abs={decodeAbs(item.full)}
          itemId={item.id}
          t={t}
          smooth={smooth}
          stageRef={stageRef}
          fullscreen={isFullscreen}
          chromeHidden={hideChrome}
          onFullscreen={toggleFullscreen}
          onPlayingChange={(p) => setVideoPaused(!p)}
        />
      ) : (
        <div ref={stageRef} className="absolute inset-0">
          <div
            className="pointer-events-none absolute inset-0 flex items-center justify-center will-change-transform"
            style={{
              transform: `translate(${t.x}px, ${t.y}px) scale(${t.s})`,
              transformOrigin: "center center",
              transition: smooth ? "transform 140ms ease-out" : "none",
            }}
          >
            {isAudio ? (
              <div
                data-media
                key={item.id}
                className="pointer-events-auto flex flex-col items-center gap-5 rounded-lg bg-white/5 p-6 shadow-2xl"
              >
                {item.thumb ? (
                  <img
                    src={item.thumb}
                    alt={item.title ?? ""}
                    draggable={false}
                    className="max-h-[58vh] max-w-[80vw] select-none rounded-md object-contain"
                    style={{ WebkitUserDrag: "none" } as React.CSSProperties}
                  />
                ) : null}
                {item.title ? (
                  <div className="max-w-[80vw] truncate text-center text-sm text-white/80">
                    {item.title}
                  </div>
                ) : null}
                <audio
                  key={item.id}
                  src={item.full}
                  controls
                  autoPlay
                  preload="metadata"
                  style={{ colorScheme: "dark" }}
                  className="w-[min(80vw,520px)]"
                />
              </div>
            ) : (
              <img
                data-media
                key={item.id}
                src={item.full}
                alt={item.title ?? ""}
                draggable={false}
                decoding="async"
                onDragStart={(e) => e.preventDefault()}
                className="pointer-events-auto max-h-full max-w-[94vw] select-none rounded-lg object-contain shadow-2xl"
                style={{ WebkitUserDrag: "none" } as React.CSSProperties}
              />
            )}
          </div>
        </div>
      )}

      <button
        data-control
        onClick={onClose}
        className={`absolute left-4 top-4 z-10 rounded-full bg-white/10 px-3 py-1.5 text-sm text-white backdrop-blur transition-opacity duration-300 hover:bg-white/20 ${
          hideChrome ? "pointer-events-none opacity-0" : "opacity-100"
        }`}
      >
        ← Back
      </button>

      {/* Toggle the info panel (title/path/date) on/off. */}
      <button
        data-control
        onClick={toggleInfo}
        aria-pressed={showInfo}
        title={showInfo ? "Hide info" : "Show info"}
        className={`absolute right-4 top-4 z-10 flex h-9 w-9 items-center justify-center rounded-full backdrop-blur transition-opacity duration-300 ${
          hideChrome ? "pointer-events-none opacity-0" : "opacity-100"
        } ${showInfo ? "bg-white/25 text-white" : "bg-white/10 text-white/70 hover:bg-white/20"}`}
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
          <circle cx="12" cy="12" r="9" />
          <path d="M12 11v5" strokeLinecap="round" />
          <circle cx="12" cy="7.5" r="0.6" fill="currentColor" stroke="none" />
        </svg>
      </button>

      <Arrow side="left" onClick={onPrev} hidden={hideChrome} />
      <Arrow side="right" onClick={onNext} hidden={hideChrome} />

      {/* Info panel sits in the reserved bottom band → never overlaps the media.
          For video it rides above the custom control bar. Toggle via the ⓘ button. */}
      {showInfo && (
      <div
        data-control
        className={`pointer-events-none absolute inset-x-0 z-10 flex justify-center px-20 transition-opacity duration-300 ${
          isVideo ? "bottom-[4.5rem]" : "bottom-5"
        } ${hideChrome ? "opacity-0" : "opacity-100"}`}
      >
        <div
          className={`max-h-24 max-w-[70vw] overflow-y-auto rounded-2xl bg-black/70 px-5 py-2 text-center backdrop-blur ring-1 ring-white/10 ${
            hideChrome ? "pointer-events-none" : "pointer-events-auto"
          }`}
        >
          <div className="text-sm font-medium text-white [overflow-wrap:anywhere]">
            {item.title || "Untitled"}
          </div>
          {item.path && (
            <button
              type="button"
              title="Click to copy path"
              onClick={() => navigator.clipboard?.writeText(item.path ?? "")}
              className="block w-full text-xs text-white/55 [overflow-wrap:anywhere] hover:text-white/80"
            >
              {item.path}
            </button>
          )}
          <div className="text-xs text-white/40">
            {index + 1} / {total}
            {dateTs != null
              ? ` · ${dateLabel ? `${dateLabel} ` : ""}${new Date(dateTs).toLocaleDateString()}`
              : ""}
          </div>
        </div>
      </div>
      )}
    </div>
  );
}

function Arrow({
  side,
  onClick,
  hidden,
}: {
  side: "left" | "right";
  onClick: () => void;
  hidden?: boolean;
}) {
  return (
    <button
      data-control
      onClick={onClick}
      aria-label={side === "left" ? "Previous" : "Next"}
      className={`absolute top-1/2 z-10 flex h-16 w-16 -translate-y-1/2 items-center justify-center rounded-full bg-black/40 text-4xl text-white/80 backdrop-blur transition hover:bg-black/60 hover:text-white ${
        side === "left" ? "left-4" : "right-4"
      } ${hidden ? "pointer-events-none opacity-0" : "opacity-100"}`}
    >
      <svg width="34" height="34" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d={side === "left" ? "M15 18l-6-6 6-6" : "M9 18l6-6-6-6"} /></svg>
    </button>
  );
}
