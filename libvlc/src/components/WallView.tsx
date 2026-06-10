import { useCallback, useEffect, useRef, useState } from "react";
import { WallScene } from "@/wall/WallScene";
import type { Feed, MediaItem } from "@/feed/types";
import {
  loadJsonFeedFromUrl,
  loadJsonFeedFromFile,
  loadJsonFeedFromText,
} from "@/feed/jsonFeed";
import { platform } from "@/platform";
import { embed, type WallController } from "@/embed/cooliris-embed";
import { Toolbar, type LoadProgress, type SortKey } from "./Toolbar";
import { OpenDialog } from "./OpenDialog";
import { JsonDialog } from "./JsonDialog";
import { SettingsDialog } from "./SettingsDialog";
import { Lightbox } from "./Lightbox";
import { Scrubber, type ScrubberHandle } from "./Scrubber";
import { Toasts, type ToastMessage } from "./Toast";

export function WallView() {
  const containerRef = useRef<HTMLDivElement>(null);
  const sceneRef = useRef<WallScene | null>(null);
  const scrubberRef = useRef<ScrubberHandle>(null);
  const hoverLabelRef = useRef<HTMLDivElement>(null);
  const masterRef = useRef<MediaItem[]>([]); // full feed, pre-filter
  const itemsRef = useRef<MediaItem[]>([]); // displayed (post-filter)
  const searchRef = useRef("");
  const fromRef = useRef(""); // yyyy-mm-dd
  const toRef = useRef("");
  const dateFieldRef = useRef<"modified" | "created">("modified");
  const sortRef = useRef<SortKey>("default");
  const feedToken = useRef(0); // only the most recent open() applies its result

  const [items, setItems] = useState<MediaItem[]>([]);
  const [feedTitle, setFeedTitle] = useState<string>();
  const [selected, setSelected] = useState(-1);
  const [closingItem, setClosingItem] = useState<MediaItem | null>(null); // fading viewer
  const closeTimer = useRef<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [slideshow, setSlideshow] = useState(false);
  const [showTitles, setShowTitles] = useState(false);
  const [gifAnim, setGifAnim] = useState(false);
  const [jsonOpen, setJsonOpen] = useState(false);
  const [openOpen, setOpenOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [loadStage, setLoadStage] = useState<{ done: number; total: number } | null>(null);
  const [search, setSearch] = useState("");
  const [fromDate, setFromDate] = useState("");
  const [toDate, setToDate] = useState("");
  const [dateField, setDateField] = useState<"modified" | "created">("modified");
  const [hasCreated, setHasCreated] = useState(false); // created dates available (Electron)
  const [sort, setSort] = useState<SortKey>("default");
  const [progress, setProgress] = useState<LoadProgress>({ loaded: 0, total: 0, pending: 0 });
  const [hover, setHover] = useState<MediaItem | null>(null);
  const [toasts, setToasts] = useState<ToastMessage[]>([]);
  const [fatal, setFatal] = useState<string | null>(null);

  const toast = useCallback((text: string, kind: ToastMessage["kind"] = "info") => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t, { id, kind, text }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 3500);
  }, []);

  // Combined filter: search text + last-modified date range.
  const applyFilters = useCallback(() => {
    const query = searchRef.current.trim().toLowerCase();
    const fromMs = parseDayInput(fromRef.current, false);
    const toMs = parseDayInput(toRef.current, true);
    const hasDate = fromMs !== null || toMs !== null;
    const useCreated = dateFieldRef.current === "created";
    const filtered = masterRef.current.filter((it) => {
      if (query && !(it.title ?? "").toLowerCase().includes(query)) return false;
      if (hasDate) {
        // Created falls back to modified for items that don't carry a created date
        // (e.g. drag-and-dropped files), so it never hides them.
        const ts = useCreated ? it.created ?? it.date : it.date;
        if (ts == null) return false;
        if (fromMs !== null && ts < fromMs) return false;
        if (toMs !== null && ts > toMs) return false;
      }
      return true;
    });
    const sorted = sortItems(filtered, sortRef.current);
    itemsRef.current = sorted;
    setItems(sorted);
    setSelected(-1);
    sceneRef.current?.setItems(sorted);
  }, []);

  const applyFeed = useCallback(
    (feed: Feed) => {
      masterRef.current = feed.items;
      setFeedTitle(feed.title);
      // Reset filters for the new feed.
      searchRef.current = "";
      fromRef.current = "";
      toRef.current = "";
      setSearch("");
      setFromDate("");
      setToDate("");
      // Created date is only available from the Electron scan (Node birthtime).
      const created = feed.items.some((it) => it.created != null);
      setHasCreated(created);
      if (!created) {
        dateFieldRef.current = "modified";
        setDateField("modified");
      }
      applyFilters();
      embed.callbacks.feedload?.(feed.items.length);
    },
    [applyFilters]
  );

  const runFeedTask = useCallback(
    async (task: (onProgress: (done: number, total: number) => void) => Promise<Feed>) => {
      const token = ++feedToken.current; // supersede any in-flight open
      setBusy(true);
      setLoadStage(null);
      // Snapshot the previous feed's resources; release them only AFTER the new
      // feed (with its own freshly-created resources) has loaded successfully.
      const oldResources = platform.snapshotResources();
      const onProgress = (done: number, total: number) => {
        if (token === feedToken.current) setLoadStage({ done, total });
      };
      try {
        const feed = await task(onProgress);
        if (token !== feedToken.current) return; // a newer open started — drop this one
        applyFeed(feed);
        platform.releaseResources(oldResources);
        toast(`Loaded ${feed.items.length} item(s)`);
      } catch (err) {
        if ((err as DOMException)?.name === "AbortError") return; // picker cancelled
        const msg = err instanceof Error ? err.message : String(err);
        toast(msg, "error");
        embed.callbacks.feederror?.(msg);
      } finally {
        if (token === feedToken.current) {
          setBusy(false);
          setLoadStage(null);
        }
      }
    },
    [applyFeed, toast]
  );

  /* ---------------------------- create the scene ---------------------------- */
  useEffect(() => {
    if (!containerRef.current) return;
    // Guard against a leftover canvas (e.g. React StrictMode double-mount) so we
    // never end up with two overlaid walls.
    containerRef.current.replaceChildren();
    let scene: WallScene;
    try {
      scene = new WallScene(containerRef.current);
    } catch (err) {
      setFatal(
        "Could not start WebGL. Your browser or GPU may have it disabled. " +
          (err instanceof Error ? err.message : "")
      );
      return;
    }
    sceneRef.current = scene;

    scene.on("select", (index: number, item: MediaItem | null) => {
      setSelected(index);
      embed.callbacks.select?.(index, item);
    });
    scene.on("deselect", () => {
      setSelected(-1);
      setSlideshow(false);
      scene.setSlideshow(false);
      embed.callbacks.deselect?.(-1);
    });
    scene.on("hover", (_index: number, item: MediaItem | null) => setHover(item));
    scene.on("progress", (loaded: number, total: number, pending: number) =>
      setProgress({ loaded, total, pending })
    );
    scene.on("scroll", (info) => scrubberRef.current?.update(info));

    const controller: WallController = {
      setFeedURL: (url) => runFeedTask(() => loadJsonFeedFromUrl(url)).then(() => undefined),
      getItems: () => itemsRef.current,
      selectItemByIndex: (i) => scene.selectIndex(i),
      selectItemByGUID: (guid) => {
        const i = itemsRef.current.findIndex((it) => it.id === guid);
        if (i >= 0) scene.selectIndex(i);
      },
      getSelectedItem: () =>
        scene.selected >= 0 ? itemsRef.current[scene.selected] ?? null : null,
      deselect: () => scene.deselect(),
    };
    embed._attach(controller);

    return () => {
      embed._detach();
      scene.dispose();
      sceneRef.current = null;
      platform.releaseAll();
    };
  }, [runFeedTask]);

  /* ----------------------------- initial feed ------------------------------ */
  useEffect(() => {
    runFeedTask(() => loadJsonFeedFromUrl("/sample-feed.json")).catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /* ------------------- sync scrubber thumb after a feed loads ------------------- */
  useEffect(() => {
    if (!items.length) return;
    const id = requestAnimationFrame(() => {
      const s = sceneRef.current;
      if (s) scrubberRef.current?.update(s.getScrollInfo());
    });
    return () => cancelAnimationFrame(id);
  }, [items.length]);

  /* ------------------------------- keyboard -------------------------------- */
  // Arrow keys scroll the wall while browsing (the Lightbox owns its own keys
  // — Esc / ← / → — while an item is open).
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (selected >= 0) return;
      if (e.key === "ArrowRight") sceneRef.current?.startScroll(1);
      else if (e.key === "ArrowLeft") sceneRef.current?.startScroll(-1);
    };
    const onKeyUp = (e: KeyboardEvent) => {
      if ((e.key === "ArrowRight" || e.key === "ArrowLeft") && selected < 0) {
        sceneRef.current?.stopScroll();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
    };
  }, [selected]);

  /* ------------------------------ handlers --------------------------------- */
  // Smooth close: reveal the wall (already scrolled to the last-viewed item) and
  // fade the viewer out over it, then unmount.
  const closeViewer = () => {
    const cur = selected >= 0 ? items[selected] : null;
    sceneRef.current?.deselect();
    if (cur) {
      setClosingItem(cur);
      if (closeTimer.current) clearTimeout(closeTimer.current);
      closeTimer.current = window.setTimeout(() => setClosingItem(null), 220);
    }
  };

  const toggleSlideshow = () => {
    const next = !slideshow;
    setSlideshow(next);
    sceneRef.current?.setSlideshow(next);
  };

  const fullscreen = () => {
    const el = containerRef.current?.parentElement ?? document.documentElement;
    if (document.fullscreenElement) document.exitFullscreen();
    else el.requestFullscreen?.();
  };

  const toggleTitles = () => {
    const next = !showTitles;
    setShowTitles(next);
    sceneRef.current?.setShowTitles(next);
  };

  const toggleGifAnim = () => {
    const next = !gifAnim;
    setGifAnim(next);
    sceneRef.current?.setGifAnim(next);
  };

  const onPointerMove = (e: React.PointerEvent) => {
    const label = hoverLabelRef.current;
    if (label) {
      label.style.left = `${e.clientX}px`;
      label.style.top = `${e.clientY - 18}px`;
    }
  };

  const selectedItem = selected >= 0 ? items[selected] : null;
  // Hover tooltip is suppressed while the Titles toggle shows every label.
  const showHover = hover && selected < 0 && !showTitles;
  const loadPct = loadStage && loadStage.total > 0
    ? Math.round((loadStage.done / loadStage.total) * 100)
    : null;

  return (
    <div className="relative h-full w-full overflow-hidden" onPointerMove={onPointerMove}>
      <div ref={containerRef} className="absolute inset-0" />

      {fatal && (
        <div className="absolute inset-0 z-40 flex items-center justify-center p-6">
          <div className="max-w-md rounded-xl bg-red-500/15 p-5 text-center text-sm text-red-100 ring-1 ring-red-400/30">
            {fatal}
          </div>
        </div>
      )}

      {/* Toolbar hides while a photo is focused (clean view; Back replaces it) */}
      {selected < 0 && (
        <Toolbar
          title={feedTitle}
          count={items.length}
          busy={busy}
          progress={progress}
          slideshow={slideshow}
          search={search}
          fromDate={fromDate}
          toDate={toDate}
          dateField={dateField}
          hasCreated={hasCreated}
          sort={sort}
          onSort={(k) => {
            setSort(k);
            sortRef.current = k;
            applyFilters();
          }}
          onOpen={() => setOpenOpen(true)}
          onSearch={(q) => {
            setSearch(q);
            searchRef.current = q;
            applyFilters();
          }}
          onFromDate={(d) => {
            setFromDate(d);
            fromRef.current = d;
            applyFilters();
          }}
          onToDate={(d) => {
            setToDate(d);
            toRef.current = d;
            applyFilters();
          }}
          onDateField={(f) => {
            setDateField(f);
            dateFieldRef.current = f;
            applyFilters();
          }}
          onToggleSlideshow={toggleSlideshow}
          onOpenSettings={() => setSettingsOpen(true)}
          onFullscreen={fullscreen}
        />
      )}

      {/* Edge arrows — hold to smoothly accelerate, release to glide to a stop */}
      {selected < 0 && items.length > 0 && (
        <>
          <EdgeArrow
            side="left"
            onStart={() => sceneRef.current?.startScroll(-1)}
            onStop={() => sceneRef.current?.stopScroll()}
          />
          <EdgeArrow
            side="right"
            onStart={() => sceneRef.current?.startScroll(1)}
            onStop={() => sceneRef.current?.stopScroll()}
          />
        </>
      )}

      {/* Empty state */}
      {items.length === 0 && !busy && (
        <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
          <div className="px-6 text-center text-white/50">
            <p className="text-lg">{search ? "No matches" : "Open media to begin"}</p>
            {!search && <p className="text-sm">Press Open to choose files, a folder, or JSON</p>}
          </div>
        </div>
      )}

      {/* Loading progress (with percent) while a folder/file set is processed */}
      {busy && (
        <div className="pointer-events-none absolute inset-0 z-40 flex items-center justify-center">
          <div className="w-72 max-w-[80vw] rounded-2xl bg-black/75 p-5 text-center text-white shadow-2xl ring-1 ring-white/10">
            <div className="mb-2 text-sm font-medium">
              {loadPct !== null
                ? `Loading ${loadPct}%`
                : loadStage
                  ? `Scanning… ${loadStage.done.toLocaleString()} files`
                  : "Loading…"}
            </div>
            <div className="h-2 overflow-hidden rounded-full bg-white/15">
              <div
                className={`h-full rounded-full bg-white ${
                  loadPct !== null ? "transition-[width] duration-150" : "animate-pulse"
                }`}
                style={{ width: loadPct !== null ? `${loadPct}%` : "100%" }}
              />
            </div>
            {loadPct !== null && loadStage && (
              <div className="mt-2 text-xs text-white/50">
                {loadStage.done.toLocaleString()} / {loadStage.total.toLocaleString()} files
              </div>
            )}
          </div>
        </div>
      )}

      {/* Hover title tooltip (follows the cursor; hidden when Titles toggle is on) */}
      <div
        ref={hoverLabelRef}
        className={`pointer-events-none fixed z-30 max-w-xs -translate-x-1/2 -translate-y-full truncate rounded-md bg-black/80 px-2 py-1 text-xs text-white shadow ring-1 ring-white/10 transition-opacity ${
          showHover ? "opacity-100" : "opacity-0"
        }`}
      >
        {hover?.title || "Untitled"}
      </div>

      {/* Bottom scrubber (visual only — input handled by the wall's scrub zone) */}
      {selected < 0 && items.length > 0 && <Scrubber ref={scrubberRef} />}

      {/* Focused viewer (opaque lightbox: zoom/pan, no residue behind). Kept
          mounted briefly after close (closingItem) so it can fade out smoothly. */}
      {(selectedItem || closingItem) && (
        <Lightbox
          item={(selectedItem ?? closingItem)!}
          index={selected >= 0 ? selected : 0}
          total={items.length}
          dateField={dateField}
          closing={!selectedItem}
          onClose={closeViewer}
          onPrev={() => sceneRef.current?.prev()}
          onNext={() => sceneRef.current?.next()}
        />
      )}

      {openOpen && (
        <OpenDialog
          fsAccess={platform.supportsFolderPicker}
          onClose={() => setOpenOpen(false)}
          onFiles={(files) => {
            setOpenOpen(false);
            runFeedTask((p) => platform.fromFileList(files, p));
          }}
          onFilePicker={() => {
            setOpenOpen(false);
            runFeedTask((p) => platform.pickFiles(p));
          }}
          onFolderPicker={() => {
            setOpenOpen(false);
            runFeedTask((p) => platform.pickFolder(p));
          }}
          onDrop={(dt) => {
            setOpenOpen(false);
            runFeedTask((p) => platform.fromDataTransfer(dt, p));
          }}
          onJson={() => {
            setOpenOpen(false);
            setJsonOpen(true);
          }}
        />
      )}

      {settingsOpen && (
        <SettingsDialog
          titles={showTitles}
          gifAnim={gifAnim}
          onClose={() => setSettingsOpen(false)}
          onToggleTitles={toggleTitles}
          onToggleGifAnim={toggleGifAnim}
        />
      )}

      {jsonOpen && (
        <JsonDialog
          onClose={() => setJsonOpen(false)}
          onUrl={(url) => {
            setJsonOpen(false);
            runFeedTask(() => loadJsonFeedFromUrl(url));
          }}
          onFile={(file) => {
            setJsonOpen(false);
            runFeedTask(() => loadJsonFeedFromFile(file));
          }}
          onText={(txt) => {
            setJsonOpen(false);
            runFeedTask(async () => loadJsonFeedFromText(txt));
          }}
        />
      )}

      <Toasts toasts={toasts} />
    </div>
  );
}

