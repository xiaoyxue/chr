// The editor of one space's local placements: 2D images and 3D models on the
// pitch; images, models and flags on point-cloud planes. Both spaces share the inspector: position, size and
// material (and a flag's artwork rotation, aspect lock and reveal) are protected by
// the lock and saved as soon as a field changes; a flag's motion (its wave, or a
// native flag's wind panel after the reference simulation's) is editable while
// locked and saved explicitly.
import { imageOpacity, withoutPitchFlags } from './local-ad.ts';
import { localAdStore } from './local-ad-store.ts';
import { serverSha256, uploadFile } from '../shared/studio-store.ts';
import { materialFacts } from '../render/send-materials.ts';
import { fetchServerFile, serverPath } from '../shared/server-files.ts';
import { glbBounds, planeFromPoints, previewReveal, validateLocalAdMaterial } from './local-ad-scene.ts';
import { REVEAL_DIRECTIONS, REVEAL_DURATION, REVEAL_LABELS, isRevealDirection } from './reveal.ts';
import { parseAspect, type Anchor } from './ground-edit.ts';
import { movePlaneAnchor, planeAnchor, planeWithAspect, resizePlane } from './plane-edit.ts';
import {
  DEFAULT_FLAG, FLAG_CLEARANCE, FLAG_LIMITS, FLAG_PEAK, NATIVE_LIMITS, NATIVE_SLACK, flagDraft, flagDraftChanged, nativeSettings,
  orientFlagPlane, parseFlagDraft, windDirectionAt, windHeading, type FlagDraft,
} from './flag.ts';
import type { AdPlane, AdSpace, LocalAd, Vector3Tuple } from './local-ad.ts';

type AdType = LocalAd['type'];
const WAVE = [['amplitude', '振幅 / m', 0.01], ['frequency', '频率（中线到边缘周数）', 0.05], ['speed', '速度 / ×', 0.05]] as const;
/** The draft fields typed in a number field and a slider. */
const PAIRED = [...WAVE.map(([key]) => key), 'wind', 'direction', 'tension'] as const;
const FLAG_INPUTS = new Set(['flagType', 'flagMaterial', 'autoDirection', ...PAIRED.flatMap((key) => [key, `${key}Slider`])]);
const typeLabel = (type: AdType) => type === 'flag' ? '旗帜' : type;
const LOCKED = '已锁定：点击“🔒 已锁定”解锁后，才能修改位置、尺寸和素材。';

/** A native flag's wind and fabric, laid out as the reference simulation's control panel. */
function nativeMarkup(prefix: string) {
  const slider = (key: 'wind' | 'tension', label: string, step: number, low: string, high: string) => {
    const bounds = `min="${NATIVE_LIMITS[key][0]}" max="${NATIVE_LIMITS[key][1]}" step="${step}"`;
    return `<div class="on-slider-row"><label for="${prefix}flag-${key}">${label}</label>
      <input id="${prefix}flag-${key}" name="${key}Slider" type="range" ${bounds} />
      <input name="${key}" type="number" ${bounds} aria-label="${label} 数值" required /></div>
      <div class="on-range-labels"><span>${low}</span><span>${high}</span></div>`;
  };
  const [min, max] = NATIVE_LIMITS.direction;
  return `${slider('wind', '风速 / m/s', 0.1, '静风', '强风')}
    <div class="on-slider-row"><span>风向</span><label class="on-check"><input name="autoDirection" type="checkbox" /> 自动风向</label></div>
    <p class="on-help">每 0.5 秒在基准角 ±45° 内选取新目标并平滑转向；每次播放的顺序相同。</p>
    <div class="on-wind-editor">
      <div data-wind-dial class="on-wind-dial" role="slider" tabindex="0" aria-label="风向表盘" aria-valuemin="${min}" aria-valuemax="${max}" aria-describedby="${prefix}wind-dial-help">
        <svg viewBox="0 0 140 140" aria-hidden="true">
          <circle class="dial-ring" cx="70" cy="70" r="44"/><path class="dial-axis" d="M70 26v88"/><path class="dial-cloth" d="M26 70h88"/>
          <text x="70" y="12">正面（读图侧）</text><text x="70" y="23">+90°</text>
          <text x="128" y="73">0°</text><text x="12" y="73">180°</text>
          <text x="70" y="126">−90°</text><text x="70" y="138">背面</text>
          <g data-wind-base><path class="dial-base-arrow" d="M70 70h38m-7-5 7 5-7 5"/></g>
          <g data-wind-arrow><path class="dial-arrow" d="M70 70h33m-8-7 8 7-8 7"/><circle class="dial-handle" cx="103" cy="70" r="5"/></g>
          <circle class="dial-center" cx="70" cy="70" r="3"/>
        </svg></div>
      <div class="on-wind-entry"><label>基准角 / °<input name="direction" type="number" min="${min}" max="${max}" step="1" required /></label>
        <span>吹向</span><output data-wind-heading></output><output data-wind-current></output><span>俯视 · 相对旗面</span></div>
    </div>
    <p data-direction-error class="on-form-error" role="status" hidden>请输入 −180° 到 +180° 之间的整数角度；表盘保持上一个有效角度，Esc 恢复。</p>
    <div class="on-wind-row"><input id="${prefix}flag-direction" name="directionSlider" type="range" min="${min}" max="${max}" step="1" aria-label="风向基准角" /></div>
    <div class="on-range-labels"><span>−180°</span><span>0° = 沿旗面向右</span><span>+180°</span></div>
    <p id="${prefix}wind-dial-help" class="on-help">拖动表盘设置基准角。实线箭头：当前风向；虚线箭头：基准。方向键 1°（Shift 10°），PageUp / PageDown 10°，Home / End 到 −180° / +180°。角度相对旗面：0° 沿旗面吹向右边（从读图侧看），+90° 吹向读图侧。</p>
    ${slider('tension', '布料张力 / %', 1, '柔软飘逸', '挺括紧致')}`;
}

