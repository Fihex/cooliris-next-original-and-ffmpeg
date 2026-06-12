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
    </div>
  );
}
