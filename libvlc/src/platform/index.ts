import type { Platform } from "./Platform";
import { webPlatform } from "./web";
import { electronPlatform } from "./electron";

export type { Platform } from "./Platform";

/**
 * Selects the active platform. Electron injects `window.electron` via its
 * preload bridge; otherwise we fall back to the browser implementation. The rest
 * of the app uses `platform.*` unchanged.
 */
function detectPlatform(): Platform {
  if (typeof window !== "undefined" && window.electron) return electronPlatform;
  // if ((window as any).Capacitor) return capacitorPlatform;
  return webPlatform;
}

export const platform: Platform = detectPlatform();