function editorMarkup(space: AdSpace, prefix: string) {
  const pitch = space === 'pitch', id = (name: string) => `id="${prefix}${name}"`;
  const field = (name: string, label: string, attrs = '', data = '') =>
    `<label ${data}>${label}<input name="${name}" type="number" step="0.01" ${attrs} required /></label>`;
  const anchorLabel = (axis: string) => `<span data-anchor-label>左上角</span> ${axis} / m`;
  const wave = WAVE.map(([key, label, step]) => {
    const bounds = `min="${FLAG_LIMITS[key][0]}" max="${FLAG_LIMITS[key][1]}" step="${step}"`;
    return `<div class="on-slider-row"><label for="${prefix}flag-${key}">${label}</label>
      <input ${id(`flag-${key}`)} name="${key}Slider" type="range" ${bounds} />
      <input name="${key}" type="number" ${bounds} aria-label="${label} 数值" required /></div>`;
  }).join('');
  return `<div class="on-card-head"><h2>${pitch ? '球场本地广告位' : '点云广告位'}</h2></div><div class="on-card-body">
    <div class="on-action-row"><button ${id('draw-local-2d')} class="on-button primary" type="button" disabled>${pitch ? '画 2D 广告' : '选面放图片'}</button>
    <button ${id('draw-local-3d')} class="on-button" type="button" disabled>${pitch ? '画 3D 广告' : '选面放模型'}</button>
    ${pitch ? '' : `<button ${id('draw-local-flag')} class="on-button" type="button" disabled>选面放旗帜</button>`}
    <button ${id('cancel-local-draw')} class="on-button" type="button" hidden>取消绘制</button></div>
    <p ${id('local-draw-status')} class="on-help" role="status"></p><p ${id('local-ad-count')} class="on-help"></p>
    <p ${id('local-ad-empty')} class="on-help">${pitch ? '本场次尚未创建本地球场广告位。' : '当前点云尚未创建广告位。'}</p>
    <label>选择${pitch ? '球场本地' : '点云'}广告位<select ${id('local-ad-select')} aria-label="选择${pitch ? '球场本地' : '点云'}广告位"></select></label>
    <div ${id('local-ad-state')} class="on-target-bar" hidden>
      <button ${id('local-ad-enabled')} class="on-visibility-toggle" type="button" role="switch" aria-checked="true"><span class="on-toggle-track" aria-hidden="true"></span><span></span></button>
      <button ${id('local-ad-lock')} class="on-lock-toggle" type="button" aria-pressed="false" title="解锁后才能修改位置、尺寸和素材">🔒 已锁定</button>
    </div>
    <div class="on-action-row"><button class="on-button on-undo" type="button" data-undo disabled>↶ 撤销</button><button class="on-button on-undo" type="button" data-redo disabled>↷ 重做</button></div>
    <form ${id('local-ad-form')} novalidate hidden><fieldset data-locked-controls>
      <label>名称<input name="name" maxlength="80" required /></label>
      <div class="on-inspector-group"><h3>位置（${pitch ? '世界坐标' : '点云坐标'}）</h3>
        <div class="on-segmented" role="radiogroup" aria-label="定位锚点"><label><input type="radio" name="anchor" value="top-left" checked />左上角</label><label><input type="radio" name="anchor" value="centre" />中心</label></div>
        <div class="on-field-grid">${field('anchorX', anchorLabel('X'))}${field('anchorY', anchorLabel('Y'))}${pitch ? '' : field('anchorZ', anchorLabel('Z'))}
        ${pitch ? field('z', '高度 Z / m（负值向上）', 'min="-1000" max="1000"', 'data-model-only') : field('offset', '法线偏移 / m', 'min="-1000" max="1000"')}</div>
        <p class="on-help">左上角是图片左上角（平面 u / v 方向的起点）${pitch ? '；俯视图中 +X 向右、+Y 向下' : ''}。</p></div>
      <div class="on-inspector-group"><h3>尺寸</h3>
        <div class="on-field-grid">${field('width', pitch ? '长 X / m' : '宽（u 方向）/ m', 'min="0.01" max="1000"')}${field('height', pitch ? '宽 Y / m' : '高（v 方向）/ m', 'min="0.01" max="1000"')}</div>
        <div class="on-aspect-row"><label data-aspect-input>宽高比 W:H<input name="aspect" type="text" inputmode="decimal" aria-label="宽高比，如 16:9 或 1.78" /></label>
        <label class="on-check" data-keep-aspect><input name="keepAspect" type="checkbox" checked /> 锁定宽高比</label>
        <label class="on-check" data-flag-group><input name="aspectLocked" type="checkbox" /> 锁定宽高比（矩形跟随图片）</label></div>
        <p class="on-help" data-aspect-input>输入 16:9 或 1.78 后按 Enter：保持宽和锚点，按比例调整高。</p></div>
      <div data-flag-group class="on-inspector-group"><h3>旗帜图片与展开</h3>
        <div class="on-field-grid"><label>图片旋转（顺时针）<select name="artworkRotation" aria-label="图片旋转">${[0, 90, 180, 270].map((angle) => `<option value="${angle}">${angle}°</option>`).join('')}</select></label>
        <label>展开<select name="revealDirection" aria-label="展开方式"><option value="">无（直接显示）</option>${REVEAL_DIRECTIONS.map((direction) => `<option value="${direction}">${REVEAL_LABELS[direction]}</option>`).join('')}</select></label>
        <label data-reveal-only>时长 / s<input name="revealDuration" type="number" min="0.01" step="any" required /></label>
        <label data-reveal-only>开始<select name="revealStart" aria-label="展开开始"><option value="load">加载时</option><option value="cue">手动触发</option></select></label></div>
        <p class="on-help">图片按所选角度顺时针旋转后拉伸到矩形；锁定宽高比时，矩形按（旋转后的）图片比例调整高度。展开方向以旗帜矩形为准（左上角起，宽向右、高向下），与 Fusion 预览一致，不随图片旋转；尚未展开的部分收在展开边缘，图片只被裁切、不会被压缩，展开完成后保持完整显示。</p></div>
      <div data-model-group class="on-inspector-group"><h3>模型</h3>
        <div class="on-field-grid">${field('scale', '整体缩放 / ×', 'min="0.01" max="1000"')}${field('yaw', '朝向 / °', 'min="-360" max="360"')}</div>
        ${pitch ? '' : '<label class="on-check"><input name="flip" type="checkbox" /> 反转模型立起方向（平面法线）</label>'}</div>
      <div data-image-group class="on-inspector-group"><h3>不透明度</h3>
        <div class="on-slider-row"><label for="${prefix}local-ad-opacity">不透明度（0–1）</label>
        <input ${id('local-ad-opacity')} name="opacitySlider" type="range" min="0" max="1" step="0.01" />
        <input name="opacity" type="number" min="0" max="1" step="0.01" aria-label="不透明度 数值" required /></div>
        <p class="on-help">图片自身的透明度再乘以此值：0 全透明，1 保持图片原样；未设置时为 0.5。</p></div>
      <p ${id('local-ad-readonly')} class="on-readonly"></p>
    </fieldset>
    <fieldset ${id('local-flag-controls')} class="on-inspector-group"><h3>旗帜飘动</h3>
      <div class="on-slider-row"><span>类型</span><div class="on-segmented" role="radiogroup" aria-label="旗帜类型"><label><input type="radio" name="flagType" value="default" />默认（正弦飘动）</label><label><input type="radio" name="flagType" value="native" />物理模拟（布料）</label></div></div>
      <div data-flag-wave>${wave}</div><div data-flag-native>${nativeMarkup(prefix)}</div>
      <div class="on-slider-row"><span>材质</span><div class="on-segmented" role="radiogroup" aria-label="旗帜材质"><label><input type="radio" name="flagMaterial" value="simple" />简易双面光照</label><label><input type="radio" name="flagMaterial" value="pbr" />PBR</label><label><input type="radio" name="flagMaterial" value="cloth" />布料</label></div></div>
      <p data-flag-note class="on-help"></p></fieldset>
    <div ${id('local-reveal-actions')} class="on-inspector-group"><div class="on-action-row">
      <button ${id('preview-reveal')} class="on-button" type="button">预览展开</button>
      <button ${id('cue-reveal')} class="on-button" type="button">触发展开</button></div>
      <p data-reveal-note class="on-help"></p></div>
    <fieldset data-locked-controls>
      <label ${id('local-ad-material-label')} for="${prefix}local-ad-material-path">素材的服务器路径（chr-server 从这里读取）</label>
      <div class="on-source-row"><input ${id('local-ad-material-path')} class="on-path-input" autocomplete="off" />
      <button ${id('load-local-ad-material')} class="on-button" type="button">按路径加载</button>
      <button ${id('choose-local-ad-material')} class="on-button" type="button">选择本地素材</button>
      <input ${id('local-ad-material')} type="file" hidden /></div>
      <p ${id('local-ad-material-name')} class="on-help"></p><div class="on-action-row">
      <button ${id('redraw-local-plane')} class="on-button" type="button">${pitch ? '重画底面' : '重选平面'}</button>
      <button ${id('delete-local-ad')} class="on-text-button" type="button">删除</button></div>
    </fieldset>
    <div class="on-inspector-group"><div class="on-action-row"><button ${id('save-local')} class="on-button primary" type="button">保存属性</button>
      <button ${id('save-send-local')} class="on-button" type="button">保存并发送到渲染器</button></div>
      <p ${id('local-save-state')} class="on-help" role="status"></p></div>
    </form><p ${id('local-ad-storage')} class="on-help"></p></div>`;
}

