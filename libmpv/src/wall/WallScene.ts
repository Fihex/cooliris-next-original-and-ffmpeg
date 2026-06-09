import * as THREE from "three";
import type { MediaItem } from "@/feed/types";
import { GifController, type GifTile } from "@/gifsAnimation/GifController";

/* Layout — a 3-row wall of aspect-preserving tiles that scrolls horizontally. */
const ROWS = 3;
const ROW_H = 1.0;
const MAX_W = 1.55;
const GAP_X = 0.16;
const GAP_Y = 0.16;
const CELL_W = MAX_W + GAP_X;
const CELL_H = ROW_H + GAP_Y;
const DEFAULT_ASPECT = 1.4;
const REFLECT_GAP = 0.09; // gap between a photo and its reflection
const SCRUB_ZONE_PX = 44; // bottom band that acts as the scrubber
const SCRUB_PAD_PX = 16; // horizontal inset of the visual track (matches Scrubber px-4)

/* Camera / motion tuning. */
const BASE_DIST = 7.2;
const MIN_DIST = 4.5;
const MAX_DIST = 13;
const FOCUS_DIST = 5.6;
const ACCEL = 22; // world units / s^2 while an arrow is held
const MAX_SPEED = 11; // world units / s
const BANK_MAX = 0.5; // radians the wall banks while scrolling
const BANK_GAIN = 0.22; // sqrt(|velocity|) -> bank radians; banks on slow scroll too
const PAN_Y_MAX = 1.7; // vertical free-pan limit
const DRAG_GAIN = 0.6; // click-drag scroll sensitivity (lower = gentler)
const KEEP_BUFFER_COLS = 10; // columns kept resident (mesh+texture) beyond the view
const MAX_INFLIGHT = 6; // cap concurrent texture decodes (a jump can't freeze the tab)
const MAX_NEW_LOADS_PER_FRAME = 2; // spread GPU uploads across frames (smoother scroll)

type WallEvent = "select" | "deselect" | "hover" | "scroll" | "progress";
type Cb = (...args: any[]) => void;

interface Tile {
  mesh: THREE.Mesh<THREE.PlaneGeometry, THREE.MeshBasicMaterial>;
  reflection?: THREE.Mesh<THREE.PlaneGeometry, THREE.MeshBasicMaterial>;
  index: number;
  baseX: number;
  baseY: number; // tile center (depends on height; bottom sits on `baseline`)
  baseline: number; // shared bottom line for the row
  w: number;
  h: number;
  state: "idle" | "loading" | "loaded" | "error";
  loadOpacity: number;
  scale: number;
  phase: number;
  fullLoaded: boolean;
  label?: THREE.Sprite; // title label (when "show titles" is on)
}


export interface ScrollInfo {
  fraction: number;
  thumb: number;
  atStart: boolean;
  atEnd: boolean;
}

export class WallScene {
  private container: HTMLElement;
  private renderer: THREE.WebGLRenderer;
  private scene: THREE.Scene;
  private camera: THREE.PerspectiveCamera;
  private wallGroup: THREE.Group;
  private strip: THREE.Group;
  private reflGradient!: THREE.Texture;
  private lastEmitVpW = -1;
  private lastEmitScrollX = Number.NaN;
  private visStart = 0; // currently-processed (visible) tile index range
  private visEnd = -1;
  private keepStart = 0; // textures resident outside this range are evicted
  private keepEnd = -1;
  private raycaster = new THREE.Raycaster();
  private pointer = new THREE.Vector2();
  private geo = new THREE.PlaneGeometry(1, 1);

  private tiles: (Tile | undefined)[] = []; // sparse: meshes created lazily on view
  private items: MediaItem[] = [];
  private loader = new THREE.TextureLoader();

  private scrollX = 0;
  private velocity = 0; // world units / s
  private inputDir = 0; // -1 / 0 / 1 from held arrows
  private scrollMin = 0; // scrollX is the view-center in content coords
  private scrollMax = 0;
  private contentWidth = 0;
  private viewportWidth = 8;
  private scrollTargetX: number | null = null; // slider-driven eased target
  private bank = 0;
  private camDist = BASE_DIST;
  private camDistTarget = BASE_DIST;
  private panY = 0; // vertical free-pan offset
  private lastVpW = 0;
  private lastVpH = 0;

  private dragging = false;
  private dragButton = 0;
  private panning = false; // middle-mouse free pan
  private scrubbing = false; // dragging within the bottom scrub zone
  private lastPointerX = 0;
  private lastPointerY = 0;
  private lastMoveT = 0;
  private pointerMoved = false;
  private pointers = new Map<number, { x: number; y: number }>();
  private pinching = false;
  private pinchDist = 0;

  private selectedIndex = -1;
  private hoverIndex = -1;
  private focus = 0;
  private focusTarget = 0;
  private camX = 0; // eased camera x (pans to the focused photo)
  private camY = 0; // eased camera y (vertical pan / focus)

  private slideshow = false;
  private slideshowTimer = 0;
  private showTitles = false;
  // GIF wall-animation lives entirely in src/gifsAnimation.
  private gifs = new GifController(() => this.generation);

  // Web Worker pool: off-main-thread decode + downscale of local images.
  private workers: Worker[] = [];
  private workerNext = 0;
  private workerJobId = 0;
  private workerJobs = new Map<number, { resolve: (b: ImageBitmap) => void; reject: (e: unknown) => void }>();

  private loadedCount = 0;
  private pendingCount = 0;
  private generation = 0; // bumped on setItems; stale async work is discarded

  private listeners: Record<WallEvent, Cb[]> = {
    select: [], deselect: [], hover: [], scroll: [], progress: [],
  };

  private running = true;
  private resizeObserver: ResizeObserver;
  private lastTick = 0;

