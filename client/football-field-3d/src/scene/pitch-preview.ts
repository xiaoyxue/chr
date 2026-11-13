import * as THREE from 'three';
import { FusionCreatives } from '../placements/fusion-creatives.ts';
import { validateDimensions } from './pitch/dimensions.ts';
import { containsGroundPlacement, isPlacementInside } from './pitch/pitch-layout.ts';
import { CAMERA_PRESETS, fitTopCamera } from './pitch/pitch-cameras.ts';
import { createPitch, disposeSceneObject } from './pitch/pitch-scene.ts';
import { DEFAULT_CAMERA_POSITION, applyPreviewCamera, clampPreviewLens, defaultPreviewLens } from './cameras/preview-camera.ts';
import { createPreviewLighting } from './lighting/preview-lighting.ts';
import { LocalAdScene, disposeLocalObject } from '../placements/local-ad-scene.ts';
import type { AdPlane, LocalAd } from '../placements/local-ad.ts';
import type { FieldDimensions, Placement, PreviewLens } from '../shared/types.ts';

type CameraId = typeof CAMERA_PRESETS[number]['id'];
type Drag = { id: number } & (
  { kind: 'placement'; start: THREE.Vector3; slot: Placement; candidate: Placement }
  | { kind: 'lens'; x: number; y: number; lens: PreviewLens }
);

export class PitchPreview {
  private readonly renderer: THREE.WebGLRenderer;
  private readonly scene = new THREE.Scene();
  private readonly camera = new THREE.PerspectiveCamera(45, 16 / 9, 1, 2000);
  private readonly lighting = createPreviewLighting();
  private readonly raycaster = new THREE.Raycaster();
  private readonly ground = new THREE.Plane(new THREE.Vector3(0, 0, 1), 0);
  private readonly resizeObserver: ResizeObserver;
  private readonly views: { id: CameraId; button: HTMLButtonElement; canvas: HTMLCanvasElement; camera: THREE.PerspectiveCamera }[] = [];
  private dimensions?: FieldDimensions;
  private field?: THREE.Group;
  private overlay?: THREE.Group;
  private creatives?: FusionCreatives;
  private slots: Placement[] = [];
  private hiddenIds: ReadonlySet<string> = new Set();
  private selectedId?: string;
  private cameraId: CameraId = 'top';
  private lens = defaultPreviewLens(DEFAULT_CAMERA_POSITION);
  private drag?: Drag;
  private onSelect: (id: string) => void;
  private onMove: (slot: Placement) => void;
  private available = true;
  private renderRequest = 0;
  private readonly localAds = new LocalAdScene(true);
  private draw?: (plane: AdPlane) => void;
  private rectangle?: { id: number; start: THREE.Vector3; plane?: AdPlane };
  private draft?: THREE.LineLoop;

  constructor(private readonly viewport: HTMLElement, cameraList: HTMLElement, onError: (error: Error) => void) {
    this.renderer = new THREE.WebGLRenderer({ antialias: true, powerPreference: 'high-performance' });
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.renderer.toneMapping = THREE.ACESFilmicToneMapping;
    this.renderer.toneMappingExposure = 0.9;
    this.renderer.shadowMap.enabled = true;
    this.renderer.shadowMap.type = THREE.PCFShadowMap;
    this.scene.background = new THREE.Color('#1a2821');
    this.scene.add(this.lighting.group);
    this.scene.add(this.localAds.group);
    const canvas = this.renderer.domElement;
    canvas.tabIndex = 0;
    canvas.setAttribute('role', 'img');
    this.viewport.append(canvas);
    for (const preset of CAMERA_PRESETS) {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'camera-card';
      button.dataset.cameraId = preset.id;
      button.setAttribute('aria-label', `${preset.code} ${preset.name}`);
      const thumbnail = document.createElement('canvas');
      thumbnail.width = 320;
      thumbnail.height = 180;
      thumbnail.setAttribute('aria-hidden', 'true');
      const wrap = document.createElement('div');
      wrap.className = 'thumbnail-wrap';
      wrap.append(thumbnail);
      for (const [className, text] of [['thumbnail-number', preset.code], ['camera-selected', ''], ['camera-card-name', preset.name]]) {
        const label = document.createElement('span');
        label.className = className;
        label.textContent = text;
        if (className === 'camera-card-name') button.append(label);
        else wrap.append(label);
      }
      button.prepend(wrap);
      button.addEventListener('click', () => {
        this.resetDrag();
        if (this.draw) {
          this.cancelGroundRectangle();
          this.viewport.dispatchEvent(new CustomEvent('rectangledrawcancel'));
        }
        this.cameraId = preset.id;
        this.refresh();
      });
      cameraList.append(button);
      this.views.push({ id: preset.id, button, canvas: thumbnail, camera: new THREE.PerspectiveCamera(45, 16 / 9, 1, 2000) });
    }
    this.bindControls(canvas);
    this.resizeObserver = new ResizeObserver(() => this.requestRefresh());
    this.resizeObserver.observe(viewport);
    canvas.addEventListener('webglcontextlost', (event) => {
      event.preventDefault();
      this.available = false;
      this.resizeObserver.disconnect();
      cancelAnimationFrame(this.renderRequest);
      onError(new Error('三维预览连接已中断，请刷新页面并检查浏览器硬件加速。'));
    });
  }

