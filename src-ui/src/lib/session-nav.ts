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
      session_token: row.session_token ?? base.session_token,
      file_path: row.file_path || base.file_path,
    };
  };

  if (terminal.resumeToken) {
    const byToken = history.find((s) => (
      s.tool === tool && s.session_token === terminal.resumeToken
    ));
    if (byToken) return claim(byToken);
  }

  const candidates = history
    .filter((s) => s.tool === tool && s.session_token && !claimed.has(`${tool}:${s.session_token}`))
    .filter((s) => !cwdKey || (!!s.cwd && normCwd(s.cwd) === cwdKey))
    .sort((a, b) => parseTime(b.created_at ?? b.saved_at) - parseTime(a.created_at ?? a.saved_at));

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
