// git-status.tsx — Per-tab git working-tree changes provider.
//
// Replaces the old session-snapshot file-stats provider. `git_changes` is
// stateless — no baseline to walk, no teardown, no "away" re-baseline — so
// this is just: poll the active tab's git status on the same triggers the
// snapshot poller used (folder/session change, agent-status, fs-refresh),
// debounced into one call per burst.
//
// Two consumers, one provider:
//   • useGitStatus() → the active tab's GitChanges (no_git / not_repo / ok).
//     ChangesBoard branches on this to render the prompts or the
//     staged·unstaged·untracked groups.
//   • useFileStats() → a flat Map<path,{added,deleted}> derived from the same
//     data, kept for the Explorer file tree's +/- badges (drop-in for the old
//     provider so the tree code is unchanged).

import { createContext, useContext, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { useAppState, resolveDiffContext } from '../store/app-state';
import type { ToolType } from '../store/app-state';
import { commands, type GitChanges, type GitFileEntry } from '../tauri';
import { subscribeAgentStatus } from './agent-status-bus';

// ── Contexts ────────────────────────────────────────────────────────────────
export type FileStats = { added: number; deleted: number; mtimeMs: number };
type FileStatsMap = Map<string, FileStats>;

const GitStatusContext = createContext<GitChanges | null>(null);
const FileStatsContext = createContext<FileStatsMap | null>(null);

/** Active tab's git working-tree status (no_git / not_repo / ok). */
export const useGitStatus = () => useContext(GitStatusContext);
/** Flat path→{added,deleted} map for the Explorer tree badges (git-derived). */
export const useFileStats = () => useContext(FileStatsContext);

// CWD-agnostic tabs have no local workspace to inspect — `openclaw` / `hermes`
// / `remote` have no local CWD; `history` / `installer` carry a folderPath but
// the user is browsing/installing, not editing a project we should track.
const CWD_AGNOSTIC_TOOLS: ReadonlySet<ToolType> = new Set<ToolType>([
  'openclaw', 'hermes', 'remote', 'history', 'installer',
]);

// 300 ms swallows an editor-save / agent-turn event burst into one git call.
const REFRESH_DEBOUNCE_MS = 300;

// Flatten the three change groups into the Explorer's path→counts map. A path
// present in both staged AND unstaged sums its deltas so the tree badge shows
// total movement. mtime isn't a git concept → 0 (the tree doesn't read it).
function deriveFileStatsMap(changes: GitChanges | null): FileStatsMap {
  const m: FileStatsMap = new Map();
  if (!changes || changes.state !== 'ok') return m;
  const add = (e: GitFileEntry) => {
    const prev = m.get(e.path);
    if (prev) {
      m.set(e.path, { added: prev.added + e.added, deleted: prev.deleted + e.deleted, mtimeMs: 0 });
    } else {
      m.set(e.path, { added: e.added, deleted: e.deleted, mtimeMs: 0 });
    }
  };
  changes.staged.forEach(add);
  changes.unstaged.forEach(add);
  changes.untracked.forEach(add);
  return m;
}

export function GitStatusProvider({ children }: { children: ReactNode }) {
  const { state } = useAppState();
  const activeSession = state.terminals.find(t => t.id === state.activeTerminalId);
  const diffCtx = resolveDiffContext(activeSession);
  const activeFolderPath = diffCtx?.folderPath ?? null;
  const activeSessionId = diffCtx?.sessionId ?? null;
  const activeTool = diffCtx?.tool ?? null;
  const cwdAgnostic = !!(activeTool && CWD_AGNOSTIC_TOOLS.has(activeTool));

  // Keyed by sessionId so flipping back to a previous tab shows its
  // last-known status instantly; the next tick reconciles.
  const [tabChanges, setTabChanges] = useState<Map<string, GitChanges>>(new Map());

  const debounceRef = useRef<number | null>(null);
  useEffect(() => {
    if (!activeFolderPath || !activeSessionId || !activeTool || cwdAgnostic) return;
    const folder = activeFolderPath;
    const sid = activeSessionId;

    const fetchChanges = () => {
      commands.gitChanges(folder).then(changes => {
        setTabChanges(prev => {
          const next = new Map(prev);
          next.set(sid, changes);
          return next;
        });
      }).catch(() => {});
    };
    const schedule = () => {
      if (debounceRef.current != null) window.clearTimeout(debounceRef.current);
      debounceRef.current = window.setTimeout(fetchChanges, REFRESH_DEBOUNCE_MS);
    };
    schedule(); // initial fetch on (folder, session) change

    // Rust fs-watcher signals OS-level changes.
    let unlistenTauri: (() => void) | null = null;
    let cancelled = false;
    (async () => {
      const { listen } = await import('@tauri-apps/api/event');
      const fn = await listen('fs-refresh', schedule);
      if (cancelled) fn();
      else unlistenTauri = fn;
    })().catch(() => {});

    // DOM fs-refresh — Explorer's local ops + our own git-init dispatch this.
    const onWindowRefresh = () => schedule();
    window.addEventListener('fs-refresh', onWindowRefresh);

    // agent-status — any AI tool state change hints something changed on disk.
    const unsubStatus = subscribeAgentStatus(schedule);

    return () => {
      cancelled = true;
      window.removeEventListener('fs-refresh', onWindowRefresh);
      unlistenTauri?.();
      unsubStatus();
      if (debounceRef.current != null) {
        window.clearTimeout(debounceRef.current);
        debounceRef.current = null;
      }
    };
  }, [activeFolderPath, activeSessionId, activeTool, cwdAgnostic]);

  // Drop entries for sessions no longer alive so the Map can't grow unbounded.
  useEffect(() => {
    const live = new Set(
      state.terminals
        .map(t => resolveDiffContext(t)?.sessionId)
        .filter((s): s is string => !!s),
    );
    setTabChanges(prev => {
      let changed = false;
      const next = new Map(prev);
      for (const sid of Array.from(next.keys())) {
        if (!live.has(sid)) { next.delete(sid); changed = true; }
      }
      return changed ? next : prev;
    });
  }, [state.terminals]);

  const activeChanges: GitChanges | null = activeSessionId
    ? tabChanges.get(activeSessionId) ?? null
    : null;
  const fileStats = useMemo(() => deriveFileStatsMap(activeChanges), [activeChanges]);

  return (
    <GitStatusContext.Provider value={activeChanges}>
      <FileStatsContext.Provider value={fileStats}>
        {children}
      </FileStatsContext.Provider>
    </GitStatusContext.Provider>
  );
}
