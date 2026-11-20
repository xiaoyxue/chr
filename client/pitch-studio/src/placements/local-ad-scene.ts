import * as THREE from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
import { simplifyForPreview } from './preview-simplify.ts';
import { flagWave } from './flag.ts';
import { adRoot, modelFit } from './local-ad-transform.ts';
import type { GlbBounds } from '../render/render-description.ts';
import { imageOpacity, type AdPlane, type LocalAd, type Vector3Tuple } from './local-ad.ts';

export function planeFromPoints(points: Vector3Tuple[]): AdPlane {
  if (points.length !== 3) throw new Error('请选择三个点确定广告平面。');
  const [a, b, c] = points.map((point) => new THREE.Vector3(...point));
  const u = b.sub(a), width = u.length();
  if (width < 0.1) throw new Error('第一点与第二点过近，请重新选点。');
  u.normalize();
  const v = c.sub(a);
  v.addScaledVector(u, -v.dot(u));
  const height = v.length();
  if (height < 0.1) throw new Error('三点共线或过近，无法确定广告平面。');
  if (width > 1000 || height > 1000) throw new Error('广告区域过大，请缩小范围。');
  v.normalize();
  return { origin: a.toArray(), u: u.toArray(), v: v.toArray(), width, height };
}

export function disposeLocalObject(root: THREE.Object3D) {
  const textures = new Set<THREE.Texture>();
  root.traverse((object) => {
    if (!(object instanceof THREE.Mesh || object instanceof THREE.Line || object instanceof THREE.Points)) return;
    object.geometry.dispose();
    for (const material of Array.isArray(object.material) ? object.material : [object.material]) {
      for (const value of Object.values(material)) if (value instanceof THREE.Texture) textures.add(value);
      material.dispose();
    }
  });
  textures.forEach((texture) => {
    if (texture.image instanceof ImageBitmap) texture.image.close();
    texture.dispose();
  });
  root.removeFromParent();
}

const FLAG_COLUMNS = 96, FLAG_ROWS = 60;

/** A flag grid of 97 × 61 vertices; its texture v runs down from the image's top row. */
function flagGeometry() {
  const uvs: number[] = [], indices: number[] = [];
  for (let j = 0; j <= FLAG_ROWS; j++) for (let i = 0; i <= FLAG_COLUMNS; i++) uvs.push(i / FLAG_COLUMNS, j / FLAG_ROWS);
  for (let j = 0; j < FLAG_ROWS; j++) for (let i = 0; i < FLAG_COLUMNS; i++) {
    const a = j * (FLAG_COLUMNS + 1) + i, b = a + FLAG_COLUMNS + 1;
    indices.push(a, b, a + 1, a + 1, b, b + 1);
  }
  const geometry = new THREE.BufferGeometry();
  geometry.setAttribute('position', new THREE.Float32BufferAttribute(new Float32Array(uvs.length / 2 * 3), 3));
  geometry.setAttribute('uv', new THREE.Float32BufferAttribute(uvs, 2));
  geometry.setIndex(indices);
  return geometry;
}

/** Moves the flag's vertices to the renderer's wave after `seconds`, in flag-local metres. */
function waveFlag(geometry: THREE.BufferGeometry, ad: LocalAd, seconds: number) {
  const position = geometry.getAttribute('position') as THREE.BufferAttribute, { width, height } = ad.plane;
  for (let j = 0, k = 0; j <= FLAG_ROWS; j++) for (let i = 0; i <= FLAG_COLUMNS; i++, k++) {
    position.setXYZ(k, ...flagWave(i / FLAG_COLUMNS, j / FLAG_ROWS, width, height, ad.flag, ad.flag.speed * seconds));
  }
  position.needsUpdate = true;
  geometry.computeVertexNormals();
  geometry.computeBoundingSphere();
}

async function parseGlb(file: File) {
  const manager = new THREE.LoadingManager();
  manager.setURLModifier((url) => {
    if (/^(blob:|data:)/.test(url)) return url;
    throw new Error('GLB 必须内嵌所有素材，不允许加载外部资源。');
  });
  const gltf = await new GLTFLoader(manager).parseAsync(await file.arrayBuffer(), '');
  const bounds = new THREE.Box3().setFromObject(gltf.scene), size = bounds.getSize(new THREE.Vector3());
  if (![...bounds.min, ...bounds.max].every(Number.isFinite) || size.x + size.z <= 0) {
    disposeLocalObject(gltf.scene);
    throw new Error('3D 素材缺少有效的底面尺寸。');
  }
  return { scene: gltf.scene, bounds };
}