  async activate(slots: Placement[], selectedId: string, onSelect: (id: string) => void, onMove: (slot: Placement) => void, field: FieldDimensions, hiddenIds: readonly string[] = []) {
    if (!this.available) throw new Error('三维预览不可用，请刷新页面并开启浏览器硬件加速。');
    const dimensions = validateDimensions(field.length, field.width);
    this.cancelGroundRectangle();
    this.resetDrag();
    this.creatives?.dispose();
    this.creatives = undefined;
    if (this.field) disposeSceneObject(this.field);
    this.dimensions = dimensions;
    this.field = createPitch(dimensions);
    this.scene.add(this.field);
    this.cameraId = 'top';
    this.lens = defaultPreviewLens(DEFAULT_CAMERA_POSITION);
    this.setPlacements(slots, selectedId, onSelect, onMove, hiddenIds);
    const loading = this.viewport.querySelector<HTMLElement>('#loading');
    loading.hidden = false;
    loading.textContent = '正在加载场次广告素材…';
    this.creatives = new FusionCreatives(this.renderer);
    this.scene.add(this.creatives.group);
    try {
      await this.creatives.load(this.slots, this.hiddenIds);
    } catch (error) {
      loading.textContent = '素材加载失败，请刷新场次配置重试。';
      throw error;
    }
    loading.hidden = true;
    this.lighting.markShadows();
    this.refresh();
  }

  setPlacements(slots: Placement[], selectedId: string, onSelect: (id: string) => void, onMove: (slot: Placement) => void, hiddenIds: readonly string[] = []) {
    this.resetDrag();
    this.slots = slots.map((slot) => ({ ...slot }));
    this.selectedId = selectedId;
    this.hiddenIds = new Set(hiddenIds);
    this.onSelect = onSelect;
    this.onMove = onMove;
    this.updatePlacements();
    this.refresh();
  }

  /** Loaded material geometry of each placement; empty until the materials load. */
  renderGeometry() {
    return this.creatives?.geometry() ?? new Map();
  }

  resetView() {
    this.resetDrag();
    this.lens = defaultPreviewLens(DEFAULT_CAMERA_POSITION);
    this.refresh();
  }

  async setLocalAds(ads: LocalAd[], selectedId?: string) {
    try { await this.localAds.update(ads, selectedId); }
    finally { this.refresh(); }
  }

  startGroundRectangle(complete: (plane: AdPlane) => void) {
    if (!this.available || !this.dimensions) throw new Error('球场预览尚未就绪。');
    this.resetDrag();
    this.cancelGroundRectangle();
    this.draw = complete;
    this.cameraId = 'top';
    this.viewport.dataset.drawingRectangle = 'true';
    this.refresh();
  }

  cancelGroundRectangle() {
    const pointerId = this.rectangle?.id;
    this.draw = undefined;
    this.rectangle = undefined;
    if (pointerId !== undefined && this.renderer.domElement.hasPointerCapture(pointerId)) this.renderer.domElement.releasePointerCapture(pointerId);
    if (this.draft) disposeLocalObject(this.draft);
    this.draft = undefined;
    this.viewport.dataset.drawingRectangle = 'false';
    this.requestRefresh();
  }

