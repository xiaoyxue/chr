import * as THREE from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js';
import type { Placement, ModelPlacement } from '../shared/types.ts';
import { resolveFusionAsset } from '../matches/offline-package.ts';
import { simplifyForPreview } from './preview-simplify.ts';
import type { GlbBounds, SlotGeometry } from '../render/render-description.ts';
import { motionRows, wireMotion, type WireMotion } from './model-motion.ts';

/**
 * Where a Fusion model stands: Fusion's heading (`modelPose.yaw`) and size factor fit
 * its footprint into the region (the neutral pose). The studio's scale (`scaleX`) and
 * rotation (`rz`, about the model's own up axis, glTF +Y) then act about the neutral
 * bottom centre without refitting, so the bottom stays on the ground and the model may
 * leave its region.
 */
export function fitFusionModel(width: number, depth: number, slot: ModelPlacement) {
  const { region, modelPose: pose } = slot;
  if (!region || !pose || ![width, depth].every(Number.isFinite) || width + depth <= 0)
    throw new Error('The Fusion model has no valid ground footprint.');
  const heading = pose.yaw * Math.PI / 180;
  const cosine = Math.abs(Math.cos(heading)), sine = Math.abs(Math.sin(heading));
  const rotatedWidth = width * cosine + depth * sine;
  const rotatedDepth = width * sine + depth * cosine;
  const neutral = Math.min(region.width / Math.max(rotatedWidth, 1e-8), region.height / Math.max(rotatedDepth, 1e-8)) * pose.scale;
  return { scale: neutral * slot.scaleX, yaw: heading + slot.rz * Math.PI / 180,
    x: slot.x + pose.x * Math.max(0, region.width - rotatedWidth * neutral) / 2,
    y: slot.y + pose.y * Math.max(0, region.height - rotatedDepth * neutral) / 2 };
}

function dispose(root: THREE.Object3D, environment?: THREE.Texture) {
  const textures = new Set<THREE.Texture>();
  root.traverse((object) => {
    if (!(object instanceof THREE.Mesh || object instanceof THREE.Line)) return;
    object.geometry.dispose();
    for (const material of Array.isArray(object.material) ? object.material : [object.material]) {
      for (const value of Object.values(material)) if (value instanceof THREE.Texture && value !== environment) textures.add(value);
      material.dispose();
    }
  });
  textures.forEach((texture) => texture.dispose());
  root.removeFromParent();
}

