import * as THREE from "three";
import type { MediaItem } from "@/feed/types";

/**
 * Self-contained wall-GIF animation. All GIF playback logic lives in this folder.
 *
 * To REMOVE the feature entirely: delete `src/gifsAnimation/`, drop the
 * `GifController` field + its call sites in WallScene (`setEnabled`, `update`,
 * `disposeTile`, `dispose`), and the Settings "Animate GIFs" toggle. Nothing else
 * depends on it — tiles don't store any GIF state (it's kept in a WeakMap here).
 */

const MAX_ACTIVE_GIFS = 48; // how many GIFs animate at once (nearest the view center)
const MAX_GIF_DECODES = 3; // concurrent GIF decodes
const GIF_FAST_VEL = 4; // don't START new decodes while scrolling faster than this
const GIF_BASE_FPS = 24; // wall GIF playback base framerate
// Perf knob: fps to SKIP (subtract) from the base on the WALL while browsing.
// 0 = 24 fps, 4 = 20 fps, 8 = 16 fps, 12 = 12 fps, ... (an opened GIF always
// plays full speed in its own overlay; this only affects wall playback).
const GIF_SKIP = 20;
const GIF_FPS_MS = 1000 / Math.max(1, GIF_BASE_FPS - GIF_SKIP);
const GIF_MAX_FRAMES = 240;
const GIF_FRAME_PX = 256; // decoded frame width

type GifMaterial = THREE.MeshBasicMaterial;

/** The minimal shape GifController needs from a wall tile. */
export interface GifTile {
  index: number;
  state: string;
  mesh: THREE.Mesh<THREE.PlaneGeometry, GifMaterial>;
  reflection?: THREE.Mesh<THREE.PlaneGeometry, GifMaterial>;
}

export interface GifUpdateCtx {
  focused: boolean;
  velocity: number;
  cullCenter: number;
  rows: number;
  cellW: number;
  now: number;
  items: MediaItem[];
  visible: GifTile[];
}

interface GifFrame {
  frame: CanvasImageSource;
  dur: number; // ms
  close: () => void;
}
interface GifAnim {
  canvas: HTMLCanvasElement;
  ctx: CanvasRenderingContext2D;
  tex: THREE.CanvasTexture;
  frames: GifFrame[];
  total: number;
  start: number;
  cur: number;
  ready: boolean;
  broken: boolean;
  staticMap: THREE.Texture | null; // the still texture, restored on revert
}

export class GifController {
  private enabled = false;
  private decodeActive = 0;
  private queue: Array<() => void> = [];
  private lastTick = 0;
  private anims = new WeakMap<GifTile, GifAnim>();

  /** `getGeneration` lets in-flight decodes bail when the feed changes. */
  constructor(private getGeneration: () => number) {}

  setEnabled(on: boolean): void {
    this.enabled = on;
  }

  /** Called every frame with the currently-visible tiles. */
  update(ctx: GifUpdateCtx): void {
    if (!this.enabled) {
      for (const t of ctx.visible) if (this.anims.has(t)) this.revert(t);
      return;
    }
    if (ctx.focused) return; // viewing one item — leave wall GIFs paused

    // Animate only the nearest few to the view center.
    const cands = ctx.visible.filter(
      (t) => ctx.items[t.index]?.animated && t.state === "loaded"
    );
    let active = cands;
    if (cands.length > MAX_ACTIVE_GIFS) {
      active = [...cands]
        .sort(
          (a, b) =>
            Math.abs(Math.floor(a.index / ctx.rows) * ctx.cellW - ctx.cullCenter) -
            Math.abs(Math.floor(b.index / ctx.rows) * ctx.cellW - ctx.cullCenter)
        )
        .slice(0, MAX_ACTIVE_GIFS);
    }
    const activeSet = new Set(active);
    // Advance GIF frames at the (base − skip) framerate.
    let advance = false;
    if (ctx.now - this.lastTick >= GIF_FPS_MS) {
      this.lastTick = ctx.now;
      advance = true;
    }
    const fast = Math.abs(ctx.velocity) > GIF_FAST_VEL;

    for (const t of ctx.visible) {
      if (activeSet.has(t)) {
        if (advance && (this.anims.has(t) || !fast)) this.animate(t, ctx.items[t.index].full);
      } else if (this.anims.has(t)) {
        this.revert(t);
      }
    }
  }

  /** Free a tile's GIF resources (call from the scene's tile destroy/clear). */
  disposeTile(tile: GifTile): void {
    const g = this.anims.get(tile);
    if (g) {
      this.disposeAnim(g);
      this.anims.delete(tile);
    }
  }

  dispose(): void {
    this.queue = [];
  }

