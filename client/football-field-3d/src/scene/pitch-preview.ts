import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { FusionCreatives } from '../placements/fusion-creatives.ts';
import { validateDimensions } from './pitch/dimensions.ts';
import { containsGroundPlacement, isPlacementInside } from './pitch/pitch-layout.ts';
import { CAMERA_PRESETS, fitTopCamera } from './pitch/pitch-cameras.ts';
import { createPitch, disposeSceneObject } from './pitch/pitch-scene.ts';
import { DEFAULT_CAMERA_POSITION, applyPreviewCamera, clampPreviewLens, defaultPreviewLens } from './cameras/preview-camera.ts';
import { createPreviewLighting } from './lighting/preview-lighting.ts';
import type { FieldDimensions, Placement, PreviewLens } from '../shared/types.ts';
import type { PlyCloud } from '../analysis/ply-result.ts';

type CameraId = typeof CAMERA_PRESETS[number]['id'];
type Drag = { id: number } & (
  { kind: 'placement'; start: THREE.Vector3; slot: Placement; candidate: Placement }
  | { kind: 'lens'; x: number; y: number; lens: PreviewLens }
);

export class PitchPreview {
  private readonly renderer: THREE.WebGLRenderer;
  private readonly scene = new THREE.Scene();
  private readonly camera = new THREE.PerspectiveCamera(45, 16 / 9, 1, 2000);
  private readonly cloudCamera = new THREE.PerspectiveCamera(45, 16 / 9, 0.01, 10000);
  private readonly cloudControls: OrbitControls;
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
  private pitchCameraId: CameraId = 'top';
  private analysis?: PlyCloud;
  private points?: THREE.Points<THREE.BufferGeometry, THREE.PointsMaterial>;
  private analysisMode = false;
  private aligned = false;
  private lens = defaultPreviewLens(DEFAULT_CAMERA_POSITION);
  private drag?: Drag;
  private onSelect: (id: string) => void;
  private onMove: (slot: Placement) => void;
  private available = true;
  private renderRequest = 0;

