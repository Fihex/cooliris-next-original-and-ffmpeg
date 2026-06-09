import { useEffect, useRef, useState } from "react";

/**
 * libmpv-backed video player (Option C). Instead of a Chromium <video>, it pulls
 * decoded RGBA frames from the native mpv player (main process) and paints them into a
 * <canvas>. mpv handles every format + composites subtitles, so there's no transcoding.
 *
 * Frames are pulled on a ~30fps gate and capped in resolution to keep the per-frame IPC
 * affordable. Playback is driven through mpv properties/commands over the bridge.
 */

const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));
const MAX_W = 1280; // cap render width → bounds the per-frame IPC payload
const FRAME_MS = 33; // ~30fps

interface Transform {
  s: number;
  x: number;
  y: number;
}

interface MpvPlayerProps {
  abs: string;
  itemId: string;
  t: Transform;
  smooth: boolean;
  stageRef: React.RefObject<HTMLDivElement>;
  fullscreen: boolean;
  chromeHidden: boolean;
  onFullscreen: () => void;
  onRequestClose: () => void;
  onPlayingChange?: (playing: boolean) => void;
}

function fmtTime(s: number): string {
  if (!isFinite(s) || s < 0) s = 0;
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = Math.floor(s % 60);
  const mm = h ? String(m).padStart(2, "0") : String(m);
  return `${h ? `${h}:` : ""}${mm}:${String(sec).padStart(2, "0")}`;
}

