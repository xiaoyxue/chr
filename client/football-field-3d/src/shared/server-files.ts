// Loads files that already sit on the onsite server (see scripts/local-files.ts) as browser File
// objects, so every import path behaves exactly like a local selection.
const MAX_FOLDER_BYTES = 1024 * 1024 * 1024;

export const isServerPath = (value: string) => value.trim().startsWith('/');

async function request(endpoint: string, path: string, signal?: AbortSignal) {
  const response = await fetch(`/local-files/${endpoint}?path=${encodeURIComponent(path.trim())}`, { cache: 'no-store', signal });
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
  return download(path, path.trim().split('/').filter(Boolean).pop(), signal);
}

/** Entries carry the same folder-relative paths as a local folder selection (`folder/sub/file`). */
export async function fetchServerFolder(path: string, signal?: AbortSignal): Promise<{ path: string; file: File }[]> {
  if (!isServerPath(path)) throw new Error('Enter an absolute folder path on the onsite server, such as /home/user/result.');
  const { files } = await (await request('folder', path, signal)).json() as { files: { path: string; size: number }[] };
  if (files.reduce((total, entry) => total + entry.size, 0) > MAX_FOLDER_BYTES)
    throw new Error('The server folder is larger than 1 GB. Enter the result package folder itself.');
  const root = path.trim().replace(/\/+$/, '');
  const entries: { path: string; file: File }[] = [];
  for (const entry of files) {
    const inside = entry.path.slice(entry.path.indexOf('/') + 1);
    entries.push({ path: entry.path, file: await download(`${root}/${inside}`, inside.split('/').pop(), signal) });
  }
  return entries;
}
