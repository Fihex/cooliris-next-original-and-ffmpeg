// Main-process side of cover-art extraction: read the file (leak-free — fs.readFile
// opens+closes; the head-read fallback closes in `finally`) and hand the bytes to the
// shared parser. Bounded by the scan's 6-wide pool.

import { promises as fs } from "node:fs";
import { parseCover, type Cover } from "../src/feed/coverArt";

const FULL_READ_CAP = 48 * 1024 * 1024; // read whole file up to this; else just the head
const HEAD_BYTES = 8 * 1024 * 1024;

export async function extractCoverArt(abs: string): Promise<Cover | null> {
  let buf: Uint8Array;
  try {
    const st = await fs.stat(abs);
    buf = st.size <= FULL_READ_CAP ? await fs.readFile(abs) : await readHead(abs, HEAD_BYTES);
  } catch {
    return null;
  }
  return parseCover(buf, abs.slice(abs.lastIndexOf(".") + 1));
}

async function readHead(abs: string, n: number): Promise<Uint8Array> {
  const fh = await fs.open(abs, "r");
  try {
    const b = Buffer.alloc(n);
    const { bytesRead } = await fh.read(b, 0, n, 0);
    return b.subarray(0, bytesRead);
  } finally {
    await fh.close();
  }
}
