// DiffPanel.tsx — unified diff view for the right-side Changes tab.
// Shows baseline (session-start) vs. current content for the file the user
// clicked in ChangesBoard. Read-only audit view: no edit, no save.
//
// Step 4 wired in: i18n placeholders + Shiki syntax highlighting. The
// highlighter loads asynchronously (and the file's language grammar loads
// the first time we touch that extension) — diff text renders plain on
// first paint and re-renders with token colors once tokenization resolves.
// Theme tracks `data-theme` via MutationObserver so theme switches re-tint
// the tokens without a remount.

import { useEffect, useMemo, useRef, useState } from 'react';
import type { CSSProperties } from 'react';
import { createPortal } from 'react-dom';
import { diffLines } from 'diff';
import { commands } from '../../tauri';
import { useT } from '../../i18n/useT';
import { useDataAttr } from '../../lib/use-data-attr';
import { tokenizeFile, getShikiTheme, type LineTokens } from '../../lib/shiki';
import './DiffPanel.css';

type DiffLine = {
  kind: 'add' | 'del' | 'eq';
  text: string;
  /** Line number in the file this row belongs to:
   *  - 'add' → new-file line number
   *  - 'del' → old-file line number
   *  - 'eq'  → either (we show new-file's). */
  lineNum: number;
  /** Pre-tokenized syntax-highlighted spans. Null until Shiki resolves;
   *  null also when the file's language isn't in LANG_MAP (plain text). */
  tokens: LineTokens | null;
};

/** A rendered row in the collapsed (hunk) view: either a single diff line,
 *  or a "gap" standing in for a run of unchanged lines folded away. The gap
 *  carries its hidden lines so expanding it is a pure render-time toggle —
 *  no recompute, no re-tokenize. */
type DiffRow =
  | { type: 'line'; line: DiffLine }
  | { type: 'gap'; key: string; lines: DiffLine[] };

type DiffResult =
  | { state: 'loading' }
  | { state: 'error'; reason: string }
  | { state: 'too_large'; added: number; deleted: number }
  | { state: 'ok'; rows: DiffRow[]; added: number; deleted: number };

// Render guards. Past these a per-line diff is both unhelpful and a
// main-thread hazard: computeUnifiedDiff allocates one object per line and
// Shiki tokenizes BOTH full texts, so a multi-MB file — a lockfile, a
// minified bundle, or any file whose baseline content was never stored
// (oldText '' → the whole file renders as additions, nothing to fold) —
// would freeze the UI the instant it opens. Above either threshold we show
// a summary card instead.
//
// DIFF_MAX_BYTES is checked against the file's REAL on-disk byte size,
// fetched up front via commands.getDiffMeta (see the load effect), so an
// oversized file is rejected BEFORE its contents are marshalled across IPC.
// Using real UTF-8 bytes — not the decoded String.length, whose UTF-16 units
// under-count multibyte CJK — is what keeps large CJK files from slipping
// through. DIFF_MAX_CHANGED_LINES catches huge rewrites from the Rust
// folder-stats badge, before any IPC at all.
const DIFF_MAX_BYTES = 1_000_000;
const DIFF_MAX_CHANGED_LINES = 5000;

// Unchanged-line folding (hunk view). Runs of equal lines far from any
// change collapse to one clickable gap, so a 2-line edit in a 3000-line
// file renders ~10 rows instead of 3000 (DOM nodes are the real cost once
// the size guards above let a diff through). CONTEXT lines are kept on each
// side of every change for orientation; a run is folded only when it would
// hide at least MIN_HIDDEN lines — folding 1-2 lines just swaps rows for a
// marker of equal height and saves nothing.
const DIFF_CONTEXT_LINES = 3;
const DIFF_COLLAPSE_MIN_HIDDEN = 4;

interface DiffPanelProps {
  path: string;
  onClose: () => void;
  expanded: boolean;
  onToggleExpanded: () => void;
  /** Height percent (0-100) for the half-paper bottom overlay.
   *  Ignored in expanded mode (which uses fixed-inset modal sizing).
   *  When omitted, the CSS default (55%) applies. */
  heightPercent?: number;
  /** Baseline→current change magnitude from the Rust folder-stats badge
   *  (multiset line diff). Used to short-circuit the render for very large
   *  diffs before the expensive jsdiff + Shiki pass (see the size guard in
   *  the load effect) and to label the summary card when we do. */
  added?: number;
  deleted?: number;
}

