import { object, number, integer, list, text, relativePath, verified, table } from './bundle-validation.ts';
import { parseDiagnosticBundle } from './diagnostic-bundle.ts';
import type { BundleFile, BundleMessage, ResultBundle, ResultFrame, ResultShot } from './bundle-result.ts';

async function parse(files: BundleFile[]): Promise<ResultBundle> {
  const index = new Map(files.map((entry) => [entry.path, entry.file]));
  if (index.size !== files.length) throw new Error('文件夹包含重复的相对路径。');
  const candidates = files.filter(({ path }) => path.split('/').pop() === 'metadata.json').sort((a, b) => a.path.length - b.path.length);
  let metadata: Record<string, unknown>, root: string;
  for (const candidate of candidates) {
    if (candidate.file.size > 4 * 1024 * 1024) throw new Error('metadata.json 超过 4 MB。');
    const raw: unknown = JSON.parse(await candidate.file.text());
    const value = object(raw);
    if (value.version !== 'vchao.three-shot-Ti-centre-bundle.v2') continue;
    if (metadata) throw new Error('所选文件夹包含多个结果包，请只选择一个结果包。');
    metadata = value;
    root = candidate.path.slice(0, -'metadata.json'.length);
  }
  if (!metadata) return parseDiagnosticBundle(files);
  const dimensions = list(object(metadata.world).pitch_dimensions_metres).map(number);
  if (dimensions.length !== 2 || dimensions.some((value) => value <= 0 || value > 300)
    || object(metadata.image_geometry).system !== 'SNC') throw new Error('不支持的球场或图像坐标约定。');
  const world = object(metadata.world);
  if (world.units !== 'metres' || !/negative\s*Z/i.test(text(world.axes))) throw new Error('结果包必须使用米制、负 Z 向上的球场坐标。');
  const get = (entry: Record<string, unknown>) => {
    const path = relativePath(entry.path), file = index.get(root + path);
    if (!file) throw new Error(`缺少运行文件：${path}`);
    return file;
  };
  const shots: ResultShot[] = [];
  const sourceShots = list(metadata.shots);
  if (!sourceShots.length || sourceShots.length > 32) throw new Error('结果包需包含 1–32 个镜头。');
  for (const raw of sourceShots) {
    const shot = object(raw), clip = object(shot.clip), tiEntry = object(shot.Ti), mapEntry = object(shot.frame_map);
    if (tiEntry.direction !== 'pitch_xy_metres_to_image_snc' || tiEntry.vectors !== 'homogeneous_column_vectors') throw new Error('不支持的 Ti 方向或矩阵约定。');
    const id = text(shot.case), centre = object(shot.C_world_metres);
    const position: ResultShot['centre'] = [number(centre.x), number(centre.y), number(centre.z)];
    if (position[2] >= 0 || position.some((value) => Math.abs(value) > 10000)) throw new Error('结果包相机中心无效。');
    const resolution = list(clip.native_resolution).map(integer);
    if (resolution.length !== 2 || resolution.some((value) => value < 1 || value > 8192)) throw new Error('原生视频尺寸无效。');
    const video = get(clip);
    await verified(video, clip, 512 * 1024 * 1024);
    const [tiRows, mapRows] = await Promise.all([
      table(get(tiEntry), tiEntry, ['pts', 'present_index', 'Ti']),
      table(get(mapEntry), mapEntry, ['clip_ordinal', 'present_index', 'source_pts', 'clip_pts', 'clip_time_base_num', 'clip_time_base_den', 'source_time_base_num', 'source_time_base_den', 'native_width', 'native_height', 'ti_row_index']),
    ]);
    if (mapRows.length !== integer(clip.frame_count)) throw new Error('视频帧数与 frame-map 不一致。');
    const ids = new Set<number>();
    let previous = -Infinity;
    const frames: ResultFrame[] = mapRows.map((row, ordinal) => {
      const presentIndex = integer(row.present_index), sourcePts = integer(row.source_pts), numerator = integer(row.clip_time_base_num), denominator = integer(row.clip_time_base_den);
      const clipPts = integer(row.clip_pts);
      const time = clipPts * numerator / denominator;
      if (integer(row.clip_ordinal) !== ordinal || ids.has(presentIndex) || numerator <= 0 || denominator <= 0 || !Number.isFinite(time) || time < 0 || time <= previous
        || integer(row.native_width) !== resolution[0] || integer(row.native_height) !== resolution[1]) throw new Error(`无效的帧映射：${id} / ${ordinal}`);
      ids.add(presentIndex);
      previous = time;
      let Ti: number[] = null;
      if (row.ti_row_index !== null) {
        const tiIndex = integer(row.ti_row_index), pose = tiRows[tiIndex];
        if (!pose || integer(pose.present_index) !== presentIndex || integer(pose.pts) !== sourcePts) throw new Error(`Ti 与原始帧不匹配：${ordinal}`);
        Ti = list(pose.Ti).flatMap((values) => list(values).map(number));
        if (Ti.length !== 9) throw new Error('Ti 必须是 3×3 矩阵。');
        const magnitude = Math.max(...Ti.map(Math.abs)), h = Ti.map((value) => value / magnitude);
        const determinant = h[0] * (h[4] * h[8] - h[5] * h[7]) - h[1] * (h[3] * h[8] - h[5] * h[6]) + h[2] * (h[3] * h[7] - h[4] * h[6]);
        if (!Number.isFinite(determinant) || Math.abs(determinant) < 1e-14) throw new Error(`Ti 矩阵退化：${ordinal}`);
      }
      return { ordinal, presentIndex, sourcePts, time, Ti };
    });
    if (shots.some((item) => item.id === id)) throw new Error('结果包镜头标识重复。');
    shots.push({ id, video, width: resolution[0], height: resolution[1], centre: position, frames,
      warnings: shot.quality_warnings === undefined ? [] : list(shot.quality_warnings).map(text) });
  }
  return { name: root.split('/').filter(Boolean).pop(), pitch: [dimensions[0], dimensions[1]], shots };
}

self.addEventListener('message', async (event: MessageEvent<BundleFile[]>) => {
  try {
    const message: BundleMessage = { ok: true, bundle: await parse(event.data) };
    self.postMessage(message);
  } catch (error) {
    const message: BundleMessage = { ok: false, error: error instanceof Error ? error.message : '结果文件夹解析失败。' };
    self.postMessage(message);
  }
});
