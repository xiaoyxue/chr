import type { BundleMessage, ResultBundle } from './bundle-result.ts';

export function importResultFolder(files: File[], signal: AbortSignal): Promise<ResultBundle> {
  if (!files.length || files.length > 20000) throw new Error('请选择完整 Ti 或 MANIFEST 诊断结果文件夹（包含原视频，最多 20000 个文件）。');
  if (signal.aborted) throw new DOMException('已取消结果导入。', 'AbortError');
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL('./bundle-worker.ts', import.meta.url), { type: 'module' });
    const finish = (message?: BundleMessage, error?: Error) => {
      clearTimeout(timer);
      signal.removeEventListener('abort', aborted);
      worker.terminate();
      if (error) reject(error);
      else if (message?.ok === true) resolve(message.bundle);
      else reject(new Error(message?.ok === false ? message.error : '无法读取结果文件夹。'));
    };
    const aborted = () => finish(undefined, new DOMException('已取消结果导入。', 'AbortError'));
    const timer = setTimeout(() => finish(undefined, new Error('结果解析超时，请检查文件夹大小或重试。')), 120000);
    signal.addEventListener('abort', aborted, { once: true });
    worker.addEventListener('message', (event: MessageEvent<BundleMessage>) => finish(event.data), { once: true });
    worker.addEventListener('error', (event) => {
      const detail = event instanceof ErrorEvent && event.message ? event.message : '解析线程异常中断，请刷新页面后重试；若仍失败，请检查浏览器控制台的 Worker 错误。';
      finish(undefined, new Error(`结果解析失败：${detail}`));
    }, { once: true });
    worker.addEventListener('messageerror', () => finish(undefined, new Error('无法读取解析后的结果。')), { once: true });
    worker.postMessage(files.map((file) => ({ path: file.webkitRelativePath || file.name, file })));
  });
}