export class LocalAdEditor {
  private repository = localAdStore;
  private matchId?: string;
  private version = 0;
  private cloudId?: string;
  private items: LocalAd[] = [];
  private selectedId?: string;
  private busy = false;
  /** The running save, so a selection can wait for it. */
  private saving: Promise<unknown> = Promise.resolve();
  private loaded = false;
  private available = false;
  private drawing?: { type: AdType; replaceId?: string };
  private planeEdit?: { original: LocalAd; candidate: LocalAd };
  // Ads open locked so a stray edit cannot move them.
  private unlocked = false;
  private anchor: Anchor = 'top-left';
  private keepAspect = true;
  /** Typed but unsaved motion fields of one flag; the preview already shows them. */
  private flagDraft?: FlagDraft & { id: string };
  /** The animation frame that turns the wind dial's current arrow under automatic wind. */
  private windFrame = 0;
  /** The last valid base angle shown for a flag, which the dial keeps while the typed angle is invalid. */
  private lastWind?: { id: string; angle: number };
  /** The last save or send outcome shown under the save buttons. */
  private notice?: { id: string; text: string; failed?: boolean };
  /** Said once after loading, until the next save. */
  private loadNotice?: string;

  constructor(private options: {
    root: HTMLElement;
    prefix: string;
    space: AdSpace;
    update: (ads: LocalAd[], selectedId?: string) => Promise<void>;
    pick: () => void;
    cancelPick: () => void;
    validate?: (ad: LocalAd) => void;
    error: (error: unknown) => void;
    /** Whether ads can be clicked in the 3D view, and dragged (only while unlocked). */
    manipulationAvailable?: (available: boolean, locked: boolean) => void;
    /** Records a committed change; `restore` puts the previous ads back and `reapply` the change again. */
    recordUndo?: (label: string, restore: () => Promise<void>, reapply: () => Promise<void>) => void;
    /** Runs the renderer bar's send action and resolves with its status message. */
    send?: () => Promise<{ text: string; failed: boolean }>;
    /** Starts the reveal of a sent flag whose reveal waits for a cue; resolves with the renderer's status message. */
    cue?: (ad: LocalAd) => Promise<{ text: string; failed: boolean }>;
    /** The ad to select when the match or point cloud loads, such as the one selected before a reload. */
    preferredSelection?: () => string | undefined;
  }) {
    options.root.innerHTML = editorMarkup(options.space, options.prefix);
    this.element('draw-local-2d').addEventListener('click', () => this.start('2D'));
    this.element('draw-local-3d').addEventListener('click', () => this.start('3D'));
    this.element('draw-local-flag')?.addEventListener('click', () => this.start('flag'));
    this.element('redraw-local-plane').addEventListener('click', () => {
      const selected = this.selected();
      if (selected) this.start(selected.type, selected.id);
    });
    this.element('cancel-local-draw').addEventListener('click', () => this.cancelDrawing());
    this.element<HTMLSelectElement>('local-ad-select').addEventListener('change', () => {
      if (!this.selectAd(this.element<HTMLSelectElement>('local-ad-select').value)) this.render();
    });
    const form = this.element<HTMLFormElement>('local-ad-form');
    form.addEventListener('submit', (event) => {
      event.preventDefault();
      if (document.activeElement instanceof HTMLInputElement) void this.apply(document.activeElement);
    });
    form.addEventListener('change', (event) => {
      const input = event.target as HTMLInputElement;
      if (input.name === 'anchor') { this.anchor = input.value as Anchor; this.render(); }
      else if (input.name === 'keepAspect') this.keepAspect = input.checked;
      else if (FLAG_INPUTS.has(input.name)) this.readFlagDraft(input);
      else if (input.name) void this.apply(input);
    });
    form.addEventListener('input', (event) => {
      const input = event.target as HTMLInputElement;
      if (FLAG_INPUTS.has(input.name)) this.readFlagDraft(input);
      else {
        if (input.name === 'opacitySlider') (this.form.elements.namedItem('opacity') as HTMLInputElement).value = input.value;
        this.renderSaveState();
      }
    });
    this.element('local-flag-controls').addEventListener('keydown', (event) => {
      if (event.key !== 'Enter' || !(event.target instanceof HTMLInputElement)) return;
      event.preventDefault();
      void this.save();
    });
    this.listenToWindDial();
    this.element('preview-reveal').addEventListener('click', () => { if (this.selectedId) previewReveal(this.selectedId); });
    this.element('cue-reveal').addEventListener('click', () => { void this.cue(); });
    this.element('save-local').addEventListener('click', () => { void this.save(); });
    this.element('save-send-local').addEventListener('click', () => { void this.saveAndSend(); });
    this.element('local-ad-enabled').addEventListener('click', () => {
      if (this.selectedId) void this.toggleVisible(this.selectedId);
    });
    this.element('local-ad-lock').addEventListener('click', () => { this.unlocked = !this.unlocked; this.render(); });
    this.element('delete-local-ad').addEventListener('click', () => {
      if (this.busy || !this.selected()) return;
      this.cancelDrawing();
      this.flagDraft = undefined;
      const removed = this.selected(), next = this.items.filter((ad) => ad.id !== this.selectedId);
      void this.commit(next, next.find((ad) => this.inWorkspace(ad))?.id, `删除 ${removed.name}`);
    });
    this.element<HTMLInputElement>('local-ad-material').addEventListener('change', (event) => {
      const input = event.target as HTMLInputElement, file = input.files[0], selected = this.selected();
      input.value = '';
      if (file && selected && !this.busy) void this.setMaterial(selected, file);
    });
    this.element('choose-local-ad-material').addEventListener('click', () => this.element<HTMLInputElement>('local-ad-material').click());
    const loadPath = async () => {
      const field = this.element<HTMLInputElement>('local-ad-material-path'), path = serverPath(field.value), selected = this.selected();
      field.value = path;
      if (!selected) return;
      if (!path) { this.options.error(new Error('请先填写素材在现场服务器上的绝对路径。')); return; }
      if (this.busy) { this.options.error(new Error('正在保存，请稍后再加载素材。')); return; }
      try { await this.setMaterial(selected, await fetchServerFile(path), path); }
      catch (error) { this.options.error(error); }
    };
    this.element('load-local-ad-material').addEventListener('click', () => { void loadPath(); });
    this.element('local-ad-material-path').addEventListener('keydown', (event) => {
      if (event.key !== 'Enter') return;
      event.preventDefault();
      void loadPath();
    });
    this.cancelDrawing();
  }

  private element<T extends HTMLElement = HTMLElement>(id: string) {
    return this.options.root.querySelector<T>(`#${this.options.prefix}${id}`);
  }
  private get form() { return this.element<HTMLFormElement>('local-ad-form'); }
  private inWorkspace(ad: LocalAd) { return this.options.space === 'pitch' || Boolean(this.cloudId && ad.cloudId === this.cloudId); }
  /** The saved ads of the current workspace, with a dragged rectangle in place. */
  get ads() {
    return this.items.filter((ad) => this.inWorkspace(ad))
      .map((ad) => ad.id === this.planeEdit?.original.id ? this.planeEdit.candidate : ad);
  }
  /** What the previews show: the ads with the selected flag's typed wave, so dragging a slider is visible at once. */
  private get previewAds() {
    const parsed = this.flagDraft && parseFlagDraft(this.flagDraft);
    return this.ads.map((ad) => ad.id === this.flagDraft?.id && parsed && 'wave' in parsed ? { ...ad, flag: { ...ad.flag, ...parsed.wave } } : ad);
  }
  /** Every saved ad of this space, including those of point clouds other than the current one. */
  get allAds(): readonly LocalAd[] { return this.items; }
  refresh() { return this.publish(); }
  private selected() { return this.ads.find((ad) => ad.id === this.selectedId); }
  private get locked() { return !this.unlocked; }
  private async publish() { await this.options.update(this.previewAds, this.selectedId); }

