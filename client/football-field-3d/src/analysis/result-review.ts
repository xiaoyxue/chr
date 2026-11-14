import * as THREE from 'three';
import { importResultFolder } from './bundle-import.ts';
import { fetchServerFolder, serverPath } from '../shared/server-files.ts';
import { LocalAdScene } from '../placements/local-ad-scene.ts';
import { FusionCreatives } from '../placements/fusion-creatives.ts';
import { applyResultCamera } from '../scene/cameras/result-camera.ts';
import { RadialResultPass } from './radial-result-pass.ts';
import type { BundleFile, ResultBundle, ResultFrame, ResultShot } from './bundle-result.ts';
import type { LocalAd } from '../placements/local-ad.ts';
import type { Placement } from '../shared/types.ts';

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

export class ResultReview {
  onChange?: () => void;
  private bundles = new Map<string, ResultBundle>();
  private matchId?: string;
  private importing?: AbortController;
  private loadVersion = 0;
  private bundle?: ResultBundle;
  private shot?: ResultShot;
  private frame?: ResultFrame;
  private source = $<HTMLCanvasElement>('result-source');
  private program = $<HTMLCanvasElement>('result-program');
  private video = $<HTMLVideoElement>('result-video');
  private url?: string;
  private callback?: number;
  private enabled = false;
  private unavailable = false;
  private renderer?: THREE.WebGLRenderer;
  private scene = new THREE.Scene();
  private camera = new THREE.PerspectiveCamera(45, 16 / 9, 0.1, 10000);
  private local = new LocalAdScene(true);
  private pitchLocal = new LocalAdScene(false);
  private fusion?: FusionCreatives;
  private slots: Placement[] = [];
  private hidden: readonly string[] = [];
  private localSlots: LocalAd[] = [];
  private pitchLocalSlots: LocalAd[] = [];
  private localAligned = false;
  private adVersion = 0;
  private showError: (error: unknown) => void;
  private loadingVideo?: AbortController;
  private placementSignature = '';
  private fusionReady = false;
  private localReady = false;
  private localVersion = 0;
  private pitchLocalReady = true;
  private pitchLocalVersion = 0;
  private radialPass?: RadialResultPass;
  private cloudRadialPass?: RadialResultPass;

  constructor(onError: (error: unknown) => void, private clearError: () => void) {
    this.showError = onError;
    this.scene.add(this.local.group, this.pitchLocal.group, new THREE.HemisphereLight('#ffffff', '#b4c4a0', 2));
    const directional = new THREE.DirectionalLight('#ffffff', 3);
    directional.position.set(-20, 10, -50);
    this.scene.add(directional);
    $('import-result-folder').addEventListener('click', () => $<HTMLInputElement>('result-folder').click());
    $<HTMLInputElement>('result-folder').addEventListener('change', (event) => {
      const input = event.target as HTMLInputElement, files = Array.from(input.files);
      input.value = '';
      if (files.length) void this.import(files.map((file) => ({ path: file.webkitRelativePath || file.name, file })));
    });
    const loadPath = () => {
      const field = $<HTMLInputElement>('result-folder-path'), path = serverPath(field.value);
      field.value = path;
      if (path) void this.import(path);
    };
    $('load-result-folder-path').addEventListener('click', loadPath);
    $('result-folder-path').addEventListener('keydown', (event) => { if (event.key === 'Enter') loadPath(); });
    $('cancel-result-folder').addEventListener('click', () => {
      this.cancelImport();
      this.status('已取消导入，之前的结果未改变。');
    });
    $('remove-result-folder').addEventListener('click', () => {
      this.cancelImport();
      this.bundles.delete(this.matchId);
      this.clearVideo();
      this.bundle = undefined;
      this.render();
    });
    $<HTMLSelectElement>('result-shot').addEventListener('change', () => {
      const shot = this.bundle?.shots.find((item) => item.id === $<HTMLSelectElement>('result-shot').value);
      if (shot) void this.load(shot).catch(onError);
    });
    $('result-toggle-ads').addEventListener('click', () => {
      if (this.unavailable || !this.shot) return;
      this.enabled = !this.enabled;
      this.draw();
    });
    $('result-extrapolate-cloud').addEventListener('change', () => this.draw());
    this.video.addEventListener('seeking', () => {
      this.frame = undefined;
      this.source.getContext('2d').clearRect(0, 0, this.source.width, this.source.height);
      this.program.getContext('2d').clearRect(0, 0, this.program.width, this.program.height);
      $('result-frame-status').textContent = '正在定位原始视频帧，暂停叠加…';
    });
    this.video.addEventListener('seeked', () => this.frameFallback());
    this.video.addEventListener('loadeddata', () => this.frameFallback());
    this.video.addEventListener('timeupdate', () => {
      if (!('requestVideoFrameCallback' in this.video)) this.frameFallback();
    });
    this.video.addEventListener('error', () => {
      if (!this.shot) return;
      this.enabled = false;
      this.frame = undefined;
      this.program.getContext('2d').clearRect(0, 0, this.program.width, this.program.height);
      onError(new Error(`结果视频无法播放：${this.video.error?.message ?? '请检查视频编码'}`));
    });
    $('result-play').addEventListener('click', () => {
      if (this.video.paused) void this.video.play().catch(onError);
      else this.video.pause();
    });
    const updatePlayback = () => {
      $('result-play').textContent = this.video.paused ? '播放' : '暂停';
      $<HTMLInputElement>('result-timeline').value = String(this.video.currentTime);
    };
    for (const event of ['play', 'pause', 'timeupdate']) this.video.addEventListener(event, updatePlayback);
    $<HTMLInputElement>('result-timeline').addEventListener('input', (event) => {
      const time = (event.target as HTMLInputElement).valueAsNumber;
      if (this.shot && Number.isFinite(time)) this.video.currentTime = Math.min(Math.max(0, time), this.video.duration);
    });
    this.render();
  }

