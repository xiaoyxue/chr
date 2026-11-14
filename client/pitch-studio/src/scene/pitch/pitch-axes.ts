import * as THREE from 'three';
import type { FieldDimensions } from '../../shared/types.ts';

// Drawn just above the grass (up is -Z) and over everything else, as a top-view reference only.
const HEIGHT = -0.08;
const COLORS = { x: '#ff7b6e', y: '#6fd3ff', origin: '#ffffff', size: '#f2f3dd' };
const metres = (value: number) => String(Math.round(value * 100) / 100);

function strip(from: THREE.Vector2, to: THREE.Vector2, width: number, material: THREE.Material) {
  const direction = to.clone().sub(from), length = direction.length();
  const mesh = new THREE.Mesh(new THREE.PlaneGeometry(length, width), material);
  mesh.position.set((from.x + to.x) / 2, (from.y + to.y) / 2, HEIGHT);
  mesh.rotation.z = Math.atan2(direction.y, direction.x);
  return mesh;
}

function arrowHead(tip: THREE.Vector2, direction: THREE.Vector2, material: THREE.Material) {
  const side = new THREE.Vector2(-direction.y, direction.x).multiplyScalar(0.9);
  const base = tip.clone().addScaledVector(direction, -2.2);
  const shape = new THREE.Shape([tip, base.clone().add(side), base.clone().sub(side)]);
  const mesh = new THREE.Mesh(new THREE.ShapeGeometry(shape), material);
  mesh.position.z = HEIGHT;
  return mesh;
}

function label(text: string, color: string, position: THREE.Vector2, height: number) {
  const canvas = document.createElement('canvas'), font = '700 66px system-ui, "Microsoft YaHei", sans-serif';
  let context = canvas.getContext('2d');
  context.font = font;
  canvas.width = Math.ceil(context.measureText(text).width + 28);
  canvas.height = 96;
  // Resizing the canvas resets the context state.
  context = canvas.getContext('2d');
  context.font = font;
  context.textAlign = 'center';
  context.textBaseline = 'middle';
  context.lineWidth = 14;
  context.lineJoin = 'round';
  context.strokeStyle = '#08100cf0';
  context.strokeText(text, canvas.width / 2, 50);
  context.fillStyle = color;
  context.fillText(text, canvas.width / 2, 50);
  const texture = new THREE.CanvasTexture(canvas);
  texture.colorSpace = THREE.SRGBColorSpace;
  const sprite = new THREE.Sprite(new THREE.SpriteMaterial({ map: texture, depthTest: false, depthWrite: false, toneMapped: false }));
  sprite.scale.set(height * canvas.width / canvas.height, height, 1);
  sprite.position.set(position.x, position.y, HEIGHT);
  return sprite;
}

/** A dimension line from `from` to `to` with end ticks across it and its length labelled beside it. */
function dimension(from: THREE.Vector2, to: THREE.Vector2, text: string, labelOffset: THREE.Vector2, material: THREE.Material) {
  const group = new THREE.Group(), unit = to.clone().sub(from).normalize(), across = new THREE.Vector2(-unit.y, unit.x);
  group.add(strip(from, to, 0.12, material));
  for (const end of [from, to]) group.add(strip(end.clone().addScaledVector(across, -1), end.clone().addScaledVector(across, 1), 0.12, material));
  group.add(label(text, COLORS.size, from.clone().add(to).multiplyScalar(0.5).add(labelOffset), 2.6));
  return group;
}

/**
 * Pitch-centred world X/Y axes with metre ticks every 5 m and labels every 10 m,
 * the pitch length and width, and the coordinates of its four corners.
 * +X points right and +Y down in the top view.
 */
export function createWorldAxes({ length, width }: FieldDimensions) {
  const group = new THREE.Group();
  group.name = 'world-axes';
  const material = (color: string, opacity = 0.95) => new THREE.MeshBasicMaterial({ color, transparent: true, opacity,
    depthTest: false, depthWrite: false, side: THREE.DoubleSide, toneMapped: false });
  const axes = [
    { name: 'X', half: length / 2, unit: new THREE.Vector2(1, 0), color: COLORS.x },
    { name: 'Y', half: width / 2, unit: new THREE.Vector2(0, 1), color: COLORS.y },
  ];
  for (const { name, half, unit, color } of axes) {
    const surface = material(color), faint = material(color, 0.75);
    const across = new THREE.Vector2(-unit.y, unit.x);
    const tip = unit.clone().multiplyScalar(half + 4);
    group.add(strip(unit.clone().multiplyScalar(-half), tip.clone().addScaledVector(unit, -2), 0.18, surface));
    group.add(arrowHead(tip, unit, surface));
    group.add(label(`+${name}`, color, tip.clone().addScaledVector(unit, 1.6).addScaledVector(across, name === 'X' ? -2.6 : 3.4), 3.2));
    for (let metres = -Math.floor(half / 5) * 5; metres <= half; metres += 5) {
      if (metres === 0) continue;
      const major = metres % 10 === 0, centre = unit.clone().multiplyScalar(metres);
      group.add(strip(centre.clone().addScaledVector(across, major ? -0.8 : -0.45), centre.clone().addScaledVector(across, major ? 0.8 : 0.45), 0.12, faint));
      if (major) group.add(label(String(metres), color, centre.clone().addScaledVector(across, name === 'X' ? 2.2 : -3.2), 2.4));
    }
  }
  const origin = new THREE.Mesh(new THREE.RingGeometry(0.45, 0.7, 24), material(COLORS.origin));
  origin.position.z = HEIGHT;
  group.add(origin);
  group.add(label('0', COLORS.origin, new THREE.Vector2(-2, -2), 2.4));
  const size = material(COLORS.size, 0.9), halfX = length / 2, halfY = width / 2;
  // Outside the touchline and the left goal line, beyond the pitch surround.
  // The length label sits off-centre, where the top caption and pointer readout are not.
  group.add(dimension(new THREE.Vector2(-halfX, -halfY - 6.5), new THREE.Vector2(halfX, -halfY - 6.5), `长 ${metres(length)} m`, new THREE.Vector2(-length / 4, -1.8), size));
  group.add(dimension(new THREE.Vector2(-halfX - 9, -halfY), new THREE.Vector2(-halfX - 9, halfY), `宽 ${metres(width)} m`, new THREE.Vector2(-5.2, 0), size));
  for (const [sx, sy] of [[-1, -1], [1, -1], [1, 1], [-1, 1]]) {
    const corner = new THREE.Vector2(sx * halfX, sy * halfY);
    const dot = new THREE.Mesh(new THREE.CircleGeometry(0.5, 20), material(COLORS.size));
    dot.position.set(corner.x, corner.y, HEIGHT);
    group.add(dot);
    // Inside the corner, clear of the dimension lines and the preview's caption.
    group.add(label(`(${metres(corner.x)}, ${metres(corner.y)})`, COLORS.size, corner.clone().add(new THREE.Vector2(-sx * 6.2, -sy * 1.8)), 1.8));
  }
  group.traverse((object) => { object.renderOrder = 1500; });
  return group;
}

export function disposeWorldAxes(group: THREE.Group) {
  group.traverse((object) => {
    if (!(object instanceof THREE.Mesh || object instanceof THREE.Sprite)) return;
    if (object instanceof THREE.Mesh) object.geometry.dispose();
    const material = object.material as THREE.MeshBasicMaterial | THREE.SpriteMaterial;
    material.map?.dispose();
    material.dispose();
  });
  group.removeFromParent();
}