  constructor(container: HTMLElement) {
    this.container = container;

    this.renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
    this.renderer.setSize(container.clientWidth, container.clientHeight);
    container.appendChild(this.renderer.domElement);

    this.scene = new THREE.Scene();
    this.scene.fog = new THREE.Fog(0x05070c, 10, 34);

    this.camera = new THREE.PerspectiveCamera(
      45,
      (container.clientWidth || 1) / (container.clientHeight || 1),
      0.1,
      100
    );
    this.camera.position.set(0, 0.3, BASE_DIST);
    this.camera.lookAt(0, 0, 0);

    this.wallGroup = new THREE.Group();
    this.scene.add(this.wallGroup);
    this.strip = new THREE.Group();
    this.wallGroup.add(this.strip);

    this.reflGradient = this.makeReflectionAlpha();
    this.computeViewport();

    const el = this.renderer.domElement;
    el.style.touchAction = "none";
    el.style.cursor = "grab";
    el.addEventListener("wheel", this.onWheel, { passive: false });
    el.addEventListener("pointerdown", this.onPointerDown);
    el.addEventListener("pointermove", this.onPointerMove);
    el.addEventListener("pointerup", this.onPointerUp);
    el.addEventListener("pointercancel", this.onPointerUp);
    el.addEventListener("pointerleave", this.onPointerLeave);
    el.addEventListener("contextmenu", this.onContextMenu);
    el.addEventListener("mousedown", this.onMouseDown);

    this.resizeObserver = new ResizeObserver(() => this.onResize());
    this.resizeObserver.observe(container);

    this.animate();
  }

  /* -------------------------------- public API -------------------------------- */

  setItems(items: MediaItem[]): void {
    this.generation++; // invalidate any in-flight decodes from the previous feed
    this.clearTiles();
    this.items = items;
    this.loadedCount = 0;
    this.pendingCount = 0;
    // No meshes are created up front — they're built lazily as tiles scroll into
    // view and destroyed when they leave, so 18 or 160,000 files cost the same.
    this.tiles = new Array(items.length);

    this.recomputeBounds();

    this.visStart = 0;
    this.visEnd = -1;
    this.keepStart = 0;
    this.keepEnd = -1;
    this.scrollX = this.scrollMin;
    this.scrollTargetX = null;
    this.velocity = 0;
    this.inputDir = 0;
    this.bank = 0;
    this.panY = 0;
    this.wallGroup.rotation.y = 0;
    this.selectedIndex = -1;
    this.focus = 0;
    this.focusTarget = 0;
    this.emitProgress();
    this.emitScroll();
  }

  on(event: WallEvent, cb: Cb): void {
    this.listeners[event].push(cb);
  }

  selectIndex(index: number): void {
    if (index < 0 || index >= this.tiles.length) return;
    this.selectedIndex = index;
    this.focusTarget = 1;
    // The wall stays put; the camera pans to this photo (zoom centers on it).
    this.velocity = 0;
    this.inputDir = 0;
    this.scrollTargetX = null;
    this.loadFull(this.ensureTile(index));
    this.emit("select", index, this.items[index] ?? null);
  }

  deselect(): void {
    if (this.selectedIndex === -1) return;
    // Leave the wall centered on the item you were last viewing (not where you
    // opened from), then reveal it.
    const baseX = Math.floor(this.selectedIndex / ROWS) * CELL_W;
    this.scrollX = clamp(baseX, this.scrollMin, this.scrollMax);
    this.camX = 0;
    this.velocity = 0;
    this.selectedIndex = -1;
    this.focusTarget = 0;
    this.emit("deselect", -1);
  }

  next(): void {
    if (this.selectedIndex < 0) return;
    this.selectIndex((this.selectedIndex + 1) % this.tiles.length);
  }

  prev(): void {
    if (this.selectedIndex < 0) return;
    this.selectIndex((this.selectedIndex - 1 + this.tiles.length) % this.tiles.length);
  }

  setSlideshow(on: boolean): void {
    this.slideshow = on;
    this.slideshowTimer = 0;
    if (on && this.selectedIndex < 0 && this.tiles.length) this.selectIndex(0);
  }

  /** Toggle always-on title labels on every tile (handy for videos). */
  setShowTitles(on: boolean): void {
    this.showTitles = on;
    if (!on) {
      for (const tile of this.tiles) if (tile?.label) tile.label.visible = false;
    }
  }

  /** Toggle live GIF playback on the wall (handled by the GifController). */
  setGifAnim(on: boolean): void {
    this.gifs.setEnabled(on);
  }

  /**
   * Build a small billboard label sprite, capped to `maxWorldW` so it never
   * overruns its tile / neighbours (text is truncated with an ellipsis).
   */
  private makeLabel(text: string, maxWorldW: number): THREE.Sprite {
    const LABEL_H = 0.12; // world-unit height
    const fontSize = 32;
    const padX = 16;
    const canvasH = fontSize + 18;
    const maxCanvasW = Math.max(60, (maxWorldW / LABEL_H) * canvasH);
    const font = `600 ${fontSize}px Inter, system-ui, sans-serif`;

    const m = document.createElement("canvas").getContext("2d")!;
    m.font = font;
    const avail = maxCanvasW - padX * 2;
    let label = text;
    if (m.measureText(label).width > avail) {
      while (label.length > 1 && m.measureText(label + "…").width > avail) {
        label = label.slice(0, -1);
      }
      label += "…";
    }
    const textW = m.measureText(label).width;

    const canvas = document.createElement("canvas");
    canvas.width = Math.ceil(Math.min(maxCanvasW, textW + padX * 2));
    canvas.height = canvasH;
    const ctx = canvas.getContext("2d")!;
    ctx.font = font;
    ctx.fillStyle = "rgba(0,0,0,0.72)";
    const w = canvas.width;
    const h = canvas.height;
    ctx.beginPath();
    if (ctx.roundRect) ctx.roundRect(0, 0, w, h, 12);
    else ctx.rect(0, 0, w, h);
    ctx.fill();
    ctx.fillStyle = "#fff";
    ctx.textBaseline = "middle";
    ctx.textAlign = "center";
    ctx.fillText(label, w / 2, h / 2 + 1);

    const tex = new THREE.CanvasTexture(canvas);
    tex.colorSpace = THREE.SRGBColorSpace;
    const mat = new THREE.SpriteMaterial({ map: tex, transparent: true, depthTest: false, fog: false });
    const sprite = new THREE.Sprite(mat);
    sprite.renderOrder = 5;
    sprite.scale.set(LABEL_H * (w / h), LABEL_H, 1);
    return sprite;
  }

