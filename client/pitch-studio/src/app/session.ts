// What the studio remembers of an operator's session per match, so that a page
// reload (an SSH tunnel drop, a discarded tab) restores it: the files imported by
// server path (re-read from the onsite server), the names of files chosen locally
// (which cannot be re-read), each point cloud's alignment confirmation and the
// selected placements. It is stored on the server per match, like the placements.
import { StoredDocument } from '../shared/studio-store.ts';

/** Where earlier versions kept every match's session in the browser. */
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

/** One stored session, validated strictly before it is written. */
export function validateSession(value: unknown): MatchSession {
  const parsed = parseSessions(JSON.stringify({ match: value })).match;
  if (!parsed || JSON.stringify(parsed) !== JSON.stringify(value)) throw new Error('会话记录无效。');
  return parsed;
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

/**
 * The sessions of the matches this page opened, kept on the server. Reads come
 * from the loaded copy; a change is applied at once and saved in the background,
 * re-applied on the latest version if another page saved first.
 */
export class SessionStore {
  private documents = new Map<string, StoredDocument<MatchSession>>();
  constructor(private onError: (error: unknown) => void) {}
  private document(matchId: string) {
    let document = this.documents.get(matchId);
    if (!document) this.documents.set(matchId, document = new StoredDocument(`matches/${encodeURIComponent(matchId)}/session`, validateSession));
    return document;
  }
  load(matchId: string) { return this.document(matchId).load(); }
  get(matchId: string): MatchSession { return this.documents.get(matchId)?.data ?? emptySession(); }
  update(matchId: string, change: (session: MatchSession) => MatchSession) {
    const document = this.document(matchId), current = this.get(matchId), next = change(current);
    if (JSON.stringify(next) === JSON.stringify(current)) return;
    // Every change sets values, so applying it again to the saved copy is harmless.
    document.data = next;
    void document.update((session) => change(session ?? emptySession())).catch(this.onError);
  }
  forget(matchId: string) { this.documents.delete(matchId); }
}
