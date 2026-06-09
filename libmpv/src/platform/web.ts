import type { Feed, ProgressFn } from "@/feed/types";
import {
  currentLocalUrls,
  feedFromDataTransfer,
  feedFromDirectoryPicker,
  feedFromFilePicker,
  feedFromFiles,
  revokeLocalUrls,
  revokeUrls,
  supportsFsAccess,
} from "@/feed/localFiles";
import type { Platform } from "./Platform";

/** Browser implementation: File System Access API + <input> + drag-and-drop. */
export const webPlatform: Platform = {
  name: "web",
  supportsFolderPicker: supportsFsAccess(),

  pickFiles: (onProgress?: ProgressFn): Promise<Feed> => feedFromFilePicker(onProgress),
  pickFolder: (onProgress?: ProgressFn): Promise<Feed> => feedFromDirectoryPicker(onProgress),
  fromDataTransfer: (dt: DataTransfer, onProgress?: ProgressFn): Promise<Feed> =>
    feedFromDataTransfer(dt, onProgress),
  fromFileList: (files: File[], onProgress?: ProgressFn): Promise<Feed> =>
    feedFromFiles(files, onProgress),

  // Web object-URL lifecycle.
  snapshotResources: () => currentLocalUrls(),
  releaseResources: (snapshot: unknown) => revokeUrls((snapshot as string[]) ?? []),
  releaseAll: () => revokeLocalUrls(),
};
