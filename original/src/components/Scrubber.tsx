import { forwardRef, useImperativeHandle, useRef } from "react";
import type { ScrollInfo } from "@/wall/WallScene";

export interface ScrubberHandle {
  update(info: ScrollInfo): void;
}

// eslint-disable-next-line @typescript-eslint/no-empty-object-type
interface ScrubberProps {}

const MIN_THUMB = 0.06;

/**
 * Visual-only position bar pinned to the very bottom edge. Input is handled by
 * the WallScene's bottom "scrub zone" (so it works with any mouse button —
 * including right-drag — and never blocks the photos). Driven imperatively via
 * `update()` from the render loop, so it never re-renders while scrolling.
 */
export const Scrubber = forwardRef<ScrubberHandle, ScrubberProps>(
  function Scrubber(_props, ref) {
    const thumbRef = useRef<HTMLDivElement>(null);

    useImperativeHandle(ref, () => ({
      update(info) {
        const thumb = thumbRef.current;
        if (!thumb) return;
        const tw = Math.max(MIN_THUMB, info.thumb);
        thumb.style.width = `${tw * 100}%`;
        thumb.style.left = `${info.fraction * (1 - tw) * 100}%`;
      },
    }));

    return (
      <div className="pointer-events-none absolute bottom-0 left-0 right-0 z-[9999] px-4 pb-2">
        <div className="relative h-5 rounded-full bg-white/15 ring-1 ring-white/10">
          <div
            className="absolute inset-0 rounded-full opacity-50"
            style={{
              backgroundImage:
                "repeating-linear-gradient(90deg, rgba(255,255,255,0.5) 0 2px, transparent 2px 16px)",
            }}
          />
          <div
            ref={thumbRef}
            className="absolute top-1/2 h-5 -translate-y-1/2 rounded-full bg-white/90 shadow"
            style={{ width: "10%", left: "0%" }}
          />
        </div>
      </div>
    );
  }
);
