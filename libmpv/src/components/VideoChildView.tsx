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

  const close = () => {
    setPlay(null); // unmount the player (stops mpv + its pump) before hiding the window
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
        key={play.nonce}
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
