import { FusionGateway, MatchRepository, validatePlacement, validateScanFile } from '../matches/match-domain.ts';
import { HttpFusionAdapter } from '../matches/fusion-client.ts';
import { importPlyFile } from '../analysis/ply-import.ts';
import '../styles/base.css';
import '../styles/pitch-preview.css';
import '../styles/onsite.css';
import type { MatchRecord } from '../shared/types.ts';
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
let currentId: string;
let selectedId: string;
let activeTab = 'preview';
let enginePromise: Promise<PitchPreview>;
let engine: PitchPreview;
let enginePending = false;
let engineReadyMatchId: string;
let matchLoadVersion = 0;
let engineQueue = Promise.resolve();
let scanURL: string;
let scanAbort: AbortController;

const htmlEscapes: Record<string, string> = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
const escapeHTML = (value: unknown) => String(value).replace(/[&<>"']/g, (char) => htmlEscapes[char]);
const current = () => records?.find((record) => record.match.id === currentId);
const selected = () => current()?.draft.find((slot) => slot.id === selectedId);

function showError(error: unknown) {
  $('#app-error').textContent = error instanceof Error ? error.message : String(error);
  $('#app-error').hidden = false;
}

function clearError() { $('#app-error').hidden = true; }

function renderMatches() {
  $('#match-count').textContent = String(records?.length ?? 0);
  $('#match-cards').innerHTML = records?.length ? records.map(({ match, draft }) => `
    <article class="on-card on-match-card"><span class="on-badge amber">Fusion configuration</span>
      <h2>${escapeHTML(match.name)}</h2><code>${escapeHTML(match.id)}</code>
      <p class="on-sub">${escapeHTML(match.date.replace('T', ' '))}　·　${escapeHTML(match.venue)}</p>
      <div class="on-match-foot"><span class="on-help">${draft.length} 个广告位 · ${analysisResults.has(match.id) ? '已导入本地点云' : '等待场地分析'}</span><a class="on-button primary" href="#match/${encodeURIComponent(match.id)}/preview">进入场次 →</a></div>
    </article>`).join('') : `<div class="on-card on-empty"><h2>${records ? '还没有添加比赛' : '本地记录读取失败'}</h2><p>${records ? '输入 Fusion 提供的比赛 ID 获取场次配置。' : '为避免覆盖原记录，已停止写入。请查看页面上方错误信息。'}</p></div>`;
}

function routeToTab(tab: string) {
  if (currentId) location.hash = `match/${encodeURIComponent(currentId)}/${tab}`;
}

function renderLists() {
  const record = current();
  if (!record) return;
  const hidden = new Set(record.hiddenPlacementIds);
  const markup = record.draft.map((slot) => `<div class="on-slot-row ${slot.id === selectedId ? 'active' : ''} ${hidden.has(slot.id) ? 'is-hidden' : ''}">
    <button type="button" class="on-slot" aria-pressed="${slot.id === selectedId}" data-select-id="${escapeHTML(slot.id)}">
      <span class="on-badge ${slot.type === '3D' ? 'amber' : ''}">${slot.type}</span><span><strong>${escapeHTML(slot.name)}</strong><small>${escapeHTML(slot.id)}<br />X ${slot.x.toFixed(1)} / Y ${slot.y.toFixed(1)} m · ${slot.rz.toFixed(1)}°</small></span>
    </button>
    <button type="button" class="on-visibility-toggle" role="switch" aria-checked="${!hidden.has(slot.id)}" aria-label="显示广告位 ${escapeHTML(slot.name)}" title="${hidden.has(slot.id) ? '显示' : '隐藏'}此广告位（仅本地预览）" data-visibility-id="${escapeHTML(slot.id)}" ${enginePending ? 'disabled' : ''}>
      <span class="on-toggle-track" aria-hidden="true"></span><span>${hidden.has(slot.id) ? '已隐藏' : '已显示'}</span>
    </button>
  </div>`).join('');
  $('#preview-placement-list').innerHTML = markup;
  $('#adjust-placement-list').innerHTML = `<label class="on-editor-select">选择广告位<select id="editor-placement-select">${record.draft.map((slot) => `<option value="${escapeHTML(slot.id)}" ${slot.id === selectedId ? 'selected' : ''}>${slot.type} · ${escapeHTML(slot.name)}</option>`).join('')}</select></label>`;
  $('#placement-count').textContent = String(record.draft.length);
}

function renderEditor() {
  const slot = selected();
  $<HTMLFormElement>('#placement-form').hidden = !slot;
  if (!slot) return;
  $('#editor-name').textContent = slot.name;
  $('#editor-info').textContent = slot.type === '2D'
    ? '地面位置 / 尺寸以米计；绕 Z 旋转，始终贴地。'
    : '位置以米计，旋转以度计，缩放为原模型的倍数；+Z 向下。';
  const labels = slot.type === '3D' && slot.region
    ? { x: 'Region X / m', y: 'Region Y / m', rz: 'Extra yaw / deg', scaleX: 'Uniform scale' }
    : slot.type === '2D'
    ? { x: 'X / m', y: 'Y / m', rz: '旋转 Z / °', sizeX: '长 X / m', sizeY: '宽 Y / m' }
    : { x: 'X / m', y: 'Y / m', z: 'Z / m', rx: '旋转 X / °', ry: '旋转 Y / °', rz: '旋转 Z / °', scaleX: '缩放 X / ×', scaleY: '缩放 Y / ×', scaleZ: '缩放 Z / ×' };
  const values: Record<string, unknown> = { ...slot };
  $('#placement-fields').innerHTML = Object.entries(labels).map(([key, label]) => {
    const min = key.startsWith('scale') || key.startsWith('size') ? 'min="0.1"' : key.startsWith('r') ? 'min="-180" max="180"' : '';
    const max = key.startsWith('scale') ? 'max="5"' : '';
    return `<label>${label}<input name="${key}" type="number" step="any" ${min} ${max} value="${values[key]}" required aria-label="${label}" /></label>`;
  }).join('');
  $('#placement-error').textContent = '';
}

function renderRecord() {
  const record = current();
  if (!record) return;
  const { match } = record;
  $('#match-title').textContent = match.name;
  $('#match-meta').textContent = `${match.date.replace('T', ' ')}　·　${match.venue}　·　场地 ${match.venueId}`;
  $('#match-code').textContent = match.id;
  $('#match-source').textContent = 'Fusion match';
  $('#resource-step').textContent = 'Loaded from Fusion';
  $('#field-size').textContent = `${match.field.length} × ${match.field.width} m`;
  $('#analysis-status').textContent = '尚未接入扫描分析接口，没有创建或提交分析任务。';
  $('#draft-badge').textContent = record.updatedAt ? '本地草稿已保存' : '场次初始配置';
  $('#save-status').textContent = record.updatedAt
    ? `本机保存于 ${new Date(record.updatedAt).toLocaleTimeString('zh-CN')}；未回传 Fusion，也未触发播出。`
    : '草稿仅保存在本机，不会回传 Fusion。';
  renderLists();
  renderEditor();
  renderAnalysis();
}

function syncEngine() {
  if (!engine || !current() || enginePending || engineReadyMatchId !== currentId) return;
  engine.setPlacements(current().draft, selectedId, selectSlot, commitSlot, current().hiddenPlacementIds);
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
  $<HTMLButtonElement>('#import-analysis').disabled = !ready || busy;
  $<HTMLButtonElement>('#import-analysis').textContent = result ? '替换 PLY 文件' : '选择 PLY 文件';
  $('#cancel-analysis').hidden = !busy;
  $('#remove-analysis').hidden = !result;
  $<HTMLButtonElement>('#remove-analysis').disabled = busy;
  $('#analysis-empty').hidden = Boolean(result && ready);
  $('#analysis-workspace').hidden = !result || !ready;
  $('#analysis-empty-title').textContent = result ? '场次预览尚未就绪' : '等待场地分析结果';
  $('#analysis-empty-description').textContent = result
    ? '本地点云已保留。场次配置载入后可继续预览；若加载失败，请刷新场次配置重试。'
    : '可在上方手动导入点云，或前往场地扫描等待 Fusion 分析接口接入。';
  const notice = analysisNotice?.matchId === currentId ? analysisNotice : undefined;
  $('#analysis-import-status').textContent = busy ? '正在本地解析点云，请稍候…'
    : notice?.text ?? (result ? `${result.cloud.fileName} · ${(result.cloud.positions.length / 3).toLocaleString('zh-CN')} 点 · ${(result.cloud.byteLength / 1024 / 1024).toFixed(1)} MB · 未上传`
      : ready ? '结果将绑定当前场次，请确认文件属于本场比赛。' : '请等待当前场次配置和广告素材载入后再导入。');
  $('#analysis-import-status').classList.toggle('on-form-error', Boolean(!busy && notice?.failed));
  $('#analysis-status').textContent = result
    ? `已为当前场次手动导入 ${result.cloud.fileName}。仅本地使用，未创建 Fusion 分析任务，也未上传文件。`
    : '尚未接入扫描分析接口，没有创建或提交分析任务；可在“广告位微调”手动导入 PLY 点云。';
  $('#adjust-step').textContent = result ? '本地点云已导入' : '等待分析结果';
  $('#result-dot').hidden = !result;
  $<HTMLFieldSetElement>('#placement-edit-controls').disabled = !ready || !result?.aligned;
  if (result) {
    $('#result-source').textContent = result.cloud.fileName;
    $('#result-camera').textContent = result.aligned ? result.cloud.declaredPitchCoordinates ? '条件球场锚定（文件声明）' : '球场坐标（手动确认）' : '未确认坐标 · 叠加关闭';
    $('#result-coverage').textContent = (result.cloud.positions.length / 3).toLocaleString('zh-CN');
    $<HTMLInputElement>('#analysis-aligned').checked = result.aligned;
    $<HTMLInputElement>('#cloud-point-size').value = String(result.pointSize);
    $('#cloud-point-size-value').textContent = result.pointSize.toFixed(2);
    $('#analysis-coordinate-note').textContent = result.cloud.declaredPitchCoordinates
      ? '文件声明：条件球场锚定、米制、−Z 向上。保持原始点坐标，不自动平移、旋转或缩放；不保证已与本场比赛精确配准。'
      : '文件未完整声明球场锚定、米制和 −Z 向上。默认不叠加广告位；仅在你确认坐标符合当前球场后再勾选，否则位置无效。';
    $('#analysis-bounds').textContent = `原始范围${result.aligned ? ' / m' : '（原始单位）'}：${['X', 'Y', 'Z'].map((axis, i) => `${axis} ${result.cloud.minimum[i].toFixed(2)} ～ ${result.cloud.maximum[i].toFixed(2)}`).join(' · ')}`;
  }
  const showCloud = Boolean(result && ready && activeTab === 'adjust');
  const previewMount = $(showCloud ? '#analysis-pitch-mount' : '#pitch-mount');
  const cameraMount = $(showCloud ? '#analysis-camera-mount' : '#camera-mount');
  const preview = $('#viewport').parentElement;
  if (preview.parentElement !== previewMount) previewMount.append(preview);
  if ($('#camera-list').parentElement !== cameraMount) cameraMount.append($('#camera-list'));
  if (ready) engine?.setAnalysis(result?.cloud, result?.aligned ?? false, result?.pointSize ?? 0.15);
  engine?.setAnalysisMode(showCloud);
}

async function selectAnalysisFile(file: File) {
  if (!current() || enginePending || engineReadyMatchId !== currentId) {
    showError(new Error('当前场次预览尚未就绪，请先完成场次配置加载。'));
    return;
  }
  cancelAnalysisImport();
  const job = { matchId: currentId, controller: new AbortController() };
  analysisImport = job;
  analysisNotice = undefined;
  clearError();
  renderAnalysis();
  try {
    const cloud = await importPlyFile(file, job.controller.signal);
    if (job.controller.signal.aborted || currentId !== job.matchId) return;
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
  selectedId = id;
  renderLists();
  renderEditor();
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
    if (hidden.has(id)) hidden.delete(id);
    else hidden.add(id);
    replaceRecord({ ...record, hiddenPlacementIds: [...hidden], updatedAt: new Date().toISOString() });
    renderRecord();
    syncEngine();
    document.querySelector<HTMLButtonElement>(`[data-visibility-id="${CSS.escape(id)}"]`)?.focus({ preventScroll: true });
  } catch (error) {
    showError(error);
  }
}

function commitSlot(slot: unknown) {
  try {
    const record = current();
    const validated = validatePlacement(slot, record.match.field);
    replaceRecord({ ...record, draft: record.draft.map((item) => item.id === validated.id ? validated : item), updatedAt: new Date().toISOString() });
    renderRecord();
    syncEngine();
  } catch (error) {
    $('#placement-error').textContent = error.message;
    showError(error);
    syncEngine();
  }
}

async function ensureEngine() {
  if (!enginePromise) {
    enginePromise = import('../scene/pitch-preview.ts').then(({ PitchPreview }) => {
      engine = new PitchPreview($('#viewport'), $('#camera-list'), (error) => {
        $('#engine-status').textContent = error.message;
        showError(error);
      });
      return engine;
    }).catch((error) => { enginePromise = undefined; throw error; });
  }
  return enginePromise;
}

function setActiveTab(tab: string) {
  activeTab = ['preview', 'scan', 'adjust'].includes(tab) ? tab : 'preview';
  for (const name of ['preview', 'scan', 'adjust']) $(`#${name}-tab`).hidden = name !== activeTab;
  document.querySelectorAll<HTMLElement>('[data-tab]').forEach((button) => {
    button.classList.toggle('active', button.dataset.tab === activeTab);
    button.setAttribute('aria-current', button.dataset.tab === activeTab ? 'step' : 'false');
  });
  const tabLabels: Record<string, string> = { preview: '广告位预览', scan: '场地扫描与分析', adjust: '广告位微调' };
  $('#page-crumb').textContent = tabLabels[activeTab];
  if (activeTab !== 'scan') $<HTMLVideoElement>('#scan-preview').pause();
  renderAnalysis();
  requestAnimationFrame(() => engine?.refresh());
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
    engine?.setAnalysisMode(false);
    resetScanPreview();
    $('#page-crumb').textContent = '比赛列表';
    renderMatches();
    if (records && parts[0] === 'match') showError(new Error('尚未添加这场比赛，请先核验比赛 ID。'));
    currentId = undefined;
    return;
  }
  if (sameMatch && !refresh) { setActiveTab(parts[2]); return; }
  const version = ++matchLoadVersion;
  clearError();
  if (!sameMatch) cancelAnalysisImport();
  currentId = record.match.id;
  const previousSelection = selectedId;
  selectedId = record.draft[0]?.id;
  enginePending = true;
  engineReadyMatchId = undefined;
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
  refreshButton.disabled = true;
  $('#configuration-status').textContent = 'Fetching the latest saved Fusion configuration...';
  $('#engine-status').textContent = 'Loading the field and assigned Fusion materials...';
  try {
    const match = await gateway.lookupMatch(record.match.id);
    if (version !== matchLoadVersion) return;
    record = { match, draft: structuredClone(match.placements),
      hiddenPlacementIds: record.hiddenPlacementIds?.filter((id) => match.placements.some((slot) => slot.id === id)) };
    replaceRecord(record);
    selectedId = sameMatch && record.draft.some((slot) => slot.id === previousSelection) ? previousSelection : record.draft[0]?.id;
    renderRecord();
    $('#configuration-status').textContent = 'Latest Fusion settings received. Loading assigned materials...';
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
      await preview.activate(record.draft, selectedId, selectSlot, commitSlot, record.match.field, record.hiddenPlacementIds);
      if (version !== matchLoadVersion) return;
      enginePending = false;
      engineReadyMatchId = currentId;
      renderLists();
      syncEngine();
      renderAnalysis();
      $('#pitch-mount').hidden = false;
      $('#pitch-mount').inert = false;
      $('#configuration-status').textContent = `Updated from Fusion at ${new Date().toLocaleTimeString()}. Refresh replaces local adjustments.`;
      $('#engine-status').textContent = 'Fusion placements and materials loaded. Local preview only; scan analysis is not connected.';
    } catch (error) {
      console.error('Unable to load match preview:', error);
      if (version === matchLoadVersion) {
        enginePending = false;
        renderLists();
        renderAnalysis();
        $('#configuration-status').textContent = 'Latest settings received, but the preview could not be loaded. Use Refresh configuration to retry.';
        $('#engine-status').textContent = 'Preview failed to load. Check the Fusion materials and connection, then reload the match.';
        showError(error);
      }
    } finally {
      if (version === matchLoadVersion) refreshButton.disabled = false;
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
  $('#scan-status').textContent = '视频仅在浏览器本地预览，不会自动上传。刷新后需重新选择文件。';
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
    $('#scan-status').textContent = '本地视频可预览，未上传 Fusion，未启动分析；刷新后需重新选择。';
  } catch (error) {
    if (error.name === 'AbortError') return;
    $('#scan-step').textContent = '视频读取失败';
    $('#scan-status').textContent = error.message;
    showError(error);
  }
}

$<HTMLFormElement>('#match-lookup-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  if (!records) return;
  clearError();
  const button = $<HTMLButtonElement>('#match-lookup-submit');
  button.disabled = true;
  $('#lookup-status').textContent = 'Requesting saved match configuration from Fusion...';
  try {
    const match = await gateway.lookupMatch($<HTMLInputElement>('#match-lookup-id').value);
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
  } catch (error) { $('#lookup-status').textContent = error.message; showError(error); }
  finally { button.disabled = false; }
});