/** A large edge arrow that smoothly scrolls while held. */
function EdgeArrow({
  side,
  onStart,
  onStop,
}: {
  side: "left" | "right";
  onStart: () => void;
  onStop: () => void;
}) {
  return (
    <button
      onPointerDown={onStart}
      onPointerUp={onStop}
      onPointerLeave={onStop}
      onPointerCancel={onStop}
      aria-label={side === "left" ? "Scroll left" : "Scroll right"}
      className={`absolute top-1/2 z-20 flex h-20 w-12 -translate-y-1/2 items-center justify-center rounded-full bg-black/20 text-3xl text-white/60 backdrop-blur-sm transition hover:bg-black/40 hover:text-white ${
        side === "left" ? "left-3" : "right-3"
      }`}
    >
      {side === "left" ? "‹" : "›"}
    </button>
  );
}

/** Parse a typed date ("YYYY-MM-DD" or anything Date.parse accepts) to epoch ms. */
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

/** Sort items by the chosen key (created falls back to modified). */
function sortItems(items: MediaItem[], key: SortKey): MediaItem[] {
  const arr = items.slice();
  const name = (i: MediaItem) => i.title ?? i.path ?? "";
  const mod = (i: MediaItem) => i.date ?? 0;
  const cre = (i: MediaItem) => i.created ?? i.date ?? 0;
  switch (key) {
    case "default": break; // keep the feed's natural (as-loaded) order
    case "name-asc": arr.sort((a, b) => collator.compare(name(a), name(b))); break;
    case "name-desc": arr.sort((a, b) => collator.compare(name(b), name(a))); break;
    case "modified-desc": arr.sort((a, b) => mod(b) - mod(a)); break;
    case "modified-asc": arr.sort((a, b) => mod(a) - mod(b)); break;
    case "created-desc": arr.sort((a, b) => cre(b) - cre(a)); break;
    case "created-asc": arr.sort((a, b) => cre(a) - cre(b)); break;
  }
  return arr;
}

function parseDayInput(value: string, endOfDay: boolean): number | null {
  const v = value.trim();
  if (!v) return null;
  const m = /^(\d{4})-(\d{1,2})-(\d{1,2})$/.exec(v);
  let d: Date;
  if (m) {
    d = new Date(+m[1], +m[2] - 1, +m[3]);
  } else {
    const t = Date.parse(v);
    if (Number.isNaN(t)) return null;
    d = new Date(t);
  }
  if (endOfDay) d.setHours(23, 59, 59, 999);
  else d.setHours(0, 0, 0, 0);
  return d.getTime();
}

