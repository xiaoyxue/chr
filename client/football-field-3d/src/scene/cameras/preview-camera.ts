import * as THREE from 'three';
import type { PreviewLens } from '../../shared/types.ts';

export const DEFAULT_CAMERA_POSITION = Object.freeze([0, 90, -35]);

export function defaultPreviewLens(position: readonly number[]): PreviewLens {
  return {
    pan: THREE.MathUtils.radToDeg(Math.atan2(-position[0], position[1])),
    tilt: THREE.MathUtils.radToDeg(Math.atan2(Math.hypot(position[0], position[1]), -position[2])),
    zoom: 1,
  };
}

export function clampPreviewLens({ pan, tilt, zoom }: PreviewLens): PreviewLens {
  if (![pan, tilt, zoom].every(Number.isFinite)) throw new Error('预览镜头参数必须是有效数字。');
  return {
    pan: THREE.MathUtils.clamp(pan, -90, 90),
    tilt: THREE.MathUtils.clamp(tilt, 5, 89),
    zoom: THREE.MathUtils.clamp(zoom, 0.5, 6),
  };
}

export function applyPreviewCamera(camera: THREE.PerspectiveCamera, position: readonly number[], lens: PreviewLens) {
  const { pan, tilt, zoom } = clampPreviewLens(lens);
  camera.position.set(position[0], position[1], position[2]);
  const panRotation = new THREE.Quaternion().setFromAxisAngle(new THREE.Vector3(0, 0, 1), THREE.MathUtils.degToRad(pan));
  const tiltRotation = new THREE.Quaternion().setFromAxisAngle(new THREE.Vector3(1, 0, 0), THREE.MathUtils.degToRad(tilt));
  const basis = new THREE.Quaternion().setFromAxisAngle(new THREE.Vector3(1, 0, 0), Math.PI);
  camera.quaternion.copy(panRotation).multiply(tiltRotation).multiply(basis);
  camera.aspect = 16 / 9;
  camera.fov = THREE.MathUtils.radToDeg(2 * Math.atan(Math.tan(THREE.MathUtils.degToRad(55) / 2) / zoom));
  camera.updateProjectionMatrix();
  camera.updateMatrixWorld();
}
