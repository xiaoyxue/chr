import { FusionGateway, MatchRepository, validatePlacement, validateScanFile } from '../matches/match-domain.ts';
import { HttpFusionAdapter } from '../matches/fusion-client.ts';
import { offlinePackages, parseOfflinePackage } from '../matches/offline-package.ts';
import { importPlyFile } from '../analysis/ply-import.ts';
import { ResultReview } from '../analysis/result-review.ts';
import { LocalAdEditor } from '../placements/local-ad-editor.ts';
import { LocalAdRepository, validatePitchAd } from '../placements/local-ad.ts';
import { PointCloudPreview } from '../scene/point-cloud-preview.ts';
import { fetchServerFile, isServerPath, serverPath } from '../shared/server-files.ts';
import { blendedMaterials, describeScene, sendDescription, sha256, type DescribedAsset } from '../render/render-description.ts';
import '../styles/base.css';
import '../styles/pitch-preview.css';
import '../styles/onsite.css';
import { anchorPoint, moveAnchor, parseAspect, resizeSide, uniformScale, withAspect, type Anchor } from '../placements/ground-edit.ts';
import { UndoStack } from '../shared/undo-stack.ts';
import type { MatchRecord, Placement } from '../shared/types.ts';
import type { PitchPreview } from '../scene/pitch-preview.ts';
import type { LocalAnalysis } from '../analysis/ply-result.ts';

const $ = <T extends Element = HTMLElement>(selector: string) => document.querySelector<T>(selector);
const gateway = new FusionGateway(new HttpFusionAdapter());
let repository: MatchRepository;
const scanFiles = new Map<string, File>();
const analysisResults = new Map<string, LocalAnalysis>();
let analysisImport: { matchId: string; controller: AbortController };
let analysisNotice: { matchId: string; text: string; failed?: boolean };
let records: MatchRecord[] | null = null;
let libraryBusy = false;
let deletingMatchId: string;
let currentId: string;
let selectedId: string;
let activeTab = 'preview';
let enginePromise: Promise<PitchPreview>;
let engine: PitchPreview;
let cloudEngine: PointCloudPreview;
let enginePending = false;
let engineReadyMatchId: string;
let matchLoadVersion = 0;
let engineQueue = Promise.resolve();
let scanURL: string;
let scanAbort: AbortController;
// Editing state of the selected Fusion placement; UI-only and never saved.
let unlockedId: string;
let anchor: Anchor = 'top-left';
let keepAspect = true;
let axesVisible = true;
const undo = new UndoStack(renderUndo);
const resultReview = new ResultReview(showError, clearError);
const pitchEditor = new LocalAdEditor({
  root: $('#pitch-local-editor'),
  prefix: 'pitch-',
  space: 'pitch',
  update: async (ads, selectedId) => {
    await Promise.all([engine?.setLocalAds(ads, selectedId), resultReview.setPitchAds(ads)]);
  },
  pick: () => {
    try { engine.startGroundRectangle((plane) => { void pitchEditor.finishPlane(plane); }); }
    catch (error) { pitchEditor.cancelDrawing(); showError(error); }
  },
  cancelPick: () => engine?.cancelGroundRectangle(),
  validate: (ad) => validatePitchAd(ad, current().match.field),
  error: showError,
  recordUndo: (label, restore) => undo.push({ label, restore }),
});
const localEditor = new LocalAdEditor({
  root: $('#cloud-local-editor'),
  prefix: '',
  space: 'cloud',
  update: async (ads, selectedId) => {
    await Promise.all([cloudEngine?.setLocalAds(ads, selectedId), resultReview.setLocalAds(ads, analysisResults.get(currentId)?.aligned ?? false)]);
  },
  pick: () => {
    try { cloudEngine.startPlanePick((count) => localEditor.picked(count), (points, normalSign) => { void localEditor.finish(points, normalSign); }); }
    catch (error) { localEditor.cancelDrawing(); showError(error); }
  },
  cancelPick: () => { cloudEngine?.cancelPlanePick(); cloudEngine?.cancelAdDrag(); },
  manipulationAvailable: (available) => cloudEngine?.setAdEditingEnabled(available),
  error: showError,
});
resultReview.onChange = () => renderAnalysis();

