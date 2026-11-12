import { PLYLoader } from 'three/addons/loaders/PLYLoader.js';
import { MAX_PLY_BYTES, MAX_PLY_POINTS } from './ply-result.ts';
import type { PlyCloud, PlyWorkerMessage } from './ply-result.ts';

function inspectHeader(buffer: ArrayBuffer) {
  const prefix = new TextDecoder('latin1').decode(new Uint8Array(buffer, 0, Math.min(buffer.byteLength, 65536)));
  const end = /(?:\r\n|\r|\n)end_header(?:\r\n|\r|\n)/.exec(prefix);
  if (!prefix.startsWith('ply') || !end) throw new Error('不是有效的 PLY 文件，或文件头超过 64 KB。');
  const header = prefix.slice(0, end.index);
  const format = /^format[ \t]+(ascii|binary_little_endian|binary_big_endian)[ \t]+1\.0[ \t]*$/m.exec(header.replace(/\r/g, ''));
  if (!format) throw new Error('仅支持 PLY 1.0 的 ASCII 或二进制点云。');
  const sizes: Record<string, number> = { char: 1, uchar: 1, int8: 1, uint8: 1, short: 2, ushort: 2, int16: 2, uint16: 2, int: 4, uint: 4, int32: 4, uint32: 4, float: 4, float32: 4, double: 8, float64: 8 };
  const properties = new Map<string, string>();
  let count = 0, stride = 0, vertex = false;
  for (const line of header.split(/\r\n|\r|\n/).map((text) => text.trim())) {
    const fields = line.split(/\s+/);
    if (fields[0] === 'element') {
      const size = Number(fields[2]);
      if (!Number.isSafeInteger(size) || size < 0) throw new Error('PLY 的元素数量无效。');
      vertex = fields[1] === 'vertex';
      if (vertex) {
        if (count || !size || size > MAX_PLY_POINTS) throw new Error('PLY 顶点数量无效，最多支持 200 万个点。');
        count = size;
      } else if (size > 0) throw new Error('请导出只含顶点的点云 PLY；当前不支持带面的网格文件。');
    } else if (fields[0] === 'property' && vertex) {
      if (fields.length !== 3 || !sizes[fields[1]] || properties.has(fields[2])) throw new Error('PLY 顶点属性无效或包含不支持的列表类型。');
      properties.set(fields[2], fields[1]);
      stride += sizes[fields[1]];
    }
  }
  if (!count || !['x', 'y', 'z'].every((name) => properties.has(name))) throw new Error('PLY 必须包含有效的 x、y、z 顶点坐标。');
  const colors = ['red', 'green', 'blue'].filter((name) => properties.has(name));
  if (colors.length && (colors.length !== 3 || colors.some((name) => !['uchar', 'uint8'].includes(properties.get(name))))) throw new Error('点云颜色需包含完整的 8 位 red、green、blue 属性。');
  if (format[1] !== 'ascii' && end.index + end[0].length + count * stride > buffer.byteLength) throw new Error('PLY 二进制数据不完整，实际大小小于声明的顶点数量。');
  const comments = header.split(/\r\n|\r|\n/).filter((line) => line.startsWith('comment ')).join(' ');
  return { count, declaredPitchCoordinates: /pitch[- ]anchored/i.test(comments) && /\bmet(?:re|er)s\b/i.test(comments) && /\bup\s*=\s*-Z\b/i.test(comments) };
}

async function parse(file: File): Promise<PlyCloud> {
  if (!(file instanceof File) || !file.size || file.size > MAX_PLY_BYTES) throw new Error('PLY 文件为空或超过 64 MB。');
  const buffer = await file.arrayBuffer();
  const header = inspectHeader(buffer);
  const geometry = new PLYLoader().parse(buffer);
  try {
    const source = geometry.getAttribute('position'), color = geometry.getAttribute('color');
    if (!source || source.count !== header.count || (color && color.count !== source.count)) throw new Error('PLY 解析后的顶点或颜色数量不匹配。');
    const positions = new Float32Array(source.count * 3);
    const colors = color ? new Float32Array(source.count * 3) : undefined;
    const minimum: PlyCloud['minimum'] = [Infinity, Infinity, Infinity], maximum: PlyCloud['maximum'] = [-Infinity, -Infinity, -Infinity];
    for (let i = 0; i < source.count; i++) {
      const values = [source.getX(i), source.getY(i), source.getZ(i)];
      const rgb = color ? [color.getX(i), color.getY(i), color.getZ(i)] : undefined;
      for (let axis = 0; axis < 3; axis++) {
        const value = values[axis];
        if (!Number.isFinite(value) || Math.abs(value) > 1_000_000) throw new Error('PLY 包含无效坐标或超过 1000 公里的坐标值，请使用局部场地坐标。');
        positions[i * 3 + axis] = value;
        minimum[axis] = Math.min(minimum[axis], value);
        maximum[axis] = Math.max(maximum[axis], value);
        if (colors) {
          if (!Number.isFinite(rgb[axis])) throw new Error('PLY 包含无效的颜色值。');
          colors[i * 3 + axis] = rgb[axis];
        }
      }
    }
    if (maximum.every((value, axis) => value === minimum[axis])) throw new Error('点云没有可查看的空间范围。');
    return { fileName: file.name, byteLength: file.size, positions, colors, minimum, maximum, declaredPitchCoordinates: header.declaredPitchCoordinates };
  } finally { geometry.dispose(); }
}

self.addEventListener('message', async (event: MessageEvent<File>) => {
  try {
    const cloud = await parse(event.data);
    const message: PlyWorkerMessage = { ok: true, cloud };
    const transfer: Transferable[] = [cloud.positions.buffer];
    if (cloud.colors) transfer.push(cloud.colors.buffer);
    self.postMessage(message, { transfer });
  } catch (error) {
    const message: PlyWorkerMessage = { ok: false, error: error instanceof Error ? `无法导入 PLY：${error.message}` : '无法解析此 PLY 文件。' };
    self.postMessage(message);
  }
});