  get hasResult() { return Boolean(this.bundle); }
  async removeMatch(id: string) {
    this.bundles.delete(id);
    if (this.matchId === id) await this.setMatch();
  }

  async setMatch(id?: string) {
    if (this.matchId === id) return;
    this.cancelImport();
    this.clearVideo();
    this.matchId = id;
    this.bundle = this.bundles.get(id);
    this.slots = [];
    this.hidden = [];
    this.localSlots = [];
    this.pitchLocalSlots = [];
    this.enabled = false;
    this.render();
    if (this.bundle) await this.load(this.bundle.shots[0]);
  }

  async setPlacements(slots: Placement[], hidden: readonly string[] = []) {
    this.slots = slots;
    this.hidden = hidden;
    if (!this.renderer) return;
    const signature = JSON.stringify([slots, hidden]);
    if (signature === this.placementSignature && this.fusion && this.fusionReady) return;
    this.placementSignature = signature;
    const version = ++this.adVersion;
    this.fusionReady = false;
    this.enabled = false;
    this.draw();
    this.fusion?.dispose();
    this.fusion = new FusionCreatives(this.renderer);
    this.scene.add(this.fusion.group);
    try {
      await this.fusion.load(slots.filter((slot) => slot.asset), new Set(hidden));
      if (version !== this.adVersion) return;
      this.fusionReady = true;
      this.draw();
    } catch (error) {
      if (version === this.adVersion) { this.placementSignature = ''; this.enabled = false; this.showError(error); this.draw(); }
    }
  }

  async setLocalAds(ads: LocalAd[], aligned: boolean) {
    const version = ++this.localVersion;
    this.localSlots = ads;
    this.localAligned = aligned;
    this.localReady = false;
    this.draw();
    try {
      await this.local.update(ads);
      if (version !== this.localVersion) return;
      this.localReady = true;
      this.draw();
    } catch (error) { this.enabled = false; this.draw(); throw error; }
  }

  async setPitchAds(ads: LocalAd[]) {
    const version = ++this.pitchLocalVersion;
    this.pitchLocalSlots = ads;
    this.pitchLocalReady = false;
    this.draw();
    try {
      await this.pitchLocal.update(ads);
      if (version !== this.pitchLocalVersion) return;
      this.pitchLocalReady = true;
      this.draw();
    } catch (error) { this.enabled = false; this.draw(); throw error; }
  }