  constructor(private readonly viewport: HTMLElement, cameraList: HTMLElement, onError: (error: Error) => void) {
    this.renderer = new THREE.WebGLRenderer({ antialias: true, powerPreference: 'high-performance' });
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.renderer.toneMapping = THREE.ACESFilmicToneMapping;
    this.renderer.toneMappingExposure = 0.9;
    this.renderer.shadowMap.enabled = true;
    this.renderer.shadowMap.type = THREE.PCFShadowMap;
    this.scene.background = new THREE.Color('#1a2821');
    this.scene.add(this.lighting.group);
    const canvas = this.renderer.domElement;
    canvas.tabIndex = 0;
    canvas.setAttribute('role', 'img');
    this.viewport.append(canvas);
    this.cloudCamera.up.set(0, 0, -1);
    this.cloudControls = new OrbitControls(this.cloudCamera, canvas);
    this.cloudControls.enabled = false;
    this.cloudControls.listenToKeyEvents(canvas);
    this.cloudControls.addEventListener('change', () => this.requestRefresh());
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
        this.cameraId = preset.id;
        this.cloudControls.enabled = this.analysisMode && this.cameraId === 'main';
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
    this.applyVisibility();
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

  resetView() {
    this.resetDrag();
    if (this.analysisMode) this.fitCloudView();
    else this.lens = defaultPreviewLens(DEFAULT_CAMERA_POSITION);
    this.refresh();
  }

  setAnalysis(cloud: PlyCloud | undefined, aligned: boolean, pointSize = 0.15) {
    if (this.analysis !== cloud) {
      this.resetDrag();
      this.disposeCloud();
      this.analysis = cloud;
      if (cloud) {
        const geometry = new THREE.BufferGeometry();
        geometry.setAttribute('position', new THREE.BufferAttribute(cloud.positions, 3));
        if (cloud.colors) geometry.setAttribute('color', new THREE.BufferAttribute(cloud.colors, 3));
        geometry.computeBoundingSphere();
        this.points = new THREE.Points(geometry, new THREE.PointsMaterial({ size: pointSize, vertexColors: Boolean(cloud.colors), color: '#ffffff', toneMapped: false }));
        this.points.name = 'analysis-point-cloud';
        this.scene.add(this.points);
        this.fitCloudView();
      }
    }
    if (this.aligned !== aligned) this.resetDrag();
    this.aligned = aligned;
    this.setPointSize(pointSize);
    this.applyVisibility();
  }

  setAnalysisMode(enabled: boolean) {
    const next = enabled && Boolean(this.analysis);
    if (next !== this.analysisMode) {
      this.resetDrag();
      if (next) { this.pitchCameraId = this.cameraId; this.cameraId = 'main'; }
      else this.cameraId = this.pitchCameraId;
      this.analysisMode = next;
    }
    this.cloudControls.enabled = next && this.cameraId === 'main';
    this.applyVisibility();
    this.refresh();
  }

  setPointSize(size: number) {
    if (!Number.isFinite(size) || size < 0.02 || size > 1) throw new Error('点大小应介于 0.02–1 米。');
    if (this.points) this.points.material.size = size;
    this.requestRefresh();
  }

  refresh() {
    if (!this.available || !this.dimensions) return;
    this.renderer.setPixelRatio(1);
    for (const view of this.views) {
      this.configureCamera(view.camera, view.id);
      this.renderer.setSize(320, 180, false);
      this.renderer.render(this.scene, view.camera);
      view.canvas.getContext('2d').drawImage(this.renderer.domElement, 0, 0);
      view.button.setAttribute('aria-pressed', String(view.id === this.cameraId));
      const preset = CAMERA_PRESETS.find((item) => item.id === view.id);
      const name = this.analysisMode ? view.id === 'top' ? '俯视微调广告位' : '三维点云浏览' : preset.name;
      view.button.querySelector('.camera-card-name').textContent = name;
      view.button.querySelector('.thumbnail-number').textContent = this.analysisMode && view.id === 'main' ? '3D' : preset.code;
      view.button.setAttribute('aria-label', name);
    }
    const preset = CAMERA_PRESETS.find((item) => item.id === this.cameraId);
    const preview = this.viewport.parentElement;
    preview.querySelector('#active-camera-id').textContent = this.analysisMode && this.cameraId === 'main' ? '3D' : preset.code;
    preview.querySelector('#active-camera-name').textContent = this.analysisMode ? this.cameraId === 'top' ? '俯视微调广告位' : '分析结果 · 三维点云' : preset.name;
    preview.querySelector('#active-camera-description').textContent = this.analysisMode
      ? this.cameraId === 'top' ? '在球场坐标内拖动已有点位；Esc 取消本次拖动。' : '拖动旋转 · 右键平移 · 滚轮缩放；切到俯视可拖动广告位。'
      : preset.description;
    preview.querySelector('.model-badge').textContent = this.analysisMode ? this.aligned ? '条件坐标对齐 · 非实测标定' : '坐标未确认 · 仅点云' : '场次广告位 · 仅本地预览';
    preview.querySelector('#active-ad-name').textContent = this.slots.find((slot) => slot.id === this.selectedId)?.name ?? '场次广告位';
    this.viewport.dataset.cameraId = this.cameraId;
    this.viewport.dataset.previewMode = 'free';
    this.viewport.dataset.analysisMode = String(this.analysisMode);
    this.renderer.domElement.setAttribute('aria-label', this.analysisMode ? '导入的彩色点云与场次广告位，条件坐标对齐，不代表视频标定' : `${preset.name} · 仅预览场次配置的广告位，不新增广告位`);
    this.configureCamera(this.camera, this.cameraId);
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
    this.renderer.setSize(Math.max(this.viewport.clientWidth, 1), Math.max(this.viewport.clientHeight, 1), false);
    this.renderer.render(this.scene, this.camera);
  }

  dispose() {
    this.available = false;
    this.resetDrag();
    this.resizeObserver.disconnect();
    this.cloudControls.dispose();
    cancelAnimationFrame(this.renderRequest);
    this.creatives?.dispose();
    this.disposeCloud();
    if (this.field) disposeSceneObject(this.field);
    if (this.overlay) disposeSceneObject(this.overlay);
    this.lighting.dispose();
    this.renderer.dispose();
  }

  private configureCamera(camera: THREE.PerspectiveCamera, id: CameraId) {
    if (this.analysisMode && id === 'main') camera.copy(this.cloudCamera);
    else if (this.analysisMode && !this.aligned) this.fitCloudTop(camera);
    else if (id === 'main') {
      camera.near = 1;
      camera.far = 2000;
      applyPreviewCamera(camera, DEFAULT_CAMERA_POSITION, this.lens);
    }
    else { camera.aspect = 16 / 9; fitTopCamera(camera, this.dimensions); }
  }

  private cloudBounds() {
    return new THREE.Box3(new THREE.Vector3(...this.analysis.minimum), new THREE.Vector3(...this.analysis.maximum));
  }

  private fitCloudView() {
    if (!this.analysis) return;
    const sphere = this.cloudBounds().getBoundingSphere(new THREE.Sphere());
    const radius = Math.max(sphere.radius, 0.01), distance = radius / Math.sin(Math.PI / 8) * 1.1;
    this.cloudCamera.position.copy(sphere.center).addScaledVector(new THREE.Vector3(0, 0.8, -0.6), distance);
    this.cloudCamera.near = Math.max(radius / 10000, 0.001);
    this.cloudCamera.far = distance + radius * 10;
    this.cloudCamera.updateProjectionMatrix();
    this.cloudControls.target.copy(sphere.center);
    this.cloudControls.minDistance = Math.max(radius / 1000, 0.01);
    this.cloudControls.maxDistance = radius * 40;
    this.cloudControls.update();
  }

  private fitCloudTop(camera: THREE.PerspectiveCamera) {
    const sphere = this.cloudBounds().getBoundingSphere(new THREE.Sphere());
    const radius = Math.max(sphere.radius, 0.01), distance = radius / Math.sin(Math.PI / 8) * 1.1;
    camera.aspect = 16 / 9;
    camera.fov = 45;
    camera.up.set(0, -1, 0);
    camera.position.copy(sphere.center).add(new THREE.Vector3(0, 0, -distance));
    camera.lookAt(sphere.center);
    camera.near = Math.max(radius / 10000, 0.001);
    camera.far = distance + radius * 10;
    camera.updateProjectionMatrix();
    camera.updateMatrixWorld();
  }

  private disposeCloud() {
    if (!this.points) return;
    this.points.geometry.dispose();
    this.points.material.dispose();
    this.points.removeFromParent();
    this.points = undefined;
  }

  private applyVisibility() {
    if (this.field) this.field.visible = !this.analysisMode;
    if (this.points) this.points.visible = this.analysisMode;
    const overlays = !this.analysisMode || this.aligned;
    if (this.creatives) this.creatives.group.visible = overlays;
    if (this.overlay) this.overlay.visible = overlays;
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
    this.applyVisibility();
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
      if (this.analysisMode && (this.cameraId === 'main' || !this.aligned)) return;
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
      const drag = this.drag;
      if (!drag || drag.id !== event.pointerId) return;
      this.drag = undefined;
      if (canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
      if (drag.kind === 'placement') this.onMove(drag.candidate);
    });
    for (const type of ['pointercancel', 'lostpointercapture'] as const) canvas.addEventListener(type, (event) => {
      if (this.drag?.id !== event.pointerId) return;
      this.resetDrag();
      this.refresh();
    });
    canvas.addEventListener('wheel', (event) => {
      if (this.cameraId !== 'main' || this.analysisMode) return;
      event.preventDefault();
      const pixels = event.deltaY * (event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? canvas.clientHeight : 1);
      this.lens = clampPreviewLens({ ...this.lens, zoom: this.lens.zoom * Math.exp(-pixels * 0.001) });
      this.requestRefresh();
    }, { passive: false });
    canvas.addEventListener('keydown', (event) => {
      if (event.key === 'Escape' && this.drag) {
        event.preventDefault();
        this.resetDrag();
        this.refresh();
        return;
      }
      if (this.cameraId !== 'main') return;
      if (this.analysisMode) {
        if (event.key === 'Home') { event.preventDefault(); this.resetView(); }
        return;
      }
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
