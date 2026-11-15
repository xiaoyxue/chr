// How the match's local placements (pitch and point cloud: 2D, 3D, flags) appear
// in the overall placement list, and which of them the renderer can draw now.
import type { LocalAd } from './local-ad.ts';

/**
 * The match's point clouds as far as sending is concerned: each cloud's saved
 * same-coordinates answer (by the PLY's SHA-256), and the one loaded now, if any.
 */
export interface CloudState { loadedId?: string; alignment: Readonly<Record<string, boolean>> }

export interface LocalRow {
  id: string;
  space: 'pitch' | 'cloud';
  type: LocalAd['type'];
  badge: string;
  name: string;
  visible: boolean;
  place: string;
  size: string;
  /** The flag's wave, or a 3D model's scale and turn; empty for 2D. */
  detail: string;
  material: string;
  /** Why the placement cannot be sent now; undefined when it can. */
  problem?: string;
  /** A sendable point-cloud placement whose cloud is not loaded. */
  note?: string;
}

const number = (value: number) => String(Math.round(value * 100) / 100);

/**
 * Why the renderer cannot draw `ad` now, or undefined. chr-server reads materials
 * from server paths. A point-cloud placement is saved in its cloud's coordinates,
 * which are pitch coordinates once that cloud was confirmed; it then needs no
 * loaded cloud to be sent.
 */
export function localProblem(ad: LocalAd, clouds: CloudState): string | undefined {
  if (ad.space !== 'pitch' && clouds.alignment[ad.cloudId] !== true)
    return ad.cloudId === clouds.loadedId || clouds.alignment[ad.cloudId] === false ? '点云未确认与球场同坐标' : '所在点云未确认与球场同坐标（请载入该点云并确认）';
  if (!ad.materialPath) return ad.material ? '未设置服务器路径' : ad.type === 'flag' ? '未设置图片' : '未设置素材';
  if (ad.type !== '3D' && !/\.(png|jpe?g)$/i.test(ad.materialPath)) return '渲染器只读取 PNG / JPEG 图片';
}

export function localRow(ad: LocalAd, clouds: CloudState): LocalRow {
  const size = `${number(ad.plane.width)} × ${number(ad.plane.height)} m`;
  const detail = ad.type === 'flag'
    ? `振幅 ${number(ad.flag.amplitude)} m · 频率 ${number(ad.flag.frequency)} · 速度 ${number(ad.flag.speed)}× · ${ad.flag.material === 'pbr' ? 'PBR' : '简易光照'}`
    : ad.type === '3D' ? `缩放 ×${number(ad.scale)} · 朝向 ${number(ad.yaw)}°` : '';
  return {
    id: ad.id, space: ad.space === 'pitch' ? 'pitch' : 'cloud', type: ad.type, badge: ad.type === 'flag' ? '旗帜' : ad.type,
    name: ad.name, visible: ad.visible, place: ad.space === 'pitch' ? '球场' : '点云',
    size: ad.type === '3D' ? `底面 ${size}` : size, detail,
    material: ad.materialPath ?? (ad.material ? `${ad.material.name}（未设置服务器路径）` : '未设置素材'),
    problem: localProblem(ad, clouds),
    ...(ad.space !== 'pitch' && clouds.alignment[ad.cloudId] === true && ad.cloudId !== clouds.loadedId
      ? { note: '点云已确认同坐标（未载入，可发送；编辑需载入点云）' } : {}),
  };
}

/**
 * What a send or export includes. Fusion placements need the offline ZIP that
 * chr-server reads; without it, sendable local placements go alone. `waiting`
 * blocks either way, so a loading ZIP match never drops its placements by accident.
 */
export function sendScope(options: { fusionProblem?: string; waiting?: string; placements: number; locals: number }):
  { fusion: boolean; blocker?: string; note: string } {
  const { fusionProblem, waiting, placements, locals } = options;
  if (!fusionProblem) return waiting ? { fusion: false, blocker: waiting, note: '' } : { fusion: true, note: '' };
  if (!locals) return { fusion: false, blocker: `${fusionProblem}；也可添加可发送的本地广告位或旗帜后单独发送。`, note: '' };
  return { fusion: false, note: `只发送 ${locals} 个本地广告位 / 旗帜${placements ? `，未发送 ${placements} 个 Fusion 广告位（${fusionProblem}）` : ''}；发送会替换 chr-server 的整个场景。` };
}
