import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { LocalAdScene, disposeLocalObject } from '../placements/local-ad-scene.ts';
import { LocalAdDragControls } from '../placements/local-ad-drag-controls.ts';
import type { LocalAd, Vector3Tuple } from '../placements/local-ad.ts';
import type { PlyCloud } from '../analysis/ply-result.ts';

export class PointCloudPreview {
  private readonly renderer: THREE.WebGLRenderer;
  private readonly scene = new THREE.Scene();
  private readonly orbitCamera = new THREE.PerspectiveCamera(45, 16 / 9, 0.01, 10000);
  private readonly topCamera = new THREE.PerspectiveCamera(45, 16 / 9, 0.01, 10000);
  private readonly controls: OrbitControls;
  private readonly raycaster = new THREE.Raycaster();
  private readonly resizeObserver: ResizeObserver;
  private readonly localAds = new LocalAdScene(true);
  private readonly adControls: LocalAdDragControls;
  private readonly pickMarkers = new THREE.Group();
  private cloud?: PlyCloud;
  private points?: THREE.Points<THREE.BufferGeometry, THREE.PointsMaterial>;
  private mode: 'orbit' | 'top' = 'orbit';
  private pick?: { points: Vector3Tuple[]; progress: (count: number) => void; complete: (points: Vector3Tuple[], normalSign: number) => void };
  private gesture?: { id: number; x: number; y: number };
  private tolerance = 0.3;
  private available = true;
  private renderRequest = 0;
  get isAvailable() { return this.available; }
  onAdEditStart?: (id: string) => boolean;
  onAdEditPreview?: (ad: LocalAd) => void;
  onAdEditEnd?: (cancelled: boolean) => void;

  constructor(private readonly viewport: HTMLElement, onError: (error: Error) => void) {
    this.renderer = new THREE.WebGLRenderer({ antialias: true, powerPreference: 'high-performance' });
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.renderer.toneMapping = THREE.ACESFilmicToneMapping;
    this.scene.background = new THREE.Color('#101e27');
    this.scene.add(this.localAds.group, this.pickMarkers, new THREE.HemisphereLight('#ffffff', '#b4c4a0', 2));
    const light = new THREE.DirectionalLight('#ffffff', 3);
    light.position.set(-20, 10, -50);
    this.scene.add(light);
    const canvas = this.renderer.domElement;
    canvas.tabIndex = 0;
    canvas.setAttribute('role', 'img');
    canvas.setAttribute('aria-label', '独立点云广告绘制区：原始点云与点云广告位');
    viewport.append(canvas);
    this.orbitCamera.up.set(0, 0, -1);
    this.topCamera.up.set(0, -1, 0);
    this.controls = new OrbitControls(this.orbitCamera, canvas);
    this.controls.listenToKeyEvents(canvas);
    this.controls.addEventListener('change', () => this.requestRefresh());
    this.adControls = new LocalAdDragControls(canvas,
      () => this.mode === 'orbit' ? this.orbitCamera : this.topCamera, this.localAds.group, this.controls, {
        begin: (id) => this.onAdEditStart?.(id) ?? false,
        preview: (ad) => this.onAdEditPreview?.(ad),
        end: (cancelled) => this.onAdEditEnd?.(cancelled),
        refresh: () => this.requestRefresh(),
      });
    this.scene.add(this.adControls.group);
    this.bindPicking(canvas);
    this.resizeObserver = new ResizeObserver(() => this.requestRefresh());
    this.resizeObserver.observe(viewport);
    canvas.addEventListener('webglcontextlost', (event) => {
      event.preventDefault();
      this.available = false;
      this.cancelPlanePick();
      this.adControls.setEnabled(false);
      this.resizeObserver.disconnect();
      cancelAnimationFrame(this.renderRequest);
      onError(new Error('点云预览连接已中断，请刷新页面并检查浏览器硬件加速。'));
    });
  }