/** A GLB's bounds in its own Y-up glTF space, as the preview fits it. */
export async function glbBounds(file: File): Promise<GlbBounds> {
  const { scene, bounds } = await parseGlb(file);
  disposeLocalObject(scene);
  return { min: bounds.min.toArray(), max: bounds.max.toArray() };
}

async function loadTexture(file: File) {
  const url = URL.createObjectURL(file);
  let texture: THREE.Texture;
  try { texture = await new THREE.TextureLoader().loadAsync(url); }
  finally { URL.revokeObjectURL(url); }
  texture.colorSpace = THREE.SRGBColorSpace;
  // Texture v then runs down the image, from its top row, like the plane's v axis.
  texture.flipY = false;
  return texture;
}

/** `shown`: the content goes into the preview, so a high-poly model is thinned for it. */
async function createContent(ad: LocalAd, placeholders: boolean, shown = true) {
  const group = new THREE.Group();
  if (ad.type === 'flag') {
    const texture = ad.material && await loadTexture(ad.material);
    if (texture || placeholders) {
      const flag = new THREE.Mesh(flagGeometry(), new THREE.MeshLambertMaterial({ ...(texture ? { map: texture } : {}), color: texture ? '#ffffff' : '#f5bc50',
        transparent: true, opacity: texture ? 1 : 0.5, side: THREE.DoubleSide }));
      flag.name = 'flag';
      group.add(flag);
    }
  } else if (ad.type === '2D') {
    const texture = ad.material && await loadTexture(ad.material);
    if (texture || placeholders) group.add(new THREE.Mesh(new THREE.PlaneGeometry(1, 1),
      new THREE.MeshBasicMaterial({ map: texture, color: texture ? '#ffffff' : '#f5bc50', transparent: true,
        opacity: texture ? 1 : 0.35, side: THREE.DoubleSide, toneMapped: false,
        polygonOffset: true, polygonOffsetFactor: -2, polygonOffsetUnits: -2 })));
  } else if (ad.material) {
    const { scene, bounds } = await parseGlb(ad.material), size = bounds.getSize(new THREE.Vector3()), centre = bounds.getCenter(new THREE.Vector3());
    if (shown) await simplifyForPreview(scene);
    scene.position.set(-centre.x, -bounds.min.y, -centre.z);
    const model = new THREE.Group();
    model.add(scene);
    group.add(model);
    group.userData.footprint = [Math.max(size.x, 0.001), Math.max(size.z, 0.001)];
  } else if (placeholders) {
    const marker = new THREE.Mesh(new THREE.BoxGeometry(1, 1, 1),
      new THREE.MeshBasicMaterial({ color: '#b7df71', wireframe: true }));
    marker.position.z = 0.5;
    group.add(marker);
  }
  return group;
}

export async function validateLocalAdMaterial(ad: LocalAd) {
  const content = await createContent(ad, false, false);
  disposeLocalObject(content);
}

export class LocalAdScene {
  readonly group = new THREE.Group();
  private entries = new Map<string, { ad: LocalAd; root: THREE.Group }>();
  private version = 0;
  private seconds = 0;
  constructor(private placeholders: boolean) { this.group.name = 'local-advertisements'; }

  /** Waves the visible flags to `seconds` (scaled by each flag's speed); false when there is none to animate. */
  animate(seconds: number) {
    this.seconds = seconds;
    let animated = false;
    for (const { ad, root } of this.entries.values()) {
      const flag = ad.type === 'flag' && ad.visible ? root.getObjectByName('flag') as THREE.Mesh | undefined : undefined;
      if (!flag) continue;
      waveFlag(flag.geometry, ad, seconds);
      animated = true;
    }
    return animated;
  }

