// The studio store's JSON documents and how each is validated, shared by the
// server (scripts/studio-store.ts), which refuses invalid writes, and the page.
import { validateLibrary } from '../matches/match-domain.ts';
import { validateStoredLocalAds } from '../placements/local-ad.ts';
import { validateSession } from '../app/session.ts';
import { validateLightingLibrary } from '../lighting/lighting.ts';
import type { MatchRecord } from './types.ts';

const MB = 1024 * 1024;
/** Upload limits by file extension: materials, offline ZIPs and their point clouds (MAX_PLY_BYTES). */
export const FILE_LIMITS: Readonly<Record<string, number>> = {
  '.png': 64 * MB, '.jpg': 64 * MB, '.jpeg': 64 * MB, '.webp': 64 * MB, '.glb': 256 * MB, '.zip': 1024 * MB, '.ply': 64 * MB,
};

/** Match ids that can name a directory of the store. */
export const MATCH_ID = /^[A-Za-z0-9][A-Za-z0-9_-]{0,79}$/;
const PACKAGE_ID = /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/;

/** An absolute server path without hidden or relative segments. */
export const isServerFilePath = (path: unknown): path is string => typeof path === 'string' && path.startsWith('/') && path.length <= 4096
  && !path.split('/').some((part) => part.startsWith('.'));

export interface MatchLibrary { version: 1; records: MatchRecord[] }
export const validateMatchLibrary = (data: unknown): MatchLibrary => ({ version: 1, records: validateLibrary(data) });

/** Offline package id → the ZIP path chr-server reads its materials from. */
export function validatePackagePaths(data: unknown): Record<string, string> {
  if (!data || typeof data !== 'object' || Array.isArray(data)
    || !Object.entries(data).every(([id, path]) => PACKAGE_ID.test(id) && isServerFilePath(path))) throw new Error('离线 ZIP 路径记录无效。');
  return data as Record<string, string>;
}

/** The validator of a document name, or undefined for a name the store does not keep. */
export function documentValidator(name: string): ((data: unknown) => unknown) | undefined {
  if (name === 'matches') return validateMatchLibrary;
  if (name === 'package-paths') return validatePackagePaths;
  if (name === 'lighting') return validateLightingLibrary;
  const [root, matchId, kind, ...rest] = name.split('/');
  if (root !== 'matches' || !MATCH_ID.test(matchId ?? '') || rest.length) return undefined;
  return kind === 'local-ads' ? validateStoredLocalAds : kind === 'session' ? validateSession : undefined;
}
