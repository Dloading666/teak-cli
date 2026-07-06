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
import type { Dispatch, SetStateAction } from 'react';
import { useAppState, resolveDiffContext } from '../../store/app-state';
import { useGitStatus, useGitPollingGate } from '../../lib/git-status';
import { commands, type GitFileEntry, type LastCommit } from '../../tauri';
import { useT } from '../../i18n/useT';
import { ScrollPanel } from '../common/ScrollPanel';
import { ContextMenu } from '../left/Explorer';
import type { CtxMenuState } from '../left/Explorer';
import { beginExplorerDrag } from '../../lib/explorer-drag';
import { DiffPanel } from './DiffPanel';
import './ChangesBoard.css';

interface ChangesBoardProps {
  selectedPath: string | null;
  setSelectedPath: Dispatch<SetStateAction<string | null>>;
  diffExpanded: boolean;
  onToggleDiffExpanded: () => void;
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
// untracked = no blob, committed = HEAD~1↔HEAD).
type Group = { tag: 'uncommitted' | 'untracked' | 'committed'; label: string; entries: GitFileEntry[]; kind: 'uncommitted' | 'untracked' | 'committed' };

// A flattened render item — section header (or a richer commit header) or
// file row — so one progressive loader / scroller covers all groups (a freshly
// `git init`'d repo can list thousands of untracked files).
type RenderItem =
  | { type: 'header'; key: string; label: string; count: number }
  | { type: 'commit-header'; key: string; commit: LastCommit }
  | { type: 'file'; key: string; entry: GitFileEntry; group: Group };

// Selection is encoded as "<group-tag>\x00<abs-path>" so the same file
// appearing in both Staged and Unstaged stays two distinct, separately
// clickable rows. Parent (TaskBoard) treats the string as opaque.
const selKey = (tag: string, path: string) => `${tag}\x00${path}`;

export function ChangesBoard({ selectedPath, setSelectedPath, diffExpanded, onToggleDiffExpanded }: ChangesBoardProps) {
  const t = useT();
  const { state } = useAppState();
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

  const startResize = (e: React.PointerEvent) => {
    if (diffExpanded) return;
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

  // Build the three groups (skip empties) → flatten to header+row items.
  const items = useMemo<RenderItem[]>(() => {
    if (!changes || changes.state !== 'ok') return [];
    const uncommitted: GitFileEntry[] = changes.uncommitted;
    const untracked: GitFileEntry[] = changes.untracked;
    const lastCommit: LastCommit | null = changes.last_commit;
    // Pending changes win — show 未提交 / 未跟踪. Only when the working tree is
    // clean do we surface the last commit as 已提交 (so the panel isn't empty
    // right after a commit — issue: "已提交就空了,修改记录就空了").
    const groups: Group[] = uncommitted.length || untracked.length
      ? [
          { tag: 'uncommitted', label: t('changes.uncommitted' as any) || 'Uncommitted', entries: uncommitted, kind: 'uncommitted' },
          { tag: 'untracked', label: t('changes.untracked' as any) || 'Untracked', entries: untracked, kind: 'untracked' },
        ]
      : lastCommit
        ? [{ tag: 'committed', label: t('changes.committed' as any) || 'Committed', entries: lastCommit.files, kind: 'committed' }]
        : [];
    const out: RenderItem[] = [];
    for (const g of groups) {
      if (g.entries.length === 0) continue;
      if (g.kind === 'committed' && lastCommit) {
        out.push({ type: 'commit-header', key: `h-${g.tag}`, commit: lastCommit });
      } else {
        out.push({ type: 'header', key: `h-${g.tag}`, label: g.label, count: g.entries.length });
      }
      for (const entry of g.entries) {
        out.push({ type: 'file', key: selKey(g.tag, entry.path), entry, group: g });
      }
    }
    return out;
  }, [changes, t]);

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

  const handleStyle = diffExpanded ? { display: 'none' as const } : { bottom: `${diffHeight}%` };

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
                <div key={it.key} className="changes-group-header changes-commit-header">
                  <span className="changes-group-label">{t('changes.committed' as any) || 'Committed'}</span>
                  <span className="changes-commit-subject" data-tip={c.message}>{c.message}</span>
                  <span className="changes-commit-hash">{c.hash}</span>
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
                onClick={() => setSelectedPath(prev => prev === it.key ? null : it.key)}
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
      {effectiveSelected && selectedFile && repoRoot && (
        <>
          <div className="diff-resize-handle" style={handleStyle} onPointerDown={startResize} aria-label="Resize diff" />
          <DiffPanel
            path={selectedFile.entry.path}
            repoRoot={repoRoot}
            rel={selectedFile.entry.rel}
            kind={selectedFile.group.kind}
            onClose={() => setSelectedPath(null)}
            expanded={diffExpanded}
            onToggleExpanded={onToggleDiffExpanded}
            heightPercent={diffHeight}
            added={selectedFile.entry.added}
            deleted={selectedFile.entry.deleted}
          />
        </>
      )}
       </>
      )}
      {ctxMenu && <ContextMenu menu={ctxMenu} onClose={() => setCtxMenu(null)} />}
    </div>
  );
}
