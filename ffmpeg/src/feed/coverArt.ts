// Embedded cover-art parser for the common audio containers (MP3/ID3v2, FLAC,
// MP4/M4A, Ogg/Opus). Pure and byte-based (Uint8Array), so it runs in BOTH the
// Electron main process (folder scan, from fs.readFile) and the renderer
// (drag-and-drop / <input>, from File.arrayBuffer()). No Node/Buffer-only APIs.

export interface Cover {
  mime: string;
  data: Uint8Array;
}

const u32be = (b: Uint8Array, o: number) =>
  ((b[o] << 24) | (b[o + 1] << 16) | (b[o + 2] << 8) | b[o + 3]) >>> 0;
const u32le = (b: Uint8Array, o: number) =>
  (b[o] | (b[o + 1] << 8) | (b[o + 2] << 16) | (b[o + 3] << 24)) >>> 0;
const u24be = (b: Uint8Array, o: number) => (b[o] << 16) | (b[o + 1] << 8) | b[o + 2];

function ascii(b: Uint8Array, start: number, end: number): string {
  let s = "";
  for (let i = start; i < end && i < b.length; i++) s += String.fromCharCode(b[i]);
  return s;
}

function indexOfByte(b: Uint8Array, val: number, from: number): number {
  for (let i = from; i < b.length; i++) if (b[i] === val) return i;
  return -1;
}

function findBytes(b: Uint8Array, needle: number[], from = 0): number {
  outer: for (let i = from; i <= b.length - needle.length; i++) {
    for (let j = 0; j < needle.length; j++) if (b[i + j] !== needle[j]) continue outer;
    return i;
  }
  return -1;
}