  /** A position, size or name field typed into but not applied yet. */
  private typedField() {
    return [...this.form.querySelectorAll<HTMLInputElement>('[data-locked-controls] input:is([type=number], [type=text], :not([type]))')]
      .find((input) => !input.disabled && input.name && input.name !== 'aspect' && input.value !== input.defaultValue);
  }
  private get flagDirty() {
    const flag = this.flagDraft && this.items.find((ad) => ad.id === this.flagDraft.id);
    return Boolean(flag && flagDraftChanged(this.flagDraft, flag.flag));
  }
  /** Whether a drawing, a plane pick or a drag is in progress. */
  get editing() { return Boolean(this.drawing || this.planeEdit); }
  private initialSelection() {
    const preferred = this.options.preferredSelection?.();
    return this.ads.find((ad) => ad.id === preferred)?.id ?? this.ads[0]?.id;
  }
  /** The name of the ad with unsaved edits, if any. */
  get unsavedChanges(): string | undefined {
    if (this.flagDirty) return this.items.find((ad) => ad.id === this.flagDraft.id).name;
    if (this.typedField()) return this.selected()?.name;
  }

  private readFlagDraft(input: HTMLInputElement) {
    const selected = this.selected(), form = this.form;
    if (selected?.type !== 'flag') return;
    const pair = (name: string) => form.elements.namedItem(name) as HTMLInputElement;
    const checked = (name: string) => form.querySelector<HTMLInputElement>(`[name=${name}]:checked`)?.value;
    // A slider and its number field show one value.
    if (input.name.endsWith('Slider')) pair(input.name.slice(0, -'Slider'.length)).value = input.value;
    else if (input.type === 'number') pair(`${input.name}Slider`).value = input.value;
    this.flagDraft = { id: selected.id, type: checked('flagType') === 'native' ? 'native' : 'default',
      amplitude: pair('amplitude').value, frequency: pair('frequency').value, speed: pair('speed').value,
      wind: pair('wind').value, direction: pair('direction').value, autoDirection: pair('autoDirection').checked, tension: pair('tension').value,
      material: (['pbr', 'cloth'] as const).find((value) => value === checked('flagMaterial')) ?? 'simple' };
    if (this.notice?.failed) this.notice = undefined;
    this.renderSaveState();
    this.renderFlagMotion(selected);
    void this.publish().catch(this.options.error);
  }

  /** The selected flag's typed draft, or its saved settings as one. */
  private draftOf(selected: LocalAd) {
    return this.flagDraft?.id === selected.id ? this.flagDraft : flagDraft(selected.flag);
  }

  /** The wind's base direction the dial shows: the typed angle while it is valid, else the last valid one. */
  private windBase(selected: LocalAd) {
    const typed = this.draftOf(selected).direction, angle = Number(typed), [min, max] = NATIVE_LIMITS.direction;
    if (typed.trim() !== '' && Number.isInteger(angle) && angle >= min && angle <= max) {
      this.lastWind = { id: selected.id, angle };
      return { valid: true, angle };
    }
    return { valid: false, angle: this.lastWind?.id === selected.id ? this.lastWind.angle : nativeSettings(selected.flag).direction };
  }

  /** Drag, arrow keys and Escape on the wind dial and its angle, as in the reference simulation. */
  private listenToWindDial() {
    const dial = this.form.querySelector<HTMLElement>('[data-wind-dial]'), field = this.form.elements.namedItem('direction') as HTMLInputElement;
    const enabled = () => !this.element<HTMLFieldSetElement>('local-flag-controls').disabled && this.selected()?.type === 'flag';
    const setDirection = (angle: number) => { field.value = String(angle); this.readFlagDraft(field); };
    let pointer: number | undefined;
    const point = (event: PointerEvent) => {
      const bounds = dial.getBoundingClientRect(), x = event.clientX - (bounds.left + bounds.width / 2), z = bounds.top + bounds.height / 2 - event.clientY;
      if (Math.hypot(x, z) >= 8) setDirection(Math.round(Math.atan2(z, x) * 180 / Math.PI));
    };
    dial.addEventListener('pointerdown', (event) => {
      if (event.button !== 0 || pointer !== undefined || !enabled()) return;
      event.preventDefault();
      pointer = event.pointerId;
      dial.focus({ preventScroll: true });
      dial.setPointerCapture(event.pointerId);
      point(event);
    });
    dial.addEventListener('pointermove', (event) => { if (event.pointerId === pointer) point(event); });
    for (const name of ['pointerup', 'pointercancel', 'lostpointercapture'] as const)
      dial.addEventListener(name, (event) => { if (event.pointerId === pointer) pointer = undefined; });
    dial.addEventListener('keydown', (event) => {
      const selected = this.selected(), [min, max] = NATIVE_LIMITS.direction;
      if (!enabled() || !selected) return;
      let angle = this.windBase(selected).angle;
      const step = event.shiftKey ? 10 : 1;
      switch (event.key) {
        case 'ArrowUp': case 'ArrowRight': angle += step; break;
        case 'ArrowDown': case 'ArrowLeft': angle -= step; break;
        case 'PageUp': angle += 10; break;
        case 'PageDown': angle -= 10; break;
        case 'Home': angle = min; break;
        case 'End': angle = max; break;
        default: return;
      }
      event.preventDefault();
      setDirection(angle > max ? angle - 360 : angle < min ? angle + 360 : angle);
    });
    field.addEventListener('keydown', (event) => {
      const selected = this.selected();
      if (event.key !== 'Escape' || !selected) return;
      event.preventDefault();
      event.stopPropagation();
      setDirection(this.windBase(selected).angle);
    });
  }

  /** Shows the selected flag's motion fields for its type, its wind dial and its note. */
  private renderFlagMotion(selected: LocalAd) {
    const draft = this.draftOf(selected), native = draft.type === 'native';
    for (const [selector, visible] of [['[data-flag-wave]', !native], ['[data-flag-native]', native]] as const) {
      const group = this.form.querySelector<HTMLElement>(selector);
      group.hidden = !visible;
      group.querySelectorAll('input').forEach((input) => { input.disabled = !visible; });
    }
    this.renderFlagNote(selected);
    this.renderWind();
  }

  /** The wind dial: the base arrow, and the current wind turning under automatic wind on this page's clock. */
  private renderWind() {
    cancelAnimationFrame(this.windFrame);
    const selected = this.selected();
    if (selected?.type !== 'flag' || this.draftOf(selected).type !== 'native') return;
    const form = this.form, dial = form.querySelector<HTMLElement>('[data-wind-dial]'), { autoDirection } = this.draftOf(selected);
    const { valid, angle: base } = this.windBase(selected);
    form.querySelector<HTMLElement>('[data-direction-error]').hidden = valid;
    (form.elements.namedItem('direction') as HTMLInputElement).setAttribute('aria-invalid', String(!valid));
    dial.setAttribute('aria-valuenow', String(base));
    dial.setAttribute('aria-valuetext', `基准风向 ${base}°`);
    const baseArrow = dial.querySelector<SVGGElement>('[data-wind-base]');
    baseArrow.style.display = autoDirection ? '' : 'none';
    baseArrow.setAttribute('transform', `rotate(${-base} 70 70)`);
    const current = windDirectionAt(performance.now() / 1000, { direction: base, autoDirection });
    dial.querySelector('[data-wind-arrow]').setAttribute('transform', `rotate(${-current} 70 70)`);
    form.querySelector('[data-wind-heading]').textContent = windHeading(current);
    form.querySelector('[data-wind-current]').textContent = `当前：${current.toFixed(1)}°`;
    if (autoDirection && dial.isConnected) this.windFrame = requestAnimationFrame(() => this.renderWind());
  }

  /** Drops unsaved edits, asking first; false when the operator keeps them. */
  private discardUnsaved() {
    const name = this.unsavedChanges;
    if (name && !window.confirm(`“${name}”有未保存的修改。放弃这些修改？`)) return false;
    this.flagDraft = undefined;
    return true;
  }

