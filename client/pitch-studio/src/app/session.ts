// What the studio remembers of an operator's session per match, so that a page
// reload (an SSH tunnel drop, a discarded tab) restores it: the files imported by
// server path (re-read from the onsite server), the names of files chosen locally
// (which cannot be re-read), each point cloud's alignment confirmation and the
// selected placements. Local placements and offline packages have their own stores.
import type { StorageAdapter } from '../shared/types.ts';

export const SESSION_KEY = 'liveshow.onsite.session.v1';
export const SOURCE_KINDS = ['cloud', 'result', 'video'] as const;
export type SourceKind = typeof SOURCE_KINDS[number];
export const SOURCE_LABEL: Record<SourceKind, string> = { cloud: 'PLY 点云', result: '结果文件夹', video: '参考视频' };
/** A server path can be read again after a reload; a locally chosen file only has its name. */
export interface SessionSource { path?: string; name: string }
export interface MatchSession {
  sources: Partial<Record<SourceKind, SessionSource>>;
  /** Point-cloud id (SHA-256 of the PLY) → whether it is confirmed to be in pitch coordinates. */
  alignment: Record<string, boolean>;
  selection: { fusion?: string; pitch?: string; cloud?: string };
}
const MAX_ALIGNMENTS = 20;

export const emptySession = (): MatchSession => ({ sources: {}, alignment: {}, selection: {} });

const isText = (value: unknown, max = 1000): value is string => typeof value === 'string' && value.length > 0 && value.length <= max;

function parseSource(value: unknown): SessionSource | undefined {
  if (!value || typeof value !== 'object') return undefined;
  const { path, name } = value as SessionSource;
  if (!isText(name) || (path !== undefined && !(isText(path, 4096) && path.startsWith('/')))) return undefined;
  return path === undefined ? { name } : { path, name };
}

/** Sessions read back from storage; anything malformed is dropped rather than trusted. */
export function parseSessions(text: string | null): Record<string, MatchSession> {
  let raw: unknown;
  try { raw = JSON.parse(text ?? '{}'); } catch { return {}; }
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return {};
  const sessions: Record<string, MatchSession> = {};
  for (const [matchId, value] of Object.entries(raw)) {
    if (!isText(matchId) || !value || typeof value !== 'object') continue;
    const { sources, alignment, selection } = value as Partial<MatchSession>;
    const session = emptySession();
    for (const kind of SOURCE_KINDS) {
      const source = parseSource(sources?.[kind]);
      if (source) session.sources[kind] = source;
    }
    for (const [cloud, aligned] of Object.entries(alignment ?? {})) {
      if (/^[0-9a-f]{64}$/.test(cloud) && typeof aligned === 'boolean') session.alignment[cloud] = aligned;
    }
    for (const key of ['fusion', 'pitch', 'cloud'] as const) {
      if (isText(selection?.[key])) session.selection[key] = selection[key];
    }
    sessions[matchId] = session;
  }
  return sessions;
}

/** Whether a newly loaded cloud is in pitch coordinates: the operator's last answer, else what the file declares. */
export const cloudAligned = (session: MatchSession, cloudId: string, declared: boolean) => session.alignment[cloudId] ?? declared;

/** Records the operator's answer for one cloud, keeping the newest few. */
export function withAlignment(session: MatchSession, cloudId: string, aligned: boolean): MatchSession {
  const entries = Object.entries(session.alignment).filter(([id]) => id !== cloudId);
  return { ...session, alignment: Object.fromEntries([...entries, [cloudId, aligned]].slice(-MAX_ALIGNMENTS)) };
}

/**
 * What to do after a reload: re-read what came from server paths (the point cloud
 * first, which the placements need), and name the locally chosen files to select again.
 * `loaded` lists what this page still holds, which needs nothing.
 */
export function restorePlan(session: MatchSession, loaded: ReadonlySet<SourceKind> = new Set()) {
  const load: { kind: SourceKind; path: string }[] = [], reselect: { kind: SourceKind; name: string }[] = [];
  for (const kind of SOURCE_KINDS) {
    const source = session.sources[kind];
    if (!source || loaded.has(kind)) continue;
    if (source.path) load.push({ kind, path: source.path });
    else reselect.push({ kind, name: source.name });
  }
  return { load, reselect };
}

export class SessionStore {
  private sessions: Record<string, MatchSession>;
  constructor(private storage: StorageAdapter) { this.sessions = parseSessions(storage.getItem(SESSION_KEY)); }
  get(matchId: string): MatchSession { return this.sessions[matchId] ?? emptySession(); }
  update(matchId: string, change: (session: MatchSession) => MatchSession) {
    const next = change(this.get(matchId));
    if (JSON.stringify(next) === JSON.stringify(this.sessions[matchId])) return;
    this.sessions = { ...this.sessions, [matchId]: next };
    this.save();
  }
  remove(matchId: string) {
    if (!(matchId in this.sessions)) return;
    this.sessions = { ...this.sessions };
    delete this.sessions[matchId];
    this.save();
  }
  private save() {
    // Losing the session (storage full or blocked) only means re-importing by hand after a reload.
    try { this.storage.setItem(SESSION_KEY, JSON.stringify(this.sessions)); }
    catch (error) { console.warn('Unable to remember the studio session.', error); }
  }
}
