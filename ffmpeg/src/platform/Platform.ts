import type { Feed, ProgressFn } from "@/feed/types";

/**
 * Abstraction over local-file access so the React/Three.js UI is shared across
 * targets. Implementations:
 *   - web        → src/platform/web.ts (File System Access API + <input> + DnD)
 *   - electron   → (future) Node fs over IPC: real folders, absolute paths, dates
 *   - capacitor  → (future) Android/iOS media plugin
 *
 * JSON feeds are parsed the same way everywhere (src/feed/jsonFeed.ts); only the
 * remote fetch differs — see `fetchRemoteText`.
 */
export interface Platform {
  readonly name: "web" | "electron" | "capacitor";

  /** Whether a recursive folder picker is available (false on mobile web). */
  readonly supportsFolderPicker: boolean;

  /**
   * Fetch a remote URL as text without browser CORS limits, if the platform can
   * (Electron does this in the main process). When undefined, callers fall back
   * to a plain browser fetch. Lets the desktop app load remote JSON feeds with no
   * third-party proxy.
   */
  fetchRemoteText?(url: string): Promise<string>;

  /** Pick one or more files. */
  pickFiles(onProgress?: ProgressFn): Promise<Feed>;

  /** Pick a folder and recurse into it. */
  pickFolder(onProgress?: ProgressFn): Promise<Feed>;

  /** Build a feed from a drag-and-drop DataTransfer (recurses dropped folders). */
  fromDataTransfer(dt: DataTransfer, onProgress?: ProgressFn): Promise<Feed>;

  /** Build a feed from an <input type="file"> FileList (fallback path). */
  fromFileList(files: File[], onProgress?: ProgressFn): Promise<Feed>;

  /**
   * Resource lifecycle. The web target creates object URLs that must be revoked
   * when a feed is replaced; native targets use file paths and no-op. Snapshot
   * the current resources before a load, then release that snapshot after the
   * new feed is live.
   */
  snapshotResources(): unknown;
  releaseResources(snapshot: unknown): void;
  /** Release everything (on teardown). */
  releaseAll(): void;
}
