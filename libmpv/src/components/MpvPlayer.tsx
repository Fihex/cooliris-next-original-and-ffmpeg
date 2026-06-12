import { useEffect, useRef, useState } from "react";
import { EMBED } from "@/embedMode";

/**
 * libmpv-backed video player (Option C). Instead of a Chromium <video>, it pulls
 * decoded RGBA frames from the native mpv player (main process) and paints them into a
 * <canvas>. mpv handles every format + composites subtitles, so there's no transcoding.
 *
 * Frames are pulled on a ~30fps gate and capped in resolution to keep the per-frame IPC
 * affordable. Playback is driven through mpv properties/commands over the bridge.
 */

const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));
const MAX_W = 1920; // cap render width → bounds the per-frame IPC payload
const FRAME_MS = 16; // up to ~60fps (was 30); the engine sets real cadence, we sample latest

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
  onPlayingChange?: (playing: boolean) => void;
  /** Two-window embed: the main process already issued the load (at click time, to overlap
   *  decode with the UI mount), so don't load again here. */
  skipLoad?: boolean;
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
  onPlayingChange,
  skipLoad,
}: MpvPlayerProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const paintRef = useRef<Worker | null>(null); // off-main-thread WebGL paint (OffscreenCanvas)
  const [playing, setPlaying] = useState(true);
  const [cur, setCur] = useState(0);
  const [dur, setDur] = useState(0);
  const [vol, setVol] = useState(100);
  const [ready, setReady] = useState(false);
  const [audioTracks, setAudioTracks] = useState<{ id: string; label: string }[]>([]);
  const [subTracks, setSubTracks] = useState<{ id: string; label: string }[]>([]);
  const [activeAid, setActiveAid] = useState("");
  const [activeSid, setActiveSid] = useState("no");
  const [audioMenu, setAudioMenu] = useState(false);
  const [capsMenu, setCapsMenu] = useState(false);
  const [capsTab, setCapsTab] = useState<"tracks" | "style">("tracks");
  // Subtitle style (applied live via mpv properties).
  const [subSize, setSubSize] = useState(44);
  const [subColor, setSubColor] = useState("#ffffff");
  const [subBg, setSubBg] = useState("#000000");
  const [subBgAlpha, setSubBgAlpha] = useState(0); // 0 = transparent … 100 = opaque
  const [subOutline, setSubOutline] = useState(true); // text outline/border on by default
  const [subOutlineColor, setSubOutlineColor] = useState("#000000");
  const shownRef = useRef(false);
  const seeking = useRef(false);
  const trackRef = useRef<HTMLDivElement>(null);

  const mpv = typeof window !== "undefined" ? window.electron : undefined;

  // Once the file is loaded, read mpv's track list (audio + subtitle) for the choosers.
  useEffect(() => {
    if (!mpv) return;
    let alive = true;
    setAudioTracks([]);
    setSubTracks([]);
    setActiveSid("no");
    // Tracks are demuxed progressively (video first, then audio + sidecar subtitles a few
    // seconds later). Keep refreshing for the lifetime of the open video — updating only
    // when the set changes — instead of latching once it briefly looks "stable" (that
    // missed late-arriving subtitle/audio tracks, which then only showed after a reopen).
    let lastSig = "";
    const tick = async () => {
      if (!alive) return;
      const count = parseInt((await mpv.mpvGet("track-list/count")) || "0", 10);
      if (count <= 0) return;
      const a: { id: string; label: string }[] = [];
      const s: { id: string; label: string }[] = [];
      for (let i = 0; i < count; i++) {
        const type = await mpv.mpvGet(`track-list/${i}/type`);
        const tid = (await mpv.mpvGet(`track-list/${i}/id`)) || "";
        const lang = await mpv.mpvGet(`track-list/${i}/lang`);
        const title = await mpv.mpvGet(`track-list/${i}/title`);
        const langStr = lang && lang !== "null" ? `${lang} ` : "";
        const label = `${langStr}${title && title !== "null" ? title : "Track " + tid}`.trim();
        if (type === "audio") a.push({ id: tid, label });
        else if (type === "sub") s.push({ id: tid, label });
      }
      if (!alive) return;
      const sig = JSON.stringify([a, s]);
      if (sig !== lastSig) {
        lastSig = sig;
        setAudioTracks(a);
        setSubTracks(s);
      }
      // Read the actual selected tracks every tick so the menus reflect mpv (incl. a
      // subtitle it auto-selected on load). setState with the same value is a no-op.
      setActiveAid((await mpv.mpvGet("aid")) || "");
      setActiveSid((await mpv.mpvGet("sid")) || "no");
    };
    tick();
    const id = window.setInterval(tick, 1000);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, [mpv, itemId, abs]);

  // Close the pop-up menus when the chrome auto-hides.
  useEffect(() => {
    if (chromeHidden) {
      setAudioMenu(false);
      setCapsMenu(false);
    }
  }, [chromeHidden]);

  const selectAudio = (id: string) => {
    mpv?.mpvSet("aid", id);
    setActiveAid(id);
    setAudioMenu(false);
  };
  const selectSub = (id: string) => {
    mpv?.mpvSet("sid", id);
    setActiveSid(id);
  };
  const applySubSize = (n: number) => {
    setSubSize(n);
    mpv?.mpvSet("sub-font-size", String(n));
  };
  const applySubColor = (c: string) => {
    setSubColor(c);
    mpv?.mpvSet("sub-color", c);
  };
  // Background box: mpv only draws one with a box border-style. With opacity 0 keep the
  // plain outline (readable, no box); above 0 switch to a background box of the chosen
  // color. mpv color is #AARRGGBB (alpha first; FF = opaque). In background-box mode
  // sub-shadow-offset acts as the box padding around the text — without it the text
  // touches the box edges; it must be reset in outline mode or it becomes a drop shadow.
  const applySubBg = (c: string, a: number) => {
    setSubBg(c);
    setSubBgAlpha(a);
    if (a > 0) {
      const aa = Math.round((a * 255) / 100)
        .toString(16)
        .padStart(2, "0");
      mpv?.mpvSet("sub-border-style", "background-box");
      mpv?.mpvSet("sub-back-color", `#${aa}${c.slice(1)}`);
      mpv?.mpvSet("sub-shadow-offset", "8"); // box padding (scaled px)
    } else {
      mpv?.mpvSet("sub-border-style", "outline-and-shadow");
      mpv?.mpvSet("sub-shadow-offset", "0");
    }
  };
  // Text outline (border): on/off via border size (0 = off), plus its colour. Live.
  const applySubOutline = (on: boolean, color: string) => {
    setSubOutline(on);
    setSubOutlineColor(color);
    mpv?.mpvSet("sub-border-size", on ? "3" : "0");
    mpv?.mpvSet("sub-border-color", color);
  };

  // Hand the canvas to a worker that paints frames with WebGL (off the main thread). Done
  // once — transferControlToOffscreen can only be called a single time per canvas. If it
  // fails, paintRef stays null and the pump falls back to main-thread putImageData.
  useEffect(() => {
    if (EMBED) return; // embed mode renders video natively (no canvas paint) — skip the worker
    const cv = canvasRef.current;
    if (!cv || paintRef.current) return;
    let worker: Worker | null = null;
    try {
      const offscreen = cv.transferControlToOffscreen();
      worker = new Worker(new URL("../video/paintWorker.ts", import.meta.url), { type: "module" });
      worker.postMessage({ canvas: offscreen }, [offscreen]);
      paintRef.current = worker;
    } catch {
      paintRef.current = null;
    }
    return () => {
      paintRef.current = null;
      worker?.terminate();
    };
  }, []);

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
    if (!skipLoad) mpv.mpvLoad(abs); // embed: main already loaded at click time to overlap decode

    // Embed (two-window): mpv renders the video natively into the child window — there's no
    // canvas to paint. Skip the whole per-frame pump (and its IPC); just poll the size
    // occasionally so the addon drains mpv events, which re-fits the surface to the window.
    if (EMBED) {
      const id = window.setInterval(() => {
        if (!cancelled) mpv.mpvSize();
      }, 700);
      return () => {
        cancelled = true;
        window.clearInterval(id);
        mpv.mpvStop();
      };
    }

    // 2D context only when the WebGL worker isn't available (it owns the canvas otherwise).
    const ctx = paintRef.current ? null : (canvasRef.current?.getContext("2d") ?? null);
    // Resolve the render size once and re-check only ~1×/s, NOT every frame. The per-frame
    // size query was a second IPC round-trip on top of the frame fetch; on Windows (slower
    // pipes) that doubled per-frame latency and is the main reason video felt low-fps.
    let rw = 0;
    let rh = 0;
    let lastSizeCheck = -1000;
    const dpr = window.devicePixelRatio || 1;
    const pump = async (ts: number) => {
      if (cancelled) return;
      if (ts - last >= FRAME_MS) {
        last = ts;
        try {
          if (rw === 0 || ts - lastSizeCheck > 1000) {
            lastSizeCheck = ts;
            const sz = await mpv.mpvSize();
            if (sz && sz.w > 0) {
              // Render at ~display width (preserving aspect) so mpv composites subtitles
              // at this size and the text stays crisp instead of being upscaled.
              const nw = Math.min(Math.round(window.screen.width * dpr), MAX_W);
              const nh = Math.max(1, Math.round((sz.h * nw) / sz.w));
              if (nw !== rw || nh !== rh) {
                rw = nw;
                rh = nh;
                // The worker sizes its OffscreenCanvas itself; only size here in 2D fallback.
                if (ctx && canvasRef.current) {
                  canvasRef.current.width = rw;
                  canvasRef.current.height = rh;
                }
              }
            }
          }
          if (rw > 0) {
            const buf = await mpv.mpvFrame(rw, rh);
            if (!cancelled && buf && buf.length === rw * rh * 4) {
              const worker = paintRef.current;
              if (worker) {
                // Hand the frame to the paint worker zero-copy (transfer its ArrayBuffer).
                const u8 = buf as Uint8Array;
                const ab =
                  u8.byteOffset === 0 && u8.byteLength === u8.buffer.byteLength
                    ? u8.buffer
                    : u8.slice().buffer;
                worker.postMessage({ buffer: ab, w: rw, h: rh }, [ab]);
              } else if (ctx) {
                ctx.putImageData(new ImageData(new Uint8ClampedArray(buf), rw, rh), 0, 0);
              }
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

  // Keyboard: Space pause/resume, ←/→ seek ∓10s, ↑/↓ volume (mirrors the video shortcuts).
  useEffect(() => {
    if (!mpv) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === " " || e.code === "Space") {
        e.preventDefault();
        mpv.mpvCmd(["cycle", "pause"]); // toggle pause/resume
      } else if (e.key === "ArrowRight") {
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
  const btn =
    "inline-flex items-center justify-center rounded p-1.5 text-white/85 transition hover:bg-white/15 hover:text-white";

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
            data-media
            ref={canvasRef}
            onClick={() => togglePlay()}
            className={`pointer-events-auto h-full w-full object-contain ${
              chromeHidden ? "cursor-none" : "cursor-pointer"
            }`}
          />
        </div>

        {!ready && !EMBED && (
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

        <button
          onClick={() => mpv?.mpvCmd(["seek", "-10", "relative"])}
          className={btn}
          aria-label="Back 10 seconds"
          title="Back 10s"
        >
          <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor">
            <path d="M11 6L5 12l6 6V6z" />
            <path d="M19 6l-6 6 6 6V6z" />
          </svg>
        </button>
        <button
          onClick={() => mpv?.mpvCmd(["seek", "10", "relative"])}
          className={btn}
          aria-label="Forward 10 seconds"
          title="Forward 10s"
        >
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
              className={`${btn} ${audioMenu ? "bg-white/20 text-white" : ""}`}
            >
              {/* Globe = language. */}
              <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                <circle cx="12" cy="12" r="9" />
                <line x1="3" y1="12" x2="21" y2="12" strokeLinecap="round" />
                <path d="M12 3c2.6 2.6 2.6 15.4 0 18M12 3c-2.6 2.6-2.6 15.4 0 18" strokeLinecap="round" />
              </svg>
            </button>
            {audioMenu && (
              <div className="absolute bottom-full right-0 mb-2 min-w-32 overflow-hidden rounded-lg bg-black/90 py-1 text-sm ring-1 ring-white/10">
                {audioTracks.map((tr) => (
                  <button
                    key={tr.id}
                    onClick={() => selectAudio(tr.id)}
                    className={`block w-full truncate px-3 py-1.5 text-left hover:bg-white/10 ${
                      activeAid === tr.id ? "text-white" : "text-white/70"
                    }`}
                  >
                    {tr.label}
                  </button>
                ))}
              </div>
            )}
          </div>
        )}

        {/* Shown only when subtitles were found. Sidecars in the video's folder now
            auto-load (any name), so this still appears for same-folder .srt files — but
            stays hidden when nothing was found. */}
        {subTracks.length > 0 && (
          <div className="relative">
            <button
              onClick={() => setCapsMenu((o) => !o)}
              aria-label="Subtitles"
              title="Subtitles"
              className={`${btn} ${activeSid !== "no" ? "bg-white/20 text-white" : ""}`}
            >
              <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                <rect x="3" y="5" width="18" height="14" rx="2" />
                <path d="M8 11h2M8 14h3M14 11h2M14 14h3" strokeLinecap="round" />
              </svg>
            </button>
            {capsMenu && (
              <div className="absolute bottom-full right-0 mb-2 w-64 overflow-hidden rounded-lg bg-black/90 text-sm ring-1 ring-white/10">
                {/* Tabs keep the menu compact — many subtitle tracks no longer push the
                    style controls off-screen. */}
                <div className="flex border-b border-white/10 text-xs">
                  {(["tracks", "style"] as const).map((tab) => (
                    <button
                      key={tab}
                      onClick={() => setCapsTab(tab)}
                      className={`flex-1 px-3 py-2 uppercase tracking-wide ${
                        capsTab === tab ? "bg-white/10 text-white" : "text-white/50 hover:text-white"
                      }`}
                    >
                      {tab === "tracks" ? "Subtitles" : "Style"}
                    </button>
                  ))}
                </div>

                {capsTab === "tracks" ? (
                  <div className="max-h-56 overflow-y-auto py-1">
                    <button
                      onClick={() => selectSub("no")}
                      className={`block w-full px-3 py-1.5 text-left hover:bg-white/10 ${
                        activeSid === "no" ? "text-white" : "text-white/70"
                      }`}
                    >
                      Off
                    </button>
                    {subTracks.map((tr) => (
                      <button
                        key={tr.id}
                        onClick={() => selectSub(tr.id)}
                        className={`block w-full truncate px-3 py-1.5 text-left hover:bg-white/10 ${
                          activeSid === tr.id ? "text-white" : "text-white/70"
                        }`}
                      >
                        {tr.label}
                      </button>
                    ))}
                  </div>
                ) : (
                  <div className="space-y-2 px-3 py-2 text-white/80">
                    <label className="flex items-center justify-between gap-2">
                      <span>Size</span>
                      <input
                        type="range"
                        min={20}
                        max={90}
                        value={subSize}
                        onChange={(e) => applySubSize(Number(e.target.value))}
                        className="w-28 accent-white"
                      />
                    </label>
                    <label className="flex items-center justify-between gap-2">
                      <span>Text color</span>
                      <input
                        type="color"
                        value={subColor}
                        onChange={(e) => applySubColor(e.target.value)}
                        className="h-6 w-10 cursor-pointer rounded bg-transparent"
                      />
                    </label>
                    <label className="flex items-center justify-between gap-2">
                      <span>Background</span>
                      <input
                        type="color"
                        value={subBg}
                        onChange={(e) => applySubBg(e.target.value, subBgAlpha)}
                        className="h-6 w-10 cursor-pointer rounded bg-transparent"
                      />
                    </label>
                    <label className="flex items-center justify-between gap-2">
                      <span>BG opacity</span>
                      <input
                        type="range"
                        min={0}
                        max={100}
                        value={subBgAlpha}
                        onChange={(e) => applySubBg(subBg, Number(e.target.value))}
                        className="w-28 accent-white"
                      />
                    </label>
                    <label className="flex items-center justify-between gap-2">
                      <span>Outline</span>
                      <span className="flex items-center gap-2">
                        <input
                          type="color"
                          value={subOutlineColor}
                          disabled={!subOutline}
                          onChange={(e) => applySubOutline(subOutline, e.target.value)}
                          className="h-6 w-10 cursor-pointer rounded bg-transparent disabled:opacity-40"
                        />
                        <input
                          type="checkbox"
                          checked={subOutline}
                          onChange={(e) => applySubOutline(e.target.checked, subOutlineColor)}
                          className="h-4 w-4 cursor-pointer accent-white"
                        />
                      </span>
                    </label>
                  </div>
                )}
              </div>
            )}
          </div>
        )}

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
