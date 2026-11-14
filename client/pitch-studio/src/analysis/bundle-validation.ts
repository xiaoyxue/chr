import { parquetReadObjects, parquetMetadata } from 'hyparquet';
import { compressors } from 'hyparquet-compressors';

export function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('结果数据对象无效。');
  return value as Record<string, unknown>;
}
export function number(value: unknown): number {
  const result = typeof value === 'bigint' ? Number(value) : value;
  if (typeof result !== 'number' || !Number.isFinite(result)) throw new Error('结果包含无效数字。');
  return result;
}
export function integer(value: unknown): number {
  const result = number(value);
  if (!Number.isSafeInteger(result)) throw new Error('帧索引或时间基数超出安全整数范围。');
  return result;
}
export function list(value: unknown): unknown[] {
  if (!Array.isArray(value)) throw new Error('结果数据列表无效。');
  return value;
}
export function text(value: unknown): string {
  if (typeof value !== 'string' || !value) throw new Error('结果数据文本无效。');
  return value;
}
export function relativePath(value: unknown): string {
  const path = text(value);
  if (path.includes('\\') || path.startsWith('/') || path.includes(':') || path.split('/').some((part) => !part || part === '.' || part === '..')) {
    throw new Error('结果运行路径必须是文件夹内的相对路径。');
  }
  return path;
}
export async function verified(file: File, entry: Record<string, unknown>, maximum: number) {
  if (!file.size || file.size > maximum || (entry.bytes !== undefined && file.size !== integer(entry.bytes))) throw new Error(`文件大小不符或超出限制：${file.name}`);
  const buffer = await file.arrayBuffer();
  if (entry.sha256 !== undefined) {
    const expected = text(entry.sha256).toLowerCase();
    const actual = Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', buffer)), (byte) => byte.toString(16).padStart(2, '0')).join('');
    if (actual !== expected) throw new Error(`SHA-256 校验失败：${file.name}`);
  }
  return buffer;
}
export async function table(file: File, entry: Record<string, unknown>, columns: string[]) {
  const buffer = await verified(file, entry, 64 * 1024 * 1024);
  const metadata = parquetMetadata(buffer);
  const count = integer(metadata.num_rows);
  if (count < 1 || count > 2_000_000 || (entry.rows !== undefined && count !== integer(entry.rows))) throw new Error(`Parquet 行数无效：${file.name}`);
  const rows: unknown[] = await parquetReadObjects({ file: buffer, compressors, columns });
  return rows.map(object);
}
export function matrix(value: unknown, columns: 3 | 4) {
  const rows = list(value).map((row) => list(row).map(number));
  if (rows.length !== 3 || rows.some((row) => row.length !== columns)) throw new Error(`投影矩阵必须为 3×${columns}。`);
  const flat = rows.flat(), magnitude = Math.max(...flat.map(Math.abs));
  const h = rows.flatMap((row) => row.slice(0, 3).map((x) => x / magnitude));
  const determinant = h[0] * (h[4] * h[8] - h[5] * h[7]) - h[1] * (h[3] * h[8] - h[5] * h[6]) + h[2] * (h[3] * h[7] - h[4] * h[6]);
  if (!Number.isFinite(determinant) || Math.abs(determinant) < 1e-14) throw new Error('投影矩阵退化，不能用于广告叠加。');
  return flat;
}
