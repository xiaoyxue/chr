// The editor of one space's local placements: 2D images and 3D models on the
// pitch; images, models and flags on point-cloud planes. Both spaces share the inspector: position, size and
// material are protected by the lock and saved as soon as a field changes; a
// flag's wave is editable while locked and saved explicitly.
import { withoutPitchFlags } from './local-ad.ts';
import { localAdStore } from './local-ad-store.ts';
import { uploadFile } from '../shared/studio-store.ts';
import { fetchServerFile, serverPath } from '../shared/server-files.ts';
import { planeFromPoints, validateLocalAdMaterial } from './local-ad-scene.ts';
import { parseAspect, type Anchor } from './ground-edit.ts';
import { movePlaneAnchor, planeAnchor, planeWithAspect, resizePlane } from './plane-edit.ts';
import { DEFAULT_FLAG, FLAG_CLEARANCE, FLAG_LIMITS, FLAG_PEAK, flagDraft, flagDraftChanged, orientFlagPlane, parseFlagDraft, type FlagDraft } from './flag.ts';
import type { AdPlane, AdSpace, LocalAd, Vector3Tuple } from './local-ad.ts';

type AdType = LocalAd['type'];
const WAVE = [['amplitude', '振幅 / m', 0.01], ['frequency', '频率（中线到边缘周数）', 0.05], ['speed', '速度 / ×', 0.05]] as const;
const FLAG_INPUTS = new Set(['flagMaterial', ...WAVE.flatMap(([key]) => [key, `${key}Slider`])]);
const typeLabel = (type: AdType) => type === 'flag' ? '旗帜' : type;
const LOCKED = '已锁定：点击“🔒 已锁定”解锁后，才能修改位置、尺寸和素材。';

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
        <div class="on-aspect-row"><label>宽高比 W:H<input name="aspect" type="text" inputmode="decimal" aria-label="宽高比，如 16:9 或 1.78" /></label>
        <label class="on-check"><input name="keepAspect" type="checkbox" checked /> 锁定宽高比</label></div>
        <p class="on-help">输入 16:9 或 1.78 后按 Enter：保持宽和锚点，按比例调整高。</p></div>
      <div data-model-group class="on-inspector-group"><h3>模型</h3>
        <div class="on-field-grid">${field('scale', '整体缩放 / ×', 'min="0.01" max="1000"')}${field('yaw', '朝向 / °', 'min="-360" max="360"')}</div>
        ${pitch ? '' : '<label class="on-check"><input name="flip" type="checkbox" /> 反转模型立起方向（平面法线）</label>'}</div>
      <p ${id('local-ad-readonly')} class="on-readonly"></p>
    </fieldset>
    <fieldset ${id('local-flag-controls')} class="on-inspector-group"><h3>旗帜飘动</h3>${wave}
      <div class="on-slider-row"><span>材质</span><div class="on-segmented" role="radiogroup" aria-label="旗帜材质"><label><input type="radio" name="flagMaterial" value="simple" />简易双面光照</label><label><input type="radio" name="flagMaterial" value="pbr" />PBR</label></div></div>
      <p data-flag-note class="on-help"></p></fieldset>
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
  /** Typed but unsaved wave fields of one flag; the preview already shows them. */
  private flagDraft?: FlagDraft & { id: string };
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
      else this.renderSaveState();
    });
    this.element('local-flag-controls').addEventListener('keydown', (event) => {
      if (event.key !== 'Enter' || !(event.target instanceof HTMLInputElement)) return;
      event.preventDefault();
      void this.save();
    });
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
    // A slider and its number field show one value.
    if (input.name.endsWith('Slider')) pair(input.name.slice(0, -'Slider'.length)).value = input.value;
    else if (input.type === 'number') pair(`${input.name}Slider`).value = input.value;
    this.flagDraft = { id: selected.id, amplitude: pair('amplitude').value, frequency: pair('frequency').value, speed: pair('speed').value,
      material: form.querySelector<HTMLInputElement>('[name=flagMaterial]:checked')?.value === 'pbr' ? 'pbr' : 'simple' };
    if (this.notice?.failed) this.notice = undefined;
    this.renderSaveState();
    this.renderFlagNote(selected);
    void this.publish().catch(this.options.error);
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
  private edited(selected: LocalAd, input: HTMLInputElement): { next: LocalAd; action: string } {
    const data = new FormData(this.form), scalar = (name: string) => Number(data.get(name)), plane = selected.plane;
    switch (input.name) {
      case 'name': return { next: { ...selected, name: String(data.get('name')).trim() }, action: '重命名' };
      case 'z': return { next: { ...selected, plane: { ...plane, origin: [plane.origin[0], plane.origin[1], scalar('z')] } }, action: '调整高度' };
      case 'offset': return { next: { ...selected, offset: scalar('offset') }, action: '调整法线偏移' };
      case 'scale': return { next: { ...selected, scale: scalar('scale') }, action: '缩放' };
      case 'yaw': return { next: { ...selected, yaw: scalar('yaw') }, action: '转向' };
      case 'flip': return { next: { ...selected, normalSign: input.checked ? -1 : 1 }, action: '反转' };
      case 'aspect': return { next: { ...selected, plane: planeWithAspect(plane, this.anchor, parseAspect(input.value)) }, action: '设置宽高比' };
      case 'width': case 'height':
        return { next: { ...selected, plane: resizePlane(plane, this.anchor, input.name, input.valueAsNumber, this.keepAspect) }, action: '调整尺寸' };
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
      return refuse(`该数值应介于 ${input.min} – ${input.max}。`);
    const { next, action } = this.edited(selected, input);
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
      if (selected.type !== '3D') {
        if (!/\.(png|jpe?g|webp)$/i.test(file.name) || file.size > 20 * 1024 * 1024 || !file.size) throw new Error('图片素材需为不超过 20 MB 的 PNG / JPEG / WebP。');
        const bitmap = await createImageBitmap(file);
        const valid = bitmap.width <= 8192 && bitmap.height <= 8192;
        bitmap.close();
        if (!valid) throw new Error('图片宽高不得超过 8192 像素。');
      } else {
        if (!/\.glb$/i.test(file.name) || !file.size || file.size > 64 * 1024 * 1024) throw new Error('模型素材需为不超过 64 MB 的自包含 GLB 文件。');
        const buffer = await file.arrayBuffer(), view = new DataView(buffer);
        if (buffer.byteLength < 20 || view.getUint32(0, true) !== 0x46546c67 || view.getUint32(4, true) !== 2 || view.getUint32(8, true) !== buffer.byteLength) throw new Error('不是有效的 GLB 2.0 文件。');
        await validateLocalAdMaterial({ ...selected, material: file });
      }
      // A file chosen on this computer is stored on the onsite server, where chr-server can read it.
      const materialPath = path ?? await uploadFile(file, file.name);
      if (version !== this.version) return;
      this.busy = false;
      await this.commit(this.items.map((ad) => ad.id !== selected.id ? ad : { ...ad, material: file, materialPath, materialName: file.name }), selected.id, `更换素材 ${selected.name}`);
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
    const parsed = this.flagDraft?.id === selected.id ? parseFlagDraft(this.flagDraft) : undefined;
    const amplitude = parsed && 'wave' in parsed ? parsed.wave.amplitude : selected.flag.amplitude, peak = (FLAG_PEAK * amplitude).toFixed(2);
    this.form.querySelector('[data-flag-note]').textContent = `中线固定、两侧向外飘动，最大摆幅约 ${peak} m（离开平面两侧）；拖动滑块即在预览中试看，保存后才生效。`
      + `旗面法线背向选面视角：法线偏移为负值时朝向观看侧，贴墙时建议 ≤ −${peak}。`;
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
    set('scale', selected.scale); set('yaw', selected.yaw);
    const flip = form.elements.namedItem('flip') as HTMLInputElement | null;
    if (flip) flip.checked = flip.defaultChecked = selected.normalSign === -1;
    this.element('local-ad-readonly').textContent = selected.type === '2D' ? `${pitch ? '贴地图片' : '贴面图片'}，图片上边沿 v 反方向 · 暂不支持调整旋转`
      : flag ? '旗帜，图片上边朝上、从选面一侧正读 · 暂不支持调整旋转'
      : `模型从平面${pitch ? '向上' : '沿法线'}立起，可调缩放与朝向`;
    if (flag) {
      // Unsaved typed values win over the saved flag, so a re-render never loses them.
      const draft = this.flagDraft?.id === selected.id ? this.flagDraft : flagDraft(selected.flag);
      for (const [key] of WAVE) for (const name of [key, `${key}Slider`]) {
        const input = form.elements.namedItem(name) as HTMLInputElement;
        if (input !== document.activeElement) input.value = draft[key];
      }
      for (const radio of form.querySelectorAll<HTMLInputElement>('[name=flagMaterial]')) radio.checked = radio.value === draft.material;
      this.renderFlagNote(selected);
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
