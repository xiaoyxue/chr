import { FusionGateway, validatePlacement, validateScanFile } from '../matches/match-domain.ts';
import { HttpFusionAdapter } from '../matches/fusion-client.ts';
import { parseOfflinePackage, setOfflineAssetFiles, storeOfflineAssets } from '../matches/offline-package.ts';
import { importPlyFile } from '../analysis/ply-import.ts';
import { ResultReview } from '../analysis/result-review.ts';
import { LocalAdEditor } from '../placements/local-ad-editor.ts';
import { validatePitchAd, type LocalAd } from '../placements/local-ad.ts';
import { localAdStore } from '../placements/local-ad-store.ts';
import { exportLocalPlacements, importedAlignment, mergeLocalPlacements, parseLocalPlacements, waitingForCloud, type ImportMode } from '../placements/local-export.ts';
import { StoreConflict, StoredDocument, connectStore, readServerFile, removeMatchDocuments, serverSha256, storeDirectory, uploadFile, writeServerJson } from '../shared/studio-store.ts';
import { validateMatchLibrary, validatePackagePaths } from '../shared/store-documents.ts';
import { migrateBrowserData } from './browser-migration.ts';
import { localProblem, localRow, sendScope, type CloudState, type LocalRow } from '../placements/placement-list.ts';
import { glbBounds } from '../placements/local-ad-scene.ts';
import { PointCloudPreview } from '../scene/point-cloud-preview.ts';
import { exportTarget, fetchServerFile, isServerPath, serverPath } from '../shared/server-files.ts';
import { describeScene, sendDescription } from '../render/render-description.ts';
import { fusionAssets, localMaterials, type MaterialAccess } from '../render/send-materials.ts';
import '../styles/base.css';
import '../styles/pitch-preview.css';
import '../styles/onsite.css';
import { anchorPoint, moveAnchor, parseAspect, resizeSide, uniformScale, withAspect, type Anchor } from '../placements/ground-edit.ts';
import { UndoStack } from '../shared/undo-stack.ts';
import { SOURCE_LABEL, SessionStore, cloudAligned, restorePlan, withAlignment, type SessionSource, type SourceKind } from './session.ts';
import type { GroundPlacement, MatchRecord, Placement } from '../shared/types.ts';
import type { PitchPreview } from '../scene/pitch-preview.ts';
import type { LocalAnalysis } from '../analysis/ply-result.ts';

const $ = <T extends Element = HTMLElement>(selector: string) => document.querySelector<T>(selector);
const gateway = new FusionGateway(new HttpFusionAdapter());
/** The match library and Fusion drafts, in the studio store on the onsite server. */
const library = new StoredDocument('matches', validateMatchLibrary);
/** Offline package id → the ZIP path chr-server reads its materials from. */
const packagePathDocument = new StoredDocument('package-paths', validatePackagePaths);
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
const sessions = new SessionStore((error) => showError(error));
/** Remembers what the operator imported or selected in the current match, for a reload. */
function remember(kind: SourceKind, source?: SessionSource, matchId = currentId) {
  if (matchId) sessions.update(matchId, (session) => ({ ...session, sources: { ...session.sources, [kind]: source } }));
}
function rememberSelection(key: 'fusion' | 'pitch' | 'cloud', id?: string) {
  if (currentId && id) sessions.update(currentId, (session) => ({ ...session, selection: { ...session.selection, [key]: id } }));
}
const resultReview = new ResultReview(showError, clearError);
const pitchEditor = new LocalAdEditor({
  root: $('#pitch-local-editor'),
  prefix: 'pitch-',
  space: 'pitch',
  update: async (ads, selectedId) => {
    renderLocalLists();
    rememberSelection('pitch', selectedId);
    await Promise.all([engine?.setLocalAds(ads, selectedId), resultReview.setPitchAds(ads)]);
  },
  preferredSelection: () => currentId && sessions.get(currentId).selection.pitch,
  pick: () => {
    try { engine.startGroundRectangle((plane) => { void pitchEditor.finishPlane(plane); }); }
    catch (error) { pitchEditor.cancelDrawing(); showError(error); }
  },
  cancelPick: () => engine?.cancelGroundRectangle(),
  validate: (ad) => validatePitchAd(ad, current().match.field),
  error: showError,
  recordUndo: (label, restore, reapply) => undo.push({ label, restore, reapply }),
  send: () => sendToRenderer(),
});
const localEditor = new LocalAdEditor({
  root: $('#cloud-local-editor'),
  prefix: '',
  space: 'cloud',
  update: async (ads, selectedId) => {
    renderLocalLists();
    rememberSelection('cloud', selectedId);
    await Promise.all([cloudEngine?.setLocalAds(ads, selectedId), resultReview.setLocalAds(ads, analysisResults.get(currentId)?.aligned ?? false)]);
  },
  pick: () => {
    try { cloudEngine.startPlanePick((count) => localEditor.picked(count), (points, normalSign) => { void localEditor.finish(points, normalSign); }); }
    catch (error) { localEditor.cancelDrawing(); showError(error); }
  },
  cancelPick: () => { cloudEngine?.cancelPlanePick(); cloudEngine?.cancelAdDrag(); },
  manipulationAvailable: (available, locked) => cloudEngine?.setAdEditingEnabled(available, locked),
  preferredSelection: () => currentId && sessions.get(currentId).selection.cloud,
  error: showError,
  recordUndo: (label, restore, reapply) => undo.push({ label, restore, reapply }),
  send: () => sendToRenderer(),
});
resultReview.onChange = () => renderAnalysis();
resultReview.onSource = (matchId, source) => remember('result', source, matchId);