export class FusionCreatives {
  readonly group = new THREE.Group();
  private readonly models = new Map<string, { group: THREE.Group; width: number; depth: number; bounds: GlbBounds; motion?: WireMotion }>();
  /** Preview time zero of the models' motion: the first animated frame after loading. */
  private started?: number;
  private readonly planes = new Map<string, THREE.Group>();
  private active = true;
  private slots: Placement[] = [];
  private hiddenPlacementIds: ReadonlySet<string> = new Set();
  private readonly environment: THREE.WebGLRenderTarget;
  constructor(renderer: THREE.WebGLRenderer) {
    this.group.name = 'fusion-creatives';
    const generator = new THREE.PMREMGenerator(renderer), room = new RoomEnvironment();
    this.environment = generator.fromScene(room, 0.04);
    room.dispose();
    generator.dispose();
  }
  async load(slots: Placement[], hiddenPlacementIds: ReadonlySet<string> = new Set()) {
    this.slots = slots;
    this.hiddenPlacementIds = hiddenPlacementIds;
    try {
      await Promise.all(slots.map(async (slot) => {
        if (!slot.asset) {
          if (slot.type === '2D' || slot.region) {
            const holder = new THREE.Group();
            holder.visible = !this.hiddenPlacementIds.has(slot.id);
            holder.position.set(slot.x, slot.y, -0.01);
            holder.add(new THREE.Mesh(new THREE.PlaneGeometry(1, 1),
              new THREE.MeshBasicMaterial({ color: '#c7a85a', opacity: 0.25, transparent: true, side: THREE.DoubleSide })));
            holder.userData.placementId = slot.id;
            this.planes.set(slot.id, holder);
            this.group.add(holder);
          }
          return;
        }
        const resource = await resolveFusionAsset(slot.asset);
        try {
          if (slot.type === '2D') {
            const texture = await new THREE.TextureLoader().loadAsync(resource.url);
            if (!this.active) { texture.dispose(); return; }
            texture.colorSpace = THREE.SRGBColorSpace;
            const image: unknown = texture.image;
            if (!(image instanceof HTMLImageElement) || !image.naturalWidth || !image.naturalHeight) {
              texture.dispose();
              throw new Error('The Fusion image has invalid dimensions.');
            }
            const mesh = new THREE.Mesh(new THREE.PlaneGeometry(image.naturalWidth, image.naturalHeight),
              new THREE.MeshBasicMaterial({ map: texture, transparent: true, side: THREE.DoubleSide,
                polygonOffset: true, polygonOffsetFactor: -2, polygonOffsetUnits: -2 }));
            mesh.rotation.x = Math.PI;
            const holder = new THREE.Group();
            holder.visible = !this.hiddenPlacementIds.has(slot.id);
            holder.userData.imageSize = { width: image.naturalWidth, height: image.naturalHeight };
            holder.add(mesh);
            holder.userData.placementId = slot.id;
            this.planes.set(slot.id, holder);
            this.group.add(holder);
          } else {
            const manager = new THREE.LoadingManager();
            manager.setURLModifier((url) => {
              if (/^(blob:|data:)/.test(url)) return url;
              throw new Error('Only embedded GLB resources are supported.');
            });
            const response = await fetch(resource.url, { cache: resource.cached ? 'default' : 'no-store' });
            if (!response.ok) throw new Error(`Model download failed (${response.status}).`);
            const gltf = await new GLTFLoader(manager).parseAsync(await response.arrayBuffer(), '');
            if (!this.active) { dispose(gltf.scene); return; }
            const bounds = new THREE.Box3().setFromObject(gltf.scene);
            const size = bounds.getSize(new THREE.Vector3()), center = bounds.getCenter(new THREE.Vector3());
            if (![...bounds.min, ...bounds.max].every(Number.isFinite) || size.x + size.z <= 0) {
              dispose(gltf.scene);
              throw new Error('The model has no finite ground footprint.');
            }
            const thinned = await simplifyForPreview(gltf.scene);
            if (thinned.after < thinned.before)
              console.info(`[preview] ${slot.asset.name}: ${thinned.before} → ${thinned.after} triangles (chr-server renders the full model)`);
            if (!this.active) { dispose(gltf.scene); return; }
            const centered = new THREE.Group();
            centered.add(gltf.scene);
            centered.position.set(-center.x, -bounds.min.y, -center.z);
            const upright = new THREE.Group();
            upright.add(centered);
            upright.rotation.x = -Math.PI / 2;
            const holder = new THREE.Group();
            // Its matrix is set from the neutral pose and, when animated, its motion.
            holder.matrixAutoUpdate = false;
            holder.visible = !this.hiddenPlacementIds.has(slot.id);
            holder.add(upright);
            holder.userData.placementId = slot.id;
            holder.traverse((object) => {
              if (!(object instanceof THREE.Mesh)) return;
              object.castShadow = true;
              object.receiveShadow = true;
              for (const material of Array.isArray(object.material) ? object.material : [object.material])
                if (material instanceof THREE.MeshStandardMaterial) material.envMap = this.environment.texture;
            });
            this.models.set(slot.id, { group: holder, width: size.x, depth: size.z,
              bounds: { min: [bounds.min.x, bounds.min.y, bounds.min.z], max: [bounds.max.x, bounds.max.y, bounds.max.z] } });
            this.group.add(holder);
          }
        } finally { resource.release(); }
      }));
      if (this.active) this.update(this.slots);
    } catch (error) {
      this.dispose();
      throw error;
    }
  }
  update(slots: Placement[], hiddenPlacementIds: ReadonlySet<string> = this.hiddenPlacementIds) {
    this.slots = slots;
    this.hiddenPlacementIds = hiddenPlacementIds;
    for (const slot of slots) {
      const plane = this.planes.get(slot.id);
      if (plane) {
        plane.visible = !hiddenPlacementIds.has(slot.id);
        plane.position.set(slot.x, slot.y, -0.02);
        const imageSize = plane.userData.imageSize;
        if (imageSize && slot.type === '2D') {
          const rotated = slot.assetRotation === 90 || slot.assetRotation === 270;
          const scale = Math.min(slot.sizeX / (rotated ? imageSize.height : imageSize.width),
            slot.sizeY / (rotated ? imageSize.width : imageSize.height));
          const image = plane.children[0] as THREE.Mesh<THREE.PlaneGeometry, THREE.MeshBasicMaterial>;
          image.scale.setScalar(scale);
          image.material.opacity = slot.assetOpacity;
        } else {
          const width = slot.type === '2D' ? slot.sizeX : slot.region.width;
          const height = slot.type === '2D' ? slot.sizeY : slot.region.height;
          plane.children[0].scale.set(width, height, 1);
        }
        plane.rotation.z = THREE.MathUtils.degToRad(slot.rz + (imageSize ? slot.assetRotation ?? 0 : 0));
      }
      const model = this.models.get(slot.id);
      if (slot.type === '3D' && model) {
        model.group.visible = !hiddenPlacementIds.has(slot.id);
        const fit = fitFusionModel(model.width, model.depth, slot);
        model.group.position.set(fit.x, fit.y, slot.z - 0.015);
        model.group.rotation.set(0, 0, fit.yaw);
        model.group.scale.setScalar(fit.scale);
        model.group.updateMatrix();
        model.motion = slot.modelPose && wireMotion(slot.modelPose, [fit.x, fit.y, slot.z - 0.015]);
      }
    }
  }
  /**
   * Poses animated models at preview time `seconds` (#157), as chr-server does from the
   * scene's first frame; returns whether any model moves, so the caller keeps rendering.
   */
  animate(seconds: number) {
    let moving = false;
    for (const model of this.models.values()) {
      if (!model.motion || !model.group.visible) continue;
      this.started ??= seconds;
      model.group.updateMatrix();
      // motionRows is row-major; fromArray reads column-major.
      model.group.matrix.premultiply(new THREE.Matrix4().fromArray(motionRows(model.motion, seconds - this.started).flat()).transpose());
      model.group.matrixWorldNeedsUpdate = true;
      moving = true;
    }
    return moving;
  }
  /**
   * The world XY axis-aligned bounding box of a placement's loaded model, from its
   * transformed vertices; undefined without a loaded, enabled model.
   */
  footprint(id: string) {
    const model = this.models.get(id);
    if (!model?.group.visible) return undefined;
    model.group.updateMatrixWorld(true);
    const box = new THREE.Box3().setFromObject(model.group, true);
    return { minX: box.min.x, maxX: box.max.x, minY: box.min.y, maxY: box.max.y };
  }
  /** Loaded material geometry of each placement, for the render description. */
  geometry(): Map<string, SlotGeometry> {
    const result = new Map<string, SlotGeometry>();
    for (const [id, model] of this.models) result.set(id, { kind: 'glb', bounds: model.bounds });
    for (const [id, plane] of this.planes) {
      const size = plane.userData.imageSize as { width: number; height: number } | undefined;
      if (size) result.set(id, { kind: 'image', width: size.width, height: size.height });
    }
    return result;
  }
  dispose() {
    if (!this.active) return;
    this.active = false;
    dispose(this.group, this.environment.texture);
    this.environment.dispose();
    this.models.clear();
    this.planes.clear();
  }
}
