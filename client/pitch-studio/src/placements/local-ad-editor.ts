import { LocalAdRepository } from './local-ad.ts';
import { fetchServerFile, serverPath } from '../shared/server-files.ts';
import { planeFromPoints, validateLocalAdMaterial } from './local-ad-scene.ts';
import { anchorPoint, moveAnchor, planeRectangle, rectanglePlane, resizeSide, type Anchor } from './ground-edit.ts';
import type { AdPlane, AdSpace, LocalAd, Vector3Tuple } from './local-ad.ts';

function editorMarkup(space: AdSpace, prefix: string) {
  const pitch = space === 'pitch', id = (name: string) => `id="${prefix}${name}"`;
  const cloudFields: [string, string, string][] = [
    ['x', '起点 X', ''], ['y', '起点 Y', ''], ['z', '起点 Z', ''],
    ['width', '平面宽', 'min="0.01" max="1000"'], ['height', '平面高', 'min="0.01" max="1000"'],
    ['offset', '法线偏移', 'min="-1000" max="1000"'],
    ['scale', '3D 缩放', 'min="0.01" max="1000"'], ['yaw', '3D 朝向 °', 'min="-360" max="360"'],
  ];
  return `<div class="on-card-head"><h2>${pitch ? '本地 2D / 3D 广告位' : '点云广告位'}</h2></div><div class="on-card-body">
    <div class="on-action-row"><button ${id('draw-local-2d')} class="on-button primary" type="button" disabled>${pitch ? '画 2D 广告' : '选面放图片'}</button>
    <button ${id('draw-local-3d')} class="on-button" type="button" disabled>${pitch ? '画 3D 广告' : '选面放模型'}</button>
    <button ${id('cancel-local-draw')} class="on-button" type="button" hidden>取消绘制</button></div>
    <p ${id('local-draw-status')} class="on-help" role="status"></p><p ${id('local-ad-count')} class="on-help"></p>
    <p ${id('local-ad-empty')} class="on-help">${pitch ? '本场次尚未创建本地球场广告位。' : '当前点云尚未创建广告位。'}</p>
    <label>选择${pitch ? '球场' : '点云'}广告位<select ${id('local-ad-select')} aria-label="选择${pitch ? '球场本地' : '点云'}广告位"></select></label>
    <div ${id('local-ad-state')} class="on-target-bar" hidden>
      <button ${id('local-ad-enabled')} class="on-visibility-toggle" type="button" role="switch" aria-checked="true"><span class="on-toggle-track" aria-hidden="true"></span><span></span></button>
      ${pitch ? `<button ${id('local-ad-lock')} class="on-lock-toggle" type="button" aria-pressed="false" title="解锁后才能修改位置、尺寸和素材">🔒 已锁定</button>` : ''}
    </div>
    <form ${id('local-ad-form')} novalidate hidden><fieldset ${id('local-ad-controls')}>
      <label>名称<input name="name" maxlength="80" required /></label>
      ${pitch ? `<div ${id('local-ad-pitch-2d')} class="on-inspector-group"><h3>位置（世界坐标）</h3>
        <div class="on-segmented" role="radiogroup" aria-label="定位锚点"><label><input type="radio" name="anchor" value="top-left" checked />左上角</label><label><input type="radio" name="anchor" value="centre" />中心</label></div>
        <div class="on-field-grid"><label><span data-anchor-label>左上角</span> X / m<input name="anchorX" type="number" step="0.01" required /></label><label><span data-anchor-label>左上角</span> Y / m<input name="anchorY" type="number" step="0.01" required /></label></div>
        <h3>尺寸</h3><div class="on-field-grid"><label>长 X / m<input name="width" type="number" step="0.01" min="0.01" max="1000" required /></label><label>宽 Y / m<input name="height" type="number" step="0.01" min="0.01" max="1000" required /></label></div>
        <label class="on-check"><input name="keepAspect" type="checkbox" checked /> 锁定宽高比</label></div>
      <div ${id('local-ad-pitch-3d')} class="on-inspector-group"><h3>位置（世界坐标）</h3>
        <div class="on-field-grid"><label>中心 X / m<input name="centreX" type="number" step="0.01" required /></label><label>中心 Y / m<input name="centreY" type="number" step="0.01" required /></label>
        <label>Z（负值向上）<input name="z" type="number" step="0.01" required /></label><label>整体缩放 / ×<input name="scale" type="number" step="0.01" min="0.01" max="1000" required /></label></div></div>
      <p ${id('local-ad-readonly')} class="on-readonly"></p>`
    : `<div class="on-field-grid">${cloudFields.map(([name, label, bounds]) => `<label>${label}<input name="${name}" type="number" step="any" ${bounds} required /></label>`).join('')}</div>
      <label><input name="flip" type="checkbox" /> 反转模型立起方向（平面法线）</label>`}
      <label for="${prefix}local-ad-material-path">此广告位的素材（本地或服务器路径）</label>
      <div class="on-source-row"><input ${id('local-ad-material-path')} class="on-path-input" placeholder="/home/…/logo.png 或 model.glb" autocomplete="off" />
      <button ${id('load-local-ad-material')} class="on-button" type="button">按路径加载</button>
      <button ${id('choose-local-ad-material')} class="on-button" type="button">选择本地素材</button>
      <input ${id('local-ad-material')} type="file" accept=".png,.jpg,.jpeg,.webp" hidden /></div>
      <p ${id('local-ad-material-name')} class="on-help"></p><div class="on-action-row">
      <button ${id('redraw-local-plane')} class="on-button" type="button">${pitch ? '重画底面' : '重选平面'}</button>
      <button ${id('delete-local-ad')} class="on-text-button" type="button">删除</button></div>
    </fieldset></form><p ${id('local-ad-storage')} class="on-help"></p></div>`;
}

