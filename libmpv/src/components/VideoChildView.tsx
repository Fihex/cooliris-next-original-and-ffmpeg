import { useEffect, useRef, useState } from "react";
import { MpvPlayer } from "./MpvPlayer";

// The renderer for the two-window embed CHILD window. It's a transparent, frameless window
// stacked over the main wall window; mpv renders hardware-decoded video into it (under the
// web layer) with the controls overlaid. The main window tells it which file to play; Back
// or Esc closes it (the main window then shows the wall again).
export function VideoChildView() {
  // `nonce` bumps on every play request so the player REMOUNTS each time (fresh mpvLoad) —
  // even when reopening the same file — fixing "video doesn't launch the second time".
  const [play, setPlay] = useState<{ abs: string; nonce: number } | null>(null);
  const stageRef = useRef<HTMLDivElement>(null);

  useEffect(
    () => window.electron?.onVideoPlay((p) => setPlay((prev) => ({ abs: p, nonce: (prev?.nonce ?? 0) + 1 }))),
    [],
  );

  // Decode-then-show: the window is hidden while mpv loads/decodes this video; once the
  // first frame is decoded (dwidth becomes known), reveal the window — so there's no blank
  // window during decode. A ~2.5s fallback shows it anyway (audio-only / odd files).
  useEffect(() => {
    if (!play) return;
    let done = false;
    let tries = 0;
    const check = async () => {
      if (done) return;
      const w = await window.electron?.mpvGet("dwidth");
      if ((w && parseInt(w, 10) > 0) || ++tries >= 30) {
        done = true;
        window.electron?.videoReady();
        return;
      }
      window.setTimeout(check, 80);
    };
    check();
    return () => {
      done = true;
    };
  }, [play]);

  const close = () => {
    setPlay(null); // unmount the player → stops mpv + frees the file while browsing
    window.electron?.closeVideo();
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  if (!play) return null;
  return (
    <div className="absolute inset-0">
      <MpvPlayer
        abs={play.abs}
        itemId={`${play.abs}#${play.nonce}`}
        t={{ s: 1, x: 0, y: 0 }}
        smooth={false}
        stageRef={stageRef}
        fullscreen
        chromeHidden={false}
        onFullscreen={() => {}}
      />
      <button
        onClick={close}
        aria-label="Back"
        className="absolute left-4 top-4 z-50 rounded-full bg-black/50 px-3 py-1.5 text-sm text-white backdrop-blur transition hover:bg-black/70"
      >
        ‹ Back
      </button>
    </div>
  );
}
