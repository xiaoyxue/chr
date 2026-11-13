import * as THREE from 'three';

export function applyResultCamera(camera: THREE.PerspectiveCamera, Ti: number[], centre: [number, number, number], width: number, height: number) {
  const magnitude = Math.max(...Ti.map(Math.abs));
  if (!Number.isFinite(magnitude) || magnitude === 0 || Ti.length !== 9) throw new Error('结果 Ti 矩阵无效。');
  const h = Ti.map((value) => value / magnitude), scale = Math.sqrt(width * height) / 2;
  const native = [
    ...[0, 1, 2].map((i) => scale * h[i] + width / 2 * h[i + 6]),
    ...[0, 1, 2].map((i) => scale * h[i + 3] + height / 2 * h[i + 6]), ...h.slice(6),
  ];
  const q = [centre[0] / centre[2], centre[1] / centre[2], 1 / centre[2]];
  // P = [H[:,0], H[:,1], -H*q, H[:,2]] preserves Ti on Z=0 and has centre C.
  const rows = [0, 3, 6].map((i) => new THREE.Vector3(native[i], native[i + 1],
    -native.slice(i, i + 3).reduce((sum, value, j) => sum + value * q[j], 0)));
  applyCameraRows(camera, rows, centre, width, height);
}

export function applyAuxiliaryCamera(camera: THREE.PerspectiveCamera, projection: number[], centre: [number, number, number], support: [number, number, number, number]) {
  const [left, top, right, bottom] = support;
  const rows = [0, 4, 8].map((i) => new THREE.Vector3(...projection.slice(i, i + 3)));
  // Align integer auxiliary pixel centres with render-target texel centres, not the mathematical mapping.
  rows[0].addScaledVector(rows[2], 0.5 - left);
  rows[1].addScaledVector(rows[2], 0.5 - top);
  applyCameraRows(camera, rows, centre, right - left + 1, bottom - top + 1);
}

function applyCameraRows(camera: THREE.PerspectiveCamera, rows: THREE.Vector3[], centre: [number, number, number], width: number, height: number) {
  const determinant = new THREE.Matrix3().set(
    rows[0].x, rows[0].y, rows[0].z, rows[1].x, rows[1].y, rows[1].z, rows[2].x, rows[2].y, rows[2].z,
  ).determinant();
  if (!Number.isFinite(determinant) || determinant === 0) throw new Error('结果投影退化，停止广告叠加。');
  rows.forEach((row) => row.multiplyScalar(Math.sign(determinant)));
  const k33 = rows[2].length(), r3 = rows[2].clone().divideScalar(k33);
  const k23 = rows[1].dot(r3), residual = rows[1].clone().addScaledVector(r3, -k23), k22 = residual.length();
  if (k22 <= rows[1].length() * 1e-10) throw new Error('结果投影不稳定，停止广告叠加。');
  const r2 = residual.divideScalar(k22), r1 = new THREE.Vector3().crossVectors(r2, r3).normalize(), k11 = rows[0].dot(r1);
  if (k11 <= rows[0].length() * 1e-10) throw new Error('无法恢复等价相机焦距，停止叠加。');
  const fx = k11 / k33, skew = rows[0].dot(r2) / k33, cx = rows[0].dot(r3) / k33;
  const fy = k22 / k33, cy = k23 / k33;
  const rotation = new THREE.Matrix4().set(
    r1.x, r1.y, r1.z, 0, r2.x, r2.y, r2.z, 0, r3.x, r3.y, r3.z, 0, 0, 0, 0, 1,
  );
  camera.position.set(...centre);
  camera.quaternion.setFromRotationMatrix(rotation.transpose().multiply(new THREE.Matrix4().makeScale(1, -1, -1)));
  camera.aspect = width / height;
  const { near, far } = camera;
  camera.projectionMatrix.set(
    2 * fx / width, -2 * skew / width, 1 - 2 * cx / width, 0,
    0, 2 * fy / height, 2 * cy / height - 1, 0,
    0, 0, -(far + near) / (far - near), -2 * far * near / (far - near),
    0, 0, -1, 0,
  );
  camera.projectionMatrixInverse.copy(camera.projectionMatrix).invert();
  camera.updateMatrixWorld(true);
}