  setCloud(cloud?: PlyCloud, pointSize = 0.15) {
    if (cloud !== this.cloud) {
      this.cancelPlanePick();
      this.cancelAdDrag();
      this.disposePoints();
      this.cloud = cloud;
      if (cloud) {
        const geometry = new THREE.BufferGeometry();
        geometry.setAttribute('position', new THREE.BufferAttribute(cloud.positions, 3));
        if (cloud.colors) geometry.setAttribute('color', new THREE.BufferAttribute(cloud.colors, 3));
        geometry.computeBoundingSphere();
        this.points = new THREE.Points(geometry, new THREE.PointsMaterial({
          size: pointSize, vertexColors: Boolean(cloud.colors), color: '#ffffff', toneMapped: false,
        }));
        this.points.name = 'analysis-point-cloud';
        this.scene.add(this.points);
        this.mode = 'orbit';
        this.viewport.dispatchEvent(new CustomEvent('cloudviewchange', { detail: this.mode }));
        this.fitCameras();
      }
    }
    this.controls.enabled = Boolean(cloud && this.mode === 'orbit' && this.available);
    this.setPointSize(pointSize);
    this.requestRefresh();
  }

  async setLocalAds(ads: LocalAd[], selectedId?: string) {
    this.adControls.setAds(ads, selectedId);
    try { await this.localAds.update(ads, selectedId); }
    finally { this.requestRefresh(); }
  }

  setView(mode: 'orbit' | 'top') {
    this.cancelAdDrag();
    this.mode = mode;
    this.controls.enabled = mode === 'orbit' && Boolean(this.cloud) && this.available;
    this.refresh();
  }

  resetView() { this.cancelAdDrag(); this.fitCameras(); this.refresh(); }

  setAdEditingEnabled(enabled: boolean) { this.adControls.setEnabled(enabled && this.available && Boolean(this.cloud)); }
  cancelAdDrag() { this.adControls.cancel(); }

  startPlanePick(progress: (count: number) => void, complete: (points: Vector3Tuple[], normalSign: number) => void) {
    if (!this.available || !this.points) throw new Error('请先在下方点云区导入 PLY 文件。');
    this.cancelPlanePick();
    this.cancelAdDrag();
    this.pick = { points: [], progress, complete };
    this.viewport.dataset.pickingPlane = 'true';
  }

  cancelPlanePick() {
    this.pick = undefined;
    this.gesture = undefined;
    this.viewport.dataset.pickingPlane = 'false';
    for (const child of [...this.pickMarkers.children]) disposeLocalObject(child);
    this.requestRefresh();
  }

  setPickTolerance(value: number) {
    if (!Number.isFinite(value) || value < 0.01 || value > 10) throw new Error('选点半径需介于 0.01–10 个坐标单位。');
    this.tolerance = value;
  }

  setPointSize(size: number) {
    if (!Number.isFinite(size) || size < 0.02 || size > 1) throw new Error('点大小应介于 0.02–1 个原始坐标单位。');
    if (this.points) this.points.material.size = size;
    this.requestRefresh();
  }

  refresh() {
    if (!this.available || !this.cloud || !this.viewport.clientWidth || !this.viewport.clientHeight) return;
    const camera = this.mode === 'orbit' ? this.orbitCamera : this.topCamera;
    camera.aspect = this.viewport.clientWidth / this.viewport.clientHeight;
    camera.updateProjectionMatrix();
    this.viewport.dataset.cloudView = this.mode;
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
    this.renderer.setSize(this.viewport.clientWidth, this.viewport.clientHeight, false);
    this.adControls.refresh();
    this.renderer.render(this.scene, camera);
  }

