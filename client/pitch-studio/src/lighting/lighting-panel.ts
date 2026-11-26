// 广告位预览 › 场景灯光: one global library of lighting presets, the preset each match uses,
// and an editor for every value. Editing a built-in saves the operator's version of it,
// which can be reset. Every change is saved to the studio store at once.
import { StoreConflict, type StoredDocument } from '../shared/studio-store.ts';
import type { FieldDimensions } from '../shared/types.ts';
import {
  DEFAULT_EXR, DEFAULT_MAX_RADIANCE, MAX_LIGHTS, availablePresets, builtInPresets, emptyLibrary, environmentPath, selectedPreset,
  type LightingLibrary, type LightingPreset, type Rgb, type StudioLight,
} from './lighting.ts';

const escape = (text: string) => text.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
const shortId = () => crypto.randomUUID().replace(/-/g, '').slice(0, 10);
const round = (value: number) => String(Math.round(value * 1000) / 1000);
// <input type=color> works in sRGB code values; presets keep linear multipliers.
const toHex = (rgb: Rgb) => `#${rgb.map((v) => {
  const s = v <= 0.0031308 ? v * 12.92 : 1.055 * v ** (1 / 2.4) - 0.055;
  return Math.round(Math.min(Math.max(s, 0), 1) * 255).toString(16).padStart(2, '0');
}).join('')}`;
function fromHex(hex: string): Rgb | undefined {
  const linear = [1, 3, 5].map((i) => {
    const s = parseInt(hex.slice(i, i + 2), 16) / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  }) as Rgb;
  const max = Math.max(...linear);
  return max > 0 ? linear.map((v) => v / max) as Rgb : undefined;
}

/** chr-server reads assets up to this size (server/runtime/src/program/assets.rs). */
const MAX_ASSET_BYTES = 256 * 1024 * 1024;
const KIND_LABEL = { directional: '平行光（太阳）', spot: '聚光灯', point: '点光源' } as const;

export class LightingPanel {
  /** Panoramas found in the IBL folder, as absolute server paths. */
  private panoramas: string[] = [];
  private status = '';
  /** Lights whose position and angle fields are shown. */
  private readonly expanded = new Set<string>();

  constructor(
    private readonly root: HTMLElement,
    private readonly summary: HTMLElement,
    private readonly document: StoredDocument<LightingLibrary>,
    private readonly context: { matchId(): string | undefined; field(): FieldDimensions | undefined; iblDir(): string; onError(error: unknown): void },
  ) {
    root.addEventListener('change', (event) => { void this.changed(event.target as HTMLElement); });
    root.addEventListener('click', (event) => {
      const button = (event.target as HTMLElement).closest<HTMLButtonElement>('[data-lighting-action]');
      if (button) void this.act(button.dataset.lightingAction, button.dataset.lightId);
    });
  }

  private get library() { return this.document.data ?? emptyLibrary(); }
  private builtIns() { return builtInPresets(this.context.field() ?? { length: 105, width: 68 }); }
  private allPresets() { return availablePresets(this.library, this.builtIns()); }

  /** The preset the current match uses, for the render description. */
  preset(): LightingPreset | undefined {
    const matchId = this.context.matchId();
    return matchId ? selectedPreset(this.library, this.builtIns(), matchId) : undefined;
  }

