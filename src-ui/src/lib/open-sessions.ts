// Snapshot of the left-rail live sessions.
//
// The rail is PTY tabs in memory — a restart or in-app update used to wipe
// it. We write the same JSON to teak-open-sessions (WebView localStorage)
// and ~/.teak-cli/open-sessions.json (survives a WebView profile reset).
// On boot, localStorage hydrates the first paint; the disk file is the
// fallback when that store is empty.

import { commands, isTauri } from '../tauri';
import { prefGet, prefSet } from './prefs';
import { getNavOrderSnapshot, hydrateNavOrder } from './session-nav-order';

const PREF_KEY = 'open-sessions';

const PERSISTABLE_TOOLS = new Set([
  'claude', 'qwen', 'hermes', 'opencode', 'mimocode', 'kilo', 'openclaw',
  'codex', 'grok', 'antigravity', 'pi', 'crush', 'aider', 'kimicode',
  'goose', 'copilot', 'cursor', 'cline', 'omp', 'terminal',
]);

export interface OpenSessionSnap {
  id: string;
  tool: string;
  folderPath: string | null;
  resumeToken?: string;
  toolTitle?: string;
  startedAt?: number;
  viewMode?: 'terminal' | 'chat';
}

export interface OpenSessionsFile {
  version: 1;
  activeId: string | null;
  sessions: OpenSessionSnap[];
  navOrder?: Record<string, string[]>;
  savedAt: number;
}

interface PersistableTerminal {
  id: string;
  tool: string | null;
  folderPath: string | null;
  resumeToken?: string;
  toolTitle?: string;
  startedAt?: number;
  isHidden?: boolean;
  viewMode?: 'terminal' | 'chat';
}

export function isPersistableTool(tool: string | null | undefined): boolean {
  return !!tool && PERSISTABLE_TOOLS.has(tool);
}

export function parseOpenSessions(raw: string | null | undefined): OpenSessionsFile | null {
  if (!raw) return null;
  try {
    const obj = JSON.parse(raw) as Partial<OpenSessionsFile>;
    if (!obj || obj.version !== 1 || !Array.isArray(obj.sessions)) return null;
    const sessions: OpenSessionSnap[] = [];
    for (const row of obj.sessions) {
      if (!row || typeof row !== 'object') continue;
      if (typeof row.id !== 'string' || !row.id) continue;
      if (!isPersistableTool(row.tool)) continue;
      const resumeToken = typeof row.resumeToken === 'string' && row.resumeToken
        ? row.resumeToken
        : undefined;
      sessions.push({
        id: row.id,
        tool: row.tool,
        folderPath: typeof row.folderPath === 'string' ? row.folderPath : null,
        resumeToken,
        toolTitle: typeof row.toolTitle === 'string' && row.toolTitle.trim()
          ? row.toolTitle
          : undefined,
        startedAt: typeof row.startedAt === 'number' ? row.startedAt : undefined,
        viewMode: row.viewMode === 'chat' || row.viewMode === 'terminal'
          ? row.viewMode
          : undefined,
      });
    }
    if (sessions.length === 0) return null;
    const ids = new Set(sessions.map((s) => s.id));
    const activeId = typeof obj.activeId === 'string' && ids.has(obj.activeId)
      ? obj.activeId
      : sessions[0].id;
    const navOrder: Record<string, string[]> = {};
    if (obj.navOrder && typeof obj.navOrder === 'object' && !Array.isArray(obj.navOrder)) {
      for (const [key, value] of Object.entries(obj.navOrder)) {
        if (!Array.isArray(value)) continue;
        const list = value.filter((id): id is string => typeof id === 'string' && ids.has(id));
        if (list.length > 0) navOrder[key] = list;
      }
    }
    return {
      version: 1,
      activeId,
      sessions,
      navOrder: Object.keys(navOrder).length > 0 ? navOrder : undefined,
      savedAt: typeof obj.savedAt === 'number' ? obj.savedAt : 0,
    };
  } catch {
    return null;
  }
}

export function loadOpenSessionsPref(): OpenSessionsFile | null {
  return parseOpenSessions(prefGet(PREF_KEY));
}

export function applyNavOrderFromSnapshot(snap: OpenSessionsFile | null): void {
  if (snap?.navOrder) hydrateNavOrder(snap.navOrder);
}

function buildSnapshot(
  terminals: PersistableTerminal[],
  activeId: string | null,
): OpenSessionsFile | null {
  const sessions: OpenSessionSnap[] = [];
  for (const term of terminals) {
    if (term.isHidden) continue;
    if (!isPersistableTool(term.tool)) continue;
    sessions.push({
      id: term.id,
      tool: term.tool as string,
      folderPath: term.folderPath,
      resumeToken: term.resumeToken,
      toolTitle: term.toolTitle,
      startedAt: term.startedAt,
      viewMode: term.viewMode,
    });
  }
  if (sessions.length === 0) return null;
  const ids = new Set(sessions.map((s) => s.id));
  const navRaw = getNavOrderSnapshot();
  const navOrder: Record<string, string[]> = {};
  for (const [key, value] of Object.entries(navRaw)) {
    const list = value.filter((id) => ids.has(id));
    if (list.length > 0) navOrder[key] = list;
  }
  return {
    version: 1,
    activeId: activeId && ids.has(activeId) ? activeId : sessions[0].id,
    sessions,
    navOrder: Object.keys(navOrder).length > 0 ? navOrder : undefined,
    savedAt: Date.now(),
  };
}

let lastWritten = '';

export function saveOpenSessionsNow(
  terminals: PersistableTerminal[],
  activeId: string | null,
): void {
  const snap = buildSnapshot(terminals, activeId);
  const json = snap ? JSON.stringify(snap) : '';
  if (json === lastWritten) return;
  lastWritten = json;
  try {
    prefSet(PREF_KEY, json || '');
  } catch { /* quota */ }
  if (isTauri) {
    commands.saveOpenSessions(json || '{}').catch(() => {});
  }
}

export async function loadOpenSessionsFromDisk(): Promise<OpenSessionsFile | null> {
  if (!isTauri) return loadOpenSessionsPref();
  try {
    const raw = await commands.loadOpenSessions();
    const fromDisk = parseOpenSessions(raw);
    if (fromDisk) return fromDisk;
  } catch { /* disk missing / IPC down */ }
  return loadOpenSessionsPref();
}
