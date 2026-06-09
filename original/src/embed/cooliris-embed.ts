import type { MediaItem } from "@/feed/types";

/**
 * A modern, Flash-free shim that mirrors the surface of the original
 * `cooliris.embed.*` JavaScript API (cooliris/embed-wall) so existing
 * integration code keeps working. It delegates to the live React wall.
 */

export interface WallCallbacks {
  select?: (index: number, item: MediaItem | null) => void;
  deselect?: (index: number) => void;
  feedload?: (count: number) => void;
  feederror?: (message: string) => void;
}

/** The running wall exposes this to the shim. */
export interface WallController {
  setFeedURL(url: string): Promise<void>;
  getItems(): MediaItem[];
  selectItemByIndex(index: number): void;
  selectItemByGUID(guid: string): void;
  getSelectedItem(): MediaItem | null;
  deselect(): void;
}

class CoolirisEmbed {
  private controller: WallController | null = null;
  private pending: Array<(c: WallController) => void> = [];
  callbacks: WallCallbacks = {};

  /** Internal: the React app registers the live wall here. */
  _attach(controller: WallController): void {
    this.controller = controller;
    const queued = this.pending;
    this.pending = [];
    for (const fn of queued) fn(controller);
  }

  _detach(): void {
    this.controller = null;
  }

  private withController(fn: (c: WallController) => void): void {
    if (this.controller) fn(this.controller);
    else this.pending.push(fn);
  }

  /** Kept for API compatibility — the wall is created by the React app. */
  createWall(): void {
    /* no-op: the <WallView> component owns the canvas */
  }

  setFeedURL(url: string): void {
    this.withController((c) => void c.setFeedURL(url));
  }

  getItems(): MediaItem[] {
    return this.controller?.getItems() ?? [];
  }

  selectItemByIndex(index: number): void {
    this.withController((c) => c.selectItemByIndex(index));
  }

  selectItemByGUID(guid: string): void {
    this.withController((c) => c.selectItemByGUID(guid));
  }

  getSelectedItem(): MediaItem | null {
    return this.controller?.getSelectedItem() ?? null;
  }

  setCallbacks(callbacks: WallCallbacks): void {
    this.callbacks = { ...this.callbacks, ...callbacks };
  }
}

export const embed = new CoolirisEmbed();

declare global {
  interface Window {
    cooliris?: { embed: CoolirisEmbed };
  }
}

if (typeof window !== "undefined") {
  window.cooliris = { embed };
}