  private fitCameras() {
    if (!this.cloud) return;
    const bounds = new THREE.Box3(new THREE.Vector3(...this.cloud.minimum), new THREE.Vector3(...this.cloud.maximum));
    const sphere = bounds.getBoundingSphere(new THREE.Sphere()), radius = Math.max(sphere.radius, 0.01);
    const distance = radius / Math.sin(Math.PI / 8) * 1.1;
    for (const camera of [this.orbitCamera, this.topCamera]) {
      camera.near = Math.max(radius / 10000, 0.001);
      camera.far = distance + radius * 10;
      camera.position.copy(sphere.center).addScaledVector(
        camera === this.orbitCamera ? new THREE.Vector3(0, 0.8, -0.6) : new THREE.Vector3(0, 0, -1), distance);
      camera.lookAt(sphere.center);
      camera.updateProjectionMatrix();
      camera.updateMatrixWorld();
    }
    this.controls.target.copy(sphere.center);
    this.controls.minDistance = Math.max(radius / 1000, 0.01);
    this.controls.maxDistance = radius * 40;
    this.controls.update();
  }

  private requestRefresh() {
    if (this.renderRequest || !this.available) return;
    this.renderRequest = requestAnimationFrame(() => { this.renderRequest = 0; this.refresh(); });
  }

  private bindPicking(canvas: HTMLCanvasElement) {
    canvas.addEventListener('pointerdown', (event) => {
      if (!this.pick || event.button !== 0 || !event.isPrimary) return;
      this.gesture = { id: event.pointerId, x: event.clientX, y: event.clientY };
      canvas.focus({ preventScroll: true });
    });
    canvas.addEventListener('pointerup', (event) => {
      if (!this.pick || this.gesture?.id !== event.pointerId) return;
      const gesture = this.gesture;
      this.gesture = undefined;
      if (Math.hypot(event.clientX - gesture.x, event.clientY - gesture.y) > 6) return;
      const rect = canvas.getBoundingClientRect(), camera = this.mode === 'orbit' ? this.orbitCamera : this.topCamera;
      this.raycaster.params.Points.threshold = this.tolerance;
      this.raycaster.setFromCamera(new THREE.Vector2(
        (event.clientX - rect.left) / rect.width * 2 - 1, 1 - (event.clientY - rect.top) / rect.height * 2), camera);
      const hit = this.raycaster.intersectObject(this.points, false)[0];
      if (!hit || hit.index === undefined) { this.viewport.dispatchEvent(new CustomEvent('pointpickmiss')); return; }
      const point = new THREE.Vector3().fromBufferAttribute(this.points.geometry.getAttribute('position'), hit.index);
      this.pick.points.push(point.toArray());
      const marker = new THREE.Mesh(new THREE.SphereGeometry(Math.max(this.tolerance / 2, 0.03), 12, 8),
        new THREE.MeshBasicMaterial({ color: '#ddff54', depthTest: false }));
      marker.position.copy(point);
      marker.renderOrder = 1000;
      this.pickMarkers.add(marker);
      this.pick.progress(this.pick.points.length);
      if (this.pick.points.length === 3) {
        const { complete, points } = this.pick;
        const [a, b, c] = points.map((value) => new THREE.Vector3(...value));
        const normal = new THREE.Vector3().crossVectors(b.sub(a), c.sub(a));
        const normalSign = normal.dot(camera.position.clone().sub(a)) < 0 ? -1 : 1;
        this.cancelPlanePick();
        complete(points, normalSign);
      }
      this.requestRefresh();
    });
    for (const event of ['pointercancel', 'lostpointercapture'] as const) canvas.addEventListener(event, () => { this.gesture = undefined; });
    canvas.addEventListener('keydown', (event) => {
      if (event.key === 'Escape' && this.pick) {
        event.preventDefault();
        this.cancelPlanePick();
        this.viewport.dispatchEvent(new CustomEvent('pointpickcancel'));
      } else if (event.key === 'Home') { event.preventDefault(); this.resetView(); }
    });
  }

  private disposePoints() {
    if (!this.points) return;
    this.points.geometry.dispose();
    this.points.material.dispose();
    this.points.removeFromParent();
    this.points = undefined;
  }

  dispose() {
    this.available = false;
    this.cancelPlanePick();
    this.resizeObserver.disconnect();
    cancelAnimationFrame(this.renderRequest);
    this.controls.dispose();
    this.adControls.dispose();
    this.localAds.dispose();
    this.disposePoints();
    this.renderer.dispose();
  }
}
