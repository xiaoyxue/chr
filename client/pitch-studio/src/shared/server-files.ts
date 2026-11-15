// Loads files that already sit on the onsite server (see scripts/local-files.ts) as browser File
// objects, so every import path behaves exactly like a local selection.
const MAX_FOLDER_BYTES = 1024 * 1024 * 1024;

/** A path as typed or pasted, without surrounding whitespace or the quotes "Copy as path" adds. */
export const serverPath = (value: string) => value.replace(/^[\s"']+|[\s"']+$/g, '');
export const isServerPath = (value: string) => serverPath(value).startsWith('/');

/** Where an export goes: a `.json` path as typed, else `name` inside the typed folder (or `folder` when nothing is typed). */
export function exportTarget(typed: string, folder: string, name: string) {
  const path = serverPath(typed) || folder;
  if (!path.startsWith('/')) throw new Error('导出路径须为现场服务器上的绝对路径，如 /home/…/exports/。');
  return /\.json$/i.test(path) ? path : `${path.replace(/\/+$/, '')}/${name}`;
}

async function request(endpoint: string, path: string, signal?: AbortSignal) {
  const response = await fetch(`/local-files/${endpoint}?path=${encodeURIComponent(serverPath(path))}`, { cache: 'no-store', signal });
  if (!response.ok) {
    const body: unknown = await response.json().catch((): null => null);
    const error = body && typeof body === 'object' && 'error' in body ? String(body.error) : '';
    throw new Error(error || `Server path request failed (${response.status}).`);
  }
  return response;
}

async function download(path: string, name: string, signal?: AbortSignal) {
  const blob = await (await request('file', path, signal)).blob();
  return new File([blob], name, { type: blob.type });
}

export async function fetchServerFile(path: string, signal?: AbortSignal): Promise<File> {
  if (!isServerPath(path)) throw new Error('Enter an absolute path on the onsite server, such as /home/user/match.zip.');
  return download(path, serverPath(path).split('/').filter(Boolean).pop(), signal);
}

/** Entries carry the same folder-relative paths as a local folder selection (`folder/sub/file`). */
export async function fetchServerFolder(path: string, signal?: AbortSignal): Promise<{ path: string; file: File }[]> {
  if (!isServerPath(path)) throw new Error('Enter an absolute folder path on the onsite server, such as /home/user/result.');
  const { files } = await (await request('folder', path, signal)).json() as { files: { path: string; size: number }[] };
  if (files.reduce((total, entry) => total + entry.size, 0) > MAX_FOLDER_BYTES)
    throw new Error('The server folder is larger than 1 GB. Enter the result package folder itself.');
  const root = serverPath(path).replace(/\/+$/, '');
  const entries: { path: string; file: File }[] = [];
  for (const entry of files) {
    const inside = entry.path.slice(entry.path.indexOf('/') + 1);
    entries.push({ path: entry.path, file: await download(`${root}/${inside}`, inside.split('/').pop(), signal) });
  }
  return entries;
}