document.addEventListener('click', (event) => {
  if (!(event.target instanceof Element)) return;
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
  if ($<HTMLFieldSetElement>('#placement-edit-controls').disabled) {
    showError(new Error('请先确认点云使用当前球场坐标，再微调广告位。'));
    return;
  }
  const slot: Record<string, unknown> = { ...selected() };
  $('#placement-fields').querySelectorAll('input').forEach((input) => { slot[input.name] = input.valueAsNumber; });
  const original = selected();
  if (original?.type === '3D' && original.region) slot.scaleY = slot.scaleZ = slot.scaleX;
  commitSlot(slot);
});
$<HTMLButtonElement>('#reset-placement').addEventListener('click', () => {
  if ($<HTMLFieldSetElement>('#placement-edit-controls').disabled) return;
  const original = current().match.placements.find((slot) => slot.id === selectedId);
  commitSlot({ ...original });
});
$<HTMLButtonElement>('#reset-business-view').addEventListener('click', () => {
  engine?.resetView();
});
$<HTMLButtonElement>('#reset-analysis-view').addEventListener('click', () => engine?.resetView());
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
  engine?.setPointSize(result.pointSize);
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
$<HTMLButtonElement>('#replace-scan').addEventListener('click', () => $<HTMLInputElement>('#scan-file').click());
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
window.addEventListener('hashchange', () => { void renderRoute(); });
window.addEventListener('beforeunload', () => { cancelAnalysisImport(); if (scanURL) URL.revokeObjectURL(scanURL); engine?.dispose(); });

try { repository = new MatchRepository(localStorage); records = repository.read(); }
catch (error) { showError(error); $<HTMLButtonElement>('#match-lookup-submit').disabled = true; }
renderMatches();
void renderRoute();