  /** Lists the panoramas of the IBL folder through the page server; a missing folder leaves the list empty. */
  async loadPanoramas() {
    const dir = this.context.iblDir().replace(/\/+$/, '');
    try {
      const response = await fetch(`/local-files/folder?path=${encodeURIComponent(dir)}`, { cache: 'no-store' });
      if (!response.ok) throw new Error(((await response.json().catch((): null => null)) as { error?: string } | null)?.error ?? `HTTP ${response.status}`);
      const { files } = await response.json() as { files: { path: string; size: number }[] };
      const exrs = files.map((file) => ({ name: file.path.slice(file.path.indexOf('/') + 1), size: file.size }))
        .filter(({ name }) => !name.includes('/') && /\.exr$/i.test(name));
      const large = exrs.filter(({ size }) => size > MAX_ASSET_BYTES).map(({ name }) => name);
      this.panoramas = exrs.filter(({ size }) => size <= MAX_ASSET_BYTES).map(({ name }) => name).sort();
      this.status = large.length ? `未列出超过 256 MB（渲染器素材上限）的环境贴图：${large.join('、')}。` : '';
    } catch (error) {
      this.panoramas = [];
      this.status = `无法列出环境贴图文件夹 ${dir}：${error instanceof Error ? error.message : String(error)}（可用 STUDIO_IBL_DIR 指定，并初始化 asset 子模块）`;
    }
    this.render();
  }