  private cancelImport() {
    this.importing?.abort();
    this.importing = undefined;
    $('cancel-result-folder').hidden = true;
    $<HTMLButtonElement>('import-result-folder').disabled = $<HTMLButtonElement>('load-result-folder-path').disabled = !this.matchId;
    $<HTMLButtonElement>('remove-result-folder').disabled = false;
  }
  private status(text: string) { $('result-folder-status').textContent = text; }
  private async import(source: BundleFile[] | string) {
    if (!this.matchId) return;
    this.cancelImport();
    this.clearError();
    const controller = new AbortController(), id = this.matchId;
    this.importing = controller;
    $('cancel-result-folder').hidden = false;
    $<HTMLButtonElement>('import-result-folder').disabled = $<HTMLButtonElement>('load-result-folder-path').disabled = true;
    $<HTMLButtonElement>('remove-result-folder').disabled = true;
    try {
      let files = source;
      if (typeof files === 'string') {
        this.status('正在从现场服务器读取结果文件夹…');
        files = await fetchServerFolder(files, controller.signal);
      }
      this.status('正在本地校验文件并读取投影参数 / 帧映射，不会上传文件…');
      const bundle = await importResultFolder(files, controller.signal);
      if (controller.signal.aborted || id !== this.matchId) return;
      this.bundles.set(id, bundle);
      this.bundle = bundle;
      this.render();
      await this.load(bundle.shots[0]);
    } catch (error) {
      if (error instanceof DOMException && error.name === 'AbortError') return;
      this.status(`${error instanceof Error ? error.message : String(error)}${this.bundle ? ' 已保留导入的结果。' : ''}`);
      this.showError(error);
    } finally { if (this.importing === controller) this.cancelImport(); }
  }

  private render() {
    $<HTMLButtonElement>('import-result-folder').disabled = $<HTMLButtonElement>('load-result-folder-path').disabled = !this.matchId || Boolean(this.importing);
    $('remove-result-folder').hidden = !this.bundle;
    $('result-review').hidden = !this.bundle;
    const select = $<HTMLSelectElement>('result-shot');
    select.replaceChildren(...(this.bundle?.shots ?? []).map((shot) => new Option(shot.id, shot.id)));
    $('adjust-step').textContent = this.bundle ? '结果文件夹已导入' : '等待结果文件夹';
    $('analysis-step').textContent = this.bundle ? '投影 / 视频已导入' : '等待结果文件夹';
    $('result-dot').hidden = !this.bundle;
    this.status(this.bundle
      ? `${this.bundle.name} · ${this.bundle.shots.length} 个镜头 · 运行文件校验通过 · 本地导入，刷新后需重新选择文件夹。`
      : '请选择完整 Ti 结果包（metadata.json），或诊断投影包（MANIFEST.json、media、model）。不是 PLY 文件。');
    this.onChange?.();
  }

  private ensureRenderer() {
    if (this.renderer) return;
    this.renderer = new THREE.WebGLRenderer({ alpha: true, antialias: true, preserveDrawingBuffer: true });
    this.renderer.debug.onShaderError = (gl, program) => {
      throw new Error(`广告投影着色器编译失败：${gl.getProgramInfoLog(program) || '请检查 GPU / WebGL 支持。'}`);
    };
    this.renderer.setClearColor(0, 0);
    this.renderer.setPixelRatio(1);
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.renderer.domElement.addEventListener('webglcontextlost', (event) => {
      event.preventDefault();
      this.unavailable = true;
      this.enabled = false;
      this.draw();
      this.showError(new Error('广告渲染连接已中断，已停止叠加；请刷新页面。'));
    });
  }