  /**
   * Saves the selected ad: a typed position or size field, and a flag's typed wave
   * as one change. Resolves whether everything is saved; says why when it is not.
   */
  async save(): Promise<boolean> {
    const selected = this.selected();
    if (!selected) return false;
    const fail = (text: string) => {
      this.notice = { id: selected.id, text, failed: true };
      this.renderSaveState();
      this.options.error(new Error(text));
      return false;
    };
    if (this.busy) await this.saving.catch((): undefined => undefined);
    if (this.planeEdit || this.drawing) return fail('正在拖动或绘制，请完成后再保存。');
    const typed = this.typedField();
    if (typed && !await this.apply(typed)) return fail('位置或尺寸的修改未能保存。');
    if (this.flagDirty) {
      const parsed = parseFlagDraft(this.flagDraft);
      if ('error' in parsed) return fail(parsed.error);
      const current = this.items.find((ad) => ad.id === selected.id), next = { ...current, flag: { ...current.flag, ...parsed.wave } };
      if (!await this.commit(this.items.map((ad) => ad.id === next.id ? next : ad), next.id, `保存旗帜属性 ${next.name}`)) return fail('保存失败，修改仍未保存。');
      this.flagDraft = undefined;
    }
    this.notice = { id: selected.id, text: '已保存。' };
    this.render();
    return true;
  }

  private async saveAndSend() {
    const id = this.selectedId;
    if (!await this.save() || !this.options.send) return;
    this.notice = { id, text: '已保存，正在发送到渲染器…' };
    this.renderSaveState();
    const { text, failed } = await this.options.send();
    this.notice = { id, text: `已保存。${text}`, failed };
    this.renderSaveState();
  }

  /** Cues the selected flag's reveal on the renderer, as last sent. */
  private async cue() {
    const selected = this.selected();
    if (!selected?.reveal || !this.options.cue) return;
    this.notice = { id: selected.id, text: `正在触发“${selected.name}”展开…` };
    this.renderSaveState();
    const { text, failed } = await this.options.cue(selected);
    this.notice = { id: selected.id, text, failed };
    this.renderSaveState();
  }

  /** The image's width / height as the flag shows it, turned by its artwork rotation. */
  private async imageAspect(ad: LocalAd, file = ad.material) {
    if (!file) throw new Error('旗帜图片尚未载入，无法按图片锁定宽高比；请先加载图片。');
    const bitmap = await createImageBitmap(file), ratio = bitmap.width / bitmap.height;
    bitmap.close();
    return ad.artworkRotation === 90 || ad.artworkRotation === 270 ? 1 / ratio : ratio;
  }

  /** Enables or disables one ad, as its switch here or in the overall list does. */
  async toggleVisible(id: string) {
    const ad = this.items.find((item) => item.id === id);
    if (!ad) return;
    if (this.busy) await this.saving.catch((): undefined => undefined);
    if (this.planeEdit || this.drawing) {
      this.options.error(new Error('本地广告位正在拖动或绘制，请完成后再切换启用状态。'));
      return;
    }
    const next = { ...ad, visible: !ad.visible };
    if (!next.visible && id === this.selectedId) this.unlocked = false;
    await this.commit(this.items.map((item) => item.id === id ? next : item), this.selectedId, `${next.visible ? '启用' : '停用'} ${next.name}`);
  }

  /** Why `id` cannot be selected now, or undefined after selecting it (or after the operator kept unsaved edits). */
  async selectFromList(id: string): Promise<string | undefined> {
    if (this.busy) await this.saving.catch((): undefined => undefined);
    if (!this.items.some((ad) => ad.id === id)) return '找不到该广告位，可能已被删除。';
    if (!this.ads.some((ad) => ad.id === id)) return '该广告位所在的点云未导入：在下方导入同一 PLY 后才能编辑。';
    if (!this.available || !this.loaded) return '编辑器尚未就绪：等待预览加载完成后再选择。';
    if (this.drawing || this.planeEdit) return '编辑器正在绘制或拖动，请完成后再选择。';
    this.selectAd(id);
  }

  selectAd(id: string) {
    if (!this.available || !this.loaded || this.busy || this.drawing || this.planeEdit || !this.ads.some((ad) => ad.id === id)) return false;
    if (id !== this.selectedId && !this.discardUnsaved()) return false;
    this.cancelDrawing();
    if (id !== this.selectedId) { this.unlocked = false; this.notice = undefined; }
    this.selectedId = id;
    this.render();
    void this.publish().catch(this.options.error);
    return true;
  }

  /** A click on `id` in the 3D view selects it; dragging it needs it unlocked. */
  beginPlaneEdit(id: string) {
    const selectedBefore = id === this.selectedId;
    if (!this.selectAd(id)) return false;
    if (this.locked) {
      if (selectedBefore) this.options.error(new Error('已锁定：点击“🔒 已锁定”解锁后，才能拖动移动或调整大小。'));
      return false;
    }
    const original = this.selected();
    this.planeEdit = { original, candidate: original };
    this.render();
    return true;
  }

  previewPlane(ad: LocalAd) {
    if (this.planeEdit?.original.id !== ad.id) return;
    this.planeEdit.candidate = { ...this.planeEdit.original, plane: ad.plane };
    this.render();
    void this.publish().catch(this.options.error);
  }

  async finishPlaneEdit(cancelled: boolean) {
    const edit = this.planeEdit;
    if (!edit) return;
    this.planeEdit = undefined;
    if (!cancelled && edit.candidate !== edit.original) {
      await this.commit(this.items.map((ad) => ad.id === edit.original.id ? edit.candidate : ad), edit.original.id, `拖动 ${edit.original.name}`);
    }
    this.render();
    await this.publish();
  }

  /** Reads the match's placements again, such as after another page changed them. */
  async reload() {
    const id = this.matchId;
    this.matchId = undefined;
    await this.setMatch(id);
  }

  async setMatch(id?: string) {
    if (this.matchId === id) return;
    const version = ++this.version;
    this.cancelDrawing();
    this.matchId = id;
    this.items = [];
    this.selectedId = undefined;
    this.flagDraft = this.notice = this.loadNotice = undefined;
    this.unlocked = false;
    this.loaded = false;
    this.busy = false;
    this.render();
    await this.publish();
    if (!id) return;
    try {
      const stored = await this.repository.read(id);
      let ads = stored.ads.filter((ad) => (ad.space ?? 'cloud') === this.options.space);
      if (version !== this.version) return;
      const missing = stored.missing.filter((text) => ads.some((ad) => text.startsWith(`${ad.name}（`)));
      if (missing.length) this.loadNotice = `无法从服务器读取以下素材，预览中显示占位区域：${missing.join('、')}。`;
      const { kept, removed } = withoutPitchFlags(ads);
      if (removed) {
        // Flags are point-cloud only now; storing the rest says so only once.
        await this.repository.write(id, kept, this.options.space);
        if (version !== this.version) return;
        ads = kept;
        this.loadNotice = `已移除 ${removed} 个球场旗帜：旗帜现在只能在点云平面上放置；其他广告位不变。`;
      }
      this.items = ads;
      this.loaded = true;
      this.selectedId = this.initialSelection();
      this.render();
      await this.publish();
    } catch (error) {
      if (version === this.version) { this.options.error(error); this.render(); }
    }
  }

