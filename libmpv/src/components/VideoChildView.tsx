import { useEffect, useRef, useState } from "react";
import { MpvPlayer } from "./MpvPlayer";

// The renderer for the two-window embed CHILD window. It's a transparent, frameless window
// stacked over the main wall window; mpv renders hardware-decoded video into it (under the
// web layer) with the controls overlaid. The main window tells it which file to play; Back
// or Esc closes it (the main window then shows the wall again).
export function VideoChildView() {
  const [play, setPlay] = useState<{ abs: string; nonce: number } | null>(null);
  const [chromeHidden, setChromeHidden] = useState(false);
  const [fullscreen, setFullscreen] = useState(false);
  const stageRef = useRef<HTMLDivElement>(null);

  useEffect(
    () => window.electron?.onVideoPlay((p) => setPlay((prev) => ({ abs: p, nonce: (prev?.nonce ?? 0) + 1 }))),
    [],
  );

  // Decode-then-show: the window is hidden while mpv loads/decodes this video; once the
  // first frame is decoded (dwidth becomes known) reveal it — no blank window during
  // decode. ~2.4s fallback shows it anyway (audio-only / odd files).
  useEffect(() => {
    if (!play) return;
    let done = false;
    let tries = 0;
    const check = async () => {
      if (done) return;
      const w = await window.electron?.mpvGet("dwidth");
      if ((w && parseInt(w, 10) > 0) || ++tries >= 60) {
        done = true;
        window.electron?.videoReady();
        return;
      }
      window.setTimeout(check, 40);
    };
    check();
    return () => {
      done = true;
    };
  }, [play]);

  // Auto-hide the controls + cursor when the pointer is idle (and show them on any move).
  useEffect(() => {
    if (!play) return;
    let timer = 0;
    const wake = () => {
      setChromeHidden(false);
      window.clearTimeout(timer);
      timer = window.setTimeout(() => setChromeHidden(true), 2500);
    };
    wake();
    window.addEventListener("pointermove", wake);
    window.addEventListener("pointerdown", wake);
    window.addEventListener("keydown", wake);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("pointermove", wake);
      window.removeEventListener("pointerdown", wake);
      window.removeEventListener("keydown", wake);
    };
  }, [play]);

  // Zoom (wheel) + pan (drag) the NATIVE video via mpv's own video-zoom / video-pan-x/y
  // (a CSS transform wouldn't move the native surface). Reset on each new video.
  useEffect(() => {
    if (!play) return;
    const e = window.electron;
    const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));
    let zoom = 0;
    let panx = 0;
    let pany = 0;

    // pointermove/wheel fire 100+×/sec, and each mpvSet is an IPC round-trip
    // (renderer→main→forked host→mpv). Writing on every event backs that channel up so the
    // picture lags the cursor and only catches up when you stop. Coalesce to one write per
    // animation frame: the handlers just accumulate state (cheap JS), the rAF flushes the
    // latest value — so we never queue more than a single frame of work.
    let dirty = 0; // bit 1 = zoom, 2 = pan-x, 4 = pan-y
    let raf = 0;
    const flush = () => {
      raf = 0;
      const props: Record<string, string> = {};
      if (dirty & 1) props["video-zoom"] = zoom.toFixed(4);
      if (dirty & 2) props["video-pan-x"] = panx.toFixed(4);
      if (dirty & 4) props["video-pan-y"] = pany.toFixed(4);
      dirty = 0;
      e?.mpvSetFast(props); // one batched, non-awaited message per frame
    };
    const schedule = (bits: number) => {
      dirty |= bits;
      if (!raf) raf = requestAnimationFrame(flush);
    };
    e?.mpvSet("video-zoom", "0");
    e?.mpvSet("video-pan-x", "0");
    e?.mpvSet("video-pan-y", "0");

    const onWheel = (ev: WheelEvent) => {
      ev.preventDefault();
      zoom = clamp(zoom + (ev.deltaY < 0 ? 0.15 : -0.15), 0, 3);
      if (zoom === 0) {
        panx = pany = 0;
        schedule(1 | 2 | 4);
      } else {
        schedule(1);
      }
    };

    // Drag to pan (only when zoomed in). Swallow the click that follows a real drag so it
    // doesn't toggle pause.
    let dragging = false;
    let moved = false;
    let lx = 0;
    let ly = 0;
    const down = (ev: PointerEvent) => {
      if (zoom <= 0) return;
      dragging = true;
      moved = false;
      lx = ev.clientX;
      ly = ev.clientY;
    };
    const move = (ev: PointerEvent) => {
      if (!dragging) return;
      const dx = ev.clientX - lx;
      const dy = ev.clientY - ly;
      if (Math.abs(dx) > 2 || Math.abs(dy) > 2) moved = true;
      panx = clamp(panx + dx / window.innerWidth, -1.5, 1.5);
      pany = clamp(pany + dy / window.innerHeight, -1.5, 1.5);
      lx = ev.clientX;
      ly = ev.clientY;
      schedule(2 | 4);
    };
    const up = () => {
      if (dragging && moved) {
        const swallow = (ce: MouseEvent) => {
          ce.stopPropagation();
          ce.preventDefault();
          window.removeEventListener("click", swallow, true);
        };
        window.addEventListener("click", swallow, true);
      }
      dragging = false;
    };

    window.addEventListener("wheel", onWheel, { passive: false });
    window.addEventListener("pointerdown", down);
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    return () => {
      if (raf) cancelAnimationFrame(raf);
      window.removeEventListener("wheel", onWheel);
      window.removeEventListener("pointerdown", down);
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
  }, [play]);

  const close = () => {
    setPlay(null); // unmount the player → stops mpv + frees the file while browsing
    setFullscreen(false);
    window.electron?.closeVideo();
  };

  const toggleFullscreen = () => {
    window.electron?.winFullscreen(!fullscreen).then((on) => setFullscreen(!!on));
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (fullscreen) toggleFullscreen();
        else close();
      } else if (e.key === "f" || e.key === "F") {
        toggleFullscreen();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fullscreen]);

  if (!play) return null;
  return (
    <div className={`absolute inset-0 ${chromeHidden ? "cursor-none" : ""}`}>
      <MpvPlayer
        abs={play.abs}
        itemId={`${play.abs}#${play.nonce}`}
        t={{ s: 1, x: 0, y: 0 }}
        smooth={false}
        stageRef={stageRef}
        fullscreen={fullscreen}
        chromeHidden={chromeHidden}
        onFullscreen={toggleFullscreen}
        skipLoad
      />
      <button
        onClick={close}
        aria-label="Back"
        className={`absolute left-4 top-4 z-50 rounded-full bg-black/50 px-3 py-1.5 text-sm text-white backdrop-blur transition hover:bg-black/70 ${
          chromeHidden ? "pointer-events-none opacity-0" : "opacity-100"
        }`}
      >
        ‹ Back
      </button>
      {(["prev", "next"] as const).map((dir) => (
        <button
          key={dir}
          onClick={() => window.electron?.videoNav(dir)}
          aria-label={dir === "prev" ? "Previous video" : "Next video"}
          className={`absolute top-1/2 z-50 flex h-12 w-12 -translate-y-1/2 items-center justify-center rounded-full bg-black/40 text-white backdrop-blur transition hover:bg-black/60 ${
            dir === "prev" ? "left-3" : "right-3"
          } ${chromeHidden ? "pointer-events-none opacity-0" : "opacity-100"}`}
        >
          <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
            <path d={dir === "prev" ? "M15 18l-6-6 6-6" : "M9 18l6-6-6-6"} />
          </svg>
        </button>
      ))}
    </div>
  );
}