  /** Begin/stop continuous scrolling in a direction (arrow keys / edge buttons). */
  startScroll(dir: number): void {
    if (this.selectedIndex >= 0) return;
    this.inputDir = Math.sign(dir);
    this.scrollTargetX = null;
  }
  stopScroll(): void {
    this.inputDir = 0;
  }

  zoomBy(delta: number): void {
    this.camDistTarget = clamp(this.camDistTarget + delta, MIN_DIST, MAX_DIST);
  }

  scrollToFraction(f: number): void {
    if (this.selectedIndex >= 0) return;
    // Set an eased target instead of snapping, so dragging the slider animates
    // the wall (with the directional lean) just like a drag.
    this.scrollTargetX = this.scrollMin + clamp(f, 0, 1) * (this.scrollMax - this.scrollMin);
  }

  getScrollInfo(): ScrollInfo {
    const range = this.scrollMax - this.scrollMin;
    const fraction = range > 1e-6 ? (this.scrollX - this.scrollMin) / range : 0;
    const thumb = this.contentWidth > 0 ? Math.min(1, this.viewportWidth / this.contentWidth) : 1;
    return {
      fraction: clamp(fraction, 0, 1),
      thumb,
      atStart: range < 1e-6 || this.scrollX <= this.scrollMin + 0.01,
      atEnd: range < 1e-6 || this.scrollX >= this.scrollMax - 0.01,
    };
  }

  get selected(): number {
    return this.selectedIndex;
  }

  dispose(): void {
    this.running = false;
    this.resizeObserver.disconnect();
    const el = this.renderer.domElement;
    el.removeEventListener("wheel", this.onWheel);
    el.removeEventListener("pointerdown", this.onPointerDown);
    el.removeEventListener("pointermove", this.onPointerMove);
    el.removeEventListener("pointerup", this.onPointerUp);
    el.removeEventListener("pointercancel", this.onPointerUp);
    el.removeEventListener("pointerleave", this.onPointerLeave);
    el.removeEventListener("contextmenu", this.onContextMenu);
    el.removeEventListener("mousedown", this.onMouseDown);
    this.gifs.dispose();
    this.clearTiles();
    this.geo.dispose();
    this.reflGradient.dispose();
    for (const w of this.workers) w.terminate();
    this.workers = [];
    this.workerJobs.clear();
    this.renderer.dispose();
    el.remove();
  }

  /* -------------------------------- internals -------------------------------- */

  private emit(event: WallEvent, ...args: any[]): void {
    for (const cb of this.listeners[event]) cb(...args);
  }
  private emitProgress(): void {
    this.emit("progress", this.loadedCount, this.items.length, this.pendingCount);
  }
  private emitScroll(): void {
    this.emit("scroll", this.getScrollInfo());
  }

  /**
   * Grayscale alpha gradient for the mirrored reflection under each photo:
   * fully opaque touching the photo, fading to nothing by ~half the height.
   * (Used as an alphaMap; green channel = alpha.)
   */
  private makeReflectionAlpha(): THREE.CanvasTexture {
    const canvas = document.createElement("canvas");
    canvas.width = 4;
    canvas.height = 256;
    const ctx = canvas.getContext("2d")!;
    const g = ctx.createLinearGradient(0, 0, 0, 256);
    // Default texture flipY=true → canvas bottom (y=256) maps to UV v=0,
    // which (after the mesh's negative Y scale) is the edge touching the photo.
    g.addColorStop(0.0, "#000"); // far end of reflection — invisible
    g.addColorStop(0.5, "#000");
    g.addColorStop(0.72, "#777");
    g.addColorStop(1.0, "#fff"); // touching the photo — fully visible
    ctx.fillStyle = g;
    ctx.fillRect(0, 0, 4, 256);
    return new THREE.CanvasTexture(canvas);
  }

  /** Get the tile for an item index, creating its mesh on first view. */
  private ensureTile(index: number): Tile {
    const existing = this.tiles[index];
    if (existing) return existing;
    const item = this.items[index];
    const col = Math.floor(index / ROWS);
    const row = index % ROWS;
    const baseX = col * CELL_W;
    // Photos bottom-align to a shared row baseline (like a shelf) so photos of
    // different heights — and their reflections — line up consistently.
    const baseline = ((ROWS - 1) / 2 - row) * CELL_H - ROW_H / 2;
    const h = ROW_H;
    const w = Math.min(MAX_W, ROW_H * DEFAULT_ASPECT);
    const baseY = baseline + h / 2;

    const material = new THREE.MeshBasicMaterial({
      color: 0x1a1d24, transparent: true, opacity: 1,
    });
    const mesh = new THREE.Mesh(this.geo, material);
    mesh.position.set(baseX, baseY, 0);
    mesh.scale.set(w, h, 1);
    mesh.userData.index = index;
    // Off-screen by default; only the visible window is shown/updated each frame
    // (so 16k+ tiles don't cost anything until they scroll into view).
    mesh.visible = false;
    mesh.matrixAutoUpdate = false;
    mesh.updateMatrix();
    this.strip.add(mesh);

    const tile: Tile = {
      mesh, index, baseX, baseY, baseline, w, h,
      state: item.thumb ? "idle" : "error",
      loadOpacity: 1, scale: 1, phase: Math.random() * Math.PI * 2, fullLoaded: false,
    };

    // Mirrored reflection under the bottom row (the wall "sits" on glass).
    if (row === ROWS - 1) {
      const rMat = new THREE.MeshBasicMaterial({
        color: 0xffffff,
        transparent: true,
        depthWrite: false,
        side: THREE.DoubleSide,
        opacity: 0,
        alphaMap: this.reflGradient,
      });
      const rMesh = new THREE.Mesh(this.geo, rMat);
      rMesh.position.set(baseX, baseline - REFLECT_GAP - h / 2, 0);
      rMesh.scale.set(w, -h, 1);
      rMesh.visible = false;
      rMesh.matrixAutoUpdate = false;
      rMesh.updateMatrix();
      this.strip.add(rMesh);
      tile.reflection = rMesh;
    }

    this.tiles[index] = tile;
    return tile;
  }