function base64ToBytes(s: string): Uint8Array {
  const bin = atob(s.replace(/\s+/g, "")); // global in browsers and Node 18+
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function sniffMime(d: Uint8Array): string {
  if (d[0] === 0xff && d[1] === 0xd8 && d[2] === 0xff) return "image/jpeg";
  if (d[0] === 0x89 && d[1] === 0x50 && d[2] === 0x4e && d[3] === 0x47) return "image/png";
  if (ascii(d, 0, 4) === "RIFF" && ascii(d, 8, 12) === "WEBP") return "image/webp";
  if (d[0] === 0x47 && d[1] === 0x49 && d[2] === 0x46) return "image/gif";
  return "image/jpeg";
}

function finalize(c: Cover | null): Cover | null {
  if (!c || !c.data || c.data.length < 8) return null;
  if (!c.mime || c.mime.indexOf("/") < 0) c.mime = sniffMime(c.data);
  return c;
}

/** Parse embedded cover art from a file's bytes (whole file or a generous head). */
export function parseCover(buf: Uint8Array, ext?: string): Cover | null {
  try {
    if (buf.length >= 12) {
      if (ascii(buf, 0, 3) === "ID3") return finalize(parseId3(buf));
      const tag4 = ascii(buf, 0, 4);
      if (tag4 === "fLaC") return finalize(parseFlac(buf));
      if (tag4 === "OggS") return finalize(parseOgg(buf));
      if (ascii(buf, 4, 8) === "ftyp") return finalize(parseMp4(buf));
    }
    const e = (ext || "").toLowerCase();
    if (e === "m4a" || e === "mp4" || e === "aac") return finalize(parseMp4(buf));
    if (e === "ogg" || e === "oga" || e === "opus") return finalize(parseOgg(buf));
    if (e === "flac") return finalize(parseFlac(buf));
    return null;
  } catch {
    return null;
  }
}

/* --------------------------------- MP3 / ID3v2 --------------------------------- */

const synchsafe = (b: Uint8Array, o: number) =>
  ((b[o] & 0x7f) << 21) | ((b[o + 1] & 0x7f) << 14) | ((b[o + 2] & 0x7f) << 7) | (b[o + 3] & 0x7f);

function parseId3(buf: Uint8Array): Cover | null {
  const major = buf[3];
  const flags = buf[5];
  const end = Math.min(buf.length, 10 + synchsafe(buf, 6));
  let pos = 10;
  if (flags & 0x40) {
    const extSize = major === 4 ? synchsafe(buf, pos) : u32be(buf, pos);
    pos += major === 4 ? extSize : extSize + 4;
  }
  while (pos + (major === 2 ? 6 : 10) <= end) {
    let id: string;
    let frameSize: number;
    let headerLen: number;
    if (major === 2) {
      id = ascii(buf, pos, pos + 3);
      frameSize = u24be(buf, pos + 3);
      headerLen = 6;
    } else {
      id = ascii(buf, pos, pos + 4);
      frameSize = major === 4 ? synchsafe(buf, pos + 4) : u32be(buf, pos + 4);
      headerLen = 10;
    }
    if (!/^[A-Z0-9]{3,4}$/.test(id) || frameSize <= 0 || pos + headerLen + frameSize > end + 1) break;
    if (id === "APIC" || id === "PIC") {
      const c = parseApic(buf.subarray(pos + headerLen, pos + headerLen + frameSize), id);
      if (c) return c;
    }
    pos += headerLen + frameSize;
  }
  return null;
}

function parseApic(b: Uint8Array, id: string): Cover | null {
  let p = 0;
  const enc = b[p++];
  let mime = "";
  if (id === "PIC") {
    const fmt = ascii(b, p, p + 3).toUpperCase();
    p += 3;
    mime = fmt.indexOf("PNG") >= 0 ? "image/png" : "image/jpeg";
  } else {
    const z = indexOfByte(b, 0, p);
    if (z < 0) return null;
    mime = ascii(b, p, z);
    p = z + 1;
  }
  p++; // picture type
  if (enc === 1 || enc === 2) {
    while (p + 1 < b.length && !(b[p] === 0 && b[p + 1] === 0)) p += 2;
    p += 2;
  } else {
    const z = indexOfByte(b, 0, p);
    if (z < 0) return null;
    p = z + 1;
  }
  const data = b.subarray(p);
  return data.length ? { mime, data } : null;
}

/* ------------------------------------ FLAC ------------------------------------ */

function parseFlac(buf: Uint8Array): Cover | null {
  let pos = 4;
  while (pos + 4 <= buf.length) {
    const header = buf[pos];
    const last = (header & 0x80) !== 0;
    const type = header & 0x7f;
    const len = u24be(buf, pos + 1);
    pos += 4;
    if (type === 6) return parseFlacPicture(buf.subarray(pos, pos + len));
    pos += len;
    if (last) break;
  }
  return null;
}

function parseFlacPicture(b: Uint8Array): Cover | null {
  if (b.length < 32) return null;
  let p = 4;
  const mimeLen = u32be(b, p);
  p += 4;
  const mime = ascii(b, p, p + mimeLen);
  p += mimeLen;
  const descLen = u32be(b, p);
  p += 4 + descLen;
  p += 16;
  const dataLen = u32be(b, p);
  p += 4;
  const data = b.subarray(p, p + dataLen);
  return data.length ? { mime, data } : null;
}

/* --------------------------------- MP4 / M4A ---------------------------------- */

function parseMp4(buf: Uint8Array): Cover | null {
  const findBox = (from: number, to: number, name: string): { start: number; end: number } | null => {
    let p = from;
    while (p + 8 <= to) {
      let size = u32be(buf, p);
      const type = ascii(buf, p + 4, p + 8);
      let header = 8;
      if (size === 1) {
        size = u32be(buf, p + 8) * 0x100000000 + u32be(buf, p + 12);
        header = 16;
      } else if (size === 0) {
        size = to - p;
      }
      if (size < header || p + size > to) break;
      if (type === name) return { start: p + header, end: p + size };
      p += size;
    }
    return null;
  };
  const moov = findBox(0, buf.length, "moov");
  if (!moov) return null;
  const udta = findBox(moov.start, moov.end, "udta");
  if (!udta) return null;
  const meta = findBox(udta.start, udta.end, "meta");
  if (!meta) return null;
  const ilst = findBox(meta.start + 4, meta.end, "ilst"); // meta is a fullbox (+4)
  if (!ilst) return null;
  const covr = findBox(ilst.start, ilst.end, "covr");
  if (!covr) return null;
  const data = findBox(covr.start, covr.end, "data");
  if (!data) return null;
  const typeIndicator = u32be(buf, data.start) & 0xff; // 13=jpeg, 14=png
  const img = buf.subarray(data.start + 8, data.end);
  if (!img.length) return null;
  return { mime: typeIndicator === 14 ? "image/png" : "image/jpeg", data: img };
}

/* ------------------------------- Ogg / Opus ----------------------------------- */

function parseOgg(buf: Uint8Array): Cover | null {
  const chunks: Uint8Array[] = [];
  let pos = 0;
  let total = 0;
  while (pos + 27 <= buf.length && ascii(buf, pos, pos + 4) === "OggS") {
    const segs = buf[pos + 26];
    const tableEnd = pos + 27 + segs;
    if (tableEnd > buf.length) break;
    let dataLen = 0;
    for (let i = 0; i < segs; i++) dataLen += buf[pos + 27 + i];
    chunks.push(buf.subarray(tableEnd, tableEnd + dataLen));
    total += dataLen;
    pos = tableEnd + dataLen;
    if (total > 4 * 1024 * 1024) break;
  }
  const stream = new Uint8Array(total);
  let off = 0;
  for (const c of chunks) {
    stream.set(c, off);
    off += c.length;
  }

  let cpos = -1;
  const opus = findBytes(stream, [0x4f, 0x70, 0x75, 0x73, 0x54, 0x61, 0x67, 0x73]); // OpusTags
  if (opus >= 0) cpos = opus + 8;
  else {
    const vorbis = findBytes(stream, [0x03, 0x76, 0x6f, 0x72, 0x62, 0x69, 0x73]); // \x03vorbis
    if (vorbis >= 0) cpos = vorbis + 7;
  }
  if (cpos < 0 || cpos + 4 > stream.length) return null;

  let p = cpos;
  const vendorLen = u32le(stream, p);
  p += 4 + vendorLen;
  if (p + 4 > stream.length) return null;
  const count = u32le(stream, p);
  p += 4;
  for (let i = 0; i < count && p + 4 <= stream.length; i++) {
    const len = u32le(stream, p);
    p += 4;
    const comment = stream.subarray(p, p + len);
    p += len;
    const eq = indexOfByte(comment, 0x3d, 0); // '='
    if (eq < 0) continue;
    if (ascii(comment, 0, eq).toUpperCase() === "METADATA_BLOCK_PICTURE") {
      const c = parseFlacPicture(base64ToBytes(ascii(comment, eq + 1, comment.length)));
      if (c) return c;
    }
  }
  return null;
}