  render() {
    const preset = this.preset();
    if (!preset) { this.root.innerHTML = '<p class="on-help">打开一场比赛后设置灯光。</p>'; return; }
    const focused = this.root.contains(document.activeElement) && document.activeElement instanceof HTMLElement
      ? `[data-field="${document.activeElement.dataset.field}"]${document.activeElement.dataset.lightId ? `[data-light-id="${document.activeElement.dataset.lightId}"]` : ''}` : undefined;
    const builtIn = Boolean(preset.builtIn);
    const enabled = preset.lights.filter((light) => light.enabled);
    const number = (field: string, label: string, value: number, bounds = '', id = '') =>
      `<label>${label}<input type="number" step="any" data-field="${field}" ${id ? `data-light-id="${id}"` : ''} value="${round(value)}" ${bounds} /></label>`;
    const options = this.allPresets().map((item) =>
      `<option value="${escape(item.id)}" ${item.id === preset.id ? 'selected' : ''}>${item.builtIn ? '内置 · ' : ''}${escape(item.name)}</option>`).join('');
    const light = (item: StudioLight) => {
      const id = item.id, open = this.expanded.has(id);
      const main = item.kind === 'directional' ? number('irradiance', 'W/m²', item.irradiance, 'min="0"', id) : number('intensity', 'W/sr', item.intensity, 'min="0"', id);
      const details = item.kind === 'directional'
        ? number('azimuth', '来向方位 / °', item.azimuth, '', id) + number('elevation', '高度角 / °', item.elevation, 'min="0" max="90"', id)
        : number('x', 'X / m', item.position[0], '', id) + number('y', 'Y / m', item.position[1], '', id)
          + number('height', '高度 / m', item.position[2], 'min="0"', id)
          + (item.kind === 'spot' ? number('targetX', '对准 X / m', item.target[0], '', id) + number('targetY', '对准 Y / m', item.target[1], '', id)
            + number('inner', '内角 / °', item.inner, 'min="0" max="89"', id) + number('outer', '外角 / °', item.outer, 'min="0" max="89.9"', id) : '')
          + number('radius', '光源半径 / m', item.radius, 'min="0"', id);
      return `<div class="on-light ${item.enabled ? '' : 'is-off'}">
        <div class="on-light-row"><span class="on-badge ${item.kind === 'directional' ? 'amber' : ''}">${KIND_LABEL[item.kind]}</span>
          <input type="text" data-field="name" data-light-id="${id}" value="${escape(item.name)}" aria-label="灯光名称" />
          <label class="on-check"><input type="checkbox" data-field="enabled" data-light-id="${id}" ${item.enabled ? 'checked' : ''} /> 启用</label>
          <label class="on-check"><input type="checkbox" data-field="shadows" data-light-id="${id}" ${item.shadows ? 'checked' : ''} /> 阴影</label>
          <input type="color" data-field="color" data-light-id="${id}" value="${toHex(item.color)}" aria-label="颜色" />
          <span class="on-light-main">${main}</span>
          <button class="on-text-button" type="button" data-lighting-action="toggle" data-light-id="${id}" aria-expanded="${open}">${open ? '收起 ▴' : '位置 ▾'}</button>
          <button class="on-text-button" type="button" data-lighting-action="remove-light" data-light-id="${id}" aria-label="删除 ${escape(item.name)}">删除</button></div>
        ${open ? `<div class="on-field-grid on-light-details">${details}</div>` : ''}</div>`;
    };
    const panoramas = [...new Set([...this.panoramas, ...(preset.environment.path ? [preset.environment.path] : [])])];
    const skyPath = environmentPath(preset, this.context.iblDir());
    const cap = preset.environment.maxRadiance;
    const sky = skyPath ? `IBL ${skyPath.split('/').pop()} × ${round(preset.environment.gain)}${cap ? `（上限 ${round(cap)}）` : ''}` : 'IBL 关闭';
    const fill = preset.constant.enabled ? `常量光 ${round(preset.constant.intensity)}` : '常量光关闭';
    this.summary.textContent = `本场使用“${preset.name}”${preset.edited ? '（已修改）' : ''}：${enabled.length} 盏灯 · ${sky} · ${fill}。预设库所有场次共用，发送到渲染器时一并发送。`;
    const toggle = (field: string, on: boolean) =>
      `<label class="on-check on-toggle-check"><input type="checkbox" data-field="${field}" ${on ? 'checked' : ''} /> 启用</label>`;
    this.root.innerHTML = `<div class="on-lighting-grid">
      <section class="on-card on-lighting-lights"><div class="on-card-head"><div><h2>光源 <span class="on-badge ${enabled.length > MAX_LIGHTS ? 'amber' : ''}">${enabled.length} / ${MAX_LIGHTS}</span></h2>
          <p class="on-help">球场米制坐标：X 沿长边，Y 沿宽边（俯视向下），高度向上；方位角从 +X 转向 +Y。最多启用 ${MAX_LIGHTS} 盏。</p></div>
        <div class="on-action-row">${(['spot', 'point', 'directional'] as const).map((kind) =>
          `<button class="on-button" type="button" data-lighting-action="add-${kind}">＋ ${KIND_LABEL[kind]}</button>`).join('')}</div></div>
        <div class="on-lights">${preset.lights.map(light).join('') || '<p class="on-help">没有光源。</p>'}</div></section>
      <div class="on-lighting-side">
        <section class="on-card"><div class="on-card-head"><h2>灯光预设</h2>${builtIn ? `<span class="on-badge ${preset.edited ? 'amber' : ''}">${preset.edited ? '内置 · 已修改' : '内置'}</span>` : ''}</div>
          <div class="on-card-body"><select data-field="preset" aria-label="本场灯光预设">${options}</select>
            <div class="on-action-row"><button class="on-button" type="button" data-lighting-action="copy">复制为新预设</button>
              ${builtIn ? (preset.edited ? '<button class="on-button" type="button" data-lighting-action="reset">恢复内置默认</button>' : '')
                : '<button class="on-button" type="button" data-lighting-action="rename">重命名</button><button class="on-button" type="button" data-lighting-action="delete">删除</button>'}</div>
            <p class="on-help" role="status">${escape(this.status) || (builtIn ? '内置预设可直接修改，修改保存为它的现场版本，可恢复内置默认。' : '修改即时保存到现场服务器。')}</p></div></section>
        <section class="on-card ${preset.environment.enabled ? '' : 'is-off'}"><div class="on-card-head"><div><h2>环境光 IBL</h2><p class="on-help">EXR 全景图照明，无阴影、不显示背景。图中有太阳时开启亮度上限，截掉太阳，避免与平行光重复并在金属上闪点。</p></div>${toggle('environmentEnabled', preset.environment.enabled)}</div>
          <div class="on-card-body"><label>环境贴图<select data-field="environment"><option value="">无</option>${panoramas.map((path) =>
            `<option value="${escape(path)}" ${path === preset.environment.path ? 'selected' : ''}>${escape(path)}</option>`).join('')}</select></label>
            <div class="on-field-grid">${number('gain', 'gain', preset.environment.gain, 'min="0" max="100"')}${number('rotation', '旋转 / °', preset.environment.rotation, 'min="-360" max="360"')}
              <label>亮度上限（gain 前）<span class="on-inline-field"><input type="checkbox" data-field="maxRadianceEnabled" aria-label="启用亮度上限" ${cap ? 'checked' : ''} />
                <input type="number" step="any" data-field="maxRadiance" value="${round(cap ?? DEFAULT_MAX_RADIANCE)}" min="0" aria-label="亮度上限" ${cap ? '' : 'disabled'} /></span></label></div></div></section>
        <section class="on-card ${preset.constant.enabled ? '' : 'is-off'}"><div class="on-card-head"><div><h2>常量光</h2><p class="on-help">各方向均匀补光，无阴影，与环境光叠加，整体提亮暗部。</p></div>${toggle('constantEnabled', preset.constant.enabled)}</div>
          <div class="on-card-body"><div class="on-field-grid">${number('constant', '强度 / W·m⁻²·sr⁻¹', preset.constant.intensity, 'min="0" max="100"')}
            <label>颜色<input type="color" data-field="constantColor" value="${toHex(preset.constant.color)}" /></label></div></div></section>
      </div></div>`;
    if (focused) this.root.querySelector<HTMLElement>(focused)?.focus();
  }

