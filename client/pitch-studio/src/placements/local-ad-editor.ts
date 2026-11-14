import { LocalAdRepository } from './local-ad.ts';
import { fetchServerFile, serverPath } from '../shared/server-files.ts';
import { planeFromPoints, validateLocalAdMaterial } from './local-ad-scene.ts';
import type { AdPlane, AdSpace, LocalAd, Vector3Tuple } from './local-ad.ts';

function editorMarkup(space: AdSpace, prefix: string) {
  const pitch = space === 'pitch', id = (name: string) => `id="${prefix}${name}"`;
  const fields: [string, string, string][] = [
    ['x', '起点 X', ''], ['y', '起点 Y', ''], ['z', pitch ? 'Z（负值向上）' : '起点 Z', ''],
    ['width', pitch ? '底面长' : '平面宽', 'min="0.01" max="1000"'],
    ['height', pitch ? '底面宽' : '平面高', 'min="0.01" max="1000"'],
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
    <form ${id('local-ad-form')} hidden><fieldset ${id('local-ad-controls')}>
      <label>名称<input name="name" maxlength="80" required /></label><div class="on-field-grid">
      ${fields.map(([name, label, bounds]) => `<label ${pitch && name === 'offset' ? 'hidden' : ''}>${label}<input name="${name}" type="number" step="any" ${bounds} required /></label>`).join('')}
      </div><label><input name="visible" type="checkbox" checked /> 显示此广告位</label>
      <label ${pitch ? 'hidden' : ''}><input name="flip" type="checkbox" /> 反转模型立起方向（平面法线）</label>
      <label for="${prefix}local-ad-material-path">此广告位的素材（本地或服务器路径）</label>
      <div class="on-source-row"><input ${id('local-ad-material-path')} class="on-path-input" placeholder="/home/…/logo.png 或 model.glb" autocomplete="off" />
      <button ${id('load-local-ad-material')} class="on-button" type="button">按路径加载</button>
      <button ${id('choose-local-ad-material')} class="on-button" type="button">选择本地素材</button>
      <input ${id('local-ad-material')} type="file" accept=".png,.jpg,.jpeg,.webp" hidden /></div>
      <p ${id('local-ad-material-name')} class="on-help"></p><div class="on-action-row">
      <button class="on-button primary" type="submit">保存本地广告位</button>
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
    this.element('local-ad-form').addEventListener('submit', (event) => {
      event.preventDefault();
      const selected = this.selected();
      if (!selected || this.busy || this.planeEdit) return;
      const data = new FormData(this.element<HTMLFormElement>('local-ad-form'));
      const scalar = (name: string) => Number(data.get(name)), pitch = options.space === 'pitch';
      const next: LocalAd = { ...selected, name: String(data.get('name')).trim(),
        plane: { ...selected.plane, origin: [scalar('x'), scalar('y'), pitch && selected.type === '2D' ? 0 : scalar('z')], width: scalar('width'), height: scalar('height') },
        offset: pitch ? 0 : scalar('offset'), scale: scalar('scale'), yaw: scalar('yaw'),
        normalSign: pitch || data.has('flip') ? -1 : 1, visible: data.has('visible') };
      if (!next.name) { this.options.error(new Error('请输入广告位名称。')); return; }
      void this.commit(this.items.map((ad) => ad.id === next.id ? next : ad), next.id);
    });
    this.element('delete-local-ad').addEventListener('click', () => {
      if (this.busy || !this.selected()) return;
      this.cancelDrawing();
      const next = this.items.filter((ad) => ad.id !== this.selectedId);
      void this.commit(next, next.find((ad) => this.inWorkspace(ad))?.id);
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
  private async publish() { await this.options.update(this.ads, this.selectedId); }

  selectAd(id: string) {
    if (!this.available || !this.loaded || this.busy || this.drawing || this.planeEdit || !this.ads.some((ad) => ad.id === id)) return false;
    this.cancelDrawing();
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
      await this.commit(this.items.map((ad) => ad.id === edit.original.id ? edit.candidate : ad), edit.original.id);
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
    await this.commit([...this.items.filter((item) => item.id !== ad.id), ad], ad.id);
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
      await this.commit(this.items.map((ad) => ad.id === selected.id ? { ...ad, material: file } : ad), selected.id);
    } catch (error) { this.options.error(error); }
    finally { if (version === this.version) { this.busy = false; this.render(); } }
  }
  private async commit(next: LocalAd[], selectedId?: string) {
    if (!this.matchId || this.busy || !this.loaded) return;
    const id = this.matchId, version = this.version;
    this.busy = true;
    this.render();
    try {
      next.forEach((ad) => this.options.validate?.(ad));
      await this.repository.write(id, next, this.options.space);
      if (version !== this.version) return;
      this.items = next;
      this.selectedId = selectedId;
      await this.publish();
    } catch (error) { if (version === this.version) this.options.error(error); }
    finally { if (version === this.version) { this.busy = false; this.render(); } }
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
    select.replaceChildren(...this.ads.map((ad) => new Option(`${pitch ? ad.type : ad.type === '2D' ? '图片' : '模型'} · ${ad.name}${ad.material ? '' : '（待上传素材）'}`, ad.id)));
    if (this.selectedId) select.value = this.selectedId;
    select.disabled = !ready || Boolean(this.drawing);
    const selected = this.selected();
    this.element('local-ad-form').hidden = !selected;
    this.element<HTMLFieldSetElement>('local-ad-controls').disabled = !ready || Boolean(this.drawing);
    this.element('local-ad-count').textContent = `${this.ads.length} 个${pitch ? '球场本地' : '点云'}广告位`;
    this.element('local-ad-empty').hidden = Boolean(this.ads.length);
    this.element('local-ad-storage').textContent = this.planeEdit ? '正在拖动预览；松开自动保存，Esc 恢复原位置和尺寸。' : this.busy ? '正在保存本地广告…'
      : pitch ? '几何与素材按场次保存于本机，不回传 Fusion；无需 PLY 即可恢复。'
        : '几何与素材按场次保存于本机，不回传 Fusion；确认点云坐标后，可在 PROGRAM 中查看，未上传素材时显示占位区域。刷新后重新导入相同 PLY 可恢复。';
    if (!selected) return;
    const form = this.element<HTMLFormElement>('local-ad-form');
    const fields = { name: selected.name, x: selected.plane.origin[0], y: selected.plane.origin[1], z: selected.plane.origin[2],
      width: selected.plane.width, height: selected.plane.height, offset: selected.offset, scale: selected.scale, yaw: selected.yaw };
    for (const [name, value] of Object.entries(fields)) (form.elements.namedItem(name) as HTMLInputElement).value = String(value);
    (form.elements.namedItem('visible') as HTMLInputElement).checked = selected.visible;
    (form.elements.namedItem('flip') as HTMLInputElement).checked = selected.normalSign === -1;
    (form.elements.namedItem('z') as HTMLInputElement).readOnly = pitch && selected.type === '2D';
    const input = this.element<HTMLInputElement>('local-ad-material');
    input.accept = selected.type === '2D' ? '.png,.jpg,.jpeg,.webp' : '.glb';
    this.element('local-ad-material-name').textContent = selected.material?.name ?? (selected.type === '2D' ? '请选择图片素材' : '请选择内嵌贴图的 GLB 模型');
  }
}
