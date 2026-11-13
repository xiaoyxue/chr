import { object, number, integer, list, text, relativePath, verified, table, matrix } from './bundle-validation.ts';
import type { BundleFile, DiagnosticProjection, ResultBundle, ResultFrame } from './bundle-result.ts';

export async function parseDiagnosticBundle(files: BundleFile[]): Promise<ResultBundle> {
  let manifest: Record<string, unknown>, root: string;
  for (const candidate of files.filter(({ path }) => path.split('/').pop() === 'MANIFEST.json')) {
    if (!candidate.file.size || candidate.file.size > 4 * 1024 * 1024) throw new Error('MANIFEST.json 大小无效。');
    const value = object(JSON.parse(await candidate.file.text()));
    if (value.status !== 'selected_for_diagnostic_delivery' || !text(value.artifact).startsWith('vchao-')) continue;
    if (manifest) throw new Error('文件夹包含多个诊断投影包，请只选择一个结果包。');
    manifest = value;
    root = candidate.path.slice(0, -'MANIFEST.json'.length);
  }
  if (!manifest) throw new Error('未找到支持的结果包：请选择包含 metadata.json 的 Ti 包，或包含 MANIFEST.json 的诊断投影包；不是 PLY 文件。');
  const index = new Map(files.map((entry) => [entry.path, entry.file]));
  const entries = new Map<string, Record<string, unknown>>();
  for (const raw of list(manifest.files)) {
    const entry = object(raw), path = relativePath(entry.path);
    if (entries.has(path) || !/^[a-f0-9]{64}$/i.test(text(entry.sha256)) || integer(entry.bytes) < 1) throw new Error('MANIFEST 文件清单无效或路径重复。');
    entries.set(path, entry);
  }
  const get = (path: string) => {
    relativePath(path);
    const file = index.get(root + path), entry = entries.get(path);
    if (!file || !entry) throw new Error(`结果包缺少运行文件或清单记录：${path}`);
    return { file, entry };
  };
  const json = async (path: string) => {
    const { file, entry } = get(path);
    const buffer = await verified(file, entry, 4 * 1024 * 1024);
    try { return object(JSON.parse(new TextDecoder().decode(buffer))); }
    catch (error) { throw new Error(`JSON 解析失败（${path}）：${error instanceof Error ? error.message : String(error)}`); }
  };
  const readTable = async (path: string, columns: string[]) => {
    const { file, entry } = get(path);
    return table(file, entry, columns);
  };
  const projection = await json('model/compatible-3d-projection-v1.json');
  const frameData = await json(`model/${relativePath(projection.frame_data)}`);
  const source = await json('media/source.json'), media = object(source.media), identity = object(source.frame_identity);
  const radialPaths = [...entries.keys()].filter((path) => /^model\/[^/]+-native-radial-map-v2\.json$/.test(path));
  if (radialPaths.length !== 1) throw new Error('诊断包需包含唯一的 native-radial-map-v2.json。');
  const radialModel = await json(radialPaths[0]), radial = object(radialModel.radial);
  const hash = text(source.sha256), variant = text(projection.variant);
  if (projection.version !== 1 || frameData.version !== 1 || radialModel.version !== 1
    || projection.status !== 'selected_for_diagnostic_delivery' || variant !== manifest.variant
    || projection.physical_gravity_or_lens_calibration_validated !== false
    || [projection, frameData, radialModel].some((value) => value.source_media_sha256 !== hash)) throw new Error('诊断包版本、变体或源媒体身份不一致。');
  const dimensions = list(radialModel.pitch_dimensions_assumed_m).map(number);
  const resolution = list(frameData.native_extent).map(integer), timeBase = list(frameData.time_base).map(integer);
  const range = list(frameData.present_index_range).map(integer);
  const count = integer(media.container_reported_frames), stream = integer(frameData.stream_index), id = text(frameData.shot_id);
  if (dimensions.length !== 2 || dimensions.some((x) => x <= 0 || x > 300)
    || resolution.length !== 2 || resolution.some((x) => x < 1 || x > 8192)
    || timeBase.length !== 2 || timeBase.some((x) => x <= 0) || range.length !== 2 || range[0] < 0 || range[1] - range[0] !== count || count < 1 || count > 2_000_000
    || stream !== integer(media.video_stream_index) || stream !== integer(identity.stream_index) || id !== identity.shot_id
    || integer(media.width) !== resolution[0] || integer(media.height) !== resolution[1]
    || integer(radialModel.native_width) !== resolution[0] || integer(radialModel.native_height) !== resolution[1]
    || integer(media.time_base_num) !== timeBase[0] || integer(media.time_base_den) !== timeBase[1]
    || JSON.stringify(identity.time_base) !== JSON.stringify(timeBase) || JSON.stringify(identity.present_index_range) !== JSON.stringify(range)
    || integer(radialModel.reference_pi) !== integer(frameData.reference_present_index)) throw new Error('视频尺寸、帧范围或时间基与诊断投影不一致。');
  const position = list(projection.C_world_metres).map(number);
  const p = matrix(projection.P_aux_world_XYZW_to_auxiliary_reference_native, 4);
  const h = matrix(radialModel.H_ground_metres_to_auxiliary_reference_native, 3);
  if (position.length !== 3 || position[2] >= 0 || position.some((x) => Math.abs(x) > 10000)) throw new Error('诊断投影的条件相机中心无效。');
  for (let row = 0; row < 3; row++) {
    const coefficients = p.slice(row * 4, row * 4 + 4);
    if (Math.abs(coefficients.reduce((sum, x, i) => sum + x * [...position, 1][i], 0)) > 1e-8 * Math.max(...coefficients.map(Math.abs)) * Math.max(1, ...position.map(Math.abs))
      || [0, 1, 3].some((column, j) => Math.abs(coefficients[column] - h[row * 3 + j]) > 1e-10 * Math.max(1, Math.abs(coefficients[column])))) {
      throw new Error('P_aux、地面 H 或条件相机中心不一致。');
    }
  }
  const centre = list(radial.centre_native_px).map(number), support = list(radial.validated_auxiliary_reference_native_support_rectangle).map(integer);
  const scale = number(radial.scale_native_px), k1 = number(radial.k1);
  if (centre.length !== 2 || support.length !== 4 || scale <= 0
    || support[2] <= support[0] || support[3] <= support[1] || support[2] - support[0] + 1 > 8192 || support[3] - support[1] + 1 > 8192) throw new Error('径向畸变中心、尺度或支持矩形无效。');
  const radiusSquared = Math.max(...[support[0], support[2]].flatMap((x) => [support[1], support[3]].map((y) => ((x - centre[0]) / scale) ** 2 + ((y - centre[1]) / scale) ** 2)));
  if (!Number.isFinite(radiusSquared) || 1 + 3 * k1 * radiusSquared <= 1e-6) throw new Error('支持域跨越径向折叠，无法安全执行单调分支映射。');
  const postwarpEntry = object(radialModel.postwarp_frame_maps);
  if (postwarpEntry.direction !== `Reference PI${integer(radialModel.reference_pi)} ORIGINAL native pixels to frame ORIGINAL SNC`) throw new Error('不支持的 postwarp 方向；不能将其视为纯 Ti。');
  const videoEntry = get(`media/${relativePath(source.video_file)}`);
  if (videoEntry.entry.sha256 !== hash) throw new Error('原视频 SHA-256 与模型媒体身份不同。');
  await verified(videoEntry.file, videoEntry.entry, 512 * 1024 * 1024);
  const [mapRows, warpRows] = await Promise.all([
    readTable(`media/${relativePath(identity.frame_map_file)}`, ['media_id', 'stream_index', 'present_index', 'pts', 'time_base_num', 'time_base_den', 'shot_id', 'width', 'height']),
    readTable(`model/${relativePath(postwarpEntry.file)}`, ['pts', 'present_index', 'postwarp_reference_native_to_frame_SNC']),
  ]);
  const jsonFrames = list(frameData.frames).map(object);
  if ([mapRows.length, warpRows.length, jsonFrames.length].some((length) => length !== count)) throw new Error('JSON、Parquet 帧表和视频的帧数不一致。');
  const byFrame = (rows: Record<string, unknown>[]) => {
    const result = new Map(rows.map((row) => [`${integer(row.present_index)}:${integer(row.pts)}`, row]));
    if (result.size !== count) throw new Error('帧身份包含重复 PI / PTS。');
    return result;
  };
  const warps = byFrame(warpRows), serialized = byFrame(jsonFrames);
  let previous = -Infinity;
  const frames = mapRows.map((row, ordinal): ResultFrame => {
    const presentIndex = integer(row.present_index), sourcePts = integer(row.pts), key = `${presentIndex}:${sourcePts}`;
    const warp = warps.get(key), saved = serialized.get(key), time = sourcePts * timeBase[0] / timeBase[1];
    if (!warp || !saved || presentIndex !== range[0] + ordinal || time < 0 || time <= previous
      || row.media_id !== hash || row.shot_id !== id || integer(row.stream_index) !== stream
      || integer(row.time_base_num) !== timeBase[0] || integer(row.time_base_den) !== timeBase[1]
      || integer(row.width) !== resolution[0] || integer(row.height) !== resolution[1]) throw new Error(`帧映射身份或时序不一致：${id} / ${ordinal}`);
    previous = time;
    const postwarp = matrix(warp.postwarp_reference_native_to_frame_SNC, 3);
    const copy = matrix(saved.postwarp_reference_native_to_frame_SNC, 3);
    if (postwarp.some((x, i) => x !== copy[i])) throw new Error(`frames.json 与 postwarp Parquet 不一致：PI ${presentIndex}`);
    return { ordinal, presentIndex, sourcePts, time, Ti: null, postwarp };
  });
  const model: DiagnosticProjection = { kind: 'auxiliary-radial-postwarp', variant, matrix: p,
    radial: { centre: [centre[0], centre[1]], scale, k1, support: [support[0], support[1], support[2], support[3]] } };
  return { name: text(manifest.artifact), pitch: [dimensions[0], dimensions[1]], shots: [{
    id, video: videoEntry.file, width: resolution[0], height: resolution[1],
    centre: [position[0], position[1], position[2]], frames, projection: model,
    warnings: [
      `${variant}：条件性诊断投影，未经物理 K/R、重力或镜头标定验证；20 m 高度准确度未验证。`,
      '完整链路为 P_aux → 经验径向畸变 → 逐帧 postwarp → 原视频像素；不等同于纯 3×3 Ti。',
      '标准支持域模式仅显示当前辅助投影图表中、支持矩形内且径向单调的可渲染片段；图表裁剪不代表物理前后判定。该模式下域外省略，域内任意高度也不代表已验证。无球员遮挡模型。',
    ],
  }] };
}