export class LocalAdEditor {
  private repository = new LocalAdRepository();
  private matchId?: string;
  private version = 0;
  private cloudId?: string;
  private items: LocalAd[] = [];
  private selectedId?: string;
  private busy = false;
  private loaded = false;
  private available = false;
  private drawing?: { type: '2D' | '3D'; replaceId?: string };
  private planeEdit?: { original: LocalAd; candidate: LocalAd };
  // Pitch ads open locked so a stray edit cannot move them; point-cloud ads have no lock.
  private unlocked = false;
  private anchor: Anchor = 'top-left';
  private keepAspect = true;

  constructor(private options: {
    root: HTMLElement;
    prefix: string;
    space: AdSpace;
    update: (ads: LocalAd[], selectedId?: string) => Promise<void>;
    pick: () => void;
    cancelPick: () => void;
    validate?: (ad: LocalAd) => void;
    error: (error: unknown) => void;
    manipulationAvailable?: (available: boolean) => void;
    /** Records a committed change; `restore` puts the previous ads back. */
    recordUndo?: (label: string, restore: () => Promise<void>) => void;
  }) {
    options.root.innerHTML = editorMarkup(options.space, options.prefix);
    this.element('draw-local-2d').addEventListener('click', () => this.start('2D'));
    this.element('draw-local-3d').addEventListener('click', () => this.start('3D'));
    this.element('redraw-local-plane').addEventListener('click', () => {
      const selected = this.selected();
      if (selected) this.start(selected.type, selected.id);
    });
    this.element('cancel-local-draw').addEventListener('click', () => this.cancelDrawing());
    this.element<HTMLSelectElement>('local-ad-select').addEventListener('change', () => {
      const id = this.element<HTMLSelectElement>('local-ad-select').value;
      this.selectAd(id);
    });
    const form = this.element<HTMLFormElement>('local-ad-form');
    form.addEventListener('submit', (event) => {
      event.preventDefault();
      if (document.activeElement instanceof HTMLInputElement) this.apply(document.activeElement);
    });
    form.addEventListener('change', (event) => {
      const input = event.target as HTMLInputElement;
      if (input.name === 'anchor') { this.anchor = input.value as Anchor; this.render(); }
      else if (input.name === 'keepAspect') this.keepAspect = input.checked;
      else if (input.name) this.apply(input);
    });
    this.element('local-ad-enabled').addEventListener('click', () => {
      const selected = this.selected();
      if (!selected || this.busy || this.planeEdit) return;
      const next = { ...selected, visible: !selected.visible };
      if (!next.visible) this.unlocked = false;
      void this.commit(this.items.map((ad) => ad.id === next.id ? next : ad), next.id, `${next.visible ? '启用' : '停用'} ${next.name}`);
    });
    this.element('local-ad-lock')?.addEventListener('click', () => { this.unlocked = !this.unlocked; this.render(); });
    this.element('delete-local-ad').addEventListener('click', () => {
      if (this.busy || !this.selected()) return;
      this.cancelDrawing();
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
      if (!path || !selected || this.busy) return;
      try { await this.setMaterial(selected, await fetchServerFile(path)); }
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
  private inWorkspace(ad: LocalAd) { return this.options.space === 'pitch' || Boolean(this.cloudId && ad.cloudId === this.cloudId); }
  get ads() {
    return this.items.filter((ad) => this.inWorkspace(ad))
      .map((ad) => ad.id === this.planeEdit?.original.id ? this.planeEdit.candidate : ad);
  }
  refresh() { return this.publish(); }
  private selected() { return this.ads.find((ad) => ad.id === this.selectedId); }
  private get locked() { return this.options.space === 'pitch' && !this.unlocked; }

  /** The selected ad with the form's values, after the operator changed `input`. */
  private edited(selected: LocalAd, input: HTMLInputElement): { next: LocalAd; action: string } {
    const data = new FormData(this.element<HTMLFormElement>('local-ad-form'));
    const scalar = (name: string) => Number(data.get(name));
    const name = String(data.get('name')).trim();
    if (input.name === 'name') return { next: { ...selected, name }, action: '重命名' };
    if (this.options.space === 'cloud') {
      return { action: '编辑', next: { ...selected, name,
        plane: { ...selected.plane, origin: [scalar('x'), scalar('y'), scalar('z')], width: scalar('width'), height: scalar('height') },
        offset: scalar('offset'), scale: scalar('scale'), yaw: scalar('yaw'), normalSign: data.has('flip') ? -1 : 1 } };
    }
    let rect = planeRectangle(selected.plane);
    if (selected.type === '3D') {
      if (input.name === 'scale') return { next: { ...selected, scale: scalar('scale') }, action: '缩放' };
      rect = moveAnchor(rect, 'centre', { x: scalar('centreX'), y: scalar('centreY') });
      const plane = rectanglePlane(selected.plane, rect);
      return { next: { ...selected, plane: { ...plane, origin: [plane.origin[0], plane.origin[1], scalar('z')] } }, action: '移动' };
    }
    if (input.name === 'width' || input.name === 'height') {
      rect = resizeSide(rect, this.anchor, input.name === 'width' ? 'sizeX' : 'sizeY', input.valueAsNumber, this.keepAspect);
      return { next: { ...selected, plane: rectanglePlane(selected.plane, rect) }, action: '调整尺寸' };
    }
    rect = moveAnchor(rect, this.anchor, { x: scalar('anchorX'), y: scalar('anchorY') });
    return { next: { ...selected, plane: rectanglePlane(selected.plane, rect) }, action: '移动' };
  }

  private apply(input: HTMLInputElement) {
    const selected = this.selected();
    const unchanged = input.type === 'checkbox' ? input.checked === input.defaultChecked : input.value === input.defaultValue;
    if (!selected || this.busy || this.planeEdit || this.locked || unchanged) return;
    if (input.type === 'number' && !Number.isFinite(input.valueAsNumber)) { this.render(); return; }
    const { next, action } = this.edited(selected, input);
    if (!next.name) { this.options.error(new Error('请输入广告位名称。')); this.render(); return; }
    void this.commit(this.items.map((ad) => ad.id === next.id ? next : ad), next.id, `${action} ${next.name}`);
  }
  private async publish() { await this.options.update(this.ads, this.selectedId); }

  selectAd(id: string) {
    if (!this.available || !this.loaded || this.busy || this.drawing || this.planeEdit || !this.ads.some((ad) => ad.id === id)) return false;
    this.cancelDrawing();
    if (id !== this.selectedId) this.unlocked = false;
    this.selectedId = id;
    this.render();
    void this.publish().catch(this.options.error);
    return true;
  }

  beginPlaneEdit(id: string) {
    if (!this.selectAd(id)) return false;
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

  async setMatch(id?: string) {
    if (this.matchId === id) return;
    const version = ++this.version;
    this.cancelDrawing();
    this.matchId = id;
    this.items = [];
    this.selectedId = undefined;
    this.unlocked = false;
    this.loaded = false;
    this.busy = false;
    this.render();
    await this.publish();
    if (!id) return;
    try {
      const ads = await this.repository.read(id);
      if (version !== this.version) return;
      this.items = ads.filter((ad) => (ad.space ?? 'cloud') === this.options.space);
      this.loaded = true;
      this.selectedId = this.ads[0]?.id;
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
    this.selectedId = this.ads[0]?.id;
    this.unlocked = false;
    this.render();
    void this.publish().catch(this.options.error);
  }
  private start(type: '2D' | '3D', replaceId?: string) {
    if (!this.available || !this.loaded || this.busy || (this.options.space === 'cloud' && !this.cloudId)) return;
    this.drawing = { type, replaceId };
    this.element('local-draw-status').textContent = this.options.space === 'pitch'
      ? '已切换俯视图：在上方草坪内拖出矩形底面，再上传图片或 GLB；Esc 取消。'
      : '请依次点击下方点云：① 起点 ② 宽度方向端点 ③ 高度方向点。拖动旋转，滚轮缩放；Esc 取消。';
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
    const previous = this.items.find((ad) => ad.id === drawing.replaceId);
    const ad: LocalAd = previous ? { ...previous, plane, normalSign }
      : { id: crypto.randomUUID(), name: `${this.options.space === 'pitch' ? '球场' : '点云'}广告 ${this.items.length + 1}`, type: drawing.type,
        space: this.options.space, ...(this.options.space === 'cloud' ? { cloudId: this.cloudId } : {}),
        plane, normalSign, offset: 0, scale: 1, yaw: 0, visible: true };
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
      ? '在俯视图拖出底面：2D 图片贴地，3D 模型向上立起。与下方点云广告独立，无需导入 PLY。'
      : '点击广告选中；拖广告内部移动，拖边角控制点调整大小（保持原平面）。松开自动保存，Esc 取消；拖空白旋转，右键平移。';
    this.render();
  }

  private async setMaterial(selected: LocalAd, file: File) {
    const version = this.version;
    this.busy = true;
    this.render();
    try {
      if (selected.type === '2D') {
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
      if (version !== this.version) return;
      this.busy = false;
      await this.commit(this.items.map((ad) => ad.id === selected.id ? { ...ad, material: file } : ad), selected.id, `更换素材 ${selected.name}`);
    } catch (error) { this.options.error(error); }
    finally { if (version === this.version) { this.busy = false; this.render(); } }
  }
  /** Saves `next`; with `label` the change becomes an undo step. Resolves whether it was saved. */
  private async commit(next: LocalAd[], selectedId: string | undefined, label?: string): Promise<boolean> {
    if (!this.matchId || this.busy || !this.loaded) return false;
    const id = this.matchId, version = this.version, before = { items: this.items, selectedId: this.selectedId };
    this.busy = true;
    this.render();
    try {
      next.forEach((ad) => this.options.validate?.(ad));
      await this.repository.write(id, next, this.options.space);
      if (version !== this.version) return false;
      this.items = next;
      if (selectedId !== this.selectedId) this.unlocked = false;
      this.selectedId = selectedId;
      if (label) this.options.recordUndo?.(label, () => this.restore(version, before.items, before.selectedId));
      await this.publish();
      return true;
    } catch (error) { if (version === this.version) this.options.error(error); return false; }
    finally { if (version === this.version) { this.busy = false; this.render(); } }
  }

  private async restore(version: number, items: LocalAd[], selectedId?: string) {
    if (version !== this.version) return;
    if (this.busy || this.drawing || this.planeEdit) throw new Error('本地广告位正在编辑或保存，请稍后再撤销。');
    if (!items.some((ad) => ad.id === selectedId && ad.visible)) this.unlocked = false;
    if (!await this.commit(items, selectedId)) throw new Error('撤销本地广告位失败，请重试。');
  }

  private render() {
    const pitch = this.options.space === 'pitch';
    const available = Boolean(this.matchId && (pitch || this.cloudId) && this.loaded && this.available && !this.busy);
    const ready = available && !this.planeEdit;
    this.options.manipulationAvailable?.(available && !this.drawing);
    this.element<HTMLButtonElement>('draw-local-2d').disabled = !ready || Boolean(this.drawing);
    this.element<HTMLButtonElement>('draw-local-3d').disabled = !ready || Boolean(this.drawing);
    this.element('cancel-local-draw').hidden = !this.drawing;
    const select = this.element<HTMLSelectElement>('local-ad-select');
    select.replaceChildren(...this.ads.map((ad) => new Option(`${pitch ? ad.type : ad.type === '2D' ? '图片' : '模型'} · ${ad.name}${ad.material ? '' : '（待上传素材）'}${ad.visible ? '' : '（已停用）'}`, ad.id)));
    if (this.selectedId) select.value = this.selectedId;
    select.disabled = !ready || Boolean(this.drawing);
    const selected = this.selected();
    if (selected && !selected.visible) this.unlocked = false;
    this.element('local-ad-form').hidden = !selected;
    this.element('local-ad-state').hidden = !selected;
    // Saving does not disable the fields: that would drop keyboard focus on Tab / Enter. `apply` ignores edits while busy.
    const editable = Boolean(this.matchId && (pitch || this.cloudId) && this.loaded && this.available) && !this.planeEdit
      && !this.drawing && Boolean(selected?.visible) && !this.locked;
    this.element<HTMLFieldSetElement>('local-ad-controls').disabled = !editable;
    this.element('local-ad-count').textContent = `${this.ads.length} 个${pitch ? '球场本地' : '点云'}广告位`;
    this.element('local-ad-empty').hidden = Boolean(this.ads.length);
    this.element('local-ad-storage').textContent = this.planeEdit ? '正在拖动预览；松开自动保存，Esc 恢复原位置和尺寸。' : this.busy ? '正在保存本地广告…'
      : selected && !selected.visible ? '此广告位已停用，不在预览中显示；启用后才能编辑。'
      : selected && this.locked ? '已锁定，避免误触；解锁后修改数值，按 Enter 或离开输入框即保存。'
      : pitch ? '几何与素材按场次保存于本机，不回传 Fusion；无需 PLY 即可恢复。'
        : '几何与素材按场次保存于本机，不回传 Fusion；确认点云坐标后，可在 PROGRAM 中查看，未上传素材时显示占位区域。刷新后重新导入相同 PLY 可恢复。';
    if (!selected) return;
    const enabled = this.element<HTMLButtonElement>('local-ad-enabled');
    enabled.disabled = !ready || Boolean(this.drawing);
    enabled.setAttribute('aria-checked', String(selected.visible));
    enabled.setAttribute('aria-label', `启用广告位 ${selected.name}`);
    enabled.lastElementChild.textContent = selected.visible ? '启用' : '停用';
    const lock = this.element<HTMLButtonElement>('local-ad-lock');
    if (lock) {
      lock.disabled = !ready || Boolean(this.drawing) || !selected.visible;
      lock.setAttribute('aria-pressed', String(this.unlocked));
      lock.textContent = this.unlocked ? '🔓 已解锁' : '🔒 已锁定';
    }
    const form = this.element<HTMLFormElement>('local-ad-form');
    const set = (name: string, value: number | string) => {
      const input = form.elements.namedItem(name) as HTMLInputElement, text = typeof value === 'number' ? String(Math.round(value * 1000) / 1000) : value;
      // Keep what the operator is typing in the focused field; it is applied on its own change.
      if (input === document.activeElement && input.value !== input.defaultValue) input.defaultValue = text;
      else input.value = input.defaultValue = text;
    };
    set('name', selected.name);
    if (pitch) {
      const rect = planeRectangle(selected.plane), point = anchorPoint(rect, this.anchor);
      this.element('local-ad-pitch-2d').hidden = selected.type !== '2D';
      this.element('local-ad-pitch-3d').hidden = selected.type !== '3D';
      for (const input of this.element('local-ad-pitch-2d').querySelectorAll('input')) input.disabled = selected.type !== '2D';
      for (const input of this.element('local-ad-pitch-3d').querySelectorAll('input')) input.disabled = selected.type !== '3D';
      (form.querySelector(`[name=anchor][value=${this.anchor}]`) as HTMLInputElement).checked = true;
      (form.elements.namedItem('keepAspect') as HTMLInputElement).checked = this.keepAspect;
      form.querySelectorAll('[data-anchor-label]').forEach((label) => { label.textContent = this.anchor === 'top-left' ? '左上角' : '中心'; });
      set('anchorX', point.x); set('anchorY', point.y); set('width', rect.sizeX); set('height', rect.sizeY);
      set('centreX', rect.x); set('centreY', rect.y); set('z', selected.plane.origin[2]); set('scale', selected.scale);
      this.element('local-ad-readonly').textContent = selected.type === '2D' ? '贴地图片 · 暂不支持调整旋转'
        : `朝向 ${selected.yaw.toFixed(1)}° · 暂不支持调整旋转；底面大小请用“重画底面”`;
    } else {
      const fields = { x: selected.plane.origin[0], y: selected.plane.origin[1], z: selected.plane.origin[2],
        width: selected.plane.width, height: selected.plane.height, offset: selected.offset, scale: selected.scale, yaw: selected.yaw };
      for (const [name, value] of Object.entries(fields)) set(name, value);
      const flip = form.elements.namedItem('flip') as HTMLInputElement;
      flip.checked = flip.defaultChecked = selected.normalSign === -1;
    }
    const input = this.element<HTMLInputElement>('local-ad-material');
    input.accept = selected.type === '2D' ? '.png,.jpg,.jpeg,.webp' : '.glb';
    this.element('local-ad-material-name').textContent = selected.material?.name ?? (selected.type === '2D' ? '请选择图片素材' : '请选择内嵌贴图的 GLB 模型');
  }
}
