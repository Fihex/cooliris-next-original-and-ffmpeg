import { useEffect, useRef, useState } from "react";

/**
 * libVLC-backed video player (Option C). Instead of a Chromium <video>, it pulls
 * decoded RGBA frames from the native libVLC player (main process) and paints them into a
 * <canvas>. VLC handles every format + composites subtitles, so there's no transcoding.
 *
 * Frames are pulled on a ~30fps gate and capped in resolution to keep the per-frame IPC
 * affordable. Playback is driven through mpv-style properties (shimmed in the addon)/commands over the bridge.
 */

const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));
const FRAME_MS = 33; // ~30fps

interface Transform {
  s: number;
  x: number;
  y: number;
}

interface VlcPlayerProps {
  abs: string;
  itemId: string;
  t: Transform;
  smooth: boolean;
  stageRef: React.RefObject<HTMLDivElement>;
  fullscreen: boolean;
  chromeHidden: boolean;
  onFullscreen: () => void;
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

export function VlcPlayer({
  abs,
  itemId,
  t,
  smooth,
  stageRef,
  fullscreen,
  chromeHidden,
  onFullscreen,
  onPlayingChange,
}: VlcPlayerProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
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
  // Subtitle style. libVLC 3 only takes these at player creation, so applying a change
  // recreates the player (host restores file/position/tracks — a brief reload).
  const [subSize, setSubSize] = useState(48);
  const [subColor, setSubColor] = useState("#ffffff");
  const [subBg, setSubBg] = useState("#000000");
  const [subBgAlpha, setSubBgAlpha] = useState(0); // 0 = transparent … 100 = opaque
  const styleTimer = useRef<number>(0);
  const shownRef = useRef(false);
  const seeking = useRef(false);
  const trackRef = useRef<HTMLDivElement>(null);

  const vlc = typeof window !== "undefined" ? window.electron : undefined;

  // Once the file is loaded, read vlc's track list (audio + subtitle) for the choosers.
  useEffect(() => {
    if (!vlc) return;
    let alive = true;
    setAudioTracks([]);
    setSubTracks([]);
    setActiveSid("no");
    let done = false;
    const tick = async () => {
      if (!alive || done) return;
      const count = parseInt((await vlc.vlcGet("track-list/count")) || "0", 10);
      if (count > 0) {
        done = true;
        const a: { id: string; label: string }[] = [];
        const s: { id: string; label: string }[] = [];
        for (let i = 0; i < count; i++) {
          const type = await vlc.vlcGet(`track-list/${i}/type`);
          const tid = (await vlc.vlcGet(`track-list/${i}/id`)) || "";
          const lang = await vlc.vlcGet(`track-list/${i}/lang`);
          const title = await vlc.vlcGet(`track-list/${i}/title`);
          const langStr = lang && lang !== "null" ? `${lang} ` : "";
          const label = `${langStr}${title && title !== "null" ? title : "Track " + tid}`.trim();
          if (type === "audio") a.push({ id: tid, label });
          else if (type === "sub") s.push({ id: tid, label });
        }
        if (alive) {
          setAudioTracks(a);
          setSubTracks(s);
          setActiveAid((await vlc.vlcGet("aid")) || "");
          // VLC auto-enables a subtitle track on load — keep subs off until chosen.
          vlc.vlcSet("sid", "no");
        }
      }
    };
    const id = window.setInterval(tick, 300);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, [vlc, itemId, abs]);

  // Close the pop-up menus when the chrome auto-hides.
  useEffect(() => {
    if (chromeHidden) {
      setAudioMenu(false);
      setCapsMenu(false);
    }
  }, [chromeHidden]);

  const selectAudio = (id: string) => {
    vlc?.vlcSet("aid", id);
    setActiveAid(id);
    setAudioMenu(false);
  };
  const selectSub = (id: string) => {
    vlc?.vlcSet("sid", id);
    setActiveSid(id);
  };

  // Debounced style apply: sliders fire many events, and each apply means a player
  // recreate + reload — batch them ~600ms after the last change.
  const applyStyle = (size: number, color: string, bg: string, bgAlpha: number) => {
    setSubSize(size);
    setSubColor(color);
    setSubBg(bg);
    setSubBgAlpha(bgAlpha);
    window.clearTimeout(styleTimer.current);
    styleTimer.current = window.setTimeout(() => {
      vlc?.vlcStyle([
        `--freetype-fontsize=${size}`,
        `--freetype-color=${parseInt(color.slice(1), 16)}`,
        `--freetype-background-color=${parseInt(bg.slice(1), 16)}`,
        `--freetype-background-opacity=${Math.round(bgAlpha * 2.55)}`,
      ]);
    }, 600);
  };

  // Load the file and pump frames into the canvas while this item is shown.
  useEffect(() => {
    if (!vlc) return;
    let cancelled = false;
    let raf = 0;
    let last = 0;
    shownRef.current = false;
    setReady(false);
    setCur(0);
    setDur(0);
    setPlaying(true);
    // Ask VLC to render (video + subtitles) at ~display resolution, not the file's —
    // this is what keeps subtitle text crisp when the video is smaller than the screen.
    const dpr = window.devicePixelRatio || 1;
    const targetW = Math.min(Math.round(window.screen.width * dpr), 1920);
    vlc.vlcSet("render-width", String(targetW));
    vlc.vlcLoad(abs);

    const ctx = canvasRef.current?.getContext("2d") ?? null;
    const pump = async (ts: number) => {
      if (cancelled) return;
      if (ts - last >= FRAME_MS) {
        last = ts;
        try {
          const sz = await vlc.vlcSize();
          if (sz && sz.w > 0 && ctx && canvasRef.current) {
            // The addon reports the exact buffer size it renders at — request that.
            const rw = sz.w;
            const rh = sz.h;
            if (canvasRef.current.width !== rw || canvasRef.current.height !== rh) {
              canvasRef.current.width = rw;
              canvasRef.current.height = rh;
            }
            const buf = await vlc.vlcFrame(rw, rh);
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
      vlc.vlcStop();
    };
  }, [abs, itemId, vlc]);

  // Poll playback state for the control bar.
  useEffect(() => {
    if (!vlc) return;
    let alive = true;
    const id = window.setInterval(async () => {
      if (!alive) return;
      const [tp, d, p, vv] = await Promise.all([
        vlc.vlcGet("time-pos"),
        vlc.vlcGet("duration"),
        vlc.vlcGet("pause"),
        vlc.vlcGet("volume"),
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
  }, [vlc, itemId, onPlayingChange]);

  const togglePlay = () => vlc?.vlcCmd(["cycle", "pause"]);
  const seekTo = (clientX: number) => {
    const el = trackRef.current;
    if (!el || !dur) return;
    const r = el.getBoundingClientRect();
    const frac = clamp((clientX - r.left) / r.width, 0, 1);
    setCur(frac * dur);
    vlc?.vlcCmd(["seek", String(frac * dur), "absolute"]);
  };
  const setVolume = (value: number) => {
    setVol(value);
    vlc?.vlcSet("volume", String(value));
  };

  // Keyboard: ←/→ seek ∓10s, ↑/↓ volume (mirrors the video shortcuts).
  useEffect(() => {
    if (!vlc) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "ArrowRight") {
        e.preventDefault();
        vlc.vlcCmd(["seek", "10", "relative"]);
      } else if (e.key === "ArrowLeft") {
        e.preventDefault();
        vlc.vlcCmd(["seek", "-10", "relative"]);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setVolume(clamp(vol + 10, 0, 100));
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        setVolume(clamp(vol - 10, 0, 100));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [vlc, vol]);

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
            data-media
            ref={canvasRef}
            onClick={() => togglePlay()}
            className={`pointer-events-auto h-full w-full object-contain ${
              chromeHidden ? "cursor-none" : "cursor-pointer"
            }`}
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
          max={100}
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
          onClick={() => vlc?.vlcCmd(["seek", "-10", "relative"])}
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
          onClick={() => vlc?.vlcCmd(["seek", "10", "relative"])}
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
                        max={100}
                        value={subSize}
                        onChange={(e) => applyStyle(Number(e.target.value), subColor, subBg, subBgAlpha)}
                        className="w-28 accent-white"
                      />
                    </label>
                    <label className="flex items-center justify-between gap-2">
                      <span>Text color</span>
                      <input
                        type="color"
                        value={subColor}
                        onChange={(e) => applyStyle(subSize, e.target.value, subBg, subBgAlpha)}
                        className="h-6 w-10 cursor-pointer rounded bg-transparent"
                      />
                    </label>
                    <label className="flex items-center justify-between gap-2">
                      <span>Background</span>
                      <input
                        type="color"
                        value={subBg}
                        onChange={(e) => applyStyle(subSize, subColor, e.target.value, subBgAlpha)}
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
                        onChange={(e) => applyStyle(subSize, subColor, subBg, Number(e.target.value))}
                        className="w-28 accent-white"
                      />
                    </label>
                    <div className="pt-1 text-[11px] leading-snug text-white/40">
                      Applies with a quick reload (VLC sets style at startup).
                    </div>
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
