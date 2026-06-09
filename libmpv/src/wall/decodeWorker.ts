/// <reference lib="webworker" />
// Off-main-thread image decode + downscale. Fetches the (blob/data) URL, decodes
// and resizes to `maxEdge` width, and transfers the ImageBitmap back.

interface DecodeRequest {
  id: number;
  url: string;
  maxEdge: number;
}

self.onmessage = async (e: MessageEvent<DecodeRequest>) => {
  const { id, url, maxEdge } = e.data;
  try {
    const blob = await (await fetch(url)).blob();
    const bitmap = await createImageBitmap(blob, {
      resizeWidth: maxEdge,
      resizeQuality: "medium",
      imageOrientation: "flipY",
    });
    (self as unknown as Worker).postMessage({ id, bitmap }, [bitmap]);
  } catch (err) {
    (self as unknown as Worker).postMessage({ id, error: String(err) });
  }
};
