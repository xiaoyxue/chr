// How the match's local placements (pitch and point cloud: 2D, 3D, flags) appear
// in the overall placement list, and which of them the renderer can draw now.
import type { LocalAd } from './local-ad.ts';

/** The point cloud loaded for the match and whether it was confirmed to be in pitch coordinates. */
export interface LoadedCloud { id: string; aligned: boolean }

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
}

const number = (value: number) => String(Math.round(value * 100) / 100);

/**
 * Why the renderer cannot draw `ad` now, or undefined. chr-server reads materials
 * from server paths, and point-cloud placements need their cloud in pitch coordinates.
 */
export function localProblem(ad: LocalAd, cloud?: LoadedCloud): string | undefined {
  if (ad.space !== 'pitch' && ad.cloudId !== cloud?.id) return '所在点云未导入，无法换算为球场坐标';
  if (ad.space !== 'pitch' && !cloud.aligned) return '点云未确认与球场同坐标';
  if (!ad.materialPath) return ad.material ? '未设置服务器路径' : ad.type === 'flag' ? '未设置图片' : '未设置素材';
  if (ad.type !== '3D' && !/\.(png|jpe?g)$/i.test(ad.materialPath)) return '渲染器只读取 PNG / JPEG 图片';
}

export function localRow(ad: LocalAd, cloud?: LoadedCloud): LocalRow {
  const size = `${number(ad.plane.width)} × ${number(ad.plane.height)} m`;
  const detail = ad.type === 'flag'
    ? `振幅 ${number(ad.flag.amplitude)} m · 频率 ${number(ad.flag.frequency)} · 速度 ${number(ad.flag.speed)}× · ${ad.flag.material === 'pbr' ? 'PBR' : '简易光照'}`
    : ad.type === '3D' ? `缩放 ×${number(ad.scale)} · 朝向 ${number(ad.yaw)}°` : '';
  return {
    id: ad.id, space: ad.space === 'pitch' ? 'pitch' : 'cloud', type: ad.type, badge: ad.type === 'flag' ? '旗帜' : ad.type,
    name: ad.name, visible: ad.visible, place: ad.space === 'pitch' ? '球场' : '点云',
    size: ad.type === '3D' ? `底面 ${size}` : size, detail,
    material: ad.materialPath ?? (ad.material ? `${ad.material.name}（仅本机，未设置服务器路径）` : '未设置素材'),
    problem: localProblem(ad, cloud),
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