  private applyTexture(tile: Tile, texture: THREE.Texture): void {
    texture.colorSpace = THREE.SRGBColorSpace;
    texture.minFilter = THREE.LinearMipmapLinearFilter;
    texture.generateMipmaps = true;
    const img = texture.image as { width: number; height: number };
    const aspect = img.width && img.height ? img.width / img.height : DEFAULT_ASPECT;
    let w = ROW_H * aspect;
    let h = ROW_H;
    if (w > MAX_W) { w = MAX_W; h = MAX_W / aspect; }
    tile.w = w;
    tile.h = h;
    tile.baseY = tile.baseline + h / 2; // keep the photo's bottom on the row baseline
    const mat = tile.mesh.material;
    mat.map?.dispose();
    mat.map = texture;
    mat.color.set(0xffffff);
    mat.needsUpdate = true;

    if (tile.reflection) {
      const rm = tile.reflection.material;
      rm.map = texture; // shares the photo texture
      rm.color.set(0xffffff);
      rm.needsUpdate = true;
    }
  }

  private loadTile(tile: Tile): void {
    const item = this.items[tile.index];
    if (!item || !item.thumb) {
      tile.state = "error";
      tile.mesh.material.color.set(0x2a1518);
      return;
    }
    tile.state = "loading";
    this.pendingCount++;
    this.emitProgress();
    const gen = this.generation;
    this.acquireTexture(item.thumb)
      .then((texture) => {
        if (gen !== this.generation || tile.state !== "loading") {
          texture.dispose(); // feed changed or tile evicted mid-load — discard
          return;
        }
        this.applyTexture(tile, texture);
        tile.mesh.material.opacity = 0;
        tile.loadOpacity = 0;
        tile.state = "loaded";
        this.loadedCount++;
        this.pendingCount--;
        this.emitProgress();
      })
      .catch(() => {
        if (gen !== this.generation) return;
        if (tile.state === "loading") this.pendingCount--;
        tile.state = "error";
        tile.mesh.material.color.set(0x2a1518);
        this.emitProgress();
      });
  }

  /**
   * Load a texture, downscaling local images so a huge photo doesn't upload a 50MB
   * texture. blob:/data:/coolmedia: are decoded off the main thread in a Web Worker
   * (coolmedia:// sends CORS, so it isn't canvas-tainted) — this keeps scrolling
   * smooth. Remote http(s) URLs go through the plain loader (may lack CORS). Full-res
   * is swapped back in on focus via loadFull().
   */
  private async acquireTexture(url: string, maxEdge = 512): Promise<THREE.Texture> {
    if (url.startsWith("blob:") || url.startsWith("data:") || url.startsWith("coolmedia:")) {
      // 1) Decode + downscale in a Web Worker (off the main thread entirely).
      try {
        const bmp = await this.workerDecode(url, maxEdge);
        const tex = new THREE.Texture(bmp);
        tex.flipY = false; // bitmap already oriented (imageOrientation:flipY)
        tex.needsUpdate = true;
        return tex;
      } catch {
        /* fall through to main-thread decode */
      }
      // 2) Main-thread createImageBitmap fallback.
      try {
        const blob = await (await fetch(url)).blob();
        const bmp = await createImageBitmap(blob, {
          resizeWidth: maxEdge,
          resizeQuality: "medium",
          imageOrientation: "flipY",
        });
        const tex = new THREE.Texture(bmp);
        tex.flipY = false;
        tex.needsUpdate = true;
        return tex;
      } catch {
        // 3) Last resort (e.g. some GIFs): load via <img>, first frame.
        return new Promise<THREE.Texture>((resolve, reject) => {
          this.loader.load(url, resolve, undefined, reject);
        });
      }
    }
    return new Promise<THREE.Texture>((resolve, reject) => {
      this.loader.load(url, resolve, undefined, reject);
    });
  }

  /** Decode + downscale an image URL in the worker pool; resolves an ImageBitmap. */
  private workerDecode(url: string, maxEdge: number): Promise<ImageBitmap> {
    if (this.workers.length === 0) {
      const n = Math.max(2, Math.min(4, navigator.hardwareConcurrency || 4));
      for (let i = 0; i < n; i++) {
        const w = new Worker(new URL("./decodeWorker.ts", import.meta.url), { type: "module" });
        w.onmessage = (e: MessageEvent<{ id: number; bitmap?: ImageBitmap; error?: string }>) => {
          const job = this.workerJobs.get(e.data.id);
          if (!job) return;
          this.workerJobs.delete(e.data.id);
          if (e.data.bitmap) job.resolve(e.data.bitmap);
          else job.reject(new Error(e.data.error || "decode failed"));
        };
        this.workers.push(w);
      }
    }
    const id = ++this.workerJobId;
    const w = this.workers[this.workerNext];
    this.workerNext = (this.workerNext + 1) % this.workers.length;
    return new Promise<ImageBitmap>((resolve, reject) => {
      this.workerJobs.set(id, { resolve, reject });
      w.postMessage({ id, url, maxEdge });
    });
  }

  /** Destroy a tile's mesh + texture when it scrolls outside the resident window. */
  private destroyTile(i: number): void {
    const tile = this.tiles[i];
    if (!tile) return;
    if (tile.state === "loading") this.pendingCount = Math.max(0, this.pendingCount - 1);
    if (tile.state === "loaded") this.loadedCount = Math.max(0, this.loadedCount - 1);
    tile.state = "error"; // makes any in-flight load discard its result
    this.gifs.disposeTile(tile);
    const mat = tile.mesh.material;
    mat.map?.dispose();
    mat.dispose();
    this.strip.remove(tile.mesh);
    if (tile.reflection) {
      tile.reflection.material.dispose(); // shares map (disposed) + alphaMap (kept)
      this.strip.remove(tile.reflection);
    }
    if (tile.label) {
      tile.label.material.map?.dispose();
      tile.label.material.dispose();
      this.strip.remove(tile.label);
    }
    this.tiles[i] = undefined;
  }

  /** Swap in a higher-resolution image when a tile is focused. */
  private loadFull(tile: Tile): void {
    const item = this.items[tile.index];
    if (!item || tile.fullLoaded || item.type === "video" || item.type === "audio" || !item.full)
      return;
    const isLocal = item.full.startsWith("blob:") || item.full.startsWith("data:");
    if (!isLocal && item.full === item.thumb) return; // remote: nothing crisper to load
    tile.fullLoaded = true;
    this.acquireTexture(item.full, 2048).then((tex) => {
      if (this.selectedIndex === tile.index) this.applyTexture(tile, tex);
      else tex.dispose();
    });
  }

