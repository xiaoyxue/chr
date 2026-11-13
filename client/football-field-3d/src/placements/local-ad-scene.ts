import * as THREE from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
import type { AdPlane, LocalAd, Vector3Tuple } from './local-ad.ts';

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

async function createContent(ad: LocalAd, placeholders: boolean) {
  const group = new THREE.Group();
  if (ad.type === '2D') {
    let texture: THREE.Texture;
    if (ad.material) {
      const url = URL.createObjectURL(ad.material);
      try { texture = await new THREE.TextureLoader().loadAsync(url); }
      finally { URL.revokeObjectURL(url); }
      texture.colorSpace = THREE.SRGBColorSpace;
      texture.flipY = false;
    }
    if (texture || placeholders) group.add(new THREE.Mesh(new THREE.PlaneGeometry(1, 1),
      new THREE.MeshBasicMaterial({ map: texture, color: texture ? '#ffffff' : '#f5bc50', transparent: true,
        opacity: texture ? 1 : 0.35, side: THREE.DoubleSide, toneMapped: false,
        polygonOffset: true, polygonOffsetFactor: -2, polygonOffsetUnits: -2 })));
  } else if (ad.material) {
    const manager = new THREE.LoadingManager();
    manager.setURLModifier((url) => {
      if (/^(blob:|data:)/.test(url)) return url;
      throw new Error('GLB 必须内嵌所有素材，不允许加载外部资源。');
    });
    const gltf = await new GLTFLoader(manager).parseAsync(await ad.material.arrayBuffer(), '');
    const bounds = new THREE.Box3().setFromObject(gltf.scene), size = bounds.getSize(new THREE.Vector3()), centre = bounds.getCenter(new THREE.Vector3());
    if (![...bounds.min, ...bounds.max].every(Number.isFinite) || size.x + size.z <= 0) {
      disposeLocalObject(gltf.scene);
      throw new Error('3D 素材缺少有效的底面尺寸。');
    }
    gltf.scene.position.set(-centre.x, -bounds.min.y, -centre.z);
    const model = new THREE.Group();
    model.add(gltf.scene);
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
  const content = await createContent(ad, false);
  disposeLocalObject(content);
}

export class LocalAdScene {
  readonly group = new THREE.Group();
  private entries = new Map<string, { ad: LocalAd; root: THREE.Group }>();
  private version = 0;
  constructor(private placeholders: boolean) { this.group.name = 'local-advertisements'; }

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
      const { plane } = ad, u = new THREE.Vector3(...plane.u), v = new THREE.Vector3(...plane.v), normal = new THREE.Vector3().crossVectors(u, v).normalize();
      entry.root.position.copy(new THREE.Vector3(...plane.origin).addScaledVector(u, plane.width / 2).addScaledVector(v, plane.height / 2).addScaledVector(normal, ad.offset + 0.015 * ad.normalSign));
      entry.root.quaternion.setFromRotationMatrix(new THREE.Matrix4().makeBasis(u, v, normal));
      entry.root.visible = ad.visible;
      if (ad.type === '2D') {
        entry.root.children[0]?.scale.set(plane.width, plane.height, 1);
      } else {
        const content = entry.root.children[0];
        if (content) {
          const footprint: number[] = entry.root.userData.footprint;
          if (footprint) {
            content.rotation.set(ad.normalSign * Math.PI / 2, ad.yaw * Math.PI / 180, 0);
            content.scale.setScalar(Math.min(plane.width / footprint[0], plane.height / footprint[1]) * ad.scale);
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