  /** Saves `change` applied to the library; on a conflict reloads it and says so. */
  private async save(change: (library: LightingLibrary) => LightingLibrary) {
    try { await this.document.save((data) => change(structuredClone(data ?? emptyLibrary()))); }
    catch (error) {
      if (error instanceof StoreConflict) {
        await this.document.load();
        this.context.onError(new Error('另一个页面修改了灯光预设，已载入最新版本；刚才的修改未保存，请重新操作。'));
      } else this.context.onError(error);
    }
    this.render();
  }

  /** Applies `edit` to the stored preset; a built-in's first edit stores the operator's version of it. */
  private editPreset(edit: (preset: LightingPreset) => void) {
    const current = this.preset();
    return this.save((library) => {
      let preset = library.presets.find((item) => item.id === current.id);
      if (!preset) {
        preset = structuredClone(current);
        delete preset.builtIn;
        delete preset.edited;
        library.presets.push(preset);
      }
      edit(preset);
      return library;
    });
  }

  private async changed(target: HTMLElement) {
    if (!(target instanceof HTMLInputElement || target instanceof HTMLSelectElement)) return;
    const field = target.dataset.field, lightId = target.dataset.lightId, matchId = this.context.matchId();
    if (!field || !matchId) return;
    if (field === 'preset') {
      await this.save((library) => ({ ...library, selected: { ...library.selected, [matchId]: target.value } }));
      return;
    }
    const value = target instanceof HTMLInputElement && target.type === 'number' ? target.valueAsNumber : NaN;
    if (target instanceof HTMLInputElement && target.type === 'number' && !Number.isFinite(value)) { this.render(); return; }
    await this.editPreset((preset) => {
      if (field === 'environment') Object.assign(preset.environment, { path: target.value || null, enabled: Boolean(target.value) });
      else if (field === 'environmentEnabled') {
        const path = preset.environment.path ?? this.panoramas.find((item) => item === DEFAULT_EXR) ?? this.panoramas[0] ?? null;
        Object.assign(preset.environment, { path, enabled: (target as HTMLInputElement).checked && path !== null });
      } else if (field === 'constantEnabled') preset.constant.enabled = (target as HTMLInputElement).checked;
      else if (field === 'gain') preset.environment.gain = value;
      else if (field === 'rotation') preset.environment.rotation = value;
      else if (field === 'maxRadianceEnabled') preset.environment.maxRadiance = (target as HTMLInputElement).checked ? DEFAULT_MAX_RADIANCE : null;
      else if (field === 'maxRadiance' && value > 0) preset.environment.maxRadiance = value;
      else if (field === 'constant') preset.constant.intensity = value;
      else if (field === 'constantColor') preset.constant.color = fromHex(target.value) ?? preset.constant.color;
      const light = preset.lights.find((item) => item.id === lightId);
      if (!light) return;
      const input = target as HTMLInputElement;
      if (field === 'name') light.name = input.value.slice(0, 80);
      else if (field === 'enabled') light.enabled = input.checked;
      else if (field === 'shadows') light.shadows = input.checked;
      else if (field === 'color') light.color = fromHex(input.value) ?? light.color;
      else if (light.kind === 'directional') {
        if (field === 'azimuth' || field === 'elevation' || field === 'irradiance') light[field] = value;
      } else if (field === 'x' || field === 'y' || field === 'height') light.position[{ x: 0, y: 1, height: 2 }[field]] = value;
      else if (field === 'intensity' || field === 'radius') light[field] = value;
      else if (light.kind === 'spot' && (field === 'targetX' || field === 'targetY')) light.target[field === 'targetX' ? 0 : 1] = value;
      else if (light.kind === 'spot' && (field === 'inner' || field === 'outer')) light[field] = value;
    });
  }