  refresh() {
    if (!this.available || !this.dimensions || !this.viewport.clientWidth || !this.viewport.clientHeight) return;
    this.renderer.setPixelRatio(1);
    for (const view of this.views) {
      this.configureCamera(view.camera, view.id);
      this.renderer.setSize(320, 180, false);
      this.renderer.render(this.scene, view.camera);
      view.canvas.getContext('2d').drawImage(this.renderer.domElement, 0, 0);
      view.button.setAttribute('aria-pressed', String(view.id === this.cameraId));
      const preset = CAMERA_PRESETS.find((item) => item.id === view.id);
      view.button.setAttribute('aria-label', preset.name);
    }
    const preset = CAMERA_PRESETS.find((item) => item.id === this.cameraId);
    const preview = this.viewport.parentElement;
    preview.querySelector('#active-camera-id').textContent = preset.code;
    preview.querySelector('#active-camera-name').textContent = preset.name;
    preview.querySelector('#active-camera-description').textContent = preset.description;
    preview.querySelector('#active-ad-name').textContent = this.slots.find((slot) => slot.id === this.selectedId)?.name ?? '场次广告位';
    this.viewport.dataset.cameraId = this.cameraId;
    this.viewport.dataset.previewMode = 'free';
    this.renderer.domElement.setAttribute('aria-label', `${preset.name} · 球场 2D / 3D 广告位预览`);
    this.configureCamera(this.camera, this.cameraId);
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
    this.renderer.setSize(Math.max(this.viewport.clientWidth, 1), Math.max(this.viewport.clientHeight, 1), false);
    this.renderer.render(this.scene, this.camera);
  }

  dispose() {
    this.available = false;
    this.resetDrag();
    this.resizeObserver.disconnect();
    cancelAnimationFrame(this.renderRequest);
    this.creatives?.dispose();
    this.localAds.dispose();
    this.cancelGroundRectangle();
    if (this.field) disposeSceneObject(this.field);
    if (this.overlay) disposeSceneObject(this.overlay);
    this.lighting.dispose();
    this.renderer.dispose();
  }

  private configureCamera(camera: THREE.PerspectiveCamera, id: CameraId) {
    if (id === 'main') {
      camera.near = 1;
      camera.far = 2000;
      applyPreviewCamera(camera, DEFAULT_CAMERA_POSITION, this.lens);
    }
    else { camera.aspect = 16 / 9; fitTopCamera(camera, this.dimensions); }
  }

  private requestRefresh() {
    if (this.renderRequest || !this.available) return;
    this.renderRequest = requestAnimationFrame(() => { this.renderRequest = 0; this.refresh(); });
  }

  private updatePlacements() {
    this.creatives?.update(this.slots, this.hiddenIds);
    this.lighting.markShadows();
    if (this.overlay) disposeSceneObject(this.overlay);
    this.overlay = new THREE.Group();
    for (const slot of this.slots) {
      if (slot.type !== '2D' || this.hiddenIds.has(slot.id)) continue;
      const selected = slot.id === this.selectedId;
      const group = new THREE.Group();
      const surface = new THREE.MeshBasicMaterial({ color: '#ffac48', transparent: true, opacity: selected ? 0.5 : 0.12, depthWrite: false, side: THREE.DoubleSide, polygonOffset: true, polygonOffsetFactor: -2, polygonOffsetUnits: -2 });
      const mesh = new THREE.Mesh(new THREE.PlaneGeometry(slot.sizeX, slot.sizeY), surface);
      mesh.rotation.x = Math.PI;
      group.add(mesh);
      const corners = [[-slot.sizeX / 2, -slot.sizeY / 2], [slot.sizeX / 2, -slot.sizeY / 2], [slot.sizeX / 2, slot.sizeY / 2], [-slot.sizeX / 2, slot.sizeY / 2]];
      group.add(new THREE.LineLoop(new THREE.BufferGeometry().setFromPoints(corners.map(([x, y]) => new THREE.Vector3(x, y, 0))), new THREE.LineBasicMaterial({ color: selected ? '#ffd297' : '#829b75' })));
      if (selected) {
        const geometry = new THREE.SphereGeometry(0.3, 10, 8), material = new THREE.MeshBasicMaterial({ color: '#ffe2b7' });
        for (const [x, y] of corners) {
          const marker = new THREE.Mesh(geometry, material);
          marker.position.set(x, y, 0);
          group.add(marker);
        }
      }
      group.position.set(slot.x, slot.y, 0);
      group.rotation.z = THREE.MathUtils.degToRad(slot.rz);
      this.overlay.add(group);
    }
    this.scene.add(this.overlay);
  }