  private async load(shot: ResultShot) {
    this.clearVideo();
    const version = this.loadVersion;
    this.shot = shot;
    this.enabled = false;
    this.ensureRenderer();
    this.source.width = this.program.width = shot.width;
    this.source.height = this.program.height = shot.height;
    this.renderer.setSize(shot.width, shot.height, false);
    $('result-extrapolate-control').hidden = !shot.projection;
    $('result-quality').textContent = `条件射影估计，球场 ${this.bundle.pitch.join(' × ')} m（包内假设），不是实测相机标定；无球员遮挡处理。${shot.warnings.join('；')}`;
    $('result-frame-status').textContent = '正在载入结果原视频…';
    const controller = new AbortController();
    this.loadingVideo = controller;
    const ready = new Promise<void>((resolve, reject) => {
      const clean = () => {
        this.video.removeEventListener('loadedmetadata', loaded);
        this.video.removeEventListener('error', failed);
        controller.signal.removeEventListener('abort', aborted);
        clearTimeout(timer);
      };
      const loaded = () => { clean(); resolve(); };
      const failed = () => { clean(); reject(new Error('无法解码结果包中的原视频。')); };
      const aborted = () => { clean(); reject(new DOMException('已取消视频载入。', 'AbortError')); };
      const timer = setTimeout(() => { clean(); reject(new Error('载入结果视频超时。')); }, 20000);
      this.video.addEventListener('loadedmetadata', loaded, { once: true });
      this.video.addEventListener('error', failed, { once: true });
      controller.signal.addEventListener('abort', aborted, { once: true });
    });
    this.url = URL.createObjectURL(shot.video);
    this.video.src = this.url;
    this.video.load();
    try {
      await ready;
      if (version !== this.loadVersion) return;
      if (this.video.videoWidth !== shot.width || this.video.videoHeight !== shot.height
        || !Number.isFinite(this.video.duration) || this.video.duration <= shot.frames.at(-1).time) throw new Error('原视频尺寸或时长与结果帧索引不符。');
      $<HTMLInputElement>('result-timeline').max = String(shot.frames.at(-1).time);
      await this.setPlacements(this.slots, this.hidden);
      if (version !== this.loadVersion) return;
      await Promise.all([this.setLocalAds(this.localSlots, this.localAligned), this.setPitchAds(this.pitchLocalSlots)]);
      this.queueFrame();
      this.frameFallback();
    } catch (error) {
      if (error instanceof DOMException && error.name === 'AbortError') return;
      this.enabled = false;
      this.shot = undefined;
      $('result-frame-status').textContent = '结果视频载入失败，未叠加广告。';
      throw error;
    }
  }

  private queueFrame() {
    if (!this.shot || !('requestVideoFrameCallback' in this.video)) return;
    this.callback = this.video.requestVideoFrameCallback((_time, metadata) => {
      this.updateFrame(metadata.mediaTime);
      this.queueFrame();
    });
  }

  private frameFallback() {
    if (!this.shot || this.video.seeking || this.video.readyState < 2) return;
    if (!('requestVideoFrameCallback' in this.video) || this.video.paused) this.updateFrame(this.video.currentTime);
  }

  private updateFrame(time: number) {
    if (!this.shot || this.video.seeking || this.video.readyState < 2) return;
    let low = 0, high = this.shot.frames.length;
    while (low < high) {
      const middle = (low + high) >>> 1;
      if (this.shot.frames[middle].time <= time + 1e-6) low = middle + 1;
      else high = middle;
    }
    this.frame = low ? this.shot.frames[low - 1] : undefined;
    this.source.getContext('2d').drawImage(this.video, 0, 0, this.source.width, this.source.height);
    this.draw();
  }

