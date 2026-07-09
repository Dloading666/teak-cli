// ChangesBoard.tsx — git working-tree changes for the active tab's folder.
//
// Sourced from `useGitStatus()` (which polls `git_changes` on agent-status +
// fs-refresh). Three top-level states drive the view:
//   • no_git   → "install git" prompt (feature unavailable).
//   • not_repo → "not a git repo" prompt + an "initialize here" button.
//   • ok       → files grouped into 未提交 (Uncommitted) + 未跟踪 (Untracked).
//               When the working tree is clean, the last commit (HEAD) is
//               shown as a 已提交 (Committed) group instead of an empty panel,
//               so the board isn't blank right after a commit.
//
// Layout (unchanged from the snapshot version): full-height list, click a row
// → DiffPanel mounts as a bottom overlay (~55%); ⤢ promotes it to a
// full-window modal; ⤓ back to half; × / Esc closes. Right-click a row = the
// read-only file-actions menu.

import { useEffect, useMemo, useRef, useState } from 'react';
import { useAppState, resolveDiffContext, type DiffSelection } from '../../store/app-state';
import { useGitStatus, useGitPollingGate } from '../../lib/git-status';
import { commands, type GitFileEntry } from '../../tauri';
import { useT } from '../../i18n/useT';
import { ScrollPanel } from '../common/ScrollPanel';
import { ContextMenu } from '../left/Explorer';
import type { CtxMenuState } from '../left/Explorer';
import { beginExplorerDrag } from '../../lib/explorer-drag';
import { DiffPanel } from './DiffPanel';
import './ChangesBoard.css';

interface ChangesBoardProps {
  selectedPath: string | null;
  diffMode: 'overlay' | 'tab';
}

const DIFF_HEIGHT_KEY = 'coffee:diff-half-height';
const DIFF_HEIGHT_MIN = 20;
const DIFF_HEIGHT_MAX = 90;
const DIFF_HEIGHT_DEFAULT = 55;

function loadStoredDiffHeight(): number {
  try {
    const raw = localStorage.getItem(DIFF_HEIGHT_KEY);
    if (!raw) return DIFF_HEIGHT_DEFAULT;
    const n = parseFloat(raw);
    if (!Number.isFinite(n)) return DIFF_HEIGHT_DEFAULT;
    return Math.min(DIFF_HEIGHT_MAX, Math.max(DIFF_HEIGHT_MIN, n));
  } catch {
    return DIFF_HEIGHT_DEFAULT;
  }
}

// One group of changed files. `kind` travels with the group so a selected
// row knows which diff to ask git for (uncommitted = HEAD↔worktree,
// untracked = no blob, committed = <hash>~1↔<hash>). `commitHash` scopes the
// committed diff to a specific session commit.
type Group = { tag: 'uncommitted' | 'untracked' | 'committed'; label: string; entries: GitFileEntry[]; kind: 'uncommitted' | 'untracked' | 'committed'; commitHash?: string };

// A commit's display fields (mirrors CommitMeta from tauri.ts).
type CommitRow = { hash: string; message: string; author: string; time: number };

// A flattened render item — section header, a (toggleable) commit header, or
// a file row — so one progressive loader / scroller covers all groups (a freshly
// `git init`'d repo can list thousands of untracked files).
type RenderItem =
  | { type: 'header'; key: string; label: string; count: number }
  | { type: 'commit-header'; key: string; commit: CommitRow; toggleable: boolean }
  | { type: 'file'; key: string; entry: GitFileEntry; group: Group };

// Selection is encoded as "<group-tag>\x00<abs-path>" so the same file
// appearing in both Staged and Unstaged stays two distinct, separately
// clickable rows. Parent (TaskBoard) treats the string as opaque.
const selKey = (tag: string, path: string) => `${tag}\x00${path}`;

// Relative time for a commit (epoch seconds) — Intl.RelativeTimeFormat yields
// locale-correct "5分钟前" / "3 hours ago" with zero per-locale string tables.
// Session commits cluster in minutes-to-hours, so we want finer granularity
// than HistoryBoard's just-now/today/yesterday scheme (which would label every
// commit in a burst "刚刚"). <60s reuses time.just_now; >=1 week falls back to
// a locale short date.
function formatCommitTime(epochSec: number, t: (k: any) => string, lang: string): string {
  const diffSec = Math.max(0, Math.floor((Date.now() / 1000) - epochSec));
  if (diffSec < 60) return t('time.just_now' as any) || 'Just now';
  const rtf = new Intl.RelativeTimeFormat(lang, { numeric: 'auto' });
  if (diffSec < 3600) return rtf.format(-Math.floor(diffSec / 60), 'minute');
  if (diffSec < 86400) return rtf.format(-Math.floor(diffSec / 3600), 'hour');
  if (diffSec < 604800) return rtf.format(-Math.floor(diffSec / 86400), 'day');
  return new Date(epochSec * 1000).toLocaleDateString(lang, { month: 'short', day: 'numeric' });
}

