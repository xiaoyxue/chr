import * as THREE from 'three';
import type { FieldDimensions } from '../../shared/types.ts';

export const CAMERA_PRESETS = [
  { id: 'top', code: 'TOP', name: '全场俯视机位', description: '点击选中广告位 · 解锁后拖动移动、拖角点调整大小' },
  { id: 'main', code: 'CAM 01', name: '主看台机位', description: '赛前近似预览 · 可拖动镜头、滚轮变焦' },
] as const;

export function fitTopCamera(camera: THREE.PerspectiveCamera, { width, length }: FieldDimensions) {
  camera.fov = 45;
  const tangent = Math.tan(THREE.MathUtils.degToRad(camera.fov) / 2);
  const distance = Math.max((length / 2 + 8) / (tangent * camera.aspect), (width / 2 + 8) / tangent) * 1.08;
  camera.up.set(0, -1, 0);
  camera.position.set(0, 0, -distance);
  camera.lookAt(0, 0, 0);
  camera.updateProjectionMatrix();
  camera.updateMatrixWorld();
}
