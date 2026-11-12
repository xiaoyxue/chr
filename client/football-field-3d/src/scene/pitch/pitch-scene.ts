import * as THREE from 'three';
import type { FieldDimensions } from '../../shared/types.ts';

function material(color: THREE.ColorRepresentation, options: THREE.MeshBasicMaterialParameters = {}) {
  return new THREE.MeshBasicMaterial({ color, toneMapped: false, ...options });
}

function box(group: THREE.Group, length: number, width: number, height: number, x: number, y: number, z: number, surface: THREE.Material) {
  const mesh = new THREE.Mesh(new THREE.BoxGeometry(length, width, height), surface);
  mesh.position.set(x, y, z);
  group.add(mesh);
}

function markings(group: THREE.Group, points: number[][], surface: THREE.LineBasicMaterial, closed = false) {
  const geometry = new THREE.BufferGeometry().setFromPoints(points.map(([x, y]) => new THREE.Vector3(x, y, 0)));
  group.add(closed ? new THREE.LineLoop(geometry, surface) : new THREE.Line(geometry, surface));
}

function arc(group: THREE.Group, x: number, y: number, radius: number, start: number, end: number, surface: THREE.LineBasicMaterial) {
  markings(group, Array.from({ length: 97 }, (_, i) => {
    const angle = start + (end - start) * i / 96;
    return [x + Math.cos(angle) * radius, y + Math.sin(angle) * radius];
  }), surface);
}

function spot(group: THREE.Group, x: number, y: number, surface: THREE.Material) {
  const mesh = new THREE.Mesh(new THREE.CircleGeometry(0.18, 20), surface);
  mesh.position.set(x, y, 0);
  group.add(mesh);
}

function pole(group: THREE.Group, start: number[], end: number[], surface: THREE.Material) {
  const a = new THREE.Vector3(...start), b = new THREE.Vector3(...end);
  const direction = b.clone().sub(a);
  const mesh = new THREE.Mesh(new THREE.CylinderGeometry(0.06, 0.06, direction.length(), 10), surface);
  mesh.position.copy(a).add(b).multiplyScalar(0.5);
  mesh.quaternion.setFromUnitVectors(new THREE.Vector3(0, 1, 0), direction.normalize());
  mesh.castShadow = true;
  group.add(mesh);
}

function goal(group: THREE.Group, end: number, surface: THREE.Material, netSurface: THREE.LineBasicMaterial, length: number) {
  const front = end * length / 2, back = front + end * 2, half = 7.32 / 2;
  pole(group, [front, -half, 0], [front, -half, -2.44], surface);
  pole(group, [front, half, 0], [front, half, -2.44], surface);
  pole(group, [front, -half, -2.44], [front, half, -2.44], surface);
  for (const y of [-half, half]) {
    pole(group, [front, y, -2.44], [back, y, -2.44], surface);
    pole(group, [back, y, -2.44], [back, y, 0], surface);
    pole(group, [back, y, 0], [front, y, 0], surface);
  }
  pole(group, [back, -half, 0], [back, half, 0], surface);
  const points: THREE.Vector3[] = [];
  const segment = (a: number[], b: number[]) => points.push(new THREE.Vector3(...a), new THREE.Vector3(...b));
  for (let i = 0; i <= 30; i++) {
    const y = -half + 7.32 * i / 30;
    segment([back, y, 0], [back, y, -2.44]);
    segment([back, y, -2.44], [front, y, -2.44]);
  }
  for (let i = 0; i <= 10; i++) {
    const z = -2.44 * i / 10;
    segment([back, -half, z], [back, half, z]);
    for (const y of [-half, half]) segment([front, y, z], [back, y, z]);
    const x = front + (back - front) * i / 10;
    segment([x, -half, -2.44], [x, half, -2.44]);
    for (const y of [-half, half]) segment([x, y, 0], [x, y, -2.44]);
  }
  group.add(new THREE.LineSegments(new THREE.BufferGeometry().setFromPoints(points), netSurface));
}

export function disposeSceneObject(group: THREE.Object3D) {
  const geometries = new Set<THREE.BufferGeometry>(), materials = new Set<THREE.Material>();
  group.traverse((object) => {
    if (!(object instanceof THREE.Mesh || object instanceof THREE.Line || object instanceof THREE.Points)) return;
    geometries.add(object.geometry);
    for (const surface of Array.isArray(object.material) ? object.material : [object.material]) materials.add(surface);
  });
  geometries.forEach((geometry) => geometry.dispose());
  materials.forEach((surface) => surface.dispose());
  group.removeFromParent();
}

export function createPitch({ length, width }: FieldDimensions) {
  const group = new THREE.Group();
  const grass = ['#4d913f', '#438338'].map((color) => material(color, { polygonOffset: true, polygonOffsetFactor: 0, polygonOffsetUnits: 1 }));
  const white = material('#f2f3dd', { side: THREE.DoubleSide, polygonOffset: true, polygonOffsetFactor: -1, polygonOffsetUnits: -1 });
  const lineSurface = new THREE.LineBasicMaterial({ color: '#edf2d7' });
  const netSurface = new THREE.LineBasicMaterial({ color: '#d6e0d3', transparent: true, opacity: 0.45 });
  box(group, length + 12, width + 10, 1.6, 0, 0, 1.15, material('#334a38'));
  box(group, length + 11.6, width + 9.6, 0.3, 0, 0, 0.2, material('#416743'));
  for (let i = 0; i < 20; i++) {
    box(group, length / 20, width, 0.1, -length / 2 + (i + 0.5) * length / 20, 0, 0.05, grass[i % 2]);
  }
  markings(group, [[-length / 2, -width / 2], [length / 2, -width / 2], [length / 2, width / 2], [-length / 2, width / 2]], lineSurface, true);
  markings(group, [[0, -width / 2], [0, width / 2]], lineSurface);
  arc(group, 0, 0, 9.15, 0, Math.PI * 2, lineSurface);
  spot(group, 0, 0, white);
  for (const end of [-1, 1]) {
    const x = end * length / 2;
    for (const [halfWidth, depth] of [[20.16, 16.5], [9.16, 5.5]]) {
      markings(group, [[x, -halfWidth], [x - end * depth, -halfWidth], [x - end * depth, halfWidth], [x, halfWidth]], lineSurface);
    }
    const penaltyX = x - end * 11;
    spot(group, penaltyX, 0, white);
    const angle = Math.acos(5.5 / 9.15), centerAngle = end === 1 ? Math.PI : 0;
    arc(group, penaltyX, 0, 9.15, centerAngle - angle, centerAngle + angle, lineSurface);
    for (const side of [-1, 1]) {
      const start = end === 1 ? (side === 1 ? Math.PI : Math.PI / 2) : (side === 1 ? -Math.PI / 2 : 0);
      arc(group, x, side * width / 2, 1, start, start + Math.PI / 2, lineSurface);
    }
    goal(group, end, white, netSurface, length);
  }
  return group;
}
