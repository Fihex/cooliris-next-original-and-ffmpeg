import { useEffect, useRef, useState } from "react";
import { MpvPlayer } from "./MpvPlayer";

// The renderer for the two-window embed CHILD window. It's a transparent, frameless window
// stacked over the main wall window; mpv renders hardware-decoded video into it (under the
// web layer) with the controls overlaid. The main window tells it which file to play; Back
// or Esc closes it (the main window then shows the wall again).
export function VideoChildView() {
  const [abs, setAbs] = useState<string | null>(null);
  const stageRef = useRef<HTMLDivElement>(null);

  useEffect(() => window.electron?.onVideoPlay((p) => setAbs(p)), []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") window.electron?.closeVideo();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  if (!abs) return null;
  return (
    <div className="absolute inset-0">
      <MpvPlayer
        abs={abs}
        itemId={abs}
        t={{ s: 1, x: 0, y: 0 }}
        smooth={false}
        stageRef={stageRef}
        fullscreen
        chromeHidden={false}
        onFullscreen={() => {}}
      />
      <button
        onClick={() => window.electron?.closeVideo()}
        aria-label="Back"
        className="absolute left-4 top-4 z-50 rounded-full bg-black/50 px-3 py-1.5 text-sm text-white backdrop-blur transition hover:bg-black/70"
      >
        ‹ Back
      </button>
    </div>
  );
}
