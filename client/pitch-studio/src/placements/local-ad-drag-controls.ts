import * as THREE from 'three';
import type { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import type { LocalAd } from './local-ad.ts';

const anchors = [[0, 0], [0.5, 0], [1, 0], [1, 0.5], [1, 1], [0.5, 1], [0, 1], [0, 0.5]] as const;

export class LocalAdDragControls {
  readonly group = new THREE.Group();
  private readonly material = new THREE.SpriteMaterial({ color: '#ddff54', depthTest: false, depthWrite: false, toneMapped: false });
  private readonly handles = anchors.map(() => new THREE.Sprite(this.material));
  private readonly raycaster = new THREE.Raycaster();
  private readonly listeners = new AbortController();
  private ads: LocalAd[] = [];
  private selectedId?: string;
  private enabled = false;
  /** Resize handles show only while the selected ad is unlocked; clicks still select. */
  private handlesShown = false;
  private frame = 0;
  private drag?: {
    pointerId: number;
    original: LocalAd;
    candidate: LocalAd;
    published?: LocalAd;
    start: THREE.Vector3;
    screen: THREE.Vector2;
    moved: boolean;
    plane: THREE.Plane;
    anchor?: readonly [number, number];
    orbitEnabled: boolean;
  };

  constructor(
    private canvas: HTMLCanvasElement,
    private camera: () => THREE.PerspectiveCamera,
    private advertisements: THREE.Group,
    private orbit: OrbitControls,
    private callbacks: {
      begin: (id: string) => boolean;
      preview: (ad: LocalAd) => void;
      end: (cancelled: boolean) => void;
      refresh: () => void;
    },
  ) {
    this.group.name = 'local-ad-resize-handles';
    for (const handle of this.handles) {
      handle.renderOrder = 2000;
      this.group.add(handle);
    }
    const options = { capture: true, signal: this.listeners.signal };
    canvas.addEventListener('pointerdown', (event) => this.start(event), options);
    canvas.addEventListener('pointermove', (event) => this.move(event), options);
    canvas.addEventListener('pointerup', (event) => {
      if (this.drag?.pointerId !== event.pointerId) return;
      this.move(event);
      this.finish(false);
      event.preventDefault();
      event.stopImmediatePropagation();
    }, options);
    for (const type of ['pointercancel', 'lostpointercapture'] as const) canvas.addEventListener(type, (event) => {
      if (this.drag?.pointerId === event.pointerId) this.cancel();
    }, options);
    canvas.addEventListener('keydown', (event) => {
      if (event.key !== 'Escape' || !this.drag) return;
      this.cancel();
      event.preventDefault();
      event.stopImmediatePropagation();
    }, options);
    window.addEventListener('blur', () => this.cancel(), { signal: this.listeners.signal });
  }

  setEnabled(enabled: boolean) {
    if (!enabled) this.cancel();
    this.enabled = enabled;
    this.group.visible = enabled;
    if (!enabled) this.canvas.style.cursor = '';
    this.callbacks.refresh();
  }

  setHandlesVisible(visible: boolean) {
    this.handlesShown = visible;
    this.callbacks.refresh();
  }

  setAds(ads: LocalAd[], selectedId?: string) {
    if (this.drag && (selectedId !== this.drag.original.id || !ads.some((ad) => ad.id === selectedId && ad.visible))) this.cancel();
    this.ads = ads;
    this.selectedId = selectedId;
  }

  refresh() {
    const ad = this.ads.find((item) => item.id === this.selectedId && item.visible);
    this.group.visible = Boolean(this.enabled && ad && this.handlesShown);
    if (!ad) return;
    const camera = this.camera();
    camera.updateMatrixWorld();
    const { origin, u, v, width, height } = ad.plane;
    const normal = new THREE.Vector3(...u).cross(new THREE.Vector3(...v));
    for (const [index, [a, b]] of anchors.entries()) {
      const handle = this.handles[index];
      handle.position.set(...origin).addScaledVector(new THREE.Vector3(...u), width * a)
        .addScaledVector(new THREE.Vector3(...v), height * b).addScaledVector(normal, ad.offset + 0.015 * ad.normalSign);
      const depth = -handle.position.clone().applyMatrix4(camera.matrixWorldInverse).z;
      handle.visible = depth > camera.near;
      handle.scale.setScalar(12 * 2 * depth * Math.tan(THREE.MathUtils.degToRad(camera.fov / 2)) / Math.max(this.canvas.clientHeight, 1));
    }
  }

  cancel() { this.finish(true); }

  private setRay(event: PointerEvent) {
    const rect = this.canvas.getBoundingClientRect();
    this.raycaster.setFromCamera(new THREE.Vector2(
      (event.clientX - rect.left) / rect.width * 2 - 1,
      1 - (event.clientY - rect.top) / rect.height * 2,
    ), this.camera());
  }

  private hit(event: PointerEvent): { ad: LocalAd; anchor?: readonly [number, number] } | undefined {
    this.refresh();
    const selected = this.ads.find((ad) => ad.id === this.selectedId && ad.visible);
    const rect = this.canvas.getBoundingClientRect();
    if (selected && this.handlesShown) {
      let closest = 11, anchor: readonly [number, number] | undefined;
      for (const [index, handle] of this.handles.entries()) {
        if (!handle.visible) continue;
        const projected = handle.position.clone().project(this.camera());
        if (projected.z < -1 || projected.z > 1) continue;
        const distance = Math.hypot(
          rect.left + (projected.x + 1) * rect.width / 2 - event.clientX,
          rect.top + (1 - projected.y) * rect.height / 2 - event.clientY,
        );
        if (distance < closest) { closest = distance; anchor = anchors[index]; }
      }
      if (anchor) return { ad: selected, anchor };
    }
    this.setRay(event);
    this.advertisements.updateWorldMatrix(true, true);
    for (const hit of this.raycaster.intersectObjects(this.advertisements.children.filter((root) => root.visible), true)) {
      let root: THREE.Object3D = hit.object;
      while (root.parent && root.parent !== this.advertisements) root = root.parent;
      const ad = this.ads.find((item) => item.id === root.userData.localAdId && item.visible);
      if (ad) return { ad };
    }
  }

  private start(event: PointerEvent) {
    if (!this.enabled || this.drag || event.button !== 0 || !event.isPrimary) return;
    const hit = this.hit(event);
    if (!hit) return;
    const { ad, anchor } = hit, normal = new THREE.Vector3(...ad.plane.u).cross(new THREE.Vector3(...ad.plane.v)).normalize();
    const plane = new THREE.Plane().setFromNormalAndCoplanarPoint(normal,
      new THREE.Vector3(...ad.plane.origin).addScaledVector(normal, ad.offset + 0.015 * ad.normalSign));
    this.setRay(event);
    if (Math.abs(this.raycaster.ray.direction.dot(normal)) < 1e-4) return;
    const start = this.raycaster.ray.intersectPlane(plane, new THREE.Vector3());
    if (!start || !this.callbacks.begin(ad.id)) return;
    this.drag = { pointerId: event.pointerId, original: ad, candidate: ad, start, screen: new THREE.Vector2(event.clientX, event.clientY),
      moved: false, plane, anchor, orbitEnabled: this.orbit.enabled };
    this.orbit.enabled = false;
    this.canvas.setPointerCapture(event.pointerId);
    this.canvas.focus({ preventScroll: true });
    this.canvas.style.cursor = anchor ? this.resizeCursor(ad, anchor) : 'move';
    event.preventDefault();
    event.stopImmediatePropagation();
  }

  private move(event: PointerEvent) {
    const drag = this.drag;
    if (!drag) {
      if (this.enabled && !event.buttons) {
        const hit = this.hit(event);
        this.canvas.style.cursor = hit ? hit.anchor ? this.resizeCursor(hit.ad, hit.anchor) : 'move' : '';
      }
      return;
    }
    if (drag.pointerId !== event.pointerId) return;
    if (!drag.moved && drag.screen.distanceTo(new THREE.Vector2(event.clientX, event.clientY)) < 3) {
      event.preventDefault();
      event.stopImmediatePropagation();
      return;
    }
    drag.moved = true;
    this.setRay(event);
    const point = this.raycaster.ray.intersectPlane(drag.plane, new THREE.Vector3());
    if (point) {
      const delta = point.sub(drag.start), { original, anchor } = drag;
      const u = new THREE.Vector3(...original.plane.u), v = new THREE.Vector3(...original.plane.v);
      const origin = new THREE.Vector3(...original.plane.origin);
      let { width, height } = original.plane;
      if (anchor) {
        if (anchor[0] !== 0.5) width = THREE.MathUtils.clamp(width + delta.dot(u) * (anchor[0] === 0 ? -1 : 1), 0.01, 1000);
        if (anchor[1] !== 0.5) height = THREE.MathUtils.clamp(height + delta.dot(v) * (anchor[1] === 0 ? -1 : 1), 0.01, 1000);
        if (original.aspectLocked) {
          const ratio = original.plane.width / original.plane.height;
          if (anchor[0] !== 0.5) height = width / ratio;
          else width = height * ratio;
        }
        // The side opposite the handle stays; an edge handle keeps the other axis centred.
        origin.addScaledVector(u, (original.plane.width - width) * (1 - anchor[0]))
          .addScaledVector(v, (original.plane.height - height) * (1 - anchor[1]));
      } else {
        origin.add(delta);
      }
      drag.candidate = { ...original, plane: { ...original.plane, origin: origin.toArray(), width, height } };
      if (!this.frame) this.frame = requestAnimationFrame(() => { this.frame = 0; this.publish(); });
    }
    event.preventDefault();
    event.stopImmediatePropagation();
  }

  private publish() {
    if (this.drag && this.drag.candidate !== this.drag.original && this.drag.candidate !== this.drag.published) {
      this.drag.published = this.drag.candidate;
      this.callbacks.preview(this.drag.candidate);
    }
  }

  private resizeCursor(ad: LocalAd, anchor: readonly [number, number]) {
    const { origin, u, v, width, height } = ad.plane, camera = this.camera();
    const centre = new THREE.Vector3(...origin).addScaledVector(new THREE.Vector3(...u), width / 2)
      .addScaledVector(new THREE.Vector3(...v), height / 2);
    const endpoint = centre.clone().addScaledVector(new THREE.Vector3(...u), (anchor[0] - 0.5) * width)
      .addScaledVector(new THREE.Vector3(...v), (anchor[1] - 0.5) * height).project(camera);
    centre.project(camera);
    const angle = Math.atan2((centre.y - endpoint.y) * this.canvas.clientHeight, (endpoint.x - centre.x) * this.canvas.clientWidth);
    const direction = ((Math.round(angle / (Math.PI / 4)) % 4) + 4) % 4;
    return ['ew-resize', 'nwse-resize', 'ns-resize', 'nesw-resize'][direction];
  }

  private finish(cancelled: boolean) {
    const drag = this.drag;
    if (!drag) return;
    cancelAnimationFrame(this.frame);
    this.frame = 0;
    if (!cancelled) this.publish();
    this.drag = undefined;
    this.orbit.enabled = drag.orbitEnabled;
    if (this.canvas.hasPointerCapture(drag.pointerId)) this.canvas.releasePointerCapture(drag.pointerId);
    this.canvas.style.cursor = '';
    this.callbacks.end(cancelled);
    this.callbacks.refresh();
  }

  dispose() {
    this.cancel();
    this.listeners.abort();
    this.material.dispose();
    this.group.removeFromParent();
  }
}