  private draw() {
    if (!this.shot) return;
    const context = this.program.getContext('2d');
    context.clearRect(0, 0, this.program.width, this.program.height);
    context.drawImage(this.source, 0, 0);
    const assetsReady = this.fusionReady && this.localReady && this.pitchLocalReady;
    const calibrated = this.shot.projection ? this.frame?.postwarp : this.frame?.Ti;
    const label = this.shot.projection ? '完整径向投影' : 'Ti';
    const available = Boolean(calibrated && !this.video.seeking && !this.unavailable && assetsReady);
    $('result-frame-status').textContent = this.frame
      ? `${this.shot.id} · PI ${this.frame.presentIndex} · ${this.frame.time.toFixed(3)} s · ${!assetsReady ? '广告素材载入中，暂停叠加' : available ? `本帧 ${label} 有效` : `无有效 ${label}，暂停叠加`}`
      : '等待解码原始画面，暂不叠加。';
    $('result-toggle-ads').textContent = this.enabled ? '隐藏广告' : '显示广告';
    $('result-toggle-ads').setAttribute('aria-pressed', String(this.enabled));
    $<HTMLButtonElement>('result-toggle-ads').disabled = !available;
    const cloudAds = this.localSlots.filter((ad) => ad.visible);
    const blockedLocal = cloudAds.length > 0 && !this.localAligned;
    const extrapolate = Boolean(this.shot.projection && this.localAligned && cloudAds.length
      && $<HTMLInputElement>('result-extrapolate-cloud').checked);
    $('result-overlay-note').textContent = (blockedLocal ? '点云坐标未确认：仅暂停点云广告叠加；上方球场 2D / 3D 广告和场次素材仍按球场坐标预览。' : '仅本地效果预览；广告默认关闭，SOURCE 始终不含广告。')
      + (cloudAds.some((ad) => !ad.material) ? ' 未上传素材的点云广告显示为占位区域；上传素材后自动替换。' : '')
      + (extrapolate ? ' 点云广告已允许实验性外推：范围外的位置和高度精度未经验证，不可作为播出标定依据；球场广告仍限制在包内支持域。'
        : this.shot.projection ? ' 诊断投影仅显示支持域内可渲染片段，域外省略；看台 / 棚顶可启用点云广告实验性外推，高度精度未经验证。' : '');
    this.local.group.visible = this.localAligned;
    if (this.enabled && available) {
      try {
        if (this.shot.projection) {
          this.radialPass ??= new RadialResultPass();
          if (extrapolate) {
            const bounds = new THREE.Box3();
            for (const child of this.local.group.children) if (child.visible) bounds.expandByObject(child);
            const fusionVisible = this.fusion?.group.visible, pitchVisible = this.pitchLocal.group.visible;
            try {
              this.local.group.visible = false;
              this.radialPass.render(this.renderer, this.scene, this.camera, this.shot, this.frame);
              context.drawImage(this.renderer.domElement, 0, 0);
              this.local.group.visible = true;
              this.pitchLocal.group.visible = false;
              if (this.fusion) this.fusion.group.visible = false;
              this.cloudRadialPass ??= new RadialResultPass();
              this.cloudRadialPass.render(this.renderer, this.scene, this.camera, this.shot, this.frame, bounds);
            } finally {
              this.local.group.visible = this.localAligned;
              this.pitchLocal.group.visible = pitchVisible;
              if (this.fusion) this.fusion.group.visible = fusionVisible;
            }
          } else {
            this.radialPass.render(this.renderer, this.scene, this.camera, this.shot, this.frame);
          }
        } else {
          applyResultCamera(this.camera, this.frame.Ti, this.shot.centre, this.shot.width, this.shot.height);
          this.renderer.render(this.scene, this.camera);
        }
        context.drawImage(this.renderer.domElement, 0, 0);
      } catch (error) {
        context.clearRect(0, 0, this.program.width, this.program.height);
        context.drawImage(this.source, 0, 0);
        this.enabled = false;
        this.showError(error);
        $('result-frame-status').textContent = '投影失败，已停止广告叠加。';
        $('result-toggle-ads').textContent = '显示广告';
        $('result-toggle-ads').setAttribute('aria-pressed', 'false');
      }
    }
  }

  private clearVideo() {
    ++this.loadVersion;
    ++this.adVersion;
    this.loadingVideo?.abort();
    this.loadingVideo = undefined;
    if (this.callback !== undefined) this.video.cancelVideoFrameCallback(this.callback);
    this.callback = undefined;
    this.shot = undefined;
    this.frame = undefined;
    this.video.pause();
    this.video.removeAttribute('src');
    this.video.load();
    if (this.url) URL.revokeObjectURL(this.url);
    this.url = undefined;
    this.source.getContext('2d').clearRect(0, 0, this.source.width, this.source.height);
    this.program.getContext('2d').clearRect(0, 0, this.program.width, this.program.height);
  }

  pause() { this.video.pause(); }
  dispose() {
    this.cancelImport();
    this.clearVideo();
    this.local.dispose();
    this.pitchLocal.dispose();
    this.radialPass?.dispose();
    this.cloudRadialPass?.dispose();
    this.fusion?.dispose();
    this.renderer?.dispose();
  }
}