  /* -------------------------------- internals -------------------------------- */

  private animate(tile: GifTile, url: string): void {
    let g = this.anims.get(tile);
    if (!g) {
      g = this.createAnim(url);
      this.anims.set(tile, g);
    }
    if (g.broken || !g.ready) return;
    const elapsed = (performance.now() - g.start) % g.total;
    let acc = 0;
    let idx = g.frames.length - 1;
    for (let i = 0; i < g.frames.length; i++) {
      acc += g.frames[i].dur;
      if (elapsed < acc) { idx = i; break; }
    }
    if (idx !== g.cur) {
      g.cur = idx;
      try {
        g.ctx.drawImage(g.frames[idx].frame, 0, 0, g.canvas.width, g.canvas.height);
        g.tex.needsUpdate = true;
      } catch {
        g.broken = true;
        return;
      }
    }
    const mat = tile.mesh.material;
    if (mat.map !== g.tex) {
      if (g.staticMap === null) g.staticMap = mat.map; // capture still to restore later
      mat.map = g.tex;
      mat.color.set(0xffffff);
      mat.needsUpdate = true;
      if (tile.reflection) {
        tile.reflection.material.map = g.tex;
        tile.reflection.material.needsUpdate = true;
      }
    }
  }

  private revert(tile: GifTile): void {
    const g = this.anims.get(tile);
    if (!g) return;
    const mat = tile.mesh.material;
    mat.map = g.staticMap;
    mat.needsUpdate = true;
    if (tile.reflection) {
      tile.reflection.material.map = g.staticMap;
      tile.reflection.material.needsUpdate = true;
    }
    this.disposeAnim(g);
    this.anims.delete(tile);
  }

  private createAnim(url: string): GifAnim {
    const canvas = document.createElement("canvas");
    canvas.width = 16;
    canvas.height = 16;
    const tex = new THREE.CanvasTexture(canvas);
    tex.colorSpace = THREE.SRGBColorSpace;
    const g: GifAnim = {
      canvas, ctx: canvas.getContext("2d")!, tex,
      frames: [], total: 0, start: performance.now(), cur: -1,
      ready: false, broken: false, staticMap: null,
    };
    this.runDecode(() => this.decode(url, g));
    return g;
  }

  /** Bounded-concurrency decode queue (decoding many GIFs at once freezes). */
  private runDecode(task: () => Promise<void>): void {
    const start = () => {
      this.decodeActive++;
      task().finally(() => {
        this.decodeActive--;
        const next = this.queue.shift();
        if (next) next();
      });
    };
    if (this.decodeActive < MAX_GIF_DECODES) start();
    else this.queue.push(start);
  }

  private async decode(url: string, g: GifAnim): Promise<void> {
    const Decoder = (window as unknown as { ImageDecoder?: any }).ImageDecoder;
    if (!Decoder) { g.broken = true; return; }
    const gen = this.getGeneration();
    try {
      const buf = await (await fetch(url)).arrayBuffer();
      if (gen !== this.getGeneration()) { g.broken = true; return; }
      const dec = new Decoder({ data: buf, type: "image/gif" });
      await dec.tracks.ready;
      const count: number = Math.min(dec.tracks.selectedTrack?.frameCount ?? 1, GIF_MAX_FRAMES);
      const frames: GifFrame[] = [];
      let total = 0;
      let sw = GIF_FRAME_PX;
      let sh = GIF_FRAME_PX;
      for (let i = 0; i < count; i++) {
        const { image } = await dec.decode({ frameIndex: i });
        const dur = (image.duration ?? 100000) / 1000 || 100; // µs → ms
        let bmp: ImageBitmap;
        try {
          bmp = await createImageBitmap(image, { resizeWidth: GIF_FRAME_PX, resizeQuality: "low" });
        } finally {
          image.close?.();
        }
        if (i === 0) { sw = bmp.width; sh = bmp.height; }
        frames.push({ frame: bmp, dur, close: () => bmp.close() });
        total += dur;
      }
      dec.close?.();
      if (gen !== this.getGeneration()) {
        for (const f of frames) f.close();
        g.broken = true;
        return;
      }
      if (!frames.length) { g.broken = true; return; }
      const w = Math.min(GIF_FRAME_PX, sw);
      g.canvas.width = Math.max(1, Math.round(w));
      g.canvas.height = Math.max(1, Math.round(w * (sh / sw)));
      g.frames = frames;
      g.total = total || frames.length * 100;
      g.start = performance.now();
      g.ready = true;
    } catch {
      g.broken = true;
    }
  }

  private disposeAnim(g: GifAnim): void {
    for (const f of g.frames) f.close();
    g.frames = [];
    g.tex.dispose();
  }
}