export function ChangesBoard({ selectedPath, diffMode }: ChangesBoardProps) {
  const t = useT();
  const { state, dispatch } = useAppState();
  const activeSession = state.terminals.find(s => s.id === state.activeTerminalId);
  const activeFolderPath = resolveDiffContext(activeSession)?.folderPath ?? null;
  const changes = useGitStatus();
  // Drive git polling only while this panel is on screen — ChangesBoard
  // unmounts when its tab is inactive, so this gates the expensive git query.
  useGitPollingGate();
  const [ctxMenu, setCtxMenu] = useState<CtxMenuState | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [diffHeight, setDiffHeight] = useState<number>(loadStoredDiffHeight);
  const [initializing, setInitializing] = useState(false);
  // Session-commit expand state + lazy file cache. Expanding a session commit
  // fetches its files via `gitCommitFiles` (cached), so the poll stays cheap
  // (one `git log` for metadata) and per-commit file cost is paid only on
  // expand.
  const [expandedCommits, setExpandedCommits] = useState<Set<string>>(new Set());
  const [commitFiles, setCommitFiles] = useState<Map<string, GitFileEntry[]>>(new Map());

  const startResize = (e: React.PointerEvent) => {
    if (diffMode === 'tab') return;
    const container = containerRef.current;
    if (!container) return;
    e.preventDefault();
    e.stopPropagation();
    const rect = container.getBoundingClientRect();
    const onMove = (ev: PointerEvent) => {
      const fromBottomPx = rect.bottom - ev.clientY;
      const pct = (fromBottomPx / rect.height) * 100;
      setDiffHeight(Math.min(DIFF_HEIGHT_MAX, Math.max(DIFF_HEIGHT_MIN, pct)));
    };
    const onUp = () => {
      document.removeEventListener('pointermove', onMove);
      document.removeEventListener('pointerup', onUp);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    };
    document.addEventListener('pointermove', onMove);
    document.addEventListener('pointerup', onUp);
    document.body.style.cursor = 'ns-resize';
    document.body.style.userSelect = 'none';
  };

  useEffect(() => {
    try { localStorage.setItem(DIFF_HEIGHT_KEY, String(diffHeight)); } catch {}
  }, [diffHeight]);

  const repoRoot = changes?.state === 'ok' ? changes.repo_root : null;

  // Flatten to render items: session commits made this window (if any), then
  // uncommitted / untracked below when the tree is dirty. An old project just
  // opened has no session commits and (when clean) shows the empty/clean state
  // — HEAD is NOT surfaced as a fallback (matches VSCode/GitHub Desktop: a
  // Changes view shows working-tree changes, not history).
  const items = useMemo<RenderItem[]>(() => {
    if (!changes || changes.state !== 'ok') return [];
    const out: RenderItem[] = [];

    // Session commits only — no HEAD fallback. An old repo opened this window
    // contributes nothing here until the agent makes a commit.
    const commits: CommitRow[] = changes.session_commits.map(c => ({ hash: c.hash, message: c.message, author: c.author, time: c.time }));
    if (commits.length) {
      out.push({ type: 'header', key: 'h-committed', label: t('changes.committed' as any) || 'Committed', count: commits.length });
    }
    for (const c of commits) {
      // Session commits are collapsed-by-default + lazy on expand.
      const expanded = expandedCommits.has(c.hash);
      const files = expanded ? (commitFiles.get(c.hash) ?? []) : [];
      out.push({ type: 'commit-header', key: `commit-${c.hash}`, commit: c, toggleable: true });
      for (const entry of files) {
        out.push({ type: 'file', key: selKey('committed', entry.path), entry, group: { tag: 'committed', label: '', entries: [], kind: 'committed', commitHash: c.hash } });
      }
    }

    // Uncommitted + untracked — below the commits, when the tree is dirty.
    const uncommitted = changes.uncommitted;
    const untracked = changes.untracked;
    if (uncommitted.length) {
      out.push({ type: 'header', key: 'h-uncommitted', label: t('changes.uncommitted' as any) || 'Uncommitted', count: uncommitted.length });
      for (const entry of uncommitted) {
        out.push({ type: 'file', key: selKey('uncommitted', entry.path), entry, group: { tag: 'uncommitted', label: '', entries: [], kind: 'uncommitted' } });
      }
    }
    if (untracked.length) {
      out.push({ type: 'header', key: 'h-untracked', label: t('changes.untracked' as any) || 'Untracked', count: untracked.length });
      for (const entry of untracked) {
        out.push({ type: 'file', key: selKey('untracked', entry.path), entry, group: { tag: 'untracked', label: '', entries: [], kind: 'untracked' } });
      }
    }
    return out;
  }, [changes, t, expandedCommits, commitFiles]);

  // Progressive load over the flattened list — caps DOM nodes when a fresh
  // repo lists thousands of untracked files.
  const PAGE_SIZE = 80;
  const [visibleCount, setVisibleCount] = useState(PAGE_SIZE);
  useEffect(() => { setVisibleCount(PAGE_SIZE); }, [items.length]);
  const sentinelRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const el = sentinelRef.current;
    if (!el) return;
    const observer = new IntersectionObserver(
      ([entry]) => { if (entry.isIntersecting) setVisibleCount(c => Math.min(items.length, c + PAGE_SIZE)); },
      { rootMargin: '300px' },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [items.length]);
  const visibleItems = useMemo(() => items.slice(0, visibleCount), [items, visibleCount]);

  // Resolve the selected row back to its entry + group (or drop it if it
  // vanished from the list — staged, reverted, tab switched).
  const selectedFile = useMemo(() => {
    if (!selectedPath) return null;
    const hit = items.find(it => it.type === 'file' && it.key === selectedPath);
    return hit && hit.type === 'file' ? hit : null;
  }, [items, selectedPath]);
  const effectiveSelected = selectedFile ? selectedPath : null;

  const toggleCommit = (hash: string) => {
    if (!repoRoot) return;
    // Collapse: just flip the Set.
    if (expandedCommits.has(hash)) {
      setExpandedCommits(prev => { const n = new Set(prev); n.delete(hash); return n; });
      return;
    }
    // Expand: fetch files (if not cached) BEFORE flipping the Set — keep the
    // updater pure (React.StrictMode double-invokes updaters in dev, which
    // would double-fire the IPC if it lived inside the setter).
    if (!commitFiles.has(hash)) {
      commands.gitCommitFiles(repoRoot, hash)
        .then(files => setCommitFiles(m => new Map(m).set(hash, files)))
        .catch(() => setCommitFiles(m => new Map(m).set(hash, []))); // stable empty, not pending-forever
    }
    setExpandedCommits(prev => { const n = new Set(prev); n.add(hash); return n; });
  };

  const handleInit = async () => {
    if (!activeFolderPath || initializing) return;
    setInitializing(true);
    try {
      await commands.gitInit(activeFolderPath);
      window.dispatchEvent(new CustomEvent('fs-refresh', { detail: { dirPath: activeFolderPath } }));
    } catch {}
    setInitializing(false);
  };

  // ── Prompt / empty states ────────────────────────────────────────────────
  if (!changes) {
    return <div className="task-empty"><div className="task-empty-text">{t('diff.loading' as any) || 'Loading…'}</div></div>;
  }
  if (changes.state === 'no_git') {
    return (
      <div className="task-empty">
        <div className="task-empty-text">
          {t('changes.no_git' as any) || 'Git is not installed — code diff, branches and other git features are unavailable.'}
        </div>
      </div>
    );
  }
  if (changes.state === 'not_repo') {
    return (
      <div className="task-empty">
        <div className="task-empty-text">
          {t('changes.not_repo' as any) || 'This folder is not a Git repository.'}
        </div>
        {activeFolderPath && (
          <button className="changes-init-btn" onClick={handleInit} disabled={initializing}>
            {initializing
              ? (t('changes.initializing' as any) || 'Initializing…')
              : (t('changes.init_here' as any) || 'Initialize Git here')}
          </button>
        )}
      </div>
    );
  }
  // changes.state === 'ok' from here (no_git / not_repo / null returned above).
  // Header total = sum of the listed (uncommitted) changes — coherent with the
  // file list below. Committed files (the 已提交 group, shown only when clean)
  // don't count toward pending totals. Untracked entries carry 0/0 so they
  // don't inflate it.
  let totalAdded = 0;
  let totalDeleted = 0;
  for (const e of changes.uncommitted) { totalAdded += e.added; totalDeleted += e.deleted; }
  for (const e of changes.untracked) { totalAdded += e.added; totalDeleted += e.deleted; }

  // The resize handle sits at the overlay's top edge. In tab mode the overlay
  // isn't rendered here (the diff lives in the center tab), so hide the handle.
  const handleStyle = diffMode === 'tab' ? { display: 'none' as const } : { bottom: `${diffHeight}%` };

  return (
    <div className="changes-fullview" ref={containerRef}>
      <div className="changes-branch-header">
        <span className="changes-branch" title={changes.branch}>
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <circle cx="6" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M6 9v6"/><path d="M18 6a9 9 0 0 1-9 9"/><circle cx="18" cy="6" r="3"/>
          </svg>
          <span className="changes-branch-name">{changes.branch}</span>
        </span>
        {(totalAdded > 0 || totalDeleted > 0) && (
          <span className="changes-branch-stats">
            <span className="diff-add">+{totalAdded}</span>
            <span className="diff-del">-{totalDeleted}</span>
          </span>
        )}
      </div>
      {items.length === 0 ? (
        <div className="task-empty"><div className="task-empty-text">{t('changes.clean' as any) || 'No changes — working tree clean.'}</div></div>
      ) : (
       <>
      <ScrollPanel>
        <div className="changes-list">
          {visibleItems.map(it => {
            if (it.type === 'header') {
              return (
                <div key={it.key} className="changes-group-header">
                  <span className="changes-group-label">{it.label}</span>
                  <span className="changes-group-count">{it.count}</span>
                </div>
              );
            }
            if (it.type === 'commit-header') {
              const c = it.commit;
              return (
                <div
                  key={it.key}
                  className={`changes-commit-row${it.toggleable ? ' changes-commit-toggleable' : ''}`}
                  role={it.toggleable ? 'button' : undefined}
                  tabIndex={it.toggleable ? 0 : undefined}
                  onClick={it.toggleable ? () => toggleCommit(c.hash) : undefined}
                  onKeyDown={it.toggleable ? (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggleCommit(c.hash); } } : undefined}
                >
                  <span className="changes-commit-hash">{c.hash}</span>
                  <span className="changes-commit-subject" data-tip={c.message}>{c.message}</span>
                  <span className="changes-commit-time">{formatCommitTime(c.time, t, state.currentLang || 'en')}</span>
                </div>
              );
            }
            const { entry, group } = it;
            const basename = entry.rel.split('/').pop() || entry.rel;
            const dir = entry.rel === basename ? '' : entry.rel.slice(0, -basename.length - 1);
            return (
              <div
                key={it.key}
                className={`changes-row ${effectiveSelected === it.key ? 'selected' : ''}`}
                onClick={() => {
                  // Toggle off when re-clicking the already-selected row.
                  if (effectiveSelected === it.key) {
                    dispatch({ type: 'CLEAR_DIFF' });
                    return;
                  }
                  // Snapshot the active terminal tab's folderPath alongside the
                  // clicked row's diff params — a center diff tab is not a
                  // terminal and has no own cwd to resolveDiffContext against
                  // later, so the folderPath must travel with the selection.
                  const folderPath = resolveDiffContext(activeSession)?.folderPath ?? '';
                  const selection: DiffSelection = {
                    repoRoot: repoRoot ?? '',
                    folderPath,
                    path: entry.path,
                    rel: entry.rel,
                    kind: group.kind,
                    commitHash: group.commitHash,
                    added: entry.added,
                    deleted: entry.deleted,
                  };
                  dispatch({ type: 'SET_DIFF_SELECTION', selection });
                }}
                onMouseDown={(e) => beginExplorerDrag(entry.path, e)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setCtxMenu({ x: e.clientX, y: e.clientY, absolutePath: entry.path, relativePath: entry.rel, isDir: false, compact: true });
                }}
              >
                <span className={`changes-status changes-status-${entry.status === '?' ? 'untracked' : entry.status.toLowerCase()}`}>
                  {entry.status}
                </span>
                <span className="changes-name">{basename}</span>
                <span className="changes-path">{dir}</span>
                <span className="changes-stats">
                  <span className="diff-add">+{entry.added}</span>
                  {group.kind !== "untracked" && <span className="diff-del">-{entry.deleted}</span>}
                </span>
              </div>
            );
          })}
          {visibleCount < items.length && <div ref={sentinelRef} className="changes-sentinel" aria-hidden="true" />}
        </div>
      </ScrollPanel>
      {diffMode === 'overlay' && state.diffSelection && repoRoot && (
        <>
          <div className="diff-resize-handle" style={handleStyle} onPointerDown={startResize} aria-label="Resize diff" />
          <DiffPanel
            mode="overlay"
            path={state.diffSelection.path}
            repoRoot={state.diffSelection.repoRoot}
            rel={state.diffSelection.rel}
            kind={state.diffSelection.kind}
            commitHash={state.diffSelection.commitHash}
            onClose={() => dispatch({ type: 'CLEAR_DIFF' })}
            onToggleExpanded={() => dispatch({ type: 'SET_DIFF_MODE', mode: 'tab' })}
            heightPercent={diffHeight}
            added={state.diffSelection.added}
            deleted={state.diffSelection.deleted}
          />
        </>
      )}
       </>
      )}
      {ctxMenu && <ContextMenu menu={ctxMenu} onClose={() => setCtxMenu(null)} />}
    </div>
  );
}