export function MpvPlayer({
  abs,
  itemId,
  t,
  smooth,
  stageRef,
  fullscreen,
  chromeHidden,
  onFullscreen,
  onRequestClose,
  onPlayingChange,
}: MpvPlayerProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [playing, setPlaying] = useState(true);
  const [cur, setCur] = useState(0);
  const [dur, setDur] = useState(0);
  const [vol, setVol] = useState(100);
  const [ready, setReady] = useState(false);
  const shownRef = useRef(false);
  const seeking = useRef(false);
  const trackRef = useRef<HTMLDivElement>(null);

  const mpv = typeof window !== "undefined" ? window.electron : undefined;

  // Load the file and pump frames into the canvas while this item is shown.
  useEffect(() => {
    if (!mpv) return;
    let cancelled = false;
    let raf = 0;
    let last = 0;
    shownRef.current = false;
    setReady(false);
    setCur(0);
    setDur(0);
    setPlaying(true);
    mpv.mpvLoad(abs);

    const ctx = canvasRef.current?.getContext("2d") ?? null;
    const pump = async (ts: number) => {
      if (cancelled) return;
      if (ts - last >= FRAME_MS) {
        last = ts;
        try {
          const sz = await mpv.mpvSize();
          if (sz && sz.w > 0 && ctx && canvasRef.current) {
            const rw = Math.min(sz.w, MAX_W);
            const rh = Math.max(1, Math.round((sz.h * rw) / sz.w));
            if (canvasRef.current.width !== rw || canvasRef.current.height !== rh) {
              canvasRef.current.width = rw;
              canvasRef.current.height = rh;
            }
            const buf = await mpv.mpvFrame(rw, rh);
            if (!cancelled && buf && buf.length === rw * rh * 4) {
              ctx.putImageData(new ImageData(new Uint8ClampedArray(buf), rw, rh), 0, 0);
              if (!shownRef.current) {
                shownRef.current = true;
                setReady(true);
              }
            }
          }
        } catch {
          /* frame skipped */
        }
      }
      raf = requestAnimationFrame(pump);
    };
    raf = requestAnimationFrame(pump);

    return () => {
      cancelled = true;
      cancelAnimationFrame(raf);
      mpv.mpvStop();
    };
  }, [abs, itemId, mpv]);

  // Poll playback state for the control bar.
  useEffect(() => {
    if (!mpv) return;
    let alive = true;
    const id = window.setInterval(async () => {
      if (!alive) return;
      const [tp, d, p, vv] = await Promise.all([
        mpv.mpvGet("time-pos"),
        mpv.mpvGet("duration"),
        mpv.mpvGet("pause"),
        mpv.mpvGet("volume"),
      ]);
      if (!alive) return;
      if (!seeking.current && tp != null) setCur(parseFloat(tp) || 0);
      if (d != null) setDur(parseFloat(d) || 0);
      if (p != null) {
        const isPlaying = p !== "yes";
        setPlaying(isPlaying);
        onPlayingChange?.(isPlaying);
      }
      if (vv != null) setVol(parseFloat(vv) || 0);
    }, 250);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, [mpv, itemId, onPlayingChange]);

  const togglePlay = () => mpv?.mpvCmd(["cycle", "pause"]);
  const seekTo = (clientX: number) => {
    const el = trackRef.current;
    if (!el || !dur) return;
    const r = el.getBoundingClientRect();
    const frac = clamp((clientX - r.left) / r.width, 0, 1);
    setCur(frac * dur);
    mpv?.mpvCmd(["seek", String(frac * dur), "absolute"]);
  };
  const setVolume = (value: number) => {
    setVol(value);
    mpv?.mpvSet("volume", String(value));
  };

  // Keyboard: ←/→ seek ∓10s, ↑/↓ volume (mirrors the video shortcuts).
  useEffect(() => {
    if (!mpv) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "ArrowRight") {
        e.preventDefault();
        mpv.mpvCmd(["seek", "10", "relative"]);
      } else if (e.key === "ArrowLeft") {
        e.preventDefault();
        mpv.mpvCmd(["seek", "-10", "relative"]);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setVolume(clamp(vol + 10, 0, 130));
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        setVolume(clamp(vol - 10, 0, 130));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mpv, vol]);

  const pct = dur ? clamp(cur / dur, 0, 1) * 100 : 0;
  const btn = "rounded p-1.5 text-white/85 transition hover:bg-white/15 hover:text-white";

  return (
    <>
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
          <canvas
            ref={canvasRef}
            onClick={() => togglePlay()}
            className={`pointer-events-auto max-h-full max-w-full object-contain ${
              chromeHidden ? "cursor-none" : "cursor-pointer"
            }`}
            style={{ width: "auto", height: "auto" }}
          />
        </div>

        {!ready && (
          <div className="pointer-events-none absolute inset-0 z-10 flex items-center justify-center">
            <div className="rounded-lg bg-black/70 px-4 py-3 text-sm text-white">Loading…</div>
          </div>
        )}
      </div>

      <div
        data-control
        className={`absolute inset-x-0 bottom-0 z-20 flex items-center gap-3 bg-gradient-to-t from-black/90 via-black/70 to-transparent px-4 pb-3 pt-8 text-white transition-opacity duration-300 ${
          chromeHidden ? "pointer-events-none opacity-0" : "pointer-events-auto opacity-100"
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

        <input
          type="range"
          min={0}
          max={130}
          step={1}
          value={vol}
          onChange={(e) => setVolume(Number(e.target.value))}
          className="w-20 accent-white"
          aria-label="Volume"
        />

        <span className="shrink-0 text-xs tabular-nums text-white/80">
          {fmtTime(cur)} / {fmtTime(dur)}
        </span>

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
            <div className="absolute inset-y-0 left-0 bg-white" style={{ width: `${pct}%` }} />
          </div>
        </div>

        <button onClick={onRequestClose} className={btn} aria-label="Close" title="Close">
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M6 6l12 12M18 6L6 18" strokeLinecap="round" />
          </svg>
        </button>
        <button onClick={onFullscreen} className={btn} aria-label="Fullscreen" title="Fullscreen">
          {fullscreen ? (
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" />
            </svg>
          ) : (
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" />
            </svg>
          )}
        </button>
      </div>
    </>
  );
}