  setAvailable(available: boolean) {
    if (this.available === available) return;
    this.available = available;
    if (!available) this.cancelDrawing();
    else this.render();
  }
  setCloud(id?: string) {
    if (this.cloudId === id) return;
    this.cancelDrawing();
    this.cloudId = id;
    this.flagDraft = undefined;
    this.selectedId = this.initialSelection();
    this.unlocked = false;
    this.render();
    void this.publish().catch(this.options.error);
  }
  private start(type: AdType, replaceId?: string) {
    if (!this.available || !this.loaded || this.busy || (this.options.space === 'cloud' && !this.cloudId)) return;
    if (replaceId && this.locked) { this.options.error(new Error(LOCKED)); return; }
    if (!replaceId && !this.discardUnsaved()) return;
    this.drawing = { type, replaceId };
    this.element('local-draw-status').textContent = this.options.space === 'pitch'
      ? '已切换俯视图：在上方草坪内拖出矩形底面，再填写素材的服务器路径；Esc 取消。'
      : `请依次点击下方点云：① 起点 ② 宽度方向端点 ③ 高度方向点。${type === 'flag' ? '旗帜会自动摆正，使图片从当前视角正读、上边朝上。' : ''}拖动旋转，滚轮缩放；Esc 取消。`;
    this.render();
    this.options.pick();
  }
  picked(count: number) {
    this.element('local-draw-status').textContent = `已选择 ${count} / 3 点。${count === 1 ? '请选择宽度方向端点。' : '请选择高度方向点，不能与前两点共线。'}`;
  }
  async finish(points: Vector3Tuple[], normalSign: number) {
    try { await this.finishPlane(planeFromPoints(points), normalSign); }
    catch (error) { this.cancelDrawing(); this.options.error(error); }
  }
  async finishPlane(plane: AdPlane, normalSign = -1) {
    const drawing = this.drawing;
    if (!drawing || !this.available || (this.options.space === 'cloud' && !this.cloudId)) return;
    const previous = this.items.find((ad) => ad.id === drawing.replaceId), pitch = this.options.space === 'pitch';
    if (drawing.type === 'flag') {
      plane = orientFlagPlane(plane, normalSign);
      normalSign = -1;
    }
    const flag = drawing.type === 'flag';
    const ad: LocalAd = previous ? { ...previous, plane, normalSign }
      : { id: crypto.randomUUID(), name: `${pitch ? '球场' : '点云'}${flag ? '旗帜' : '广告'} ${this.items.length + 1}`, type: drawing.type,
        space: this.options.space, ...(pitch ? {} : { cloudId: this.cloudId }),
        // Off a picked surface, a flag starts in front of it (towards the viewer, -normal) so its wave clears it.
        plane, normalSign, offset: flag ? -FLAG_CLEARANCE : 0, scale: 1, yaw: 0, visible: true,
        ...(flag ? { flag: { ...DEFAULT_FLAG } } : {}) };
    this.cancelDrawing();
    await this.commit([...this.items.filter((item) => item.id !== ad.id), ad], ad.id, `${previous ? '重画' : '新建'} ${ad.name}`);
  }
  cancelDrawing() {
    if (this.planeEdit) {
      this.planeEdit = undefined;
      void this.publish().catch(this.options.error);
    }
    this.drawing = undefined;
    this.options.cancelPick();
    this.element('local-draw-status').textContent = this.options.space === 'pitch'
      ? '在俯视图拖出底面：2D 图片贴地，3D 模型向上立起；在俯视图点击可选中。旗帜请在下方点云中选面放置。无需导入 PLY。'
      : '点击广告选中；解锁后拖广告内部移动，拖边角控制点调整大小（保持原平面），松开自动保存，Esc 取消；拖空白旋转，右键平移。';
    this.render();
  }

  /** The selected ad with the form's values, after the operator changed `input`. */
  private async edited(selected: LocalAd, input: HTMLInputElement): Promise<{ next: LocalAd; action: string }> {
    const data = new FormData(this.form), scalar = (name: string) => Number(data.get(name)), plane = selected.plane;
    // A flag locked to its image's aspect follows the image whenever its rotation changes.
    const fitImage = async (ad: LocalAd) => ad.aspectLocked ? { ...ad, plane: planeWithAspect(ad.plane, this.anchor, await this.imageAspect(ad)) } : ad;
    switch (input.name) {
      case 'artworkRotation':
        return { next: await fitImage({ ...selected, artworkRotation: Number(input.value) as LocalAd['artworkRotation'] }), action: '旋转图片' };
      case 'aspectLocked':
        return { next: await fitImage({ ...selected, aspectLocked: input.checked }), action: input.checked ? '锁定宽高比' : '解除宽高比锁定' };
      case 'revealDirection': {
        const { reveal, ...rest } = selected;
        if (input.value && !isRevealDirection(input.value)) throw new Error(`未知的展开方式：${input.value}`);
        return { next: isRevealDirection(input.value) ? { ...rest, reveal: { duration: REVEAL_DURATION, start: 'load', ...reveal, direction: input.value } } : rest, action: '设置展开' };
      }
      case 'revealDuration': return { next: { ...selected, reveal: { ...selected.reveal, duration: input.valueAsNumber } }, action: '设置展开时长' };
      case 'revealStart': return { next: { ...selected, reveal: { ...selected.reveal, start: input.value === 'cue' ? 'cue' : 'load' } }, action: '设置展开开始' };
      case 'name': return { next: { ...selected, name: String(data.get('name')).trim() }, action: '重命名' };
      case 'z': return { next: { ...selected, plane: { ...plane, origin: [plane.origin[0], plane.origin[1], scalar('z')] } }, action: '调整高度' };
      case 'offset': return { next: { ...selected, offset: scalar('offset') }, action: '调整法线偏移' };
      case 'scale': return { next: { ...selected, scale: scalar('scale') }, action: '缩放' };
      case 'yaw': return { next: { ...selected, yaw: scalar('yaw') }, action: '转向' };
      case 'flip': return { next: { ...selected, normalSign: input.checked ? -1 : 1 }, action: '反转' };
      case 'opacity': case 'opacitySlider': return { next: { ...selected, opacity: input.valueAsNumber }, action: '调整不透明度' };
      case 'aspect': return { next: { ...selected, plane: planeWithAspect(plane, this.anchor, parseAspect(input.value)) }, action: '设置宽高比' };
      case 'width': case 'height': {
        const keep = selected.type === 'flag' ? Boolean(selected.aspectLocked) : this.keepAspect;
        return { next: { ...selected, plane: resizePlane(plane, this.anchor, input.name, input.valueAsNumber, keep) }, action: '调整尺寸' };
      }
      default: {
        const point = planeAnchor(plane, this.anchor);
        const to: Vector3Tuple = [scalar('anchorX'), scalar('anchorY'), this.options.space === 'pitch' ? point[2] : scalar('anchorZ')];
        return { next: { ...selected, plane: movePlaneAnchor(plane, this.anchor, to) }, action: '移动' };
      }
    }
  }

  /** Applies one changed field; resolves whether it was saved. Never ignores an edit without saying why. */
  private async apply(input: HTMLInputElement): Promise<boolean> {
    const selected = this.selected();
    const unchanged = input.type === 'checkbox' ? input.checked === input.defaultChecked : input.value === input.defaultValue;
    if (!selected || unchanged) return true;
    const refuse = (text: string) => { this.render(); this.options.error(new Error(text)); return false; };
    if (this.locked) return refuse(LOCKED);
    if (this.busy) await this.saving.catch((): undefined => undefined);
    if (this.planeEdit) return refuse('正在拖动广告位，请松开后再修改数值。');
    if ((input.type === 'number' && !Number.isFinite(input.valueAsNumber)) || (input.name === 'aspect' && !parseAspect(input.value)))
      return refuse(input.name === 'aspect' ? '宽高比请输入 16:9 这样的 W:H，或 1.78 这样的正数。' : '请输入数值。');
    if (input.type === 'number' && ((input.min && input.valueAsNumber < Number(input.min)) || (input.max && input.valueAsNumber > Number(input.max))))
      return refuse(input.max ? `该数值应介于 ${input.min} – ${input.max}。` : `该数值应不小于 ${input.min}。`);
    let edit: { next: LocalAd; action: string };
    try { edit = await this.edited(selected, input); }
    catch (error) { return refuse(error instanceof Error ? error.message : String(error)); }
    const { next, action } = edit;
    if (!next.name) return refuse('请输入广告位名称。');
    return this.commit(this.items.map((ad) => ad.id === next.id ? next : ad), next.id, `${action} ${next.name}`);
  }

