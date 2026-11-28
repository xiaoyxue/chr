// Reads an offline ZIP by random access, ZIP64 included, so a package of several GiB is never held in
// memory whole: first its central directory (every entry's name and sizes, checked before anything is
// unpacked), then one entry at a time. The page reads a chosen file (a Blob), its server a file handle.
import { inflateSync } from 'fflate';

/** A ZIP archive's bytes, read by offset. */
export interface RandomAccess { readonly size: number; read(offset: number, length: number): Promise<Uint8Array> }
export interface ZipEntry { name: string; deflated: boolean; compressedSize: number; size: number; crc32: number; offset: number }

export const bytesSource = (bytes: Uint8Array): RandomAccess =>
  ({ size: bytes.length, read: async (offset, length) => bytes.subarray(offset, offset + length) });
export const blobSource = (blob: Blob): RandomAccess =>
  ({ size: blob.size, read: async (offset, length) => new Uint8Array(await blob.slice(offset, offset + length).arrayBuffer()) });

const END = 0x06054b50, ZIP64_LOCATOR = 0x07064b50, ZIP64_END = 0x06064b50, CENTRAL = 0x02014b50, LOCAL = 0x04034b50;
/** Far more than the central directory of an offline package's at most 1025 entries. */
const MAX_DIRECTORY_BYTES = 4 * 1024 * 1024;

function damaged(): never { throw new Error('The offline ZIP is damaged or not a ZIP archive.'); }
const view = (bytes: Uint8Array) => new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
function u64(data: DataView, offset: number) {
  const value = data.getUint32(offset, true) + data.getUint32(offset + 4, true) * 2 ** 32;
  return Number.isSafeInteger(value) ? value : damaged();
}

async function readRange(source: RandomAccess, offset: number, length: number) {
  if (offset < 0 || length < 0 || offset + length > source.size) damaged();
  const bytes = await source.read(offset, length);
  return bytes.length === length ? bytes : damaged();
}

/** Every entry of the archive, from its (ZIP64) end record and central directory. */
export async function centralDirectory(source: RandomAccess, maxEntries: number): Promise<ZipEntry[]> {
  const tailLength = Math.min(source.size, 20 + 22 + 0xffff);
  const tail = await readRange(source, source.size - tailLength, tailLength), t = view(tail);
  let end = tail.length - 22;
  while (end >= 0 && !(t.getUint32(end, true) === END && end + 22 + t.getUint16(end + 20, true) === tail.length)) end--;
  if (end < 0) damaged();
  let count = t.getUint16(end + 10, true), size = t.getUint32(end + 12, true), offset = t.getUint32(end + 16, true);
  if (end >= 20 && t.getUint32(end - 20, true) === ZIP64_LOCATOR) {
    const record = view(await readRange(source, u64(t, end - 12), 56));
    if (record.getUint32(0, true) !== ZIP64_END) damaged();
    [count, size, offset] = [u64(record, 32), u64(record, 40), u64(record, 48)];
  }
  if (count > maxEntries) throw new Error('The offline ZIP contains duplicate or unexpected files.');
  if (size > MAX_DIRECTORY_BYTES) damaged();
  const directory = await readRange(source, offset, size), d = view(directory);
  const entries: ZipEntry[] = [];
  for (let at = 0, index = 0; index < count; index++) {
    if (at + 46 > size || d.getUint32(at, true) !== CENTRAL) damaged();
    const flags = d.getUint16(at + 8, true), method = d.getUint16(at + 10, true);
    const nameLength = d.getUint16(at + 28, true), extraEnd = at + 46 + nameLength + d.getUint16(at + 30, true);
    const next = extraEnd + d.getUint16(at + 32, true);
    // Encrypted entries and methods other than stored / deflate are not written by Fusion.
    if (next > size || flags & 1 || (method !== 0 && method !== 8)) damaged();
    let compressedSize = d.getUint32(at + 20, true), uncompressed = d.getUint32(at + 24, true), local = d.getUint32(at + 42, true);
    // ZIP64 extended information: the 8-byte value of each field set to 0xffffffff, in this order.
    for (let extra = at + 46 + nameLength; extra + 4 <= extraEnd; extra += 4 + d.getUint16(extra + 2, true)) {
      if (d.getUint16(extra, true) !== 1) continue;
      let field = extra + 4;
      const next64 = () => { if (field + 8 > extra + 4 + d.getUint16(extra + 2, true)) damaged(); field += 8; return u64(d, field - 8); };
      if (uncompressed === 0xffffffff) uncompressed = next64();
      if (compressedSize === 0xffffffff) compressedSize = next64();
      if (local === 0xffffffff) local = next64();
    }
    let name: string;
    try { name = new TextDecoder('utf-8', { fatal: true }).decode(directory.subarray(at + 46, at + 46 + nameLength)); }
    catch { damaged(); }
    entries.push({ name, deflated: method === 8, compressedSize, size: uncompressed, crc32: d.getUint32(at + 16, true), offset: local });
    at = next;
  }
  return entries;
}

const CRC_TABLE = Uint32Array.from({ length: 256 }, (_, n) => {
  for (let k = 0; k < 8; k++) n = n & 1 ? 0xedb88320 ^ (n >>> 1) : n >>> 1;
  return n;
});
function crc32(bytes: Uint8Array) {
  let crc = 0xffffffff;
  for (let i = 0; i < bytes.length; i++) crc = CRC_TABLE[(crc ^ bytes[i]) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

/** One entry's bytes, unpacked and checked against its size and CRC-32. */
export async function readEntry(source: RandomAccess, entry: ZipEntry): Promise<Uint8Array> {
  const header = view(await readRange(source, entry.offset, 30));
  if (header.getUint32(0, true) !== LOCAL) damaged();
  const data = await readRange(source, entry.offset + 30 + header.getUint16(26, true) + header.getUint16(28, true), entry.compressedSize);
  let bytes = data;
  if (entry.deflated) {
    try { bytes = inflateSync(data, { out: new Uint8Array(entry.size) }); }
    catch { damaged(); }
  }
  return bytes.length === entry.size && crc32(bytes) === entry.crc32 ? bytes : damaged();
}