  private clearTiles(): void {
    // Dispose GPU resources, then clear the group in one shot. (Removing 16k
    // children one-by-one via strip.remove would be O(n²) and freeze.)
    for (const tile of this.tiles) {
      if (!tile) continue;
      this.gifs.disposeTile(tile);
      const mat = tile.mesh.material;
      mat.map?.dispose();
      mat.dispose();
      if (tile.reflection) tile.reflection.material.dispose(); // map shared, alphaMap kept
      if (tile.label) {
        tile.label.material.map?.dispose();
        tile.label.material.dispose();
      }
    }
    this.strip.clear();
    this.tiles = [];
    this.visStart = 0;
    this.visEnd = -1;
    this.keepStart = 0;
    this.keepEnd = -1;
  }

  /* ------------------------------ pointer/scroll ------------------------------ */

  private onWheel = (e: WheelEvent) => {
    e.preventDefault();
    this.setPointer(e.clientX, e.clientY); // zoom centers on the cursor
    if (Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
      if (this.selectedIndex < 0) {
        this.velocity += e.deltaX * 0.03; // trackpad pan
        this.scrollTargetX = null;
      }
      return;
    }
    this.zoomBy(e.deltaY * 0.01); // wheel = zoom in/out
  };

  private onPointerDown = (e: PointerEvent) => {
    (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
    this.pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    this.setPointer(e.clientX, e.clientY);

    if (this.pointers.size === 2) {
      this.beginPinch();
      return;
    }
    // Bottom scrub zone — works with any button (incl. right-drag) since the
    // canvas suppresses the context menu. This is the "slider".
    const rect = this.renderer.domElement.getBoundingClientRect();
    if (
      this.selectedIndex < 0 &&
      this.scrollMax > this.scrollMin &&
      e.clientY - rect.top > rect.height - SCRUB_ZONE_PX
    ) {
      e.preventDefault();
      this.scrubbing = true;
      this.scrubFromClientX(e.clientX, rect);
      return;
    }
    if (e.button === 1 || e.button === 2) {
      // Middle OR right mouse = free grab pan (vertical + horizontal).
      // The right-click context menu is suppressed so right-drag works as a grab.
      e.preventDefault();
      this.panning = true;
      this.lastPointerX = e.clientX;
      this.lastPointerY = e.clientY;
      this.renderer.domElement.style.cursor = "move";
      return;
    }
    // Left button: drag = horizontal scroll, tap = select.
    this.dragging = true;
    this.dragButton = e.button;
    this.pointerMoved = false;
    this.lastPointerX = e.clientX;
    this.lastMoveT = performance.now();
    this.velocity = 0;
    if (this.selectedIndex < 0) this.renderer.domElement.style.cursor = "grabbing";
  };

  private onPointerMove = (e: PointerEvent) => {
    if (this.pointers.has(e.pointerId)) {
      this.pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    }
    this.setPointer(e.clientX, e.clientY);

    if (this.pinching && this.pointers.size >= 2) {
      const pts = [...this.pointers.values()];
      const dist = Math.hypot(pts[0].x - pts[1].x, pts[0].y - pts[1].y);
      const mid = this.pinchMid();
      this.setPointer(mid.x, mid.y); // zoom toward the pinch center
      this.zoomBy(-(dist - this.pinchDist) * 0.02); // pinch out = zoom in
      this.pinchDist = dist;
      return;
    }

    if (this.scrubbing) {
      this.scrubFromClientX(e.clientX, this.renderer.domElement.getBoundingClientRect());
      return;
    }

    if (this.panning) {
      const dx = e.clientX - this.lastPointerX;
      const dy = e.clientY - this.lastPointerY;
      this.lastPointerX = e.clientX;
      this.lastPointerY = e.clientY;
      const vpH = this.viewportHeight();
      const h = this.container.clientHeight;
      this.panY = clamp(this.panY + (dy / h) * vpH, -PAN_Y_MAX, PAN_Y_MAX);
      if (this.selectedIndex < 0) {
        this.scrollX = clamp(this.scrollX - (dx / h) * vpH, this.scrollMin, this.scrollMax);
        this.scrollTargetX = null;
      }
      return;
    }

    if (!this.dragging) {
      this.updateHover();
      return;
    }
    const dx = e.clientX - this.lastPointerX;
    if (Math.abs(dx) > 2) this.pointerMoved = true;
    this.lastPointerX = e.clientX;
    if (this.selectedIndex >= 0) return;
    const now = performance.now();
    const dtm = Math.max(0.001, (now - this.lastMoveT) / 1000);
    this.lastMoveT = now;
    const worldDelta = (dx / this.container.clientHeight) * this.viewportWidth * DRAG_GAIN;
    this.scrollX = clamp(this.scrollX - worldDelta, this.scrollMin, this.scrollMax);
    this.scrollTargetX = null;
    // Clamp the fling so a fast flick can't overshoot far past the edge
    // (that overshoot is what made all the tiles briefly disappear).
    this.velocity = clamp(-worldDelta / dtm, -MAX_SPEED, MAX_SPEED);
  };

  private onPointerUp = (e: PointerEvent) => {
    this.pointers.delete(e.pointerId);
    if (this.scrubbing) {
      this.scrubbing = false;
      return;
    }
    if (this.pinching) {
      if (this.pointers.size < 2) this.pinching = false;
      return;
    }
    if (this.panning) {
      this.panning = false;
      this.renderer.domElement.style.cursor = this.hoverIndex >= 0 ? "pointer" : "grab";
      return;
    }
    if (!this.dragging) return;
    this.dragging = false;
    this.renderer.domElement.style.cursor = this.hoverIndex >= 0 ? "pointer" : "grab";
    if (this.pointerMoved || this.dragButton !== 0) return; // only left-click selects
    if (this.selectedIndex >= 0) {
      this.deselect(); // click background to exit zoom
    } else {
      const hit = this.pick();
      if (hit >= 0) this.selectIndex(hit);
    }
  };

  private onContextMenu = (e: Event) => e.preventDefault();

  // Suppress the browser's middle-click autoscroll so middle-drag works on the
  // wall AND the bottom slider (otherwise autoscroll steals the drag).
  private onMouseDown = (e: MouseEvent) => {
    if (e.button === 1) e.preventDefault();
  };

  private onPointerLeave = (e: PointerEvent) => {
    this.pointers.delete(e.pointerId);
    if (this.pointers.size < 2) this.pinching = false;
    this.dragging = false;
    this.panning = false;
    this.scrubbing = false;
    if (this.hoverIndex !== -1) {
      this.hoverIndex = -1;
      this.emit("hover", -1, null);
    }
  };

  private beginPinch(): void {
    this.pinching = true;
    this.dragging = false;
    this.panning = false;
    const pts = [...this.pointers.values()];
    this.pinchDist = Math.hypot(pts[0].x - pts[1].x, pts[0].y - pts[1].y);
    const mid = this.pinchMid();
    this.setPointer(mid.x, mid.y);
  }

  private pinchMid(): { x: number; y: number } {
    const pts = [...this.pointers.values()];
    return { x: (pts[0].x + pts[1].x) / 2, y: (pts[0].y + pts[1].y) / 2 };
  }

  private setPointer(clientX: number, clientY: number): void {
    const rect = this.renderer.domElement.getBoundingClientRect();
    this.pointer.x = ((clientX - rect.left) / rect.width) * 2 - 1;
    this.pointer.y = -((clientY - rect.top) / rect.height) * 2 + 1;
  }

  /** Map a clientX over the bottom track to a scroll fraction (thumb-centered). */
  private scrubFromClientX(clientX: number, rect: DOMRect): void {
    const trackW = Math.max(1, rect.width - SCRUB_PAD_PX * 2);
    const px = (clientX - (rect.left + SCRUB_PAD_PX)) / trackW;
    const tw = this.contentWidth > 0 ? Math.min(1, this.viewportWidth / this.contentWidth) : 1;
    const f = tw < 1 ? (px - tw / 2) / (1 - tw) : 0;
    this.scrollToFraction(clamp(f, 0, 1));
  }

  private pick(): number {
    this.camera.updateMatrixWorld();
    this.raycaster.setFromCamera(this.pointer, this.camera);
    // Only test the visible window (not all 16k+ tiles).
    const meshes: THREE.Object3D[] = [];
    for (let i = this.visStart; i <= this.visEnd; i++) {
      const t = this.tiles[i];
      if (t) meshes.push(t.mesh);
    }
    const hits = this.raycaster.intersectObjects(meshes, false);
    if (!hits.length) return -1;
    return (hits[0].object.userData.index as number) ?? -1;
  }

  /** Hide a tile (and its reflection/label) that scrolled out of the window. */
  private hideTile(tile: Tile): void {
    tile.mesh.visible = false;
    if (tile.reflection) tile.reflection.visible = false;
    if (tile.label) tile.label.visible = false;
  }

  private updateHover(): void {
    const hit = this.selectedIndex >= 0 ? -1 : this.pick();
    if (hit !== this.hoverIndex) {
      this.hoverIndex = hit;
      this.renderer.domElement.style.cursor = hit >= 0 ? "pointer" : "grab";
      this.emit("hover", hit, hit >= 0 ? this.items[hit] ?? null : null);
    }
  }

  private viewportHeight(): number {
    return 2 * Math.tan((this.camera.fov * Math.PI) / 180 / 2) * this.camDist;
  }
  private computeViewport(): void {
    this.viewportWidth = this.viewportHeight() * this.camera.aspect;
  }
  private recomputeBounds(): void {
    this.computeViewport();
    this.computeScrollBounds();
  }

  /**
   * scrollX is the view-center in content coordinates. Bounds run from
   * "first content edge at the left of the view" to "last content edge at the
   * right of the view", so the last column is always reachable.
   */
  private computeScrollBounds(): void {
    const cols = Math.ceil(this.items.length / ROWS);
    const PAD = CELL_W; // one empty column of breathing room at each end
    const minX = -MAX_W / 2 - PAD;
    const maxX = (cols > 0 ? (cols - 1) * CELL_W + MAX_W / 2 : MAX_W / 2) + PAD;
    this.contentWidth = maxX - minX;
    if (this.viewportWidth >= this.contentWidth) {
      this.scrollMin = this.scrollMax = (minX + maxX) / 2; // fits — center it
    } else {
      this.scrollMin = minX + this.viewportWidth / 2;
      this.scrollMax = maxX - this.viewportWidth / 2;
    }
  }

  private onResize(): void {
    const w = this.container.clientWidth;
    const h = this.container.clientHeight;
    if (!w || !h) return;
    this.renderer.setSize(w, h);
    this.camera.aspect = w / h;
    this.camera.updateProjectionMatrix();
    this.recomputeBounds();
    this.scrollX = clamp(this.scrollX, this.scrollMin, this.scrollMax);
  }

  /* --------------------------------- render --------------------------------- */

  private animate = () => {
    if (!this.running) return;
    requestAnimationFrame(this.animate);

    // Render every frame (uncapped — matches the display, like before).
    const now = performance.now();
    if (!this.lastTick) this.lastTick = now;
    const dt = Math.min((now - this.lastTick) / 1000, 0.05);
    this.lastTick = now;
    const t = now / 1000;
    const focused = this.selectedIndex >= 0;

    // Camera zoom (wheel) — pulls in when focused, restores the user's zoom after.
    const effectiveDist = focused ? FOCUS_DIST : this.camDistTarget;
    this.camDist += (effectiveDist - this.camDist) * Math.min(1, dt * 6);

    const vpH = this.viewportHeight();
    const vpW = vpH * this.camera.aspect;
    this.viewportWidth = vpW;
    this.computeScrollBounds();

    // Zoom toward the cursor: keep the point under the pointer fixed as zoom changes.
    if (!focused && this.lastVpW > 0) {
      const dW = this.lastVpW - vpW;
      const dH = this.lastVpH - vpH;
      if (dW) this.scrollX += this.pointer.x * dW * 0.5;
      if (dH) this.panY = clamp(this.panY + this.pointer.y * dH * 0.5, -PAN_Y_MAX, PAN_Y_MAX);
    }
    this.lastVpW = vpW;
    this.lastVpH = vpH;

    // Camera: when focused, pan to the selected photo so the zoom centers on it
    // (where the cursor was). Otherwise centered, with vertical free-pan.
    const selTile = focused ? this.tiles[this.selectedIndex] : undefined;
    const targetCamX = selTile ? selTile.baseX - this.scrollX : 0;
    const targetCamY = selTile ? selTile.baseY : this.panY;
    this.camX += (targetCamX - this.camX) * Math.min(1, dt * 7);
    this.camY += (targetCamY - this.camY) * Math.min(1, dt * 7);
    this.camera.position.set(this.camX, this.camY + 0.25 * (1 - this.focus), this.camDist);
    this.camera.lookAt(this.camX, this.camY, 0);

    // Horizontal scroll (the wall is frozen while a photo is focused).
    if (focused) {
      this.velocity = 0;
    } else if (this.scrollTargetX !== null) {
      // Slider-driven: ease toward the target and derive velocity (drives the lean).
      const old = this.scrollX;
      this.scrollX += (this.scrollTargetX - this.scrollX) * Math.min(1, dt * 14);
      this.velocity = dt > 0 ? (this.scrollX - old) / dt : 0;
      if (Math.abs(this.scrollTargetX - this.scrollX) < 0.002) {
        this.scrollX = this.scrollTargetX;
        this.scrollTargetX = null;
        this.velocity = 0;
      }
    } else {
      if (this.inputDir !== 0) {
        this.velocity = clamp(this.velocity + this.inputDir * ACCEL * dt, -MAX_SPEED, MAX_SPEED);
      } else {
        this.velocity *= Math.pow(0.045, dt); // momentum decay
        if (Math.abs(this.velocity) < 0.002) this.velocity = 0;
      }
      // Don't accelerate past the ends — this is what made the slider "try to
      // go back" (bounce) at the start/end.
      if ((this.scrollX <= this.scrollMin && this.velocity < 0) ||
          (this.scrollX >= this.scrollMax && this.velocity > 0)) {
        this.velocity = 0;
      }
      this.scrollX += this.velocity * dt;
      // Ease only a residual out-of-range (e.g. returning from a centered photo).
      if (this.scrollX < this.scrollMin)
        this.scrollX += (this.scrollMin - this.scrollX) * Math.min(1, dt * 12);
      else if (this.scrollX > this.scrollMax)
        this.scrollX += (this.scrollMax - this.scrollX) * Math.min(1, dt * 12);
    }
    this.strip.position.x = -this.scrollX;
    // Update the scrubber when the position OR the zoom (thumb size) changes.
    // Compare against the LAST EMITTED value (not within-frame) so changes made
    // in pointer handlers between frames (e.g. middle-mouse pan) are detected.
    if (
      Math.abs(this.scrollX - this.lastEmitScrollX) > 0.0004 ||
      Math.abs(vpW - this.lastEmitVpW) > 0.002
    ) {
      this.lastEmitScrollX = this.scrollX;
      this.lastEmitVpW = vpW;
      this.emitScroll();
    }

    // Banking: flat at rest, leans in the direction of travel. A sqrt curve so
    // even slow scrolling banks noticeably (both directions), not just fast flings.
    const v = this.velocity;
    const bankTarget = focused
      ? 0
      : clamp(Math.sign(v) * Math.sqrt(Math.abs(v)) * BANK_GAIN, -BANK_MAX, BANK_MAX);
    this.bank += (bankTarget - this.bank) * Math.min(1, dt * 6);
    this.wallGroup.rotation.y = this.bank;

    // Focus transition.
    this.focus += (this.focusTarget - this.focus) * Math.min(1, dt * 6);

    // Virtualization: only the visible window of tiles exists as meshes; tiles
    // are built lazily on view and destroyed when they leave the resident range,
    // so 18 or 160,000 files cost the same in CPU and memory. Column-major layout
    // means the item index range maps directly from a horizontal world range.
    const halfSpan = this.viewportWidth / 2 + CELL_W * 2;
    const maxIndex = this.tiles.length - 1;
    const rangeFor = (center: number, extraCols: number): [number, number] => {
      const span = halfSpan + extraCols * CELL_W;
      const first = Math.max(0, Math.floor((center - span) / CELL_W) * ROWS);
      const last = Math.min(maxIndex, (Math.ceil((center + span) / CELL_W) + 1) * ROWS - 1);
      return [first, last];
    };

    const cullCenter = this.scrollX + this.camX;
    const [newStart, newEnd] = maxIndex < 0 ? [0, -1] : rangeFor(cullCenter, 0);

    // Destroy tiles (mesh + texture) outside the resident "keep" range so mesh
    // count AND GPU memory stay bounded no matter how far you scroll.
    const [keepFirst, keepLast] =
      maxIndex < 0 ? [0, -1] : rangeFor(cullCenter, KEEP_BUFFER_COLS);
    if (this.keepEnd >= this.keepStart) {
      for (let i = this.keepStart; i < keepFirst; i++) this.destroyTile(i);
      for (let i = keepLast + 1; i <= this.keepEnd; i++) this.destroyTile(i);
    }
    this.keepStart = keepFirst;
    this.keepEnd = keepLast;

    // Hide tiles that scrolled out of the visible range (still resident).
    for (let i = this.visStart; i <= this.visEnd; i++) {
      if (i < newStart || i > newEnd) {
        const t = this.tiles[i];
        if (t) this.hideTile(t);
      }
    }
    this.visStart = newStart;
    this.visEnd = newEnd;

    // Collected for the GIF controller (see end of loop).
    const visibleTiles: GifTile[] = [];

    // Streaming load: fill the visible columns sweeping in the travel direction,
    // plus a few lookahead columns ahead of you, capped by MAX_INFLIGHT so it
    // streams in *while scrolling* (one column at a time) without flooding the
    // decoder. Jump far away and it simply loads at the new location.
    if (maxIndex >= 0 && this.pendingCount < MAX_INFLIGHT) {
      const dir = this.velocity > 0.2 ? 1 : this.velocity < -0.2 ? -1 : 0;
      const aheadCols = 6;
      const maxCol = Math.floor(maxIndex / ROWS);
      const firstCol = Math.floor(newStart / ROWS);
      const lastCol = Math.floor(newEnd / ROWS);
      const lo = Math.max(0, firstCol - (dir < 0 ? aheadCols : 0));
      const hi = Math.min(maxCol, lastCol + (dir > 0 ? aheadCols : 0));
      // Start only a few NEW loads per frame so texture uploads spread across
      // frames instead of spiking (that spike is the scroll lag while skeletons
      // are showing). Off-thread decode + this throttle keep scrolling smooth.
      let budget = MAX_NEW_LOADS_PER_FRAME;
      const loadCol = (c: number): boolean => {
        const base = c * ROWS;
        for (let r = 0; r < ROWS; r++) {
          const idx = base + r;
          if (idx > maxIndex || idx < keepFirst || idx > keepLast) continue;
          const tile = this.ensureTile(idx);
          if (tile.state === "idle") {
            this.loadTile(tile);
            budget--;
            if (budget <= 0 || this.pendingCount >= MAX_INFLIGHT) return false;
          }
        }
        return true;
      };
      if (dir < 0) {
        for (let c = hi; c >= lo; c--) if (!loadCol(c)) break;
      } else {
        for (let c = lo; c <= hi; c++) if (!loadCol(c)) break;
      }
    }

    const vHeight = 2 * Math.tan((this.camera.fov * Math.PI) / 180 / 2) * this.camDist;
    // Leave room at the bottom (caption) and sides (permanent prev/next arrows).
    const focusH = Math.min(2.4, vHeight * 0.56);
    const focusMaxW = this.viewportWidth * 0.78;

    for (let i = newStart; i <= newEnd; i++) {
      const tile = this.ensureTile(i);
      tile.mesh.visible = true;
      visibleTiles.push(tile);

      if (tile.state === "loaded" && tile.loadOpacity < 1) {
        tile.loadOpacity = Math.min(1, tile.loadOpacity + dt * 2.5);
      }
      if (tile.state === "loading" || tile.state === "idle") {
        const pulse = 0.5 + 0.5 * Math.sin(t * 3 + tile.phase);
        tile.mesh.material.color.setRGB(0.1 + pulse * 0.06, 0.11 + pulse * 0.07, 0.14 + pulse * 0.09);
      }

      const isSel = tile.index === this.selectedIndex;
      const isHover = tile.index === this.hoverIndex;

      // Target scale (multiplier on the tile's fitted size).
      let targetScale = 1;
      if (isSel && this.focus > 0.001) {
        let f = focusH / tile.h;
        if (f * tile.w > focusMaxW) f = focusMaxW / tile.w;
        targetScale = 1 + (f - 1) * this.focus;
      } else if (isHover && !focused) {
        targetScale = 1.12; // light zoom, in place (scales about the photo's center)
      }
      tile.scale += (targetScale - tile.scale) * Math.min(1, dt * 8);
      tile.mesh.scale.set(tile.w * tile.scale, tile.h * tile.scale, 1);

      // Position: the photo stays at its baseline (camera pans to it on focus).
      // Hover does NOT move in z — it scales purely about the photo's own center
      // (no perspective drift), and is drawn on top via renderOrder + depthTest.
      const targetZ = isSel ? 1.4 * this.focus : 0;
      tile.mesh.position.y += (tile.baseY - tile.mesh.position.y) * Math.min(1, dt * 7);
      tile.mesh.position.z += (targetZ - tile.mesh.position.z) * Math.min(1, dt * 7);

      const onTop = isSel || (isHover && !focused);
      tile.mesh.renderOrder = onTop ? 2 : 0;
      tile.mesh.material.depthTest = !onTop;
      tile.mesh.updateMatrix(); // matrixAutoUpdate is off

      // Opacity: dim everything except the selected tile while focused.
      const dim = focused && !isSel ? 1 - this.focus * 0.86 : 1;
      tile.mesh.material.opacity = tile.loadOpacity * dim;

      // Mirrored reflection — anchored to the photo's RESTING size/position so it
      // stays put (doesn't zoom) when the photo pops on hover; fades out on focus.
      if (tile.reflection) {
        const r = tile.reflection;
        // Reflection hangs from the shared row baseline (so all line up), not the
        // individual photo height, and is anchored (doesn't move with hover).
        r.position.set(tile.baseX, tile.baseline - REFLECT_GAP - tile.h / 2, 0);
        r.scale.set(tile.w, -tile.h, 1);
        r.updateMatrix(); // matrixAutoUpdate is off
        const ro = tile.loadOpacity * 0.42 * (1 - this.focus);
        r.material.opacity = ro;
        r.visible = ro > 0.01;
      }

      // Title label: shown on every (visible) tile when the Titles toggle is on
      // (hover uses the DOM tooltip instead). Sits at the bottom, billboarded.
      const wantLabel = !focused && this.showTitles && tile.state === "loaded";
      const title = wantLabel ? this.items[tile.index]?.title : undefined;
      if (wantLabel && title) {
        if (!tile.label) {
          // Cap the label to the tile's width so it never overlaps neighbours.
          tile.label = this.makeLabel(title, Math.min(tile.w, MAX_W) * 0.96);
          this.strip.add(tile.label);
        }
        tile.label.position.set(tile.baseX, tile.baseline + 0.14, 0.07);
        tile.label.visible = true;
        (tile.label.material as THREE.SpriteMaterial).opacity = tile.loadOpacity;
      } else if (tile.label) {
        tile.label.visible = false;
      }
    }

    // Wall GIF playback (all logic in src/gifsAnimation).
    this.gifs.update({
      focused,
      velocity: this.velocity,
      cullCenter,
      rows: ROWS,
      cellW: CELL_W,
      now,
      items: this.items,
      visible: visibleTiles,
    });

    if (this.slideshow && focused) {
      this.slideshowTimer += dt;
      if (this.slideshowTimer >= 4) { this.slideshowTimer = 0; this.next(); }
    }

    // While an item is open, the opaque lightbox covers the wall — stop rendering
    // it entirely so all the GPU goes to the viewer.
    if (!focused) this.renderer.render(this.scene, this.camera);
  };
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}