  private groundPoint(event: PointerEvent) {
    const rect = this.renderer.domElement.getBoundingClientRect();
    this.raycaster.setFromCamera(new THREE.Vector2((event.clientX - rect.left) / rect.width * 2 - 1, 1 - (event.clientY - rect.top) / rect.height * 2), this.camera);
    return this.raycaster.ray.intersectPlane(this.ground, new THREE.Vector3());
  }

  private visiblePlacementId(object: THREE.Object3D) {
    let id: string;
    for (let parent = object; parent; parent = parent.parent) {
      if (!parent.visible) return undefined;
      if (typeof parent.userData.placementId === 'string') id = parent.userData.placementId;
    }
    return id && !this.hiddenIds.has(id) && this.slots.some((slot) => slot.id === id) ? id : undefined;
  }

  private resetDrag() {
    const drag = this.drag;
    if (!drag) return;
    this.drag = undefined;
    if (drag.kind === 'placement') {
      this.slots = this.slots.map((slot) => slot.id === drag.slot.id ? drag.slot : slot);
      this.updatePlacements();
    }
    if (this.renderer.domElement.hasPointerCapture(drag.id)) this.renderer.domElement.releasePointerCapture(drag.id);
  }

  private bindControls(canvas: HTMLCanvasElement) {
    canvas.addEventListener('pointerdown', (event) => {
      if (!this.available || !this.dimensions || event.button !== 0 || !event.isPrimary) return;
      if (this.draw) {
        const point = this.groundPoint(event);
        if (!point || Math.abs(point.x) > this.dimensions.length / 2 || Math.abs(point.y) > this.dimensions.width / 2) {
          this.viewport.dispatchEvent(new CustomEvent('rectangledrawmiss'));
          return;
        }
        this.rectangle = { id: event.pointerId, start: point };
        canvas.setPointerCapture(event.pointerId);
        canvas.focus({ preventScroll: true });
        event.preventDefault();
        return;
      }
      if (this.cameraId === 'main') this.drag = { kind: 'lens', id: event.pointerId, x: event.clientX, y: event.clientY, lens: { ...this.lens } };
      else {
        const point = this.groundPoint(event);
        if (!point) return;
        const id = this.creatives && this.raycaster.intersectObject(this.creatives.group, true).map((hit) => this.visiblePlacementId(hit.object)).find(Boolean);
        const slot = id ? this.slots.find((item) => item.id === id)
          : [...this.slots].reverse().find((item) => item.type === '2D' && !this.hiddenIds.has(item.id) && containsGroundPlacement(item, point));
        if (!slot) return;
        this.onSelect(slot.id);
        this.drag = { kind: 'placement', id: event.pointerId, start: point, slot: { ...slot }, candidate: { ...slot } };
      }
      event.preventDefault();
      canvas.setPointerCapture(event.pointerId);
      canvas.focus({ preventScroll: true });
    });
    canvas.addEventListener('pointermove', (event) => {
      if (this.rectangle?.id === event.pointerId) {
        const point = this.groundPoint(event);
        if (!point) return;
        point.x = THREE.MathUtils.clamp(point.x, -this.dimensions.length / 2, this.dimensions.length / 2);
        point.y = THREE.MathUtils.clamp(point.y, -this.dimensions.width / 2, this.dimensions.width / 2);
        const start = this.rectangle.start;
        const plane: AdPlane = { origin: [Math.min(start.x, point.x), Math.min(start.y, point.y), 0],
          u: [1, 0, 0], v: [0, 1, 0], width: Math.abs(point.x - start.x), height: Math.abs(point.y - start.y) };
        this.rectangle.plane = plane;
        if (this.draft) disposeLocalObject(this.draft);
        const [x, y] = plane.origin;
        this.draft = new THREE.LineLoop(new THREE.BufferGeometry().setFromPoints([
          new THREE.Vector3(x, y, -0.04), new THREE.Vector3(x + plane.width, y, -0.04),
          new THREE.Vector3(x + plane.width, y + plane.height, -0.04), new THREE.Vector3(x, y + plane.height, -0.04),
        ]), new THREE.LineBasicMaterial({ color: '#ddff54', depthTest: false }));
        this.scene.add(this.draft);
        this.requestRefresh();
        return;
      }
      const drag = this.drag;
      if (!drag || drag.id !== event.pointerId) return;
      if (drag.kind === 'lens') this.lens = clampPreviewLens({ ...drag.lens, pan: drag.lens.pan - (event.clientX - drag.x) * 0.12, tilt: drag.lens.tilt + (event.clientY - drag.y) * 0.12 });
      else {
        const point = this.groundPoint(event);
        if (!point) return;
        const candidate = { ...drag.slot, x: drag.slot.x + point.x - drag.start.x, y: drag.slot.y + point.y - drag.start.y };
        if (Math.abs(candidate.x) > this.dimensions.length / 2 || Math.abs(candidate.y) > this.dimensions.width / 2
          || (candidate.type === '2D' && !isPlacementInside(candidate, this.dimensions))
          || (candidate.type === '3D' && candidate.region && !isPlacementInside({ x: candidate.x, y: candidate.y, sizeX: candidate.region.width, sizeY: candidate.region.height }, this.dimensions))) return;
        drag.candidate = candidate;
        this.slots = this.slots.map((slot) => slot.id === candidate.id ? candidate : slot);
        this.updatePlacements();
      }
      this.requestRefresh();
    });
    canvas.addEventListener('pointerup', (event) => {
      if (this.rectangle?.id === event.pointerId) {
        const plane = this.rectangle.plane, complete = this.draw;
        this.rectangle = undefined;
        if (canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
        if (plane && plane.width >= 0.1 && plane.height >= 0.1) {
          this.cancelGroundRectangle();
          complete(plane);
        } else this.viewport.dispatchEvent(new CustomEvent('rectangledrawmiss'));
        return;
      }
      const drag = this.drag;
      if (!drag || drag.id !== event.pointerId) return;
      this.drag = undefined;
      if (canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
      if (drag.kind === 'placement') this.onMove(drag.candidate);
    });
    for (const type of ['pointercancel', 'lostpointercapture'] as const) canvas.addEventListener(type, (event) => {
      if (this.rectangle?.id === event.pointerId) {
        this.cancelGroundRectangle();
        this.viewport.dispatchEvent(new CustomEvent('rectangledrawcancel'));
      }
      if (this.drag?.id !== event.pointerId) return;
      this.resetDrag();
      this.refresh();
    });
    canvas.addEventListener('wheel', (event) => {
      if (this.cameraId !== 'main') return;
      event.preventDefault();
      const pixels = event.deltaY * (event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? canvas.clientHeight : 1);
      this.lens = clampPreviewLens({ ...this.lens, zoom: this.lens.zoom * Math.exp(-pixels * 0.001) });
      this.requestRefresh();
    }, { passive: false });
    canvas.addEventListener('keydown', (event) => {
      if (event.key === 'Escape' && this.draw) {
        event.preventDefault();
        this.cancelGroundRectangle();
        this.viewport.dispatchEvent(new CustomEvent('rectangledrawcancel'));
        return;
      }
      if (event.key === 'Escape' && this.drag) {
        event.preventDefault();
        this.resetDrag();
        this.refresh();
        return;
      }
      if (this.cameraId !== 'main') return;
      const next = { ...this.lens };
      if (event.key === 'ArrowLeft') next.pan -= 2;
      else if (event.key === 'ArrowRight') next.pan += 2;
      else if (event.key === 'ArrowUp') next.tilt += 2;
      else if (event.key === 'ArrowDown') next.tilt -= 2;
      else if (event.key === '+' || event.key === '=') next.zoom *= 1.1;
      else if (event.key === '-') next.zoom /= 1.1;
      else if (event.key === 'Home') { event.preventDefault(); this.resetView(); return; }
      else return;
      event.preventDefault();
      this.lens = clampPreviewLens(next);
      this.requestRefresh();
    });
  }
}