  /** `path` is where the onsite server holds `file`; chr-server reads the material there. */
  private async setMaterial(selected: LocalAd, file: File, path?: string) {
    if (this.locked) { this.options.error(new Error(LOCKED)); return; }
    const version = this.version;
    this.busy = true;
    this.render();
    try {
      if (selected.type === 'flag') {
        if (!/\.(png|jpe?g)$/i.test(file.name) || file.size > 20 * 1024 * 1024 || !file.size) throw new Error('旗帜图片需为不超过 20 MB 的 PNG / JPEG。');
      }
      let plane = selected.plane;
      if (selected.type !== '3D') {
        if (!/\.(png|jpe?g|webp)$/i.test(file.name) || file.size > 20 * 1024 * 1024 || !file.size) throw new Error('图片素材需为不超过 20 MB 的 PNG / JPEG / WebP。');
        const bitmap = await createImageBitmap(file);
        const valid = bitmap.width <= 8192 && bitmap.height <= 8192;
        bitmap.close();
        if (!valid) throw new Error('图片宽高不得超过 8192 像素。');
        if (selected.aspectLocked) plane = planeWithAspect(plane, this.anchor, await this.imageAspect(selected, file));
      } else {
        if (!/\.glb$/i.test(file.name) || !file.size || file.size > 64 * 1024 * 1024) throw new Error('模型素材需为不超过 64 MB 的自包含 GLB 文件。');
        const buffer = await file.arrayBuffer(), view = new DataView(buffer);
        if (buffer.byteLength < 20 || view.getUint32(0, true) !== 0x46546c67 || view.getUint32(4, true) !== 2 || view.getUint32(8, true) !== buffer.byteLength) throw new Error('不是有效的 GLB 2.0 文件。');
        await validateLocalAdMaterial({ ...selected, material: file });
      }
      // A file chosen on this computer is stored on the onsite server, where chr-server can read it.
      const materialPath = path ?? await uploadFile(file, file.name);
      // Noted now, so a send describes the material without downloading it.
      const facts = await materialFacts(selected.type, file, await serverSha256(materialPath), { bounds: glbBounds });
      if (version !== this.version) return;
      this.busy = false;
      await this.commit(this.items.map((ad) => ad.id !== selected.id ? ad
        : { ...ad, plane, material: file, materialPath, materialName: file.name, ...facts }), selected.id, `更换素材 ${selected.name}`);
    } catch (error) { this.options.error(error); }
    finally { if (version === this.version) { this.busy = false; this.render(); } }
  }

  /** Saves `next`; with `label` the change becomes an undo step. Resolves whether it was saved. */
  private commit(next: LocalAd[], selectedId: string | undefined, label?: string): Promise<boolean> {
    if (!this.matchId || this.busy || !this.loaded) return Promise.resolve(false);
    const id = this.matchId, version = this.version, before = { items: this.items, selectedId: this.selectedId };
    this.busy = true;
    this.render();
    const saving = (async () => {
      try {
        next.forEach((ad) => this.options.validate?.(ad));
        await this.repository.write(id, next, this.options.space);
        if (version !== this.version) return false;
        this.items = next;
        this.loadNotice = undefined;
        if (selectedId !== this.selectedId) this.unlocked = false;
        this.selectedId = selectedId;
        if (label) this.options.recordUndo?.(label, () => this.restore(version, before.items, before.selectedId),
          () => this.restore(version, next, selectedId));
        await this.publish();
        return true;
      } catch (error) { if (version === this.version) this.options.error(error); return false; }
      finally { if (version === this.version) { this.busy = false; this.render(); } }
    })();
    this.saving = saving;
    return saving;
  }

  private async restore(version: number, items: LocalAd[], selectedId?: string) {
    if (version !== this.version) return;
    if (this.busy || this.drawing || this.planeEdit) throw new Error('本地广告位正在编辑或保存，请稍后再撤销或重做。');
    if (!items.some((ad) => ad.id === selectedId && ad.visible)) this.unlocked = false;
    if (!await this.commit(items, selectedId)) throw new Error('撤销或重做本地广告位失败，请重试。');
  }

  private renderSaveState() {
    const selected = this.selected(), state = this.element('local-save-state');
    if (!selected) return;
    const dirty = (this.flagDraft?.id === selected.id && this.flagDirty) || Boolean(this.typedField());
    const notice = this.notice?.id === selected.id ? this.notice : undefined;
    state.textContent = dirty
      ? `${notice?.failed ? `${notice.text} ` : ''}有未保存的修改：点“${selected.type === 'flag' ? '保存旗帜属性' : '保存属性'}”${selected.type === 'flag' ? '或在旗帜字段中按 Enter' : '、按 Enter 或离开输入框'}保存。`
      : notice?.text ?? '已保存。';
    state.classList.toggle('on-form-error', Boolean(notice?.failed));
    state.dataset.saveState = dirty ? 'unsaved' : 'saved';
  }

  private renderFlagNote(selected: LocalAd) {
    const note = this.form.querySelector('[data-flag-note]'), side = '旗面法线背向选面视角：法线偏移为负值时朝向观看侧';
    if (this.draftOf(selected).type === 'native') {
      const reach = (selected.plane.height / NATIVE_SLACK / 2).toFixed(2);
      note.textContent = '上下两边固定、左右两侧自由的布料，比上下间距长 10%，由渲染器按风速、风向和张力实时物理模拟；'
        + `本页预览只显示静止的初始弧形，飘动请在渲染输出中查看，保存并发送后生效。${side}，贴墙时建议 ≤ −${reach}。`;
      return;
    }
    const parsed = this.flagDraft?.id === selected.id ? parseFlagDraft(this.flagDraft) : undefined;
    const amplitude = (parsed && 'wave' in parsed ? parsed.wave.amplitude : undefined) ?? selected.flag.amplitude, peak = (FLAG_PEAK * amplitude).toFixed(2);
    note.textContent = `中线固定、两侧向外飘动，最大摆幅约 ${peak} m（离开平面两侧）；拖动滑块即在预览中试看，保存后才生效。${side}，贴墙时建议 ≤ −${peak}。`;
  }