export function DiffPanel({ path, onClose, expanded, onToggleExpanded, heightPercent, added, deleted }: DiffPanelProps) {
  const t = useT();
  const dataTheme = useDataAttr('data-theme');
  const [result, setResult] = useState<DiffResult>({ state: 'loading' });

  // Latest badge counts (Rust multiset deltas), mirrored into a ref so the
  // open-time size guard can read them WITHOUT the load effect depending on
  // them. They are live-polled — file-stats refreshes on every agent-status /
  // fs-refresh while an agent edits the open file — so as effect deps they
  // blanked the diff to 'loading' and re-ran both IPC reads + double Shiki
  // tokenization on every tick. The guard only needs the value at open time.
  const badgeRef = useRef({ added: 0, deleted: 0 });
  badgeRef.current = { added: added ?? 0, deleted: deleted ?? 0 };

  // Which folded gaps the user has expanded in place. Keyed by gap.key
  // (stable per file). Reset when the file changes so a new diff starts
  // fully folded.
  const [expandedGaps, setExpandedGaps] = useState<Set<string>>(() => new Set());
  useEffect(() => { setExpandedGaps(new Set()); }, [path]);
  const toggleGap = (key: string) =>
    setExpandedGaps(prev => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  // Keyboard handling — same DiffPanel element across two visual sizes:
  //   half (default): bottom-anchored overlay covering ~55% of the panel
  //   expanded: full-window portal (modal). Esc collapses expanded → half,
  //   then half → closes the diff entirely. So: Esc has a single, learnable
  //   meaning ("step back one zoom level"), unlike a UA toggle button.
  // In expanded mode we also blur the active element so keystrokes can't
  // leak into a focused Gambit textarea behind the dim layer.
  useEffect(() => {
    if (expanded) {
      (document.activeElement as HTMLElement | null)?.blur();
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        if (expanded) onToggleExpanded();
        else onClose();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [expanded, onToggleExpanded, onClose]);

  useEffect(() => {
    let cancelled = false;
    setResult({ state: 'loading' });

    // Cheap pre-read guard: the Rust badge already knows the change
    // magnitude (multiset line diff). If it's huge, skip the IPC reads,
    // jsdiff, and Shiki entirely — go straight to the summary card. Read
    // from the ref (see badgeRef above) so this effect needn't depend on the
    // live-polled counts.
    const { added: badgeAdded, deleted: badgeDeleted } = badgeRef.current;
    if (badgeAdded + badgeDeleted > DIFF_MAX_CHANGED_LINES) {
      setResult({ state: 'too_large', added: badgeAdded, deleted: badgeDeleted });
      return;
    }

    (async () => {
      try {
        // Pre-read size guard: ask Rust for the file's on-disk byte size and
        // bail to the summary for oversized files BEFORE marshalling their
        // (possibly multi-MB) contents across IPC. current_bytes is real UTF-8
        // bytes, so this catches large multibyte CJK files that a decoded
        // String.length check (UTF-16 units) under-measured.
        const meta = await commands.getDiffMeta(path);
        if (cancelled) return;
        if (!meta.current_exists) {
          setResult({ state: 'error', reason: 'unreadable' });
          return;
        }
        if (meta.current_bytes > DIFF_MAX_BYTES) {
          setResult({ state: 'too_large', added: badgeAdded, deleted: badgeDeleted });
          return;
        }

        const [baseline, current] = await Promise.all([
          commands.getBaselineContent(path),
          commands.readTextFile(path),
        ]);
        if (cancelled) return;
        if (current === null) {
          setResult({ state: 'error', reason: 'unreadable' });
          return;
        }
        const oldText = baseline ?? '';
        const newText = current;

        const lines = computeUnifiedDiff(oldText, newText);
        // Renderer-side counts (order-sensitive jsdiff), distinct from the
        // Rust badge props of the same name — name them apart to avoid
        // shadowing those props.
        const addedLines = lines.filter(l => l.kind === 'add').length;
        const deletedLines = lines.filter(l => l.kind === 'del').length;

        // Tokenize BEFORE the first 'ok' render. Painting plain text first
        // and then swapping in Shiki tokens caused a visible color flip on
        // every file open — single-shot avoids that.
        const theme = getShikiTheme(dataTheme);
        const [oldTokens, newTokens] = await Promise.all([
          tokenizeFile(oldText, path, theme),
          tokenizeFile(newText, path, theme),
        ]);
        if (cancelled) return;

        const tokenized = (oldTokens || newTokens)
          ? lines.map(line => {
              const src = line.kind === 'del' ? oldTokens : newTokens;
              return { ...line, tokens: src?.[line.lineNum - 1] ?? null };
            })
          : lines;

        // Fold unchanged runs into gaps for rendering; counts stay sourced
        // from the full flat list above.
        const rows = collapseToHunks(tokenized);
        setResult({ state: 'ok', rows, added: addedLines, deleted: deletedLines });
      } catch {
        if (cancelled) return;
        setResult({ state: 'error', reason: 'ipc' });
      }
    })();

    return () => { cancelled = true; };
  }, [path, dataTheme]);

  const basename = useMemo(() => path.replace(/\\/g, '/').split('/').pop() || path, [path]);

  const header = (
    <div className="diff-header">
      <span className="diff-header-name">{basename}</span>
      <div className="diff-header-actions">
        <button
          type="button"
          className="diff-header-btn"
          onClick={onToggleExpanded}
          aria-label={expanded ? 'Collapse diff' : 'Expand diff'}
        >
          {expanded ? '⤓' : '⤢'}
        </button>
        <button
          type="button"
          className="diff-header-btn"
          onClick={onClose}
          aria-label="Close diff"
        >
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <line x1="18" y1="6" x2="6" y2="18"/>
            <line x1="6" y1="6" x2="18" y2="18"/>
          </svg>
        </button>
      </div>
    </div>
  );

  // Same DiffPanel element rendered at two different sizes:
  //   default: in-flow / parent-anchored overlay (bottom half of changes panel)
  //   expanded: portal to body, fixed-inset modal (full window)
  // Backdrop only appears in expanded mode (modal needs a dim+blocker).
  const panelStyle: CSSProperties | undefined =
    !expanded && typeof heightPercent === 'number'
      ? { height: `${heightPercent}%` }
      : undefined;
  const panel = (
    <div
      className={`diff-panel${expanded ? ' diff-panel--expanded' : ''}`}
      style={panelStyle}
    >
      {header}
      <div className="diff-body">
        {result.state === 'loading' && (
          <div className="diff-empty">{t('diff.loading' as any) || 'Loading…'}</div>
        )}
        {result.state === 'error' && (
          <div className="diff-empty">{t('diff.error' as any) || 'Failed to load diff'}</div>
        )}
        {result.state === 'too_large' && (
          <div className="diff-toolarge">
            <div className="diff-toolarge-msg">
              {t('diff.too_large' as any) || 'File too large to show inline diff'}
            </div>
            <div className="diff-toolarge-stats">
              <span className="diff-add">+{result.added}</span>
              <span className="diff-del">-{result.deleted}</span>
            </div>
          </div>
        )}
        {result.state === 'ok' && result.added === 0 && result.deleted === 0 && (
          <div className="diff-empty">{t('diff.no_changes' as any) || 'Identical to baseline'}</div>
        )}
        {result.state === 'ok' && (result.added > 0 || result.deleted > 0) && (
          <pre className="diff-pre">
            {result.rows.map(row => {
              if (row.type === 'line') return renderDiffLine(row.line);
              if (expandedGaps.has(row.key)) return row.lines.map(renderDiffLine);
              return (
                <div
                  key={row.key}
                  className="diff-gap"
                  role="button"
                  tabIndex={0}
                  onClick={() => toggleGap(row.key)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault();
                      toggleGap(row.key);
                    }
                  }}
                >
                  {t('diff.unchanged_lines' as any, { count: row.lines.length }) ||
                    `⋯ ${row.lines.length} unchanged lines`}
                </div>
              );
            })}
          </pre>
        )}
      </div>
    </div>
  );

  if (!expanded) return panel;
  return createPortal(
    <>
      <div className="diff-backdrop" onMouseDown={onToggleExpanded} />
      {panel}
    </>,
    document.body,
  );
}

// Render one diff line. Module-level so it can be reused for both ordinary
// rows and the lines revealed when a gap is expanded. Self-keyed by
// kind+lineNum, which is unique within a single file's diff (new-file line
// numbers for add/eq, old-file for del; the kind prefix separates the two
// numbering spaces), so React reconciles stably as gaps expand/collapse.
function renderDiffLine(line: DiffLine) {
  return (
    <div key={`${line.kind}-${line.lineNum}`} className={`diff-line diff-line-${line.kind}`}>
      <span className="diff-line-num">{line.lineNum}</span>
      <span className="diff-marker">
        {line.kind === 'add' ? '+' : line.kind === 'del' ? '-' : ' '}
      </span>
      <span className="diff-text">
        {line.tokens
          ? line.tokens.map((tok, j) => (
              <span key={j} style={{ color: tok.color }}>{tok.content}</span>
            ))
          : line.text}
      </span>
    </div>
  );
}

// Fold runs of unchanged lines that sit more than DIFF_CONTEXT_LINES from
// any change into a single gap row. Keeps up to CONTEXT lines of orientation
// on each side of every change; the leading run before the first change and
// the trailing run after the last change have only one inner side, so they
// keep context on that side only. A run is folded only when it would hide at
// least DIFF_COLLAPSE_MIN_HIDDEN lines.
function collapseToHunks(lines: DiffLine[]): DiffRow[] {
  const CONTEXT = DIFF_CONTEXT_LINES;
  const rows: DiffRow[] = [];
  const n = lines.length;
  let i = 0;
  while (i < n) {
    if (lines[i].kind !== 'eq') {
      rows.push({ type: 'line', line: lines[i] });
      i++;
      continue;
    }
    // Equal run spans [i, j).
    let j = i;
    while (j < n && lines[j].kind === 'eq') j++;
    const head = i > 0 ? CONTEXT : 0; // trailing context for the change above
    const tail = j < n ? CONTEXT : 0; // leading context for the change below
    const hidden = j - i - head - tail;
    if (hidden < DIFF_COLLAPSE_MIN_HIDDEN) {
      for (let k = i; k < j; k++) rows.push({ type: 'line', line: lines[k] });
    } else {
      for (let k = i; k < i + head; k++) rows.push({ type: 'line', line: lines[k] });
      const hiddenLines = lines.slice(i + head, j - tail);
      rows.push({ type: 'gap', key: `gap-${lines[i + head].lineNum}-${hiddenLines.length}`, lines: hiddenLines });
      for (let k = j - tail; k < j; k++) rows.push({ type: 'line', line: lines[k] });
    }
    i = j;
  }
  return rows;
}

// Convert two text blobs into a flat list of unified-diff lines. We don't
// emit @@ hunk headers — for an in-app audit panel users don't need
// summary headers, just the +/- flow with line numbers. jsdiff returns
// chunks (added/removed/eq); we flatten each into individual rows.
// (collapseToHunks then folds the unchanged stretches for rendering.)
function computeUnifiedDiff(oldText: string, newText: string): DiffLine[] {
  const out: DiffLine[] = [];
  const parts = diffLines(oldText, newText);
  // Track each side's running line number. jsdiff doesn't expose these
  // (chunks are content-only), so we walk and increment per chunk type:
  // added → only new advances; removed → only old advances; eq → both.
  let oldLine = 1;
  let newLine = 1;
  for (const p of parts) {
    const lines = p.value.split('\n');
    if (lines[lines.length - 1] === '') lines.pop();
    if (p.added) {
      for (const text of lines) out.push({ kind: 'add', text, lineNum: newLine++, tokens: null });
    } else if (p.removed) {
      for (const text of lines) out.push({ kind: 'del', text, lineNum: oldLine++, tokens: null });
    } else {
      for (const text of lines) {
        out.push({ kind: 'eq', text, lineNum: newLine, tokens: null });
        oldLine++;
        newLine++;
      }
    }
  }
  return out;
}