const htmlEscapes: Record<string, string> = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
const escapeHTML = (value: unknown) => String(value).replace(/[&<>"']/g, (char) => htmlEscapes[char]);
const current = () => records?.find((record) => record.match.id === currentId);
// Onsite-server paths of offline ZIPs, which chr-server reads their materials from.
const packagePaths = (): Record<string, string> => packagePathDocument.data ?? {};
function rememberPackagePath(packageId: string, path: string) {
  if (packagePaths()[packageId] === path) return;
  packagePathDocument.data = { ...packagePaths(), [packageId]: path };
  void packagePathDocument.update((paths) => ({ ...paths, [packageId]: path })).catch(showError);
}
setOfflineAssetFiles((packageId, assetId) => records?.find((record) => record.match.offlinePackageId === packageId)?.assetFiles?.[assetId]);

/**
 * Saves the match library. The page shows the change at once; if another page
 * saved first, the latest library is loaded instead and the operator is told.
 */
async function saveLibrary(next: MatchRecord[]) {
  records = next;
  try { await library.save(() => ({ version: 1, records: next })); }
  catch (error) {
    if (!(error instanceof StoreConflict)) throw error;
    records = (await library.load())?.records ?? [];
    renderMatches();
    void renderRoute(true);
    throw new Error('另一个页面修改了比赛数据，已载入服务器上的最新版本；刚才的修改未保存，请重新操作。');
  }
}
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
        <div class="on-match-actions"><button type="button" class="on-button on-delete-match" data-delete-match="${escapeHTML(match.id)}" aria-label="删除比赛 ${escapeHTML(match.name)}（仅删除工作室存储中的记录）" ${libraryBusy ? 'disabled' : ''}>${deletingMatchId === match.id ? '删除中…' : '删除'}</button>
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
  if (!window.confirm(`删除现场服务器上保存的比赛“${record.match.name}”？\n\n本地广告位、素材及导入结果也会移除，此操作不可撤销。\n不会删除 Fusion 上的比赛，也不会删除原始视频、PLY 或结果文件夹。`)) return;
  clearError();
  deletingMatchId = id;
  setLibraryBusy(true);
  const status = $('#match-delete-status');
  status.hidden = false;
  status.textContent = '正在删除本地比赛及关联数据…';
  let removed = false;
  try {
    const next = records.filter((item) => item.match.id !== id);
    await saveLibrary(next);
    removed = true;
    scanFiles.delete(id);
    analysisResults.delete(id);
    sessions.forget(id);
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
    await removeMatchDocuments(id);
    status.textContent = `已删除比赛“${record.match.name}”及其本地广告位和会话；Fusion、原始文件和已存储的素材文件未改变。`;
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    status.textContent = `${removed ? '比赛已从列表移除，但关联数据清理未完成。' : '删除失败，比赛记录未改变。'} ${detail}`;
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
      <span class="on-badge ${slot.type === '3D' ? 'amber' : ''}">${slot.type}</span><span><strong>${escapeHTML(slot.name)}</strong><small>${escapeHTML(slot.id)}<br />X ${slot.x.toFixed(2)} / Y ${slot.y.toFixed(2)} m${slot.type === '2D' ? ` · ${slot.sizeX.toFixed(2)} × ${slot.sizeY.toFixed(2)} m` : ` · ×${slot.scaleX.toFixed(2)}`}${slot.rz ? ` · ${slot.rz.toFixed(1)}°` : ''}</small></span>
      ${active ? `<span class="on-lock-state ${unlocked ? 'unlocked' : ''}" title="${unlocked ? '已解锁，可编辑' : '已锁定'}">${unlocked ? '🔓' : '🔒'}</span>` : ''}
    </button>
    <button type="button" class="on-visibility-toggle" role="switch" aria-checked="${enabled}" aria-label="启用广告位 ${escapeHTML(slot.name)}" title="${enabled ? '停用：不在预览和渲染器中显示' : '启用此广告位'}" data-visibility-id="${escapeHTML(slot.id)}" ${enginePending ? 'disabled' : ''}>
      <span class="on-toggle-track" aria-hidden="true"></span><span>${enabled ? '启用' : '停用'}</span>
    </button>
  </div>`;
  }).join('');
  $('#preview-placement-list').innerHTML = `<p class="on-list-group" data-fusion-heading hidden>Fusion 场次广告位</p>${markup}`;
  $('#adjust-placement-list').innerHTML = `<label class="on-editor-select">选择广告位<select id="editor-placement-select">${record.draft.map((slot) => `<option value="${escapeHTML(slot.id)}" ${slot.id === selectedId ? 'selected' : ''}>${slot.type} · ${escapeHTML(slot.name)}${hidden.has(slot.id) ? '（已停用）' : ''}</option>`).join('')}</select></label>`;
  renderLocalLists();
}

/** The match's local placements of both spaces, in the order the renderer receives them. */
const matchLocals = (): LocalAd[] => [...pitchEditor.allAds, ...localEditor.allAds];
/** The current match's clouds for sending: their saved same-coordinates answers, and the loaded one. */
const cloudState = (): CloudState => ({ loadedId: analysisResults.get(currentId)?.cloud.id, alignment: currentId ? sessions.get(currentId).alignment : {} });

function localRowMarkup(row: LocalRow, toggle: boolean) {
  const key = escapeHTML(`${row.space}:${row.id}`);
  return `<div class="on-slot-row ${row.visible ? '' : 'is-hidden'}" data-local-row="${key}">
    <button type="button" class="on-slot" data-local-select="${key}" title="${toggle ? '在编辑器中编辑' : '返回广告位预览编辑'}">
      <span class="on-badge ${row.type === 'flag' ? 'flag' : row.type === '3D' ? 'amber' : ''}">${row.badge}</span><span><strong>${escapeHTML(row.name)}</strong><small>${row.place} · ${row.size}${row.detail ? `<br />${row.detail}` : ''}<br />素材：${escapeHTML(row.material)}${row.problem ? `<br /><em class="on-slot-problem">无法发送：${row.problem}</em>` : row.note ? `<br /><em>${row.note}</em>` : ''}</small></span>
    </button>
    ${toggle ? `<button type="button" class="on-visibility-toggle" role="switch" aria-checked="${row.visible}" aria-label="启用 ${escapeHTML(row.name)}" title="${row.visible ? '停用：不在预览和渲染器中显示' : '启用'}" data-local-visibility="${key}">
      <span class="on-toggle-track" aria-hidden="true"></span><span>${row.visible ? '启用' : '停用'}</span></button>` : ''}
  </div>`;
}

/**
 * The local placements follow the Fusion placements in the overall list, grouped
 * by space; the adjust tab shows them read-only with a jump to editing.
 */
function renderLocalLists() {
  const record = current();
  if (!record) return;
  const rows = matchLocals().map((ad) => localRow(ad, cloudState()));
  const pitch = rows.filter((row) => row.space === 'pitch'), cloud = rows.filter((row) => row.space === 'cloud');
  const group = (title: string, items: LocalRow[], toggle: boolean) =>
    items.length ? `<p class="on-list-group">${title}</p>${items.map((row) => localRowMarkup(row, toggle)).join('')}` : '';
  for (const [list, toggle] of [['#preview-placement-list', true], ['#adjust-placement-list', false]] as const) {
    const container = $(list);
    container.querySelector('[data-local-list]')?.remove();
    container.insertAdjacentHTML('beforeend', `<div data-local-list>${group('球场本地', pitch, toggle)}${group('点云本地', cloud, toggle)}</div>`);
  }
  $('#preview-placement-list').querySelector('[data-fusion-heading]')?.toggleAttribute('hidden', !rows.length);
  $('#placement-count').textContent = `${record.draft.length + rows.length}${rows.length ? `（Fusion ${record.draft.length} · 球场本地 ${pitch.length} · 点云本地 ${cloud.length}）` : ''}`;
  renderRendererBars();
}

/** The export / import row's status, the same on both tabs. */
function transferStatus(text: string, failed = false) {
  document.querySelectorAll('[data-transfer-status]').forEach((element) => {
    element.textContent = text;
    element.classList.toggle('on-form-error', failed);
  });
}

const exportPaths = [...document.querySelectorAll<HTMLInputElement>('[data-export-path]')];
const exportDownloads = [...document.querySelectorAll<HTMLInputElement>('[data-export-download]')];
const fileStem = (matchId: string) => matchId.replace(/[^a-zA-Z0-9_-]/g, '_');
/** The match's export folder in the store, the default target of every export. */
const defaultExportFolder = (matchId: string) => `${storeDirectory()}/matches/${encodeURIComponent(matchId)}/exports/`;

/** Shows the match's last export path, or its default folder, in both export rows. */
function renderExportPath(matchId: string) {
  const path = sessions.get(matchId).exportPath ?? defaultExportFolder(matchId);
  for (const input of exportPaths) input.value = path;
}

/**
 * Writes an export as JSON to the typed server folder (under `name`) or `.json` file,
 * asking before it replaces a file; with 同时下载到本机 the browser also downloads it.
 * Resolves with the written server path.
 */
async function exportToServer(matchId: string, name: string, data: unknown) {
  const typed = serverPath(exportPaths[0].value), target = exportTarget(typed, defaultExportFolder(matchId), name);
  const text = JSON.stringify(data, null, 2);
  let path = await writeServerJson(target, text);
  if (!path) {
    if (!window.confirm(`文件已存在，覆盖？\n\n${target}`)) throw new Error(`${target} 已存在，未覆盖。`);
    path = await writeServerJson(target, text, true);
  }
  if (typed) sessions.update(matchId, (session) => ({ ...session, exportPath: typed }));
  if (exportDownloads[0].checked) {
    const url = URL.createObjectURL(new Blob([text], { type: 'application/json' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = path.split('/').pop();
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  return path + (exportDownloads[0].checked ? '（也已下载到本机）' : '');
}

/** Exports the match's onsite placements to the export path on the onsite server. */
async function exportOnsitePlacements() {
  const record = current();
  if (!record) return;
  transferStatus('');
  try {
    await saveLocalEdits();
    const session = sessions.get(record.match.id), analysis = analysisResults.get(record.match.id);
    const cloudPaths = analysis && session.sources.cloud?.path ? { [analysis.cloud.id]: session.sources.cloud.path } : {};
    const file = exportLocalPlacements(record.match.id, matchLocals(), session.alignment, cloudPaths);
    const path = await exportToServer(record.match.id, `local-placements-${fileStem(record.match.id)}-${file.exportedAt.replace(/[:.]/g, '-')}.json`, file);
    transferStatus(`已导出 ${file.placements.length} 个现场广告位（${file.clouds.length} 个点云）到服务器 ${path}。`);
  } catch (error) {
    transferStatus(`导出失败：${error instanceof Error ? error.message : String(error)}`, true);
  }
}

/** Asks whether imported placements merge into the match's or replace them all; undefined when cancelled. */
function askImportMode(summary: string): Promise<ImportMode | undefined> {
  const dialog = $<HTMLDialogElement>('#local-import-dialog');
  $('#local-import-summary').textContent = summary;
  dialog.returnValue = '';
  dialog.showModal();
  return new Promise((resolve) => dialog.addEventListener('close', () => {
    resolve(dialog.returnValue === 'merge' || dialog.returnValue === 'replace' ? dialog.returnValue : undefined);
  }, { once: true }));
}

/**
 * Imports onsite placements from an export file or a store's local-ads.json,
 * chosen locally or by server path. Everything is validated first; the result is
 * saved through the store like any edit, so a page that saved first wins.
 */
async function importOnsitePlacements(source: File | string) {
  const record = current();
  if (!record) return;
  transferStatus('');
  try {
    await saveLocalEdits();
    if (pitchEditor.editing || localEditor.editing) throw new Error('正在绘制或拖动本地广告位，请完成后再导入。');
    const file = typeof source === 'string' ? await readServerFile(source) : source;
    let data: unknown;
    try { data = JSON.parse(await file.text()); } catch { throw new Error(`${file.name} 不是有效的 JSON 文件。`); }
    const parsed = parseLocalPlacements(data);
    if (!parsed.placements.length) throw new Error('文件中没有现场广告位。');
    for (const ad of parsed.placements.filter((item) => item.space === 'pitch')) {
      try { validatePitchAd(ad, record.match.field); }
      catch (error) { throw new Error(`球场广告位“${ad.name}”不适合本场次的球场：${error instanceof Error ? error.message : String(error)}`); }
    }
    const existing = matchLocals(), same = parsed.placements.filter((ad) => existing.some((item) => item.id === ad.id)).length;
    let mode: ImportMode = 'merge';
    if (existing.length) {
      mode = await askImportMode(`文件中有 ${parsed.placements.length} 个现场广告位${parsed.matchId && parsed.matchId !== record.match.id ? `（来自场次 ${parsed.matchId}）` : ''}，本场次已有 ${existing.length} 个，其中 ${same} 个 ID 相同。
合并：相同 ID 的由导入的替换，其余保留。
替换全部本地广告位：只保留导入的。`);
      if (!mode) { transferStatus('已取消导入，现场广告位未改变。'); return; }
    }
    await localAdStore.save(record.match.id, (saved) => mergeLocalPlacements(saved, parsed.placements, mode));
    sessions.update(record.match.id, (session) => ({ ...session, alignment: importedAlignment(session.alignment, parsed.clouds) }));
    await Promise.all([pitchEditor.reload(), localEditor.reload()]);
    const { loadedId, alignment } = cloudState();
    const waiting = waitingForCloud(parsed.placements, loadedId, alignment);
    const hidden = parsed.placements.filter((ad) => ad.space !== 'pitch' && ad.cloudId !== loadedId && alignment[ad.cloudId] === true);
    const missing = parsed.placements.filter((ad) => !ad.materialPath);
    const plyPaths = parsed.clouds.flatMap((cloud) => cloud.path && cloud.id !== loadedId ? [cloud.path] : []);
    if (plyPaths.length && !$<HTMLInputElement>('#analysis-path').value) $<HTMLInputElement>('#analysis-path').value = plyPaths[0];
    transferStatus([`已${mode === 'merge' ? '合并' : '替换'}导入 ${parsed.placements.length} 个现场广告位，已保存到服务器。`,
      parsed.droppedPitchFlags ? `已忽略 ${parsed.droppedPitchFlags} 个球场旗帜（旗帜只能放在点云上）。` : '',
      hidden.length ? `${hidden.length} 个点云广告位的点云已确认同坐标：可发送，载入同一 PLY 后才会在点云中显示和编辑。` : '',
      waiting.length ? `${waiting.length} 个点云广告位属于未载入、未确认的点云（${[...new Set(waiting.map((ad) => ad.cloudId.slice(0, 12)))].join('、')}…）：载入并确认同一 PLY 后才会显示和发送。` : '',
      plyPaths.length ? `文件记录的 PLY：${plyPaths.join('、')}。` : '',
      missing.length ? `${missing.length} 个广告位缺少素材，列表中标出原因。` : ''].filter(Boolean).join(' '));
  } catch (error) {
    transferStatus(`导入失败：${error instanceof Error ? error.message : String(error)}`, true);
  }
}

/** Downloads the Fusion placement draft as JSON. */
async function exportPlacementJson() {
  const record = current();
  if (!record) return;
  const data = { version: 1, matchId: record.match.id, venueId: record.match.venueId, units: 'metres', coordinateSystem: 'pitch-centre-X-length-Y-main-stand-Z-down', rotation: 'XYZ-degrees', savedToFusion: false, placements: record.draft };
  transferStatus('');
  try {
    const path = await exportToServer(record.match.id, `placements-${fileStem(record.match.id)}.json`, data);
    transferStatus(`已导出 ${record.draft.length} 个 Fusion 广告位的位置到服务器 ${path}。`);
  } catch (error) {
    transferStatus(`导出失败：${error instanceof Error ? error.message : String(error)}`, true);
  }
}

// Both workspace tabs carry the same export / import row, apart from the renderer bar.
for (const bar of document.querySelectorAll<HTMLElement>('[data-transfer-bar]')) {
  const file = bar.querySelector<HTMLInputElement>('[data-local-file]'), path = bar.querySelector<HTMLInputElement>('[data-local-path]');
  bar.querySelector('[data-renderer-export]').addEventListener('click', () => {
    transferStatus('');
    void exportDescription().then(({ text, failed }) => transferStatus(text, failed));
  });
  bar.querySelector('[data-export-placements]').addEventListener('click', () => { void exportPlacementJson(); });
  const exportPath = bar.querySelector<HTMLInputElement>('[data-export-path]'), download = bar.querySelector<HTMLInputElement>('[data-export-download]');
  exportPath.addEventListener('input', () => { for (const other of exportPaths) if (other !== exportPath) other.value = exportPath.value; });
  download.addEventListener('change', () => { for (const other of exportDownloads) other.checked = download.checked; });
  bar.querySelector('[data-export-local]').addEventListener('click', () => { void exportOnsitePlacements(); });
  bar.querySelector('[data-import-local]').addEventListener('click', () => file.click());
  file.addEventListener('change', () => {
    const chosen = file.files[0];
    file.value = '';
    if (chosen) void importOnsitePlacements(chosen);
  });
  const importPath = () => {
    path.value = serverPath(path.value);
    if (path.value) void importOnsitePlacements(path.value);
  };
  bar.querySelector('[data-load-local-path]').addEventListener('click', importPath);
  path.addEventListener('keydown', (event) => { if (event.key === 'Enter') importPath(); });
}

/** Selects a local placement in its own editor, on the preview tab, and brings that editor into view. */
function selectLocal(key: string) {
  const [space, id] = key.split(/:(.*)/s), editor = space === 'pitch' ? pitchEditor : localEditor;
  if (activeTab !== 'preview') routeToTab('preview');
  // Wait for the tab switch so the editor can be scrolled into view.
  setTimeout(() => {
    void editor.selectFromList(id).then((problem) => {
      if (problem) { showError(new Error(problem)); return; }
      $(space === 'pitch' ? '#pitch-local-editor' : '#cloud-local-editor').scrollIntoView({ block: 'start', behavior: 'smooth' });
    });
  }, 50);
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
      <div class="on-inspector-group"><h3>缩放与旋转</h3>
      <div class="on-field-grid">${field('scale', '缩放 / ×', fusionScale(slot), 'min="0.01"')}${field('rz', '旋转 / °', slot.rz, 'min="-180" max="180"')}</div>
      <p class="on-help">绕中心等比缩放（相对 Fusion 原尺寸）和旋转；旋转后的整个区域须在球场内。</p></div>`;
  } else {
    $('#placement-fields').innerHTML = `<div class="on-inspector-group"><h3>位置（世界坐标）</h3>
      <div class="on-field-grid">${field('x', `${slot.region ? '区域' : ''}中心 X / m`, slot.x)}${field('y', `${slot.region ? '区域' : ''}中心 Y / m`, slot.y)}</div></div>
      <div class="on-inspector-group"><h3>缩放与旋转</h3>
      <div class="on-field-grid">${field('scale', '缩放 / ×', slot.scaleX, 'min="0.1" max="5"')}${field('rz', '旋转 / °', slot.rz, 'min="-180" max="180"')}</div>
      <p class="on-help">绕模型底部中心等比缩放（0.1–5 倍）和绕模型自身竖直轴（glTF Y）旋转：底部始终贴地，旋转不改变大小，模型可超出区域${slot.modelPose ? `；Fusion 初始朝向 ${slot.modelPose.yaw.toFixed(1)}°` : ''}。</p></div>
      <p id="inspector-footprint" class="on-readonly"></p>
      ${readonly(`高度 Z ${slot.z.toFixed(2)} m · 只支持绕竖直轴旋转`)}`;
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

/** A 2D placement's size relative to its Fusion original, by its X side. */
function fusionScale(slot: GroundPlacement) {
  const original = current().match.placements.find((item) => item.id === slot.id);
  return original?.type === '2D' ? slot.sizeX / original.sizeX : 1;
}

/** Degrees in [-180, 180). */
const normalAngle = (degrees: number) => ((degrees + 180) % 360 + 360) % 360 - 180;

/** The selected placement with one inspector field set to `value`. */
function editedSlot(slot: Placement, name: string, value: number): { next: Placement; action: string } {
  if (name === 'rz') return { next: { ...slot, rz: normalAngle(value) }, action: '旋转' };
  if (slot.type === '2D') {
    if (name === 'scale') {
      const factor = value / fusionScale(slot);
      return { next: { ...slot, sizeX: slot.sizeX * factor, sizeY: slot.sizeY * factor }, action: '缩放' };
    }
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
  const label = undo.label, redoLabel = undo.redoLabel;
  document.querySelectorAll<HTMLButtonElement>('[data-undo]').forEach((button) => {
    button.disabled = !undo.canUndo;
    button.title = label ? `撤销：${label}（Ctrl+Z）` : '没有可撤销的操作';
  });
  document.querySelectorAll<HTMLButtonElement>('[data-redo]').forEach((button) => {
    button.disabled = !undo.canRedo;
    button.title = redoLabel ? `重做：${redoLabel}（Ctrl+Shift+Z / Ctrl+Y）` : '没有可重做的操作';
  });
  document.querySelectorAll('[data-undo-label]').forEach((element) => { element.textContent = label ? `上一步：${label}` : '没有可撤销的操作'; });
}

async function undoLast() {
  clearError();
  try { await undo.undo(); }
  catch (error) { showError(error); }
}

async function redoLast() {
  clearError();
  try { await undo.redo(); }
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
    ? `已保存到现场服务器 ${new Date(record.updatedAt).toLocaleTimeString('zh-CN')}；未回传 Fusion，也未触发播出。`
    : '草稿保存在现场服务器的 studio 存储中，不会回传 Fusion。';
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
  renderLocalLists();
  localEditor.setCloud(result?.cloud.id);
  localEditor.setAvailable(Boolean(current() && result && cloudEngine?.isAvailable));
  void resultReview.setLocalAds(localEditor.ads, result?.aligned ?? false).catch(showError);
}

/** Imports a PLY point cloud into the current match; resolves whether it was imported. */
async function selectAnalysisFile(source: File | string): Promise<boolean> {
  if (!current()) {
    showError(new Error('请先选择比赛场次。'));
    return false;
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
    if (job.controller.signal.aborted || currentId !== job.matchId) return false;
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
    // A cloud confirmed (or un-ticked) before keeps that answer, also after a reload.
    const aligned = cloudAligned(sessions.get(job.matchId), cloud.id, cloud.declaredPitchCoordinates);
    // The answer is saved per cloud: its placements are then sendable without loading it again.
    sessions.update(job.matchId, (session) => withAlignment(session, cloud.id, aligned));
    analysisResults.set(job.matchId, { cloud, aligned, pointSize: 0.15, importedAt: new Date().toISOString() });
    remember('cloud', typeof source === 'string' ? { path: serverPath(source), name: file.name } : { name: file.name }, job.matchId);
    renderRecord();
    renderMatches();
    return true;
  } catch (error) {
    if (error instanceof DOMException && error.name === 'AbortError') return false;
    analysisNotice = { matchId: job.matchId, failed: true, text: `${error instanceof Error ? error.message : String(error)}${analysisResults.has(job.matchId) ? ' 已保留之前导入的结果。' : ''}` };
    showError(error);
    return false;
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
  rememberSelection('fusion', id);
  renderLists();
  renderInspector();
  syncEngine();
}

function replaceRecord(nextRecord: MatchRecord) {
  void saveLibrary(records.map((record) => record.match.id === nextRecord.match.id ? nextRecord : record)).catch(showError);
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

/** Puts the draft and enabled state of `before` back when undone, and the current ones when redone. */
function recordUndo(label: string, before: MatchRecord) {
  const after = current();
  const put = (state: MatchRecord) => {
    const record = current();
    replaceRecord({ ...record, draft: state.draft, hiddenPlacementIds: state.hiddenPlacementIds, updatedAt: new Date().toISOString() });
    if (state.hiddenPlacementIds?.includes(unlockedId)) unlockedId = undefined;
    renderRecord();
    syncEngine();
  };
  undo.push({ label, restore: () => put(before), reapply: () => put(after) });
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
      engine.onLocalAdSelect = (id) => { pitchEditor.selectAd(id); };
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
  if (!sameMatch) {
    cancelAnalysisImport();
    // The remembered selections and sources come from the server before the match opens.
    try { await sessions.load(record.match.id); }
    catch (error) { showError(error); }
    if (version !== matchLoadVersion) return;
  }
  // Loading (or refreshing) a match replaces its draft, so earlier steps no longer apply.
  undo.clear();
  unlockedId = undefined;
  currentId = record.match.id;
  renderExportPath(currentId);
  const previousSelection = selectedId, remembered = sessions.get(currentId).selection.fusion;
  selectedId = record.draft.find((slot) => slot.id === remembered)?.id ?? record.draft[0]?.id;
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
  if (!sameMatch) void restoreSession(currentId);
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
      $('#configuration-status').textContent = offline ? 'Offline match and materials loaded from the onsite server store. Import a newer ZIP to update; no Fusion requests were made.'
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

/** Previews a reference video; `path` is where the onsite server holds it, so a reload can read it again. */
async function selectScanFile(file: File, path?: string) {
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
    remember('video', path ? { path, name: file.name } : { name: file.name }, matchId);
    $('#scan-file-meta').textContent = `${(file.size / 1024 / 1024).toFixed(1)} MB · ${video.videoWidth} × ${video.videoHeight} · ${Math.round(video.duration)} 秒`;
    $('#scan-step').textContent = '已选择 · 未上传';
    $('#scan-status').textContent = `本地视频可预览，无需提交 Fusion。可直接导入结果文件夹；点云 PLY 请在“广告位预览”选择。${path ? '刷新页面后会从服务器路径自动重新读取。' : '刷新后需重新选择视频。'}`;
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
    if (previous && !window.confirm('用导入的离线包替换该场次的 Fusion 配置和草稿调整？\n\n现场创建的球场本地 / 点云广告位、旗帜和会话单独保存，保留不变。')) {
      $('#offline-package-status').textContent = 'Import cancelled. Previous configuration and materials were retained.';
      return;
    }
    $('#offline-package-status').textContent = 'Storing the materials on the onsite server...';
    const { assetFiles, assetBlended } = await storeOfflineAssets(packageData);
    // A ZIP chosen on this computer is stored too, so chr-server can read its materials.
    const zipPath = typeof source === 'string' ? serverPath(source) : await uploadFile(file, `${packageData.id}.zip`);
    await saveLibrary([...records.filter((item) => item.match.id !== packageData.match.id),
      { match: packageData.match, draft: structuredClone(packageData.match.placements), hiddenPlacementIds: [], assetFiles, assetBlended }]);
    rememberPackagePath(packageData.id, zipPath);
    renderMatches();
    $('#offline-package-status').textContent = `Imported ${packageData.match.placements.length} placements and ${packageData.assets.size} materials. Saved on the onsite server (${zipPath}); no Fusion API is used.`;
    if (location.hash === startingRoute || currentId === packageData.match.id) {
      const route = `#match/${encodeURIComponent(packageData.match.id)}/preview`;
      if (location.hash !== route) history.pushState(null, '', route);
      currentId = undefined;
      await renderRoute();
    }
    await engineQueue;
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
    await saveLibrary(next);
    $('#lookup-status').textContent = `Loaded ${match.placements.length} configured placements from Fusion.`;
    renderMatches();
    currentId = undefined;
    const route = `#match/${encodeURIComponent(match.id)}/preview`;
    if (location.hash === route) void renderRoute();
    else location.hash = route;
  } catch (error) { $('#lookup-status').textContent = error.message; showError(error); }
  finally { setLibraryBusy(false); }
});

document.addEventListener('click', (event) => {
  if (!(event.target instanceof Element)) return;
  const deletion = event.target.closest<HTMLElement>('[data-delete-match]');
  if (deletion) { void deleteMatch(deletion.dataset.deleteMatch); return; }
  const visibility = event.target.closest<HTMLElement>('[data-visibility-id]');
  if (visibility) { togglePlacementVisibility(visibility.dataset.visibilityId); return; }
  const localVisibility = event.target.closest<HTMLElement>('[data-local-visibility]');
  if (localVisibility) {
    const [space, id] = localVisibility.dataset.localVisibility.split(/:(.*)/s);
    void (space === 'pitch' ? pitchEditor : localEditor).toggleVisible(id);
    return;
  }
  const local = event.target.closest<HTMLElement>('[data-local-select]');
  if (local) { selectLocal(local.dataset.localSelect); return; }
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
document.querySelectorAll('[data-redo]').forEach((button) => button.addEventListener('click', () => { void redoLast(); }));
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
  } else if ((event.ctrlKey || event.metaKey) && ((event.shiftKey && event.key.toLowerCase() === 'z') || event.key.toLowerCase() === 'y')) {
    event.preventDefault();
    void redoLast();
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
  try { await selectScanFile(await fetchServerFile(path), path); }
  catch (error) { $('#scan-status').textContent = error.message; throw error; }
};
bindServerPath('#scan-path', '#load-scan-path', loadScanPath);
bindServerPath('#cloud-video-path', '#load-cloud-video-path', loadScanPath);
bindServerPath('#analysis-path', '#load-analysis-path', async (path) => { await selectAnalysisFile(path); });
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
  remember('cloud');
  analysisNotice = undefined;
  renderRecord();
  renderMatches();
});
$<HTMLInputElement>('#analysis-aligned').addEventListener('change', (event) => {
  const result = analysisResults.get(currentId);
  if (!result || !(event.target instanceof HTMLInputElement)) return;
  result.aligned = event.target.checked;
  sessions.update(currentId, (session) => withAlignment(session, result.cloud.id, result.aligned));
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
/**
 * After a reload (or on opening a match), re-imports what the operator last
 * imported by server path and names the locally chosen files to select again.
 * A path that is gone is reported and the rest still restores.
 */
const PATH_FIELDS: Record<SourceKind, string[]> = { cloud: ['#analysis-path'], result: ['#result-folder-path'], video: ['#scan-path', '#cloud-video-path'] };
async function restoreSession(matchId: string) {
  const loaded = new Set<SourceKind>([
    ...(analysisResults.has(matchId) ? ['cloud' as const] : []),
    ...(resultReview.hasResultFor(matchId) ? ['result' as const] : []),
    ...(scanFiles.has(matchId) ? ['video' as const] : []),
  ]);
  const { load, reselect } = restorePlan(sessions.get(matchId), loaded);
  const status = $('#session-status');
  if (!load.length && !reselect.length) { status.hidden = true; return; }
  const loaders: Record<SourceKind, (path: string) => Promise<boolean>> = {
    cloud: (path) => selectAnalysisFile(path),
    result: (path) => resultReview.importPath(path),
    video: async (path) => { await loadScanPath(path); return scanFiles.has(matchId); },
  };
  const restored: string[] = [], failed: string[] = [];
  const reselectText = reselect.length ? `刷新前从本机选择的文件无法自动重新读取，请重新选择：${reselect.map(({ kind, name }) => `${SOURCE_LABEL[kind]} ${name}`).join('、')}。` : '';
  status.hidden = false;
  status.classList.remove('on-form-error');
  for (const { kind, path } of load) {
    if (currentId !== matchId) return;
    status.textContent = `正在恢复上次会话：从服务器路径读取${SOURCE_LABEL[kind]} ${path}…`;
    for (const field of PATH_FIELDS[kind]) $<HTMLInputElement>(field).value = path;
    let ok = false;
    try { ok = await loaders[kind](path); }
    catch (error) { console.warn(`Unable to restore ${kind} from ${path}.`, error); }
    (ok ? restored : failed).push(`${SOURCE_LABEL[kind]} ${path}`);
  }
  if (currentId !== matchId) return;
  status.textContent = [restored.length ? `已恢复上次会话：${restored.join('、')}。` : '',
    failed.length ? `无法从服务器路径恢复（文件不存在或无法读取，详见各区域提示）：${failed.join('、')}。` : '', reselectText].filter(Boolean).join(' ');
  status.classList.toggle('on-form-error', Boolean(failed.length || reselect.length));
}

/** Why leaving the page now would lose work, or undefined when everything is saved. */
function unsavedWork(): string | undefined {
  if (rendererBusy) return rendererBusy;
  for (const editor of [pitchEditor, localEditor]) {
    if (editor.editing) return '正在绘制或拖动本地广告位';
    const name = editor.unsavedChanges;
    if (name) return `“${name}”有未保存的修改`;
  }
  if (engine?.dragging) return '正在拖动场次广告位';
  const typed = [...document.querySelectorAll<HTMLInputElement>('#placement-fields input')].find((input) => input.value !== input.defaultValue && input.type !== 'radio' && input.type !== 'checkbox');
  if (typed) return '场次广告位有未保存的修改';
}
// Browsers show their own leave-page dialog; it appears only while something is unsaved.
window.addEventListener('beforeunload', (event) => {
  if (!unsavedWork()) return;
  event.preventDefault();
  event.returnValue = '';
});

// Both workspace tabs carry the same renderer bar; they share one package path and state.
const rendererBars = [...document.querySelectorAll<HTMLElement>('[data-renderer-bar]')];
const rendererPaths = rendererBars.map((bar) => bar.querySelector<HTMLInputElement>('[data-renderer-path]'));
/** What the running renderer action is doing, or empty when idle. */
let rendererBusy = '';
let rendererMessage = '';
let rendererFailed = false;
let rendererMatchId: string;

/**
 * What a send would include now. Fusion placements need the offline ZIP that
 * chr-server reads from its path; flags need neither.
 */
function rendererScope() {
  const record = current();
  if (!record) return { fusion: false, blocker: '请先选择比赛场次。', note: '' };
  const fusionProblem = record.match.mode !== 'offline' || !record.match.offlinePackageId ? 'Fusion 广告位仅能从离线 ZIP 导入的场次发送'
    : !isServerPath(rendererPaths[0].value) ? '未填写 chr-server 可读取的离线 ZIP 路径（--asset-root 之内）' : undefined;
  const waiting = enginePending || engineReadyMatchId !== record.match.id ? '等待球场预览加载素材后可发送到渲染器…' : undefined;
  const locals = matchLocals().filter((ad) => !localProblem(ad, cloudState())).length;
  return sendScope({ fusionProblem, waiting, placements: record.draft.length, locals });
}

function renderRendererBars() {
  const record = current();
  if (record?.match.id !== rendererMatchId) {
    rendererMatchId = record?.match.id;
    rendererMessage = '';
    const remembered = record?.match.offlinePackageId ? packagePaths()[record.match.offlinePackageId] ?? '' : '';
    for (const input of rendererPaths) input.value = remembered;
  }
  const { blocker, note } = rendererScope();
  document.querySelectorAll<HTMLButtonElement>('[data-renderer-export]').forEach((button) => { button.disabled = Boolean(rendererBusy || blocker); });
  for (const bar of rendererBars) {
    bar.querySelector<HTMLButtonElement>('[data-renderer-send]').disabled = Boolean(rendererBusy || blocker);
    const status = bar.querySelector('[data-renderer-status]');
    const message = rendererMessage && note && !rendererMessage.includes(note) ? `${rendererMessage} ${note}` : rendererMessage || note;
    status.textContent = rendererBusy
      || (blocker ?? (message || '把全部广告位（Fusion、球场本地、点云本地和旗帜，含启用状态）发送给 chr-server，替换其场景；发送前会检查 chr-server 和渲染 worker 是否就绪。'));
    status.classList.toggle('on-form-error', !rendererBusy && !blocker && rendererFailed);
  }
}

/** How a send learns about materials: hashes from the server, files only when nothing was noted. */
const materialAccess: MaterialAccess = { sha256: serverSha256, read: (path) => readServerFile(path), bounds: glbBounds };

/**
 * The match's local placements that the renderer can draw, and why the others are left
 * out. A point-cloud placement is in pitch coordinates once its cloud is confirmed to
 * share them. Each material is described as the server holds it now, from its hash.
 */
async function renderLocals() {
  const sendable: LocalAd[] = [], skipped: string[] = [];
  for (const ad of matchLocals()) {
    const problem = localProblem(ad, cloudState());
    if (problem) skipped.push(`${ad.name}（${problem}）`);
    else sendable.push(ad);
  }
  const { locals, masked } = await localMaterials(sendable, materialAccess);
  return { locals, masked, skipped: skipped.length ? ` 未发送的本地广告位：${skipped.join('；')}。` : '' };
}

/** Asks to save typed edits first, so a send never misses them; throws when the operator declines. */
async function saveLocalEdits() {
  for (const editor of [pitchEditor, localEditor]) {
    const name = editor.unsavedChanges;
    if (!name) continue;
    if (!window.confirm(`“${name}”有未保存的修改。先保存再继续？`))
      throw new Error(`“${name}”有未保存的修改：请先点击“保存属性”，或用“保存并发送到渲染器”。`);
    if (!await editor.save()) throw new Error(`“${name}”的修改未能保存，已停止。`);
  }
}

/**
 * The current draft as a render description scene: the Fusion placements with
 * assets read from the offline package when it can be sent, and the local placements.
 */
async function renderScene() {
  const record = current(), scope = rendererScope();
  if (scope.blocker) throw new Error(scope.blocker);
  await saveLocalEdits();
  const packagePath = scope.fusion ? serverPath(rendererPaths[0].value) : '';
  const { assets, masked, assetBlended } = await fusionAssets(scope.fusion ? record.draft : [], record.assetFiles ?? {}, record.assetBlended, materialAccess);
  // Older matches did not note their blended GLB materials; keep them now, so later sends read nothing.
  if (assetBlended) void saveLibrary(records.map((item) => item.match.id === record.match.id ? { ...item, assetBlended } : item)).catch(showError);
  const local = await renderLocals();
  return { masked: [...masked, ...local.masked], skipped: (scope.note ? ` ${scope.note}` : '') + local.skipped, fusion: scope.fusion,
    scene: describeScene({ field: record.match.field, slots: scope.fusion ? record.draft : [], hidden: new Set(record.hiddenPlacementIds),
      geometry: scope.fusion ? engine.renderGeometry() : new Map(), assets, packagePath, locals: local.locals }) };
}

/** Runs one renderer action at a time and resolves with its status; a second click while it runs is ignored. */
async function rendererAction(busy: string, action: () => Promise<string>, failure: string) {
  if (rendererBusy) return { text: `另一个渲染器操作正在进行：${rendererBusy}`, failed: true };
  rendererBusy = busy;
  renderRendererBars();
  rendererFailed = false;
  try { rendererMessage = await action(); }
  catch (error) {
    rendererFailed = true;
    rendererMessage = `${failure}：${error instanceof Error ? error.message : String(error)}`;
  }
  finally { rendererBusy = ''; renderRendererBars(); }
  return { text: rendererMessage, failed: rendererFailed };
}

/** Names the step a running renderer action has reached. */
function rendererProgress(step: string) {
  rendererBusy = step;
  renderRendererBars();
}

const sendToRenderer = () => rendererAction('正在检查素材…', async () => {
  const record = current(), started = performance.now();
  const { scene, masked, skipped, fusion } = await renderScene();
  const checked = performance.now();
  rendererProgress('正在发送到 chr-server…');
  const url = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/chr-render`;
  const { loaded, sdi } = await sendDescription(url, scene);
  const seconds = (from: number, to: number) => ((to - from) / 1000).toFixed(1);
  const timing = `检查素材 ${seconds(started, checked)} 秒，发送 ${seconds(checked, performance.now())} 秒`;
  if (fusion) rememberPackagePath(record.match.offlinePackageId, serverPath(rendererPaths[0].value));
  return `已发送 ${scene.instances.length} 个广告位（含本地和旗帜）到 chr-server（输出 ${sdi.video_mode}）：新导入素材 ${loaded.assets_imported} 个，复用 ${loaded.assets_reused} 个（场景版本 ${loaded.scene_revision}；${timing}）。`
    + (masked.length ? ` 渲染器不支持 GLB 半透明混合，以下材质按镂空（alpha ≥ 0.5）显示：${masked.join('；')}。` : '') + skipped;
}, '发送失败');

const exportDescription = () => rendererAction('正在检查素材…', async () => {
  const record = current();
  const { scene, skipped } = await renderScene();
  rendererProgress('正在写入服务器…');
  const path = await exportToServer(record.match.id, `render-scene-${fileStem(record.match.id)}.json`, { scene });
  return `已把渲染描述的 scene 部分导出到服务器 ${path}；启动文件还需要 camera 部分。${skipped}`;
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
}
renderRendererBars();
window.addEventListener('hashchange', () => { void renderRoute(); });
// After `beforeunload`, which the operator may cancel: only a page that is really going away releases its GPU resources.
window.addEventListener('pagehide', (event) => {
  if (event.persisted) return;
  cancelAnalysisImport(); if (scanURL) URL.revokeObjectURL(scanURL); engine?.dispose(); cloudEngine?.dispose(); resultReview.dispose();
});

localAdStore.onConflict = (matchId) => {
  if (matchId === currentId) void Promise.all([pitchEditor.reload(), localEditor.reload()]).catch(showError);
};

/** Everything lives in the studio store on the onsite server; without it the page does not fall back to the browser. */
async function start() {
  // Nothing can be imported or looked up until the library has come from the server.
  for (const selector of ['#match-lookup-submit', '#import-offline-package', '#replace-offline-package']) $<HTMLButtonElement>(selector).disabled = true;
  $('#lookup-status').textContent = '正在从现场服务器读取比赛库…';
  try {
    await connectStore();
    const migrated = await migrateBrowserData(library, packagePathDocument);
    records = (await library.load())?.records ?? [];
    await packagePathDocument.load();
    if (migrated) {
      $('#match-delete-status').hidden = false;
      $('#match-delete-status').textContent = migrated;
    }
  } catch (error) {
    records = null;
    showError(error);
    $<HTMLButtonElement>('#match-lookup-submit').disabled = true;
    $<HTMLButtonElement>('#import-offline-package').disabled = true;
  }
  $('#lookup-status').textContent = '';
  setLibraryBusy(false);
  void renderRoute();
}
void start();
