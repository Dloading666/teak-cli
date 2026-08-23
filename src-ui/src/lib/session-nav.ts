// Project → session navigation helpers.
//
// A left-rail "agent" is one session (Claude / Grok / Codex / …), not a
// tool-type folder. Sessions group by project cwd. Live PTY tabs overlay
// matching history rows via resumeToken so clicking a session focuses the
// already-open terminal instead of spawning a duplicate tab.

import type { AgentStatus, TerminalSession } from '../store/app-state';
import type { SavedSession } from '../tauri';

export function normCwd(cwd: string): string {
  return cwd.replace(/[\\/]+$/, '').replace(/\//g, '\\');
}

export function pathBasename(p: string): string {
  const trimmed = p.replace(/[\\/]+$/, '');
  const idx = Math.max(trimmed.lastIndexOf('\\'), trimmed.lastIndexOf('/'));
  return idx >= 0 ? trimmed.slice(idx + 1) : trimmed;
}

export function findLiveForSaved(
  terminals: TerminalSession[],
  saved: SavedSession,
): TerminalSession | undefined {
  const token = saved.session_token;
  if (!token) return undefined;
  return terminals.find((t) => (
    !t.isHidden
    && t.tool === saved.tool
    && t.resumeToken === token
  ));
}

export function isCoveredByHistory(
  terminal: TerminalSession,
  saved: SavedSession[],
): boolean {
  const token = terminal.resumeToken;
  if (!token) return false;
  return saved.some((s) => s.tool === terminal.tool && s.session_token === token);
}

export function isGenericSessionName(name: string, fallbackName: string): boolean {
  const a = name.trim().toLowerCase();
  const b = fallbackName.trim().toLowerCase();
  if (!a) return true;
  if (a === b) return true;
  if (a === `${b} session`) return true;
  return false;
}

function pickSessionLabel(historyName: string | undefined, oscName: string, fallbackName: string): string {
  const history = historyName?.trim() ?? '';
  const osc = oscName.trim();
  // The live CLI's OSC / generated title is the name the agent is advertising
  // right now. Archival first-prompt names lag and can belong to a pre-fork
  // session if the tab token is still pointing at the old conversation.
  if (osc && !isGenericSessionName(osc, fallbackName)) return osc;
  if (history && !isGenericSessionName(history, fallbackName)) return history;
  return osc || history || fallbackName;
}

export function liveAsSaved(terminal: TerminalSession, fallbackName: string): SavedSession {
  const cwd = terminal.folderPath ?? '';
  return {
    id: terminal.id,
    name: terminal.toolTitle?.trim() || fallbackName,
    tool: terminal.tool ?? '',
    cwd,
    session_token: terminal.resumeToken ?? null,
    saved_at: new Date(terminal.startedAt ?? Date.now()).toISOString(),
    file_path: cwd || undefined,
  };
}

function parseTime(value: string | undefined): number {
  if (!value) return 0;
  const ms = Date.parse(value);
  if (!Number.isNaN(ms)) return ms;
  const n = Number(value);
  if (!Number.isNaN(n) && n > 0) return n < 1e11 ? n * 1000 : n;
  return 0;
}

/** Bind a live terminal to its native history row so copy-session-id /
 *  copy-full-path can use the CLI's token and on-disk session path. */
export function attachHistoryToLive(
  terminal: TerminalSession,
  history: SavedSession[],
  claimed: Set<string>,
  fallbackName: string,
): SavedSession {
  const base = liveAsSaved(terminal, fallbackName);
  const tool = terminal.tool ?? '';
  const cwdKey = terminal.folderPath ? normCwd(terminal.folderPath) : '';

  const claim = (row: SavedSession | undefined): SavedSession => {
    if (!row) return base;
    const token = row.session_token ?? '';
    if (token) claimed.add(`${tool}:${token}`);
    return {
      ...base,
      name: pickSessionLabel(row.name, base.name, fallbackName),
      session_token: row.session_token ?? base.session_token,
      file_path: row.file_path || base.file_path,
      saved_at: row.saved_at || base.saved_at,
      created_at: row.created_at ?? base.created_at,
    };
  };

  const candidates = history
    .filter((s) => s.tool === tool && s.session_token && !claimed.has(`${tool}:${s.session_token}`))
    .filter((s) => !cwdKey || (!!s.cwd && normCwd(s.cwd) === cwdKey))
    .sort((a, b) => parseTime(b.created_at ?? b.saved_at) - parseTime(a.created_at ?? a.saved_at));

  const byToken = terminal.resumeToken
    ? candidates.find((s) => s.session_token === terminal.resumeToken)
    : undefined;

  const osc = (terminal.toolTitle ?? '').trim();
  const byTitle = osc && !isGenericSessionName(osc, fallbackName)
    ? candidates.find((s) => (s.name ?? '').trim() === osc)
    : undefined;

  // OSC/ai-title identifies the running conversation. A stored resume token
  // can still point at the pre-fork greeting session; prefer the live title
  // when the two disagree.
  if (byTitle && byToken && byTitle.session_token !== byToken.session_token) {
    return claim(byTitle);
  }
  if (byTitle && !byToken) return claim(byTitle);
  if (byToken) return claim(byToken);

  // A live token that history has not indexed yet must not steal another
  // conversation in the same workspace.
  if (terminal.resumeToken) return base;

  const started = terminal.startedAt ?? 0;
  const fresh = started
    ? candidates.find((s) => parseTime(s.created_at ?? s.saved_at) >= started - 120_000)
    : undefined;
  return claim(fresh ?? candidates[0]);
}

export function liveStatus(terminal: TerminalSession | undefined): AgentStatus | null {
  if (!terminal) return null;
  return terminal.agentStatus ?? 'idle';
}

/** Apply a saved id list to live rows. Missing ids are dropped; rows not in
 *  the list (new chats) sort to the top by recency so they stay discoverable. */
export function applySessionOrder<T>(
  rows: T[],
  order: string[] | undefined,
  idOf: (row: T) => string,
  recencyOf: (row: T) => number,
): T[] {
  if (!order || order.length === 0) {
    return rows.slice().sort((a, b) => recencyOf(b) - recencyOf(a));
  }
  const byId = new Map<string, T>();
  for (const row of rows) {
    const id = idOf(row);
    if (!byId.has(id)) byId.set(id, row);
  }
  const out: T[] = [];
  const seen = new Set<string>();
  for (const id of order) {
    const row = byId.get(id);
    if (!row || seen.has(id)) continue;
    out.push(row);
    seen.add(id);
  }
  const newcomers = rows
    .filter((row) => !seen.has(idOf(row)))
    .sort((a, b) => recencyOf(b) - recencyOf(a));
  return [...newcomers, ...out];
}

/** Move `id` so it sits immediately before `beforeId`. `beforeId === null`
 *  means append. `id` is inserted even if it wasn't already in `order`. */
export function moveInOrder(order: string[], id: string, beforeId: string | null): string[] {
  const without = order.filter((item) => item !== id);
  if (beforeId == null) return [...without, id];
  const insertIdx = without.indexOf(beforeId);
  if (insertIdx < 0) return [...without, id];
  return [...without.slice(0, insertIdx), id, ...without.slice(insertIdx)];
}