  private async act(action: string, lightId?: string) {
    const preset = this.preset(), matchId = this.context.matchId();
    if (!preset || !matchId) return;
    if (action === 'toggle' && lightId) {
      if (!this.expanded.delete(lightId)) this.expanded.add(lightId);
      this.render();
    } else if (action === 'copy') {
      const name = window.prompt('新预设名称', `${preset.name}（副本）`)?.trim();
      if (!name) return;
      const copy: LightingPreset = { ...structuredClone(preset), id: `preset-${shortId()}`, name: name.slice(0, 80) };
      delete copy.builtIn;
      delete copy.edited;
      await this.save((library) => ({ ...library, presets: [...library.presets, copy], selected: { ...library.selected, [matchId]: copy.id } }));
    } else if (action === 'reset') {
      if (!window.confirm(`把“${preset.name}”恢复为内置默认？现场的修改会丢弃。`)) return;
      await this.save((library) => ({ ...library, presets: library.presets.filter((item) => item.id !== preset.id) }));
    } else if (action === 'rename') {
      const name = window.prompt('预设名称', preset.name)?.trim();
      if (name) await this.editPreset((item) => { item.name = name.slice(0, 80); });
    } else if (action === 'delete') {
      if (!window.confirm(`删除灯光预设“${preset.name}”？使用它的场次会改用默认灯光。`)) return;
      await this.save((library) => ({ ...library, presets: library.presets.filter((item) => item.id !== preset.id) }));
    } else if (action === 'remove-light') {
      await this.editPreset((item) => { item.lights = item.lights.filter((light) => light.id !== lightId); });
    } else if (action.startsWith('add-')) {
      const kind = action.slice(4), id = `${kind}-${shortId()}`, color: Rgb = [1, 1, 1];
      const light: StudioLight = kind === 'directional'
        ? { kind, id, name: '平行光', enabled: true, shadows: true, color, azimuth: 230, elevation: 50, irradiance: 2 }
        : kind === 'spot'
          ? { kind, id, name: '聚光灯', enabled: true, shadows: true, color, position: [0, 40, 30], target: [0, 0], intensity: 10000, inner: 20, outer: 45, radius: 0.5 }
          : { kind: 'point', id, name: '点光源', enabled: true, shadows: false, color, position: [0, 0, 20], intensity: 2000, radius: 0.3 };
      this.expanded.add(id);
      await this.editPreset((item) => { item.lights.push(light); });
    }
  }
}