  async update(ads: LocalAd[], selectedId?: string) {
    const version = ++this.version;
    for (const [id, entry] of this.entries) {
      if (!ads.some((ad) => ad.id === id && ad.material === entry.ad.material && ad.type === entry.ad.type)) {
        disposeLocalObject(entry.root);
        this.entries.delete(id);
      }
    }
    for (const ad of ads) {
      let entry = this.entries.get(ad.id);
      if (!entry) {
        const root = await createContent(ad, this.placeholders);
        if (version !== this.version) { disposeLocalObject(root); return; }
        root.userData.localAdId = ad.id;
        this.group.add(root);
        entry = { ad, root };
        this.entries.set(ad.id, entry);
      }
      entry.ad = ad;
      const { plane } = ad;
      adRoot(ad).decompose(entry.root.position, entry.root.quaternion, entry.root.scale);
      entry.root.visible = ad.visible;
      if (ad.type === 'flag') {
        const flag = entry.root.getObjectByName('flag') as THREE.Mesh | undefined;
        if (flag) {
          waveFlag(flag.geometry, ad, this.seconds);
          // The flag-local origin sits at the top-left corner moved `offset` along the normal, as in flagModel.
          flag.position.set(-plane.width / 2, -plane.height / 2, -0.015 * ad.normalSign);
        }
      } else if (ad.type === '2D') {
        const image = entry.root.children[0] as THREE.Mesh<THREE.PlaneGeometry, THREE.MeshBasicMaterial> | undefined;
        image?.scale.set(plane.width, plane.height, 1);
        if (image?.material.map) image.material.opacity = imageOpacity(ad);
      } else {
        const content = entry.root.children[0];
        if (content) {
          const footprint: number[] = entry.root.userData.footprint;
          if (footprint) {
            const fit = modelFit(ad, footprint as [number, number]);
            content.rotation.copy(fit.rotation);
            content.scale.setScalar(fit.scale);
          } else {
            content.scale.set(plane.width, plane.height, Math.min(plane.width, plane.height) * ad.scale);
            content.position.z = Math.min(plane.width, plane.height) * ad.scale / 2 * ad.normalSign;
          }
        }
      }
      const previous = entry.root.getObjectByName('selection');
      if (previous) disposeLocalObject(previous);
      if (this.placeholders && ad.id === selectedId) {
        const selection = new THREE.Group();
        selection.name = 'selection';
        const outline = new THREE.LineLoop(new THREE.BufferGeometry().setFromPoints([
          new THREE.Vector3(-plane.width / 2, -plane.height / 2, 0.03), new THREE.Vector3(plane.width / 2, -plane.height / 2, 0.03),
          new THREE.Vector3(plane.width / 2, plane.height / 2, 0.03), new THREE.Vector3(-plane.width / 2, plane.height / 2, 0.03),
        ]), new THREE.LineBasicMaterial({ color: '#ddff54', depthTest: false }));
        selection.add(outline);
        const radius = Math.min(0.45, Math.min(plane.width, plane.height) * 0.06);
        const rim = new THREE.MeshBasicMaterial({ color: '#ffffff', depthTest: false, side: THREE.DoubleSide, toneMapped: false });
        const dot = new THREE.MeshBasicMaterial({ color: '#2f8cff', depthTest: false, side: THREE.DoubleSide, toneMapped: false });
        for (const [x, y] of [[-1, -1], [1, -1], [1, 1], [-1, 1]]) {
          for (const [geometry, material] of [[new THREE.RingGeometry(radius, radius * 1.4, 20), rim], [new THREE.CircleGeometry(radius, 20), dot]] as const) {
            const mesh = new THREE.Mesh(geometry, material);
            mesh.position.set(x * plane.width / 2, y * plane.height / 2, 0.035);
            mesh.renderOrder = 1700;
            selection.add(mesh);
          }
        }
        if (ad.type === '3D') {
          const arrow = new THREE.ArrowHelper(new THREE.Vector3(0, 0, ad.normalSign), new THREE.Vector3(), Math.min(plane.width, plane.height) * 0.6, '#ddff54');
          selection.add(arrow);
        }
        entry.root.add(selection);
      }
    }
  }
  dispose() {
    ++this.version;
    this.entries.forEach(({ root }) => disposeLocalObject(root));
    this.entries.clear();
  }
}