const htmlEscapes: Record<string, string> = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
const escapeHTML = (value: unknown) => String(value).replace(/[&<>"']/g, (char) => htmlEscapes[char]);
const current = () => records?.find((record) => record.match.id === currentId);
// Onsite-server paths of offline packages imported by path, which chr-server reads assets from.
const PACKAGE_PATHS_KEY = 'liveshow.onsite.package-paths.v1';
const packagePaths = (): Record<string, string> => {
  try { return JSON.parse(localStorage.getItem(PACKAGE_PATHS_KEY) ?? '{}') as Record<string, string>; }
  catch { return {}; }
};
const selected = () => current()?.draft.find((slot) => slot.id === selectedId);

function showError(error: unknown) {
  $('#app-error').textContent = error instanceof Error ? error.message : String(error);
  $('#app-error').hidden = false;
}

function clearError() { $('#app-error').hidden = true; }

function renderMatches() {
  $('#match-count').textContent = String(records?.length ?? 0);
  $('#match-cards').innerHTML = records?.length ? records.map(({ match, draft }) => `
    <article class="on-card on-match-card"><span class="on-badge amber">${match.mode === 'offline' ? 'Offline package' : 'Fusion configuration'}</span>
      <h2>${escapeHTML(match.name)}</h2><code>${escapeHTML(match.id)}</code>
      <p class="on-sub">${escapeHTML(match.date.replace('T', ' '))}　·　${escapeHTML(match.venue)}</p>
      <div class="on-match-foot"><span class="on-help">${draft.length} 个广告位 · ${analysisResults.has(match.id) ? '已导入本地点云' : '等待场地分析'}</span>
        <div class="on-match-actions"><button type="button" class="on-button on-delete-match" data-delete-match="${escapeHTML(match.id)}" aria-label="删除比赛 ${escapeHTML(match.name)}（仅本机）" ${libraryBusy ? 'disabled' : ''}>${deletingMatchId === match.id ? '删除中…' : '删除'}</button>
        <a class="on-button primary" href="#match/${encodeURIComponent(match.id)}/preview">进入场次 →</a></div></div>
    </article>`).join('') : `<div class="on-card on-empty"><h2>${records ? '还没有添加比赛' : '本地记录读取失败'}</h2><p>${records ? '输入 Fusion 比赛 ID，或导入离线 ZIP（本地或服务器路径）。' : '为避免覆盖原记录，已停止写入。请查看页面上方错误信息。'}</p></div>`;
}

function setLibraryBusy(busy: boolean) {
  libraryBusy = busy;
  for (const selector of ['#match-lookup-submit', '#import-offline-package', '#replace-offline-package']) {
    $<HTMLButtonElement>(selector).disabled = busy || !records;
  }
  renderMatches();
}

async function deleteMatch(id: string) {
  const record = records?.find((item) => item.match.id === id);
  if (!record || libraryBusy) return;
  if (!window.confirm(`删除本机保存的比赛“${record.match.name}”？\n\n本地广告位、素材及导入结果也会移除，此操作不可撤销。\n不会删除 Fusion 上的比赛，也不会删除原始视频、PLY 或结果文件夹。`)) return;
  clearError();
  deletingMatchId = id;
  setLibraryBusy(true);
  const status = $('#match-delete-status');
  status.hidden = false;
  status.textContent = '正在删除本地比赛及关联数据…';
  let removed = false;
  try {
    const next = records.filter((item) => item.match.id !== id);
    repository.write(next);
    records = next;
    removed = true;
    scanFiles.delete(id);
    analysisResults.delete(id);
    if (analysisImport?.matchId === id) cancelAnalysisImport();
    if (analysisNotice?.matchId === id) analysisNotice = undefined;
    if (engineReadyMatchId === id) engineReadyMatchId = undefined;
    const navigation = $<HTMLButtonElement>('#workspace-nav');
    if (navigation.dataset.matchId === id) {
      navigation.disabled = true;
      delete navigation.dataset.matchId;
    }
    if (currentId === id) {
      history.replaceState(null, '', '#matches');
      await renderRoute();
    }
    await resultReview.removeMatch(id);
    renderMatches();
    const cleanup = [new LocalAdRepository().remove(id)];
    if (record.match.offlinePackageId && !next.some((item) => item.match.offlinePackageId === record.match.offlinePackageId)) {
      cleanup.push(offlinePackages.remove(record.match.offlinePackageId));
    }
    const results = await Promise.allSettled(cleanup);
    const errors = results.flatMap((result) => result.status === 'rejected'
      ? [result.reason instanceof Error ? result.reason.message : String(result.reason)] : []);
    if (errors.length) throw new Error(errors.join('；'));
    status.textContent = `已删除本机比赛“${record.match.name}”及相关本地数据；Fusion 和原始文件未改变。`;
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    status.textContent = `${removed ? '比赛已从本机列表移除，但关联数据清理未完成。' : '删除失败，比赛记录未改变。'} ${detail}`;
    showError(new Error(status.textContent));
  } finally {
    deletingMatchId = undefined;
    setLibraryBusy(false);
  }
}

function routeToTab(tab: string) {
  if (currentId) location.hash = `match/${encodeURIComponent(currentId)}/${tab}`;
}

function renderLists() {
  const record = current();
  if (!record) return;
  const hidden = new Set(record.hiddenPlacementIds);
  const markup = record.draft.map((slot) => {
    const enabled = !hidden.has(slot.id), active = slot.id === selectedId, unlocked = active && slot.id === unlockedId;
    return `<div class="on-slot-row ${active ? 'active' : ''} ${enabled ? '' : 'is-hidden'}">
    <button type="button" class="on-slot" aria-pressed="${active}" data-select-id="${escapeHTML(slot.id)}">
      <span class="on-badge ${slot.type === '3D' ? 'amber' : ''}">${slot.type}</span><span><strong>${escapeHTML(slot.name)}</strong><small>${escapeHTML(slot.id)}<br />X ${slot.x.toFixed(2)} / Y ${slot.y.toFixed(2)} m${slot.type === '2D' ? ` · ${slot.sizeX.toFixed(2)} × ${slot.sizeY.toFixed(2)} m` : ` · ×${slot.scaleX.toFixed(2)}`}</small></span>
      ${active ? `<span class="on-lock-state ${unlocked ? 'unlocked' : ''}" title="${unlocked ? '已解锁，可编辑' : '已锁定'}">${unlocked ? '🔓' : '🔒'}</span>` : ''}
    </button>
    <button type="button" class="on-visibility-toggle" role="switch" aria-checked="${enabled}" aria-label="启用广告位 ${escapeHTML(slot.name)}" title="${enabled ? '停用：不在预览和渲染器中显示' : '启用此广告位'}" data-visibility-id="${escapeHTML(slot.id)}" ${enginePending ? 'disabled' : ''}>
      <span class="on-toggle-track" aria-hidden="true"></span><span>${enabled ? '启用' : '停用'}</span>
    </button>
  </div>`;
  }).join('');
  $('#preview-placement-list').innerHTML = markup;
  $('#adjust-placement-list').innerHTML = `<label class="on-editor-select">选择广告位<select id="editor-placement-select">${record.draft.map((slot) => `<option value="${escapeHTML(slot.id)}" ${slot.id === selectedId ? 'selected' : ''}>${slot.type} · ${escapeHTML(slot.name)}${hidden.has(slot.id) ? '（已停用）' : ''}</option>`).join('')}</select></label>`;
  $('#placement-count').textContent = String(record.draft.length);
}

const metres = (value: number) => String(Math.round(value * 1000) / 1000);
const field = (name: string, label: string, value: number, bounds = '') =>
  `<label>${label}<input name="${name}" type="number" step="0.01" ${bounds} value="${metres(value)}" required aria-label="${label}" /></label>`;

/** The inspector of the selected Fusion placement; geometry fields are editable only while it is unlocked. */
function renderInspector() {
  const slot = selected(), record = current();
  $('#placement-inspector').hidden = !slot;
  if (!slot) return;
  const ready = !enginePending && engineReadyMatchId === currentId;
  const enabled = !record.hiddenPlacementIds?.includes(slot.id), unlocked = slot.id === unlockedId;
  $('#inspector-name').textContent = slot.name;
  $('#inspector-type').textContent = slot.type;
  $('#inspector-type').classList.toggle('amber', slot.type === '3D');
  const lock = $<HTMLButtonElement>('#inspector-lock');
  lock.disabled = !ready || !enabled;
  lock.setAttribute('aria-pressed', String(unlocked));
  lock.textContent = unlocked ? '🔓 已解锁' : '🔒 已锁定';
  $('#inspector-state').textContent = !enabled ? '此广告位已停用：不在预览和渲染器中显示。启用后才能解锁编辑。'
    : !ready ? '等待球场预览加载完成…'
    : unlocked ? '已解锁：修改数值后按 Enter 或离开输入框即生效；俯视图中可拖动移动' + (slot.type === '2D' ? '、拖角点调整大小。' : '。')
    : '已锁定，避免误触。点击“已锁定”或按 L 解锁后再调整。';
  $<HTMLFieldSetElement>('#placement-edit-controls').disabled = !ready || !enabled || !unlocked;
  $<HTMLButtonElement>('#reset-placement').disabled = !ready || !enabled || !unlocked;
  const readonly = (text: string) => `<p class="on-readonly">${text}</p>`;
  const focused = document.activeElement instanceof HTMLInputElement && document.activeElement.closest('#placement-fields') ? document.activeElement.name : undefined;
  if (slot.type === '2D') {
    const point = anchorPoint(slot, anchor);
    $('#placement-fields').innerHTML = `<div class="on-inspector-group"><h3>位置（世界坐标）</h3>
      <div class="on-segmented" role="radiogroup" aria-label="定位锚点">${(['top-left', 'centre'] as const).map((value) =>
        `<label><input type="radio" name="anchor" value="${value}" ${anchor === value ? 'checked' : ''} />${value === 'top-left' ? '左上角' : '中心'}</label>`).join('')}</div>
      <div class="on-field-grid">${field('anchorX', `${anchor === 'top-left' ? '左上角' : '中心'} X / m`, point.x)}${field('anchorY', `${anchor === 'top-left' ? '左上角' : '中心'} Y / m`, point.y)}</div>
      <p class="on-help">左上角指俯视图中广告局部 −X / −Y 方向的角点（+X 向右，+Y 向下）。</p></div>
      <div class="on-inspector-group"><h3>尺寸</h3>
      <div class="on-field-grid">${field('sizeX', '长 X / m', slot.sizeX, 'min="0.1"')}${field('sizeY', '宽 Y / m', slot.sizeY, 'min="0.1"')}</div>
      <div class="on-aspect-row"><label>宽高比 W:H<input name="aspect" type="text" inputmode="decimal" value="${metres(slot.sizeX / slot.sizeY)}" aria-label="宽高比，如 16:9 或 1.78" /></label>
      <label class="on-check"><input type="checkbox" name="keepAspect" ${keepAspect ? 'checked' : ''} /> 锁定宽高比</label></div>
      <p class="on-help">输入 16:9 或 1.78 后按 Enter：保持长 X 和锚点，按比例调整宽 Y。</p></div>
      ${readonly(`旋转 Z ${slot.rz.toFixed(1)}° · 暂不支持调整旋转`)}`;
  } else {
    const maximum = slot.region ? Math.min(5, 1 / slot.modelPose.scale) : 5;
    $('#placement-fields').innerHTML = `<div class="on-inspector-group"><h3>位置（世界坐标）</h3>
      <div class="on-field-grid">${field('x', `${slot.region ? '区域' : ''}中心 X / m`, slot.x)}${field('y', `${slot.region ? '区域' : ''}中心 Y / m`, slot.y)}</div></div>
      <div class="on-inspector-group"><h3>整体缩放</h3>
      <div class="on-field-grid">${field('scale', '缩放 / ×', slot.scaleX, `min="0.1" max="${metres(maximum)}"`)}</div>
      <p class="on-help">按比例整体缩放，范围 0.1–${metres(maximum)} 倍${slot.region ? '，模型始终限制在区域内' : ''}。</p></div>
      <p id="inspector-footprint" class="on-readonly"></p>
      ${readonly(`高度 Z ${slot.z.toFixed(2)} m · 旋转 X / Y / Z ${slot.rx.toFixed(1)}° / ${slot.ry.toFixed(1)}° / ${slot.rz.toFixed(1)}° · 暂不支持调整旋转`)}`;
    renderFootprint();
  }
  // Re-rendering replaces the inputs; keep keyboard focus where the operator was.
  if (focused) $<HTMLInputElement>(`#placement-fields [name="${focused}"]:is(:checked, :not([type=radio]))`)?.focus();
  $('#placement-error').textContent = '';
}

/** The loaded model's real XY bounding box, next to its placement slot. */
function renderFootprint() {
  const slot = selected(), target = $('#inspector-footprint');
  if (!target || slot?.type !== '3D') return;
  const box = engine?.modelFootprint(slot.id);
  target.textContent = box
    ? `模型实际占地（XY 轴对齐包围盒）：X ${box.minX.toFixed(2)} ～ ${box.maxX.toFixed(2)}，Y ${box.minY.toFixed(2)} ～ ${box.maxY.toFixed(2)} m · ${(box.maxX - box.minX).toFixed(2)} × ${(box.maxY - box.minY).toFixed(2)} m`
    : `模型尚未加载或已停用；俯视图中的${slot.region ? '黄色框是广告位区域' : '圆环是广告位中心'}，不是模型本身。`;
}

/** The selected placement with one inspector field set to `value`. */
function editedSlot(slot: Placement, name: string, value: number): { next: Placement; action: string } {
  if (slot.type === '2D') {
    if (name === 'aspect') return { next: withAspect(slot, anchor, value), action: '设置宽高比' };
    if (name === 'anchorX' || name === 'anchorY') {
      const point = anchorPoint(slot, anchor);
      return { next: moveAnchor(slot, anchor, name === 'anchorX' ? { ...point, x: value } : { ...point, y: value }), action: '移动' };
    }
    return { next: resizeSide(slot, anchor, name as 'sizeX' | 'sizeY', value, keepAspect), action: '调整尺寸' };
  }
  if (name === 'scale') return { next: uniformScale(slot, value), action: '缩放' };
  return { next: { ...slot, [name]: value }, action: '移动' };
}

function applyInspectorField(input: HTMLInputElement) {
  const slot = selected();
  if (!slot || slot.id !== unlockedId || !['number', 'text'].includes(input.type) || !input.isConnected || input.value === input.defaultValue) return;
  const value = input.name === 'aspect' ? parseAspect(input.value) : input.valueAsNumber;
  if (value === undefined || !Number.isFinite(value)) {
    renderInspector();
    if (input.name === 'aspect') $('#placement-error').textContent = '宽高比请输入 16:9 这样的 W:H，或 1.78 这样的正数。';
    return;
  }
  const { next, action } = editedSlot(slot, input.name, value);
  commitSlot(next, `${action} ${slot.name}`);
}

function setUnlocked(id?: string) {
  if (unlockedId === id) return;
  unlockedId = id;
  renderLists();
  renderInspector();
  syncEngine();
}

function renderUndo() {
  const label = undo.label;
  document.querySelectorAll<HTMLButtonElement>('[data-undo]').forEach((button) => {
    button.disabled = !undo.canUndo;
    button.title = label ? `撤销：${label}（Ctrl+Z）` : '没有可撤销的操作';
  });
  document.querySelectorAll('[data-undo-label]').forEach((element) => { element.textContent = label ? `上一步：${label}` : '没有可撤销的操作'; });
}

async function undoLast() {
  clearError();
  try { await undo.undo(); }
  catch (error) { showError(error); }
}

function renderRecord() {
  const record = current();
  if (!record) return;
  const { match } = record;
  $('#match-title').textContent = match.name;
  $('#match-meta').textContent = `${match.date.replace('T', ' ')}　·　${match.venue}　·　场地 ${match.venueId}`;
  $('#match-code').textContent = match.id;
  $('#match-source').textContent = match.mode === 'offline' ? 'Offline package' : 'Fusion match';
  $('#resource-step').textContent = match.mode === 'offline' ? 'Loaded from offline ZIP' : 'Loaded from Fusion';
  $('#field-size').textContent = `${match.field.length} × ${match.field.width} m`;
  $('#draft-badge').textContent = record.updatedAt ? '本地草稿已保存' : '场次初始配置';
  $('#save-status').textContent = record.updatedAt
    ? `本机保存于 ${new Date(record.updatedAt).toLocaleTimeString('zh-CN')}；未回传 Fusion，也未触发播出。`
    : '草稿仅保存在本机，不会回传 Fusion。';
  renderLists();
  renderInspector();
  renderAnalysis();
}

function syncEngine() {
  if (!engine || !current() || enginePending || engineReadyMatchId !== currentId) return;
  engine.setEditing(unlockedId, keepAspect);
  engine.setPlacements(current().draft, selectedId, selectSlot, moveSlot, current().hiddenPlacementIds);
  renderFootprint();
  void resultReview.setPlacements(current().draft, current().hiddenPlacementIds).catch(showError);
}

function cancelAnalysisImport() {
  analysisImport?.controller.abort();
  analysisImport = undefined;
}

function renderAnalysis() {
  const result = analysisResults.get(currentId);
  const ready = Boolean(current() && !enginePending && engineReadyMatchId === currentId);
  const busy = Boolean(analysisImport && analysisImport.matchId === currentId);
  $('#analysis-import').setAttribute('aria-busy', String(busy));
  $<HTMLButtonElement>('#import-analysis').disabled = $<HTMLButtonElement>('#load-analysis-path').disabled = !current() || busy;
  $<HTMLButtonElement>('#import-analysis').textContent = result ? '替换 PLY 点云' : '选择 PLY 点云';
  $('#cancel-analysis').hidden = !busy;
  $('#remove-analysis').hidden = !result;
  $<HTMLButtonElement>('#remove-analysis').disabled = busy;
  $('#analysis-empty').hidden = Boolean(result);
  $('#analysis-workspace').hidden = !result;
  $('#cloud-controls').hidden = !result;
  $('#analysis-empty-title').textContent = '尚未导入点云';
  $('#analysis-empty-description').textContent = '导入 PLY 后在下方绘制独立点云广告；上方球场 2D / 3D 广告不需要点云。';
  const notice = analysisNotice?.matchId === currentId ? analysisNotice : undefined;
  $('#analysis-import-status').textContent = busy ? '正在读取并在本地解析点云，请稍候…'
    : notice?.text ?? (result ? `${result.cloud.fileName} · ${(result.cloud.positions.length / 3).toLocaleString('zh-CN')} 点 · ${(result.cloud.byteLength / 1024 / 1024).toFixed(1)} MB · 未上传`
      : current() ? '点云广告绑定当前场次；导入不会替换上方球场预览。' : '请先选择比赛场次。');
  $('#analysis-import-status').classList.toggle('on-form-error', Boolean(!busy && notice?.failed));
  $('#analysis-status').textContent = result
    ? '广告位预览中已载入本地点云；视频与投影结果文件夹请在“导入结果与微调”单独导入。'
    : '可直接前往“导入结果与微调”选择已有结果文件夹，无需提交视频到 Fusion。';
  if (result) {
    $('#cloud-source').textContent = result.cloud.fileName;
    $('#cloud-camera').textContent = result.aligned ? result.cloud.declaredPitchCoordinates ? '条件球场锚定（文件声明）' : '球场坐标（手动确认）' : '未确认坐标 · 叠加关闭';
    $('#cloud-coverage').textContent = (result.cloud.positions.length / 3).toLocaleString('zh-CN');
    $<HTMLInputElement>('#analysis-aligned').checked = result.aligned;
    $<HTMLInputElement>('#cloud-point-size').value = String(result.pointSize);
    $('#cloud-point-size-value').textContent = result.pointSize.toFixed(2);
    $('#analysis-coordinate-note').textContent = result.cloud.declaredPitchCoordinates
      ? '文件声明：条件球场锚定、米制、−Z 向上。保持原始点坐标，不自动平移、旋转或缩放；不保证已与本场比赛精确配准。'
      : '文件未完整声明球场锚定、米制和 −Z 向上。仍可在点云中绘制，但默认不向视频叠加点云广告；确认坐标符合当前球场后再勾选。';
    $('#analysis-bounds').textContent = `原始范围${result.aligned ? ' / m' : '（原始单位）'}：${['X', 'Y', 'Z'].map((axis, i) => `${axis} ${result.cloud.minimum[i].toFixed(2)} ～ ${result.cloud.maximum[i].toFixed(2)}`).join(' · ')}`;
  }
  cloudEngine?.setCloud(result?.cloud, result?.pointSize ?? 0.15);
  pitchEditor.setAvailable(ready);
  renderRendererBars();
  localEditor.setCloud(result?.cloud.id);
  localEditor.setAvailable(Boolean(current() && result && cloudEngine?.isAvailable));
  void resultReview.setLocalAds(localEditor.ads, result?.aligned ?? false).catch(showError);
}

async function selectAnalysisFile(source: File | string) {
  if (!current()) {
    showError(new Error('请先选择比赛场次。'));
    return;
  }
  cancelAnalysisImport();
  const job = { matchId: currentId, controller: new AbortController() };
  analysisImport = job;
  analysisNotice = undefined;
  clearError();
  renderAnalysis();
  try {
    const file = typeof source === 'string' ? await fetchServerFile(source, job.controller.signal) : source;
    const cloud = await importPlyFile(file, job.controller.signal);
    if (job.controller.signal.aborted || currentId !== job.matchId) return;
    if (!cloudEngine) {
      cloudEngine = new PointCloudPreview($('#cloud-viewport'), (error) => {
        $('#cloud-engine-status').textContent = error.message;
        localEditor.setAvailable(false);
        showError(error);
      });
      cloudEngine.setPickTolerance($<HTMLInputElement>('#cloud-pick-tolerance').valueAsNumber);
      cloudEngine.onAdEditStart = (id) => localEditor.beginPlaneEdit(id);
      cloudEngine.onAdEditPreview = (ad) => localEditor.previewPlane(ad);
      cloudEngine.onAdEditEnd = (cancelled) => { void localEditor.finishPlaneEdit(cancelled).catch(showError); };
    }
    if (!cloudEngine.isAvailable) throw new Error('点云预览不可用，请刷新页面并检查硬件加速。');
    analysisResults.set(job.matchId, { cloud, aligned: cloud.declaredPitchCoordinates, pointSize: 0.15, importedAt: new Date().toISOString() });
    renderRecord();
    renderMatches();
  } catch (error) {
    if (error instanceof DOMException && error.name === 'AbortError') return;
    analysisNotice = { matchId: job.matchId, failed: true, text: `${error instanceof Error ? error.message : String(error)}${analysisResults.has(job.matchId) ? ' 已保留之前导入的结果。' : ''}` };
    showError(error);
  } finally {
    if (analysisImport === job) {
      analysisImport = undefined;
      $<HTMLInputElement>('#analysis-file').value = '';
      renderAnalysis();
    }
  }
}

function selectSlot(id: string) {
  if (!current()?.draft.some((slot) => slot.id === id)) return;
  if (id !== selectedId) unlockedId = undefined;
  selectedId = id;
  renderLists();
  renderInspector();
  syncEngine();
}

function replaceRecord(nextRecord: MatchRecord) {
  const next = records.map((record) => record.match.id === nextRecord.match.id ? nextRecord : record);
  repository.write(next);
  records = next;
}

function togglePlacementVisibility(id: string) {
  const record = current();
  if (!record?.draft.some((slot) => slot.id === id) || enginePending) return;
  clearError();
  try {
    const hidden = new Set(record.hiddenPlacementIds);
    const enabling = hidden.has(id);
    if (enabling) hidden.delete(id);
    else hidden.add(id);
    if (!enabling && unlockedId === id) unlockedId = undefined;
    replaceRecord({ ...record, hiddenPlacementIds: [...hidden], updatedAt: new Date().toISOString() });
    recordUndo(`${enabling ? '启用' : '停用'} ${record.draft.find((slot) => slot.id === id).name}`, record);
    renderRecord();
    syncEngine();
    document.querySelector<HTMLButtonElement>(`[data-visibility-id="${CSS.escape(id)}"]`)?.focus({ preventScroll: true });
  } catch (error) {
    showError(error);
  }
}

/** Puts the draft and enabled state of `before` back when undone. */
function recordUndo(label: string, before: MatchRecord) {
  undo.push({ label, restore: () => {
    const record = current();
    replaceRecord({ ...record, draft: before.draft, hiddenPlacementIds: before.hiddenPlacementIds, updatedAt: new Date().toISOString() });
    if (before.hiddenPlacementIds?.includes(unlockedId)) unlockedId = undefined;
    renderRecord();
    syncEngine();
  } });
}

function commitSlot(slot: unknown, label: string) {
  try {
    const record = current();
    const validated = validatePlacement(slot, record.match.field);
    const previous = record.draft.find((item) => item.id === validated.id);
    if (JSON.stringify(previous) === JSON.stringify(validated)) { renderRecord(); syncEngine(); return; }
    replaceRecord({ ...record, draft: record.draft.map((item) => item.id === validated.id ? validated : item), updatedAt: new Date().toISOString() });
    recordUndo(label, record);
    renderRecord();
    syncEngine();
  } catch (error) {
    renderRecord();
    $('#placement-error').textContent = error.message;
    showError(error);
    syncEngine();
  }
}

/** A drag in the top view finished; the engine only drags the unlocked placement. */
function moveSlot(slot: Placement) {
  commitSlot(slot, `拖动 ${slot.name}`);
}

async function ensureEngine() {
  if (!enginePromise) {
    enginePromise = import('../scene/pitch-preview.ts').then(({ PitchPreview }) => {
      engine = new PitchPreview($('#viewport'), $('#camera-list'), (error) => {
        engineReadyMatchId = undefined;
        pitchEditor.setAvailable(false);
        renderRendererBars();
        $('#engine-status').textContent = error.message;
        showError(error);
      });
      return engine;
    }).catch((error) => { enginePromise = undefined; throw error; });
  }
  return enginePromise;
}

function setActiveTab(tab: string) {
  const next = ['preview', 'scan', 'adjust'].includes(tab) ? tab : 'preview';
  if (next !== activeTab) setUnlocked(undefined);
  activeTab = next;
  // One inspector serves both tabs that edit placements.
  if (activeTab !== 'scan') $(`#${activeTab}-inspector-slot`).append($('#placement-inspector'));
  for (const name of ['preview', 'scan', 'adjust']) $(`#${name}-tab`).hidden = name !== activeTab;
  document.querySelectorAll<HTMLElement>('[data-tab]').forEach((button) => {
    button.classList.toggle('active', button.dataset.tab === activeTab);
    button.setAttribute('aria-current', button.dataset.tab === activeTab ? 'step' : 'false');
  });
  const tabLabels: Record<string, string> = { preview: '广告位预览', scan: '场地视频', adjust: '导入结果与微调' };
  $('#page-crumb').textContent = tabLabels[activeTab];
  if (activeTab !== 'scan') $<HTMLVideoElement>('#scan-preview').pause();
  if (activeTab !== 'preview') {
    $<HTMLVideoElement>('#cloud-preview-video').pause();
    localEditor.cancelDrawing();
    pitchEditor.cancelDrawing();
  }
  if (activeTab !== 'adjust') resultReview.pause();
  renderAnalysis();
  requestAnimationFrame(() => { engine?.refresh(); cloudEngine?.refresh(); });
}

async function renderRoute(refresh = false) {
  let parts: string[];
  try { parts = location.hash.slice(1).split('/').map(decodeURIComponent); }
  catch { showError(new Error('页面地址中的场次 ID 无效。')); parts = []; }
  let record = parts[0] === 'match' ? records?.find((item) => item.match.id === parts[1]) : null;
  const sameMatch = Boolean(record && currentId === record.match.id);
  $('#matches-page').hidden = Boolean(record);
  $('#match-page').hidden = !record;
  $('.on-nav[href="#matches"]').classList.toggle('active', !record);
  $<HTMLButtonElement>('#workspace-nav').classList.toggle('active', Boolean(record));
  if (!record) {
    ++matchLoadVersion;
    cancelAnalysisImport();
    pitchEditor.cancelDrawing();
    localEditor.cancelDrawing();
    cloudEngine?.setCloud();
    resetScanPreview();
    $('#page-crumb').textContent = '比赛列表';
    renderMatches();
    if (records && parts[0] === 'match') showError(new Error('尚未添加这场比赛，请先核验比赛 ID。'));
    currentId = undefined;
    undo.clear();
    unlockedId = undefined;
    void Promise.all([resultReview.setMatch(), pitchEditor.setMatch(), localEditor.setMatch()]).catch(showError);
    return;
  }
  if (sameMatch && !refresh) { setActiveTab(parts[2]); return; }
  const version = ++matchLoadVersion;
  clearError();
  if (!sameMatch) cancelAnalysisImport();
  // Loading (or refreshing) a match replaces its draft, so earlier steps no longer apply.
  undo.clear();
  unlockedId = undefined;
  currentId = record.match.id;
  const previousSelection = selectedId;
  selectedId = record.draft[0]?.id;
  enginePending = true;
  engineReadyMatchId = undefined;
  void Promise.all([resultReview.setMatch(currentId), pitchEditor.setMatch(currentId), localEditor.setMatch(currentId)]).catch(showError);
  $<HTMLButtonElement>('#workspace-nav').disabled = false;
  $<HTMLButtonElement>('#workspace-nav').dataset.matchId = currentId;
  renderRecord();
  setActiveTab(parts[2]);
  resetScanPreview();
  const savedFile = scanFiles.get(currentId);
  if (savedFile) void selectScanFile(savedFile);
  $('#pitch-mount').inert = true;
  if (!sameMatch) $('#pitch-mount').hidden = true;
  const refreshButton = $<HTMLButtonElement>('#refresh-configuration');
  const offline = record.match.mode === 'offline';
  refreshButton.hidden = offline;
  $('#replace-offline-package').hidden = !offline;
  refreshButton.disabled = true;
  $('#configuration-status').textContent = offline ? 'Loading the saved offline match package...' : 'Fetching the latest saved Fusion configuration...';
  $('#engine-status').textContent = 'Loading the field and assigned Fusion materials...';
  try {
    if (!offline) {
      const match = await gateway.lookupMatch(record.match.id);
      if (version !== matchLoadVersion) return;
      record = { match, draft: structuredClone(match.placements),
        hiddenPlacementIds: record.hiddenPlacementIds?.filter((id) => match.placements.some((slot) => slot.id === id)) };
      replaceRecord(record);
      selectedId = sameMatch && record.draft.some((slot) => slot.id === previousSelection) ? previousSelection : record.draft[0]?.id;
      renderRecord();
      $('#configuration-status').textContent = 'Latest Fusion settings received. Loading assigned materials...';
    }
  } catch (error) {
    if (version === matchLoadVersion) {
      enginePending = false;
      renderLists();
      renderAnalysis();
      refreshButton.disabled = false;
      $('#pitch-mount').inert = false;
      $('#configuration-status').textContent = 'Could not refresh from Fusion. Previous configuration was retained; use Refresh configuration to retry.';
      $('#engine-status').textContent = 'Latest Fusion configuration could not be loaded.';
      showError(error);
    }
    return;
  }
  engineQueue = engineQueue.then(async () => {
    if (version !== matchLoadVersion) return;
    try {
      const preview = await ensureEngine();
      if (version !== matchLoadVersion) return;
      preview.setEditing(unlockedId, keepAspect);
      preview.setAxesVisible(axesVisible);
      await preview.activate(record.draft, selectedId, selectSlot, moveSlot, record.match.field, record.hiddenPlacementIds);
      if (version !== matchLoadVersion) return;
      enginePending = false;
      engineReadyMatchId = currentId;
      renderLists();
      renderInspector();
      syncEngine();
      void Promise.all([pitchEditor.refresh(), localEditor.refresh()]).catch(showError);
      renderAnalysis();
      $('#pitch-mount').hidden = false;
      $('#pitch-mount').inert = false;
      $('#configuration-status').textContent = offline ? 'Offline match and materials loaded from this browser. Import a newer ZIP to update; no Fusion requests were made.'
        : `Updated from Fusion at ${new Date().toLocaleTimeString()}. Refresh replaces local adjustments.`;
      $('#engine-status').textContent = offline ? 'Offline placements, images and models loaded. No Fusion connection is required.'
        : 'Fusion placements and materials loaded. 视频和分析结果仅本地使用，不会提交 Fusion。';
    } catch (error) {
      console.error('Unable to load match preview:', error);
      if (version === matchLoadVersion) {
        enginePending = false;
        renderLists();
        renderAnalysis();
        $('#configuration-status').textContent = offline ? 'The offline preview could not be loaded. Import the original Fusion ZIP again.'
          : 'Latest settings received, but the preview could not be loaded. Use Refresh configuration to retry.';
        $('#engine-status').textContent = offline ? 'Offline material loading failed. Check browser storage and reimport the ZIP.'
          : 'Preview failed to load. Check the Fusion materials and connection, then reload the match.';
        showError(error);
      }
    } finally {
      if (version === matchLoadVersion) refreshButton.disabled = offline;
    }
  });
  await engineQueue;
}

function resetScanPreview() {
  scanAbort?.abort();
  if (scanURL) URL.revokeObjectURL(scanURL);
  scanURL = undefined;
  const video = $<HTMLVideoElement>('#scan-preview');
  video.pause();
  video.removeAttribute('src');
  video.load();
  $('#scan-video-area').hidden = true;
  $('#scan-dropzone').hidden = false;
  $('#scan-step').textContent = '未选择视频';
  $<HTMLInputElement>('#scan-file').value = '';
  $('#scan-status').textContent = '视频仅在浏览器本地预览，无需提交 Fusion。已有分析结果可直接导入文件夹；点云 PLY 请在“广告位预览”选择。';
  const cloudVideo = $<HTMLVideoElement>('#cloud-preview-video');
  cloudVideo.pause();
  cloudVideo.removeAttribute('src');
  cloudVideo.load();
  $('#cloud-video-area').hidden = true;
}

async function selectScanFile(file: File) {
  const matchId = currentId;
  try { validateScanFile(file); }
  catch (error) { $('#scan-status').textContent = error.message; showError(error); return; }
  resetScanPreview();
  clearError();
  if (scanFiles.get(matchId) !== file) scanFiles.delete(matchId);
  scanAbort = new AbortController();
  const { signal } = scanAbort;
  scanURL = URL.createObjectURL(file);
  const video = $<HTMLVideoElement>('#scan-preview');
  $('#scan-video-area').hidden = false;
  $('#scan-dropzone').hidden = true;
  $('#scan-filename').textContent = file.name;
  $('#scan-file-meta').textContent = `${(file.size / 1024 / 1024).toFixed(1)} MB · 正在读取视频信息`;
  $('#scan-status').textContent = '正在读取本地视频，尚未上传。';
  try {
    await new Promise<void>((resolve, reject) => {
      const finish = (error?: Error) => { clearTimeout(timer); video.removeEventListener('loadedmetadata', loaded); video.removeEventListener('error', failed); signal.removeEventListener('abort', aborted); error ? reject(error) : resolve(); };
      const loaded = () => finish();
      const failed = () => finish(new Error('浏览器无法读取该视频，请检查编码；建议使用 H.264 MP4。'));
      const aborted = () => finish(new DOMException('文件预览已取消。', 'AbortError'));
      const timer = setTimeout(() => finish(new Error('读取视频信息超时，请重新选择或检查文件。')), 20000);
      video.addEventListener('loadedmetadata', loaded, { once: true });
      video.addEventListener('error', failed, { once: true });
      signal.addEventListener('abort', aborted, { once: true });
      video.src = scanURL;
      video.load();
    });
    if (signal.aborted || currentId !== matchId) return;
    if (!Number.isFinite(video.duration) || video.duration <= 0 || !video.videoWidth) throw new Error('视频没有有效画面或时长，未作为扫描文件使用。');
    scanFiles.set(matchId, file);
    $('#scan-file-meta').textContent = `${(file.size / 1024 / 1024).toFixed(1)} MB · ${video.videoWidth} × ${video.videoHeight} · ${Math.round(video.duration)} 秒`;
    $('#scan-step').textContent = '已选择 · 未上传';
    $('#scan-status').textContent = '本地视频可预览，无需提交 Fusion。可直接导入结果文件夹；点云 PLY 请在“广告位预览”选择。刷新后需重新选择视频。';
    $<HTMLVideoElement>('#cloud-preview-video').src = scanURL;
    $('#cloud-video-area').hidden = false;
    $('#cloud-video-name').textContent = `${file.name} · 仅参考，与结果包原视频分开；不会自动估计点云或相机。`;
  } catch (error) {
    if (error.name === 'AbortError') return;
    $('#scan-step').textContent = '视频读取失败';
    $('#scan-status').textContent = error.message;
    showError(error);
  }
}

async function importOfflinePackage(source: File | string) {
  if (!records || libraryBusy) return;
  setLibraryBusy(true);
  clearError();
  const startingRoute = location.hash;
  try {
    let file = source;
    if (typeof file === 'string') {
      $('#offline-package-status').textContent = 'Reading the offline package from the onsite server...';
      file = await fetchServerFile(file);
    }
    $('#offline-package-status').textContent = 'Verifying the complete match package and material checksums...';
    const packageData = await parseOfflinePackage(file);
    const previous = records.find((item) => item.match.id === packageData.match.id);
    if (previous && !window.confirm('Replace this match configuration and its local placement adjustments with the imported offline package?')) {
      $('#offline-package-status').textContent = 'Import cancelled. Previous configuration and materials were retained.';
      return;
    }
    await offlinePackages.write(packageData);
    if (typeof source === 'string')
      localStorage.setItem(PACKAGE_PATHS_KEY, JSON.stringify({ ...packagePaths(), [packageData.id]: serverPath(source) }));
    const next = [...records.filter((item) => item.match.id !== packageData.match.id),
      { match: packageData.match, draft: structuredClone(packageData.match.placements), hiddenPlacementIds: [] }];
    try { repository.write(next); }
    catch (error) {
      try { await offlinePackages.remove(packageData.id); }
      catch (cleanup) { console.error('Unable to remove the uncommitted offline package.', cleanup); }
      throw error;
    }
    records = next;
    renderMatches();
    $('#offline-package-status').textContent = `Imported ${packageData.match.placements.length} placements and ${packageData.assets.size} materials. Saved in this browser; no Fusion API is used.`;
    if (location.hash === startingRoute || currentId === packageData.match.id) {
      const route = `#match/${encodeURIComponent(packageData.match.id)}/preview`;
      if (location.hash !== route) history.pushState(null, '', route);
      currentId = undefined;
      await renderRoute();
    }
    await engineQueue;
    if (previous?.match.offlinePackageId && previous.match.offlinePackageId !== packageData.id) {
      try { await offlinePackages.remove(previous.match.offlinePackageId); }
      catch (error) { showError(error); }
    }
  } catch (error) {
    $('#offline-package-status').textContent = `Package import failed. Previous configuration was retained. ${error instanceof Error ? error.message : String(error)}`;
    showError(error);
  } finally {
    setLibraryBusy(false);
    $<HTMLInputElement>('#offline-package-file').value = '';
  }
}
for (const selector of ['#import-offline-package', '#replace-offline-package']) {
  $<HTMLButtonElement>(selector).addEventListener('click', () => $<HTMLInputElement>('#offline-package-file').click());
}
$<HTMLInputElement>('#offline-package-file').addEventListener('change', (event) => {
  const file = (event.target as HTMLInputElement).files[0];
  if (file) void importOfflinePackage(file);
});

$<HTMLFormElement>('#match-lookup-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  if (!records || libraryBusy) return;
  const field = $<HTMLInputElement>('#match-lookup-id'), lookup = field.value;
  if (isServerPath(lookup)) { field.value = serverPath(lookup); void importOfflinePackage(field.value); return; }
  setLibraryBusy(true);
  clearError();
  $('#lookup-status').textContent = 'Requesting saved match configuration from Fusion...';
  try {
    const match = await gateway.lookupMatch(lookup);
    const previous = records.find((record) => record.match.id === match.id);
    const next = [...records.filter((record) => record.match.id !== match.id),
      { match, draft: structuredClone(match.placements),
        hiddenPlacementIds: previous?.hiddenPlacementIds?.filter((id) => match.placements.some((slot) => slot.id === id)) }];
    repository.write(next);
    records = next;
    $('#lookup-status').textContent = `Loaded ${match.placements.length} configured placements from Fusion.`;
    renderMatches();
    currentId = undefined;
    const route = `#match/${encodeURIComponent(match.id)}/preview`;
    if (location.hash === route) void renderRoute();
    else location.hash = route;
    if (previous?.match.offlinePackageId) {
      await engineQueue;
      try { await offlinePackages.remove(previous.match.offlinePackageId); }
      catch (error) { showError(error); }
    }
  } catch (error) { $('#lookup-status').textContent = error.message; showError(error); }
  finally { setLibraryBusy(false); }
});

document.addEventListener('click', (event) => {
  if (!(event.target instanceof Element)) return;
  const deletion = event.target.closest<HTMLElement>('[data-delete-match]');
  if (deletion) { void deleteMatch(deletion.dataset.deleteMatch); return; }
  const visibility = event.target.closest<HTMLElement>('[data-visibility-id]');
  if (visibility) { togglePlacementVisibility(visibility.dataset.visibilityId); return; }
  const tab = event.target.closest<HTMLElement>('[data-tab],[data-go-tab]');
  if (tab) routeToTab(tab.dataset.tab ?? tab.dataset.goTab);
  const slot = event.target.closest<HTMLElement>('[data-select-id]');
  if (slot) selectSlot(slot.dataset.selectId);
});
$('#adjust-placement-list').addEventListener('change', (event) => {
  if (event.target instanceof HTMLSelectElement && event.target.id === 'editor-placement-select') selectSlot(event.target.value);
});

$<HTMLButtonElement>('#workspace-nav').addEventListener('click', () => {
  location.hash = `match/${encodeURIComponent($<HTMLButtonElement>('#workspace-nav').dataset.matchId)}/preview`;
});
$<HTMLButtonElement>('#refresh-configuration').addEventListener('click', () => { void renderRoute(true); });
$<HTMLFormElement>('#placement-form').addEventListener('submit', (event) => {
  event.preventDefault();
  if (document.activeElement instanceof HTMLInputElement) applyInspectorField(document.activeElement);
});
$('#placement-fields').addEventListener('change', (event) => {
  const input = event.target as HTMLInputElement;
  if (input.name === 'anchor') { anchor = input.value as Anchor; renderInspector(); }
  else if (input.name === 'keepAspect') { keepAspect = input.checked; syncEngine(); }
  // After Tab moves focus on, so the re-rendered inspector can restore it there.
  else setTimeout(() => applyInspectorField(input));
});
$<HTMLButtonElement>('#reset-placement').addEventListener('click', () => {
  const original = current().match.placements.find((slot) => slot.id === selectedId);
  if (original && selectedId === unlockedId) commitSlot({ ...original }, `恢复场次配置 ${original.name}`);
});
$<HTMLButtonElement>('#inspector-lock').addEventListener('click', () => setUnlocked(unlockedId === selectedId ? undefined : selectedId));
document.querySelectorAll('[data-undo]').forEach((button) => button.addEventListener('click', () => { void undoLast(); }));
$<HTMLButtonElement>('#toggle-axes').addEventListener('click', () => {
  axesVisible = !axesVisible;
  $('#toggle-axes').setAttribute('aria-pressed', String(axesVisible));
  engine?.setAxesVisible(axesVisible);
});
document.addEventListener('keydown', (event) => {
  const target = event.target as HTMLElement;
  if (!currentId || activeTab === 'scan' || event.altKey || target.closest('input:not([type=checkbox]):not([type=radio]),textarea,select,[contenteditable]')) return;
  if ((event.ctrlKey || event.metaKey) && !event.shiftKey && event.key.toLowerCase() === 'z') {
    event.preventDefault();
    void undoLast();
  } else if (!event.ctrlKey && !event.metaKey && event.key.toLowerCase() === 'l' && !$<HTMLButtonElement>('#inspector-lock').disabled) {
    event.preventDefault();
    $('#inspector-lock').click();
  }
});
$<HTMLButtonElement>('#reset-business-view').addEventListener('click', () => {
  engine?.resetView();
});
$<HTMLButtonElement>('#choose-cloud-video').addEventListener('click', () => $<HTMLInputElement>('#scan-file').click());
$<HTMLButtonElement>('#choose-scan-file').addEventListener('click', () => $<HTMLInputElement>('#scan-file').click());
function bindServerPath(input: string, button: string, load: (path: string) => Promise<void>) {
  const run = () => {
    const field = $<HTMLInputElement>(input), path = serverPath(field.value);
    field.value = path;
    if (path) void load(path).catch(showError);
  };
  $(button).addEventListener('click', run);
  $(input).addEventListener('keydown', (event) => { if ((event as KeyboardEvent).key === 'Enter') run(); });
}
const loadScanPath = async (path: string) => {
  $('#scan-status').textContent = '正在从现场服务器读取视频…';
  try { await selectScanFile(await fetchServerFile(path)); }
  catch (error) { $('#scan-status').textContent = error.message; throw error; }
};
bindServerPath('#scan-path', '#load-scan-path', loadScanPath);
bindServerPath('#cloud-video-path', '#load-cloud-video-path', loadScanPath);
bindServerPath('#analysis-path', '#load-analysis-path', selectAnalysisFile);
$('#viewport').addEventListener('rectangledrawcancel', () => pitchEditor.cancelDrawing());
$('#viewport').addEventListener('rectangledrawmiss', () => { $('#pitch-local-draw-status').textContent = '请从草坪内拖出矩形，长宽至少 0.1 米。'; });
$('#cloud-viewport').addEventListener('pointpickcancel', () => localEditor.cancelDrawing());
$('#cloud-viewport').addEventListener('pointpickmiss', () => { $('#local-draw-status').textContent = '该位置未命中点云，请点击可见点，或调大选点半径后重试。'; });
for (const mode of ['orbit', 'top'] as const) $(`#cloud-view-${mode}`).addEventListener('click', () => {
  cloudEngine?.setView(mode);
  for (const name of ['orbit', 'top']) $(`#cloud-view-${name}`).setAttribute('aria-pressed', String(name === mode));
});
$('#reset-cloud-view').addEventListener('click', () => cloudEngine?.resetView());
$('#cloud-viewport').addEventListener('cloudviewchange', (event) => {
  if (!(event instanceof CustomEvent)) return;
  for (const name of ['orbit', 'top']) $(`#cloud-view-${name}`).setAttribute('aria-pressed', String(name === event.detail));
});
$<HTMLInputElement>('#cloud-pick-tolerance').addEventListener('change', (event) => {
  try { cloudEngine?.setPickTolerance((event.target as HTMLInputElement).valueAsNumber); }
  catch (error) { showError(error); }
});
$<HTMLButtonElement>('#import-analysis').addEventListener('click', () => $<HTMLInputElement>('#analysis-file').click());
$<HTMLInputElement>('#analysis-file').addEventListener('change', (event) => {
  if (event.target instanceof HTMLInputElement && event.target.files[0]) void selectAnalysisFile(event.target.files[0]);
});
$<HTMLButtonElement>('#cancel-analysis').addEventListener('click', () => {
  cancelAnalysisImport();
  analysisNotice = { matchId: currentId, text: '已取消导入，之前的结果未改变。' };
  $<HTMLInputElement>('#analysis-file').value = '';
  renderAnalysis();
});
$<HTMLButtonElement>('#remove-analysis').addEventListener('click', () => {
  cancelAnalysisImport();
  analysisResults.delete(currentId);
  analysisNotice = undefined;
  renderRecord();
  renderMatches();
});
$<HTMLInputElement>('#analysis-aligned').addEventListener('change', (event) => {
  const result = analysisResults.get(currentId);
  if (!result || !(event.target instanceof HTMLInputElement)) return;
  result.aligned = event.target.checked;
  renderAnalysis();
});
$<HTMLInputElement>('#cloud-point-size').addEventListener('input', (event) => {
  const result = analysisResults.get(currentId);
  if (!result || !(event.target instanceof HTMLInputElement)) return;
  result.pointSize = event.target.valueAsNumber;
  cloudEngine?.setPointSize(result.pointSize);
  $('#cloud-point-size-value').textContent = result.pointSize.toFixed(2);
});
for (const type of ['dragenter', 'dragover']) $('#analysis-import').addEventListener(type, (event) => {
  event.preventDefault();
  $('#analysis-import').classList.add('over');
});
for (const type of ['dragleave', 'drop']) $('#analysis-import').addEventListener(type, (event) => {
  event.preventDefault();
  $('#analysis-import').classList.remove('over');
  if (type === 'drop') {
    const files = (event as DragEvent).dataTransfer.files;
    if (files.length !== 1) showError(new Error('请一次导入一个 PLY 点云文件。'));
    else void selectAnalysisFile(files[0]);
  }
});
$('#scan-dropzone').addEventListener('keydown', (event) => {
  if (['Enter', ' '].includes(event.key)) { event.preventDefault(); $<HTMLInputElement>('#scan-file').click(); }
});
$<HTMLInputElement>('#scan-file').addEventListener('change', (event) => {
  if (!(event.target instanceof HTMLInputElement)) return;
  if (event.target.files[0]) void selectScanFile(event.target.files[0]);
});
for (const type of ['dragenter', 'dragover']) $('#scan-dropzone').addEventListener(type, (event) => {
  event.preventDefault();
  $('#scan-dropzone').classList.add('over');
});
for (const type of ['dragleave', 'drop']) $('#scan-dropzone').addEventListener(type, (event) => {
  event.preventDefault();
  $('#scan-dropzone').classList.remove('over');
  if (type === 'drop') {
    const files = (event as DragEvent).dataTransfer.files;
    if (files.length !== 1) showError(new Error('请一次选择一个全场扫描视频。'));
    else void selectScanFile(files[0]);
  }
});
$<HTMLButtonElement>('#submit-scan').addEventListener('click', async () => {
  try { await gateway.submitScan({ match: current().match, file: scanFiles.get(currentId) }); }
  catch (error) { showError(error); }
});
$<HTMLButtonElement>('#export-placements').addEventListener('click', () => {
  const record = current();
  const data = { version: 1, matchId: record.match.id, venueId: record.match.venueId, units: 'metres', coordinateSystem: 'pitch-centre-X-length-Y-main-stand-Z-down', rotation: 'XYZ-degrees', savedToFusion: false, placements: record.draft };
  const url = URL.createObjectURL(new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' }));
  const link = document.createElement('a');
  link.href = url;
  link.download = `placements-${record.match.id.replace(/[^a-zA-Z0-9_-]/g, '_')}.json`;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
});
// Both workspace tabs carry the same renderer bar; they share one package path and state.
const rendererBars = [...document.querySelectorAll<HTMLElement>('[data-renderer-bar]')];
const rendererPaths = rendererBars.map((bar) => bar.querySelector<HTMLInputElement>('[data-renderer-path]'));
/** What the running renderer action is doing, or empty when idle. */
let rendererBusy = '';
let rendererMessage = '';
let rendererFailed = false;
let rendererMatchId: string;

/** Why the current draft cannot be sent yet, or undefined when it can. */
function rendererBlocker() {
  const record = current();
  if (!record || record.match.mode !== 'offline' || !record.match.offlinePackageId)
    return '仅离线 ZIP 导入的场次可以发送到渲染器。';
  if (enginePending || engineReadyMatchId !== record.match.id) return '等待球场预览加载素材后可发送到渲染器…';
  if (!isServerPath(rendererPaths[0].value)) return '填写 chr-server 可读取的离线 ZIP 路径（--asset-root 之内）后可发送。';
}

function renderRendererBars() {
  const record = current();
  if (record?.match.id !== rendererMatchId) {
    rendererMatchId = record?.match.id;
    rendererMessage = '';
    const remembered = record?.match.offlinePackageId ? packagePaths()[record.match.offlinePackageId] ?? '' : '';
    for (const input of rendererPaths) input.value = remembered;
  }
  const blocker = rendererBlocker();
  for (const bar of rendererBars) {
    bar.querySelector<HTMLButtonElement>('[data-renderer-send]').disabled = Boolean(rendererBusy || blocker);
    bar.querySelector<HTMLButtonElement>('[data-renderer-export]').disabled = Boolean(rendererBusy || blocker);
    const status = bar.querySelector('[data-renderer-status]');
    status.textContent = rendererBusy
      || (blocker ?? (rendererMessage || '把当前广告位草稿（含启用状态）发送给 chr-server，替换其场景；发送前会检查 chr-server 和渲染 worker 是否就绪。'));
    status.classList.toggle('on-form-error', !rendererBusy && !blocker && rendererFailed);
  }
}

const MATERIAL_ENTRY: Record<string, { kind: DescribedAsset['kind']; extension: string }> = {
  'model/gltf-binary': { kind: 'glb', extension: '.glb' },
  'image/png': { kind: 'image_quad', extension: '.png' },
  'image/jpeg': { kind: 'image_quad', extension: '.jpg' },
};

/** The current draft as a render description scene, with assets read from the offline package. */
async function renderScene() {
  const record = current();
  if (rendererBlocker()) throw new Error(rendererBlocker());
  const packagePath = serverPath(rendererPaths[0].value);
  const assets = new Map<string, DescribedAsset>();
  const masked: string[] = [];
  for (const slot of record.draft) {
    if (!slot.asset || assets.has(slot.asset.id)) continue;
    const entry = MATERIAL_ENTRY[slot.asset.contentType];
    if (!entry) throw new Error(`The renderer cannot draw ${slot.asset.contentType} material ${slot.asset.name}.`);
    const blob = await offlinePackages.asset(slot.asset);
    const blended = entry.kind === 'glb' ? blendedMaterials(new Uint8Array(await blob.arrayBuffer())) : [];
    if (blended.length) masked.push(`${slot.asset.name}（${blended.join('、')}）`);
    assets.set(slot.asset.id, { kind: entry.kind, entry: `assets/${slot.asset.id}${entry.extension}`, sha256: await sha256(blob),
      ...(blended.length ? { blendAs: 'mask' as const } : {}) });
  }
  return { masked, scene: describeScene({ field: record.match.field, slots: record.draft, hidden: new Set(record.hiddenPlacementIds),
    geometry: engine.renderGeometry(), assets, packagePath }) };
}

/** Runs one renderer action at a time; a second click while it runs is ignored. */
async function rendererAction(busy: string, action: () => Promise<string>, failure: string) {
  if (rendererBusy) return;
  rendererBusy = busy;
  renderRendererBars();
  rendererFailed = false;
  try { rendererMessage = await action(); }
  catch (error) {
    rendererFailed = true;
    rendererMessage = `${failure}：${error instanceof Error ? error.message : String(error)}`;
  }
  finally { rendererBusy = ''; renderRendererBars(); }
}

const sendToRenderer = () => rendererAction('正在发送到 chr-server…', async () => {
  const record = current();
  const { scene, masked } = await renderScene();
  const url = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/chr-render`;
  const { loaded, sdi } = await sendDescription(url, scene);
  localStorage.setItem(PACKAGE_PATHS_KEY, JSON.stringify({ ...packagePaths(), [record.match.offlinePackageId]: serverPath(rendererPaths[0].value) }));
  return `已发送 ${scene.instances.length} 个广告位到 chr-server（输出 ${sdi.video_mode}）：新导入素材 ${loaded.assets_imported} 个，复用 ${loaded.assets_reused} 个（场景版本 ${loaded.scene_revision}）。`
    + (masked.length ? ` 渲染器不支持 GLB 半透明混合，以下材质按镂空（alpha ≥ 0.5）显示：${masked.join('；')}。` : '');
}, '发送失败');

const exportDescription = () => rendererAction('正在生成渲染描述…', async () => {
  const record = current();
  const url = URL.createObjectURL(new Blob([JSON.stringify({ scene: (await renderScene()).scene }, null, 2)], { type: 'application/json' }));
  const link = document.createElement('a');
  link.href = url;
  link.download = `render-scene-${record.match.id.replace(/[^a-zA-Z0-9_-]/g, '_')}.json`;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
  return '已导出渲染描述的 scene 部分；启动文件还需要 camera 部分。';
}, '导出失败');

for (const bar of rendererBars) {
  const input = bar.querySelector<HTMLInputElement>('[data-renderer-path]');
  input.addEventListener('input', () => {
    for (const other of rendererPaths) if (other !== input) other.value = input.value;
    renderRendererBars();
  });
  input.addEventListener('change', () => {
    for (const other of rendererPaths) other.value = serverPath(input.value);
    renderRendererBars();
  });
  input.addEventListener('keydown', (event) => { if (event.key === 'Enter') void sendToRenderer(); });
  bar.querySelector('[data-renderer-send]').addEventListener('click', () => { void sendToRenderer(); });
  bar.querySelector('[data-renderer-export]').addEventListener('click', () => { void exportDescription(); });
}
renderRendererBars();
window.addEventListener('hashchange', () => { void renderRoute(); });
window.addEventListener('beforeunload', () => { cancelAnalysisImport(); if (scanURL) URL.revokeObjectURL(scanURL); engine?.dispose(); cloudEngine?.dispose(); resultReview.dispose(); });

try { repository = new MatchRepository(localStorage); records = repository.read(); }
catch (error) { showError(error); $<HTMLButtonElement>('#match-lookup-submit').disabled = true; $<HTMLButtonElement>('#import-offline-package').disabled = true; }
renderMatches();
void renderRoute();