  private render() {
    const pitch = this.options.space === 'pitch';
    const available = Boolean(this.matchId && (pitch || this.cloudId) && this.loaded && this.available && !this.busy);
    const ready = available && !this.planeEdit;
    this.options.manipulationAvailable?.(available && !this.drawing, this.locked);
    for (const type of ['2d', '3d', 'flag']) {
      const button = this.element<HTMLButtonElement>(`draw-local-${type}`);
      if (button) button.disabled = !ready || Boolean(this.drawing);
    }
    this.element('cancel-local-draw').hidden = !this.drawing;
    const select = this.element<HTMLSelectElement>('local-ad-select');
    select.replaceChildren(...this.ads.map((ad) => new Option(`${typeLabel(ad.type)} · ${ad.name}${ad.material ? '' : '（待上传素材）'}${ad.visible ? '' : '（已停用）'}`, ad.id)));
    if (this.selectedId) select.value = this.selectedId;
    select.disabled = !ready || Boolean(this.drawing);
    const selected = this.selected(), flag = selected?.type === 'flag';
    if (selected && !selected.visible) this.unlocked = false;
    this.form.hidden = !selected;
    this.element('local-ad-state').hidden = !selected;
    const open = Boolean(this.matchId && (pitch || this.cloudId) && this.loaded && this.available) && !this.planeEdit && !this.drawing;
    // Saving does not disable the fields: that would drop keyboard focus on Tab / Enter. `apply` waits for the save.
    const editable = open && Boolean(selected?.visible) && !this.locked;
    this.options.root.querySelectorAll<HTMLFieldSetElement>('[data-locked-controls]').forEach((fieldset) => { fieldset.disabled = !editable; });
    // The wave is not position: it stays editable while locked or disabled, and is saved explicitly.
    const flagGroup = this.element<HTMLFieldSetElement>('local-flag-controls');
    flagGroup.hidden = !flag;
    flagGroup.disabled = !flag || !open;
    if (!flag) cancelAnimationFrame(this.windFrame);
    this.element('save-send-local').hidden = !this.options.send;
    this.element('local-ad-count').textContent = `${this.ads.length} 个${pitch ? '球场本地' : '点云'}广告位`;
    this.element('local-ad-empty').hidden = Boolean(this.ads.length);
    this.element('local-ad-storage').textContent = this.planeEdit ? '正在拖动预览；松开自动保存，Esc 恢复原位置和尺寸。' : this.busy ? '正在保存本地广告…'
      : this.loadNotice ? this.loadNotice
      : selected && !selected.visible ? `此广告位已停用，不在预览中显示；启用并解锁后才能修改位置、尺寸和素材${flag ? '，旗帜飘动属性仍可修改并保存' : ''}。`
      : selected && this.locked ? `位置、尺寸和素材已锁定，解锁后修改，按 Enter 或离开输入框即保存${flag ? '；旗帜飘动属性无需解锁，修改后点“保存旗帜属性”' : ''}。`
      : pitch ? '几何与素材按场次保存在现场服务器，不回传 Fusion；无需 PLY 即可恢复。'
        : '几何与素材按场次保存在现场服务器，不回传 Fusion；确认点云坐标后才能发送到渲染器。重新导入相同 PLY 即可恢复。';
    if (!selected) return;
    const enabled = this.element<HTMLButtonElement>('local-ad-enabled');
    enabled.disabled = !ready || Boolean(this.drawing);
    enabled.setAttribute('aria-checked', String(selected.visible));
    enabled.setAttribute('aria-label', `启用广告位 ${selected.name}`);
    enabled.lastElementChild.textContent = selected.visible ? '启用' : '停用';
    const lock = this.element<HTMLButtonElement>('local-ad-lock');
    lock.disabled = !ready || Boolean(this.drawing) || !selected.visible;
    lock.setAttribute('aria-pressed', String(this.unlocked));
    lock.textContent = this.unlocked ? '🔓 已解锁' : '🔒 已锁定';
    const form = this.form;
    const set = (name: string, value: number | string) => {
      const input = form.elements.namedItem(name) as HTMLInputElement, text = typeof value === 'number' ? String(Math.round(value * 1000) / 1000) : value;
      // Keep what the operator is typing in the focused field; it is applied on its own change.
      if (input === document.activeElement && input.value !== input.defaultValue) input.defaultValue = text;
      else input.value = input.defaultValue = text;
    };
    const show = (selector: string, visible: boolean) => form.querySelectorAll<HTMLElement>(selector).forEach((element) => {
      element.hidden = !visible;
      element.querySelectorAll('input').forEach((input) => { input.disabled = !visible; });
    });
    const { plane } = selected, point = planeAnchor(plane, this.anchor);
    set('name', selected.name);
    (form.querySelector(`[name=anchor][value=${this.anchor}]`) as HTMLInputElement).checked = true;
    (form.elements.namedItem('keepAspect') as HTMLInputElement).checked = this.keepAspect;
    form.querySelectorAll('[data-anchor-label]').forEach((label) => { label.textContent = this.anchor === 'top-left' ? '左上角' : '中心'; });
    set('anchorX', point[0]); set('anchorY', point[1]); set('width', plane.width); set('height', plane.height); set('aspect', plane.width / plane.height);
    if (pitch) {
      show('[data-model-only]', selected.type === '3D');
      set('z', plane.origin[2]);
    } else {
      set('anchorZ', point[2]); set('offset', selected.offset);
    }
    show('[data-model-group]', selected.type === '3D');
    show('[data-image-group]', selected.type === '2D');
    show('[data-flag-group]', flag);
    show('[data-keep-aspect]', !flag);
    show('[data-aspect-input]', !(flag && selected.aspectLocked));
    show('[data-reveal-only]', flag && Boolean(selected.reveal));
    set('opacity', imageOpacity(selected)); set('opacitySlider', imageOpacity(selected));
    set('scale', selected.scale); set('yaw', selected.yaw);
    const flip = form.elements.namedItem('flip') as HTMLInputElement | null;
    if (flip) flip.checked = flip.defaultChecked = selected.normalSign === -1;
    this.element('local-ad-readonly').textContent = selected.type === '2D' ? `${pitch ? '贴地图片' : '贴面图片'}，图片上边沿 v 反方向 · 暂不支持调整旋转`
      : flag ? `旗帜，图片从选面一侧正读，上边沿 v 反方向${selected.fusionTifoId ? ` · 由 Fusion Tifo ${selected.fusionTifoId} 导入，可自由编辑` : ''}`
      : `模型从平面${pitch ? '向上' : '沿法线'}立起，可调缩放与朝向`;
    const actions = this.element('local-reveal-actions');
    actions.hidden = !flag || !selected.reveal;
    if (flag) {
      const select = (name: string, value: string) => { (form.elements.namedItem(name) as HTMLSelectElement).value = value; };
      select('artworkRotation', String(selected.artworkRotation ?? 0));
      select('revealDirection', selected.reveal?.direction ?? '');
      select('revealStart', selected.reveal?.start ?? 'load');
      set('revealDuration', selected.reveal?.duration ?? REVEAL_DURATION);
      const aspectLocked = form.elements.namedItem('aspectLocked') as HTMLInputElement;
      aspectLocked.checked = aspectLocked.defaultChecked = Boolean(selected.aspectLocked);
      const cue = selected.reveal?.start === 'cue';
      this.element<HTMLButtonElement>('preview-reveal').disabled = !open || !selected.visible;
      const cueButton = this.element<HTMLButtonElement>('cue-reveal');
      cueButton.hidden = !cue || !this.options.cue;
      cueButton.disabled = !open || !selected.visible;
      const how = selected.reveal ? REVEAL_LABELS[selected.reveal.direction] : '';
      actions.querySelector('[data-reveal-note]').textContent = cue
        ? `手动触发：发送到渲染器后旗帜保持隐藏，点“触发展开”后${how}；再次点击只重新展开这面旗。修改需先保存并发送。`
        : `加载时：场景载入渲染器后自动${how}。“预览展开”只在本页预览中播放。`;
      // Unsaved typed values win over the saved flag, so a re-render never loses them.
      const draft = this.draftOf(selected);
      for (const key of PAIRED) for (const name of [key, `${key}Slider`]) {
        const input = form.elements.namedItem(name) as HTMLInputElement;
        if (input !== document.activeElement) input.value = draft[key];
      }
      (form.elements.namedItem('autoDirection') as HTMLInputElement).checked = draft.autoDirection;
      for (const radio of form.querySelectorAll<HTMLInputElement>('[name=flagType]')) radio.checked = radio.value === draft.type;
      for (const radio of form.querySelectorAll<HTMLInputElement>('[name=flagMaterial]')) radio.checked = radio.value === draft.material;
      this.renderFlagMotion(selected);
    }
    this.element('save-local').textContent = flag ? '保存旗帜属性' : '保存属性';
    this.renderSaveState();
    const input = this.element<HTMLInputElement>('local-ad-material');
    input.accept = selected.type === '3D' ? '.glb' : flag ? '.png,.jpg,.jpeg' : '.png,.jpg,.jpeg,.webp';
    this.element('local-ad-material-label').textContent = `${flag ? '旗帜图片（PNG / JPEG）' : selected.type === '3D' ? 'GLB 模型' : '图片（PNG / JPEG）'}：填写服务器路径，或选择本地文件（上传到服务器存储）`;
    const path = this.element<HTMLInputElement>('local-ad-material-path');
    path.placeholder = flag ? '/srv/flags/team.png（--asset-root 之内）' : selected.type === '3D' ? '/srv/models/car.glb（--asset-root 之内）' : '/srv/ads/logo.png（--asset-root 之内）';
    if (document.activeElement !== path) path.value = selected.materialPath ?? '';
    this.element('local-ad-material-name').textContent = selected.materialPath
      ? `${selected.material?.name ?? selected.materialName ?? '未载入预览'} · 渲染器读取 ${selected.materialPath}`
      : selected.material ? `${selected.material.name}（未设置服务器路径，不会发送到渲染器）`
      : `请填写${flag ? '旗帜图片' : selected.type === '2D' ? '图片' : '内嵌贴图的 GLB'}的服务器路径，或选择本地文件`;
  }
}
