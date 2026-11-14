import { MAX_PLY_BYTES } from './ply-result.ts';
import type { PlyCloud, PlyWorkerMessage } from './ply-result.ts';

export async function importPlyFile(file: File, signal: AbortSignal): Promise<PlyCloud> {
  if (!file || !/\.ply$/i.test(file.name) || file.size === 0) throw new Error('请选择非空的 .ply 点云文件。');
  if (file.size > MAX_PLY_BYTES) throw new Error('PLY 文件超过 64 MB，请先裁剪或下采样。');
  if (signal.aborted) throw new DOMException('点云导入已取消。', 'AbortError');
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL('./ply-worker.ts', import.meta.url), { type: 'module' });
    let settled = false;
    const finish = (cloud?: PlyCloud, error?: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal.removeEventListener('abort', aborted);
      worker.terminate();
      if (error) reject(error);
      else resolve(cloud);
    };
    const aborted = () => finish(undefined, new DOMException('点云导入已取消。', 'AbortError'));
    const timer = setTimeout(() => finish(undefined, new Error('点云解析超时，请裁剪或下采样后重试。')), 120000);
    signal.addEventListener('abort', aborted, { once: true });
    worker.addEventListener('message', (event: MessageEvent<PlyWorkerMessage>) => {
      if (event.data.ok === true) finish(event.data.cloud);
      else finish(undefined, new Error(event.data.error));
    });
    worker.addEventListener('error', (event) => finish(undefined, new Error(`点云解析进程失败：${event.message || '请重新选择文件。'}`)));
    worker.addEventListener('messageerror', () => finish(undefined, new Error('无法读取解析后的点云数据，请重新选择文件。')));
    worker.postMessage(file);
  });
}
