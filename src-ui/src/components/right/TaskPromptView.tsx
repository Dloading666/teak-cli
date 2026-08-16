// TaskPromptView - prompt-library presentation of the task board. Same
// TaskItem[] data as the to-do list (TaskBoard) and sticky-note views; this
// view repurposes `title` as a CATEGORY name and groups items by it (100%
// string match), rendering each item's `description` as a prompt body.
// There is no priority/status concept here - no checkbox, no traffic lights;
// the `status` field rides along unused so the same item still reads
// correctly in list/note mode ("one dataset, three lenses").
//
//   ▼ ✎ 翻译类 (2)            ← pencil surfaces on hover
//     ┌──────────────────────────────────────┐
//     │ 帮我翻译成英文，保持术语…              │  ← description (prompt body)
//     │                       ✎  🗑  ▶        │  ← actions surface on hover
//     └──────────────────────────────────────┘
//   ▶ ✎ 代码审查 (5)
//
// Edit model: both the category title and a card's body are STATIC by
// default (display text). A pencil (✎) enters edit mode; ✓ (or blur/Esc)
// leaves it. The resting card/header is a clean drag + fold surface - edit
// affordances stay hidden until hover so the UI isn't cluttered. Edit mode
// adds NO border and NO layout shift: the input/textarea is borderless +
// transparent, identical metrics to the display text; the only visible cue
// is the caret. The fold caret stays visible while editing a title.
//
// New category: the title field pre-fills with a default name (fully
// selected, so typing replaces it). The category picker does NOT auto-open -
// the user clicks ▼ to see other categories (unfiltered, unlike a native
// <datalist>). Pick one to retarget the rename (renaming onto an existing
// category MERGES the two groups, since renameCategory retitles every item
// sharing the old title).
//
// Drag: a floating ghost (capped to ~2 lines) follows the cursor and a
// highlight bar marks the insertion point at the target card's edge - the
// same drag UX as the sticky-note view.
//
// A category has no independent existence - it's just the set of items
// sharing a title, so there's no "delete category" button. Delete the last
// item in a category and the category vanishes from the data alone. Drag a
// card across categories and its title is rewritten to the target's. Send
// pastes only the description.

import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useT } from '../../i18n/useT';
import type { TaskItem } from './task-types';
import { TaskEmptyState } from './TaskEmptyState';
import { useTextContextMenu } from '../../lib/use-text-context-menu';
import './TaskPromptView.css';

const COLLAPSED_KEY = 'cc-prompt-collapsed';

interface DropLine { id: string; position: 'before' | 'after'; }

interface TaskPromptViewProps {
  tasks: TaskItem[];
  // The item most recently added via the FAB - its category title enters
  // edit mode (pre-filled with a default name, selected) so the user can
  // name the new category immediately.
  addingId: string | null;
  // The item mid-removal - kept rendered for one exit animation frame before
  // the parent drops it from the array (shared 300ms window with list/note).
  removingId: string | null;
  canSend: boolean;
  // Rename a category: rewrites the title of EVERY item sharing `oldTitle`
  // to `newTitle` (so the whole group moves together). '' -> '翻译类' is the
  // "name a new category" case; renaming onto an existing title merges.
  onRenameCategory: (oldTitle: string, newTitle: string) => void;
  onUpdateDesc: (id: string, desc: string) => void;
  onRemove: (id: string) => void;
  onSend: (task: TaskItem) => void;
  // Functional updater (the parent's setTasks) so a drag computed at drop
  // time still composes against the freshest array - guards against a
  // multi-window sync landing mid-drag.
  onReorder: (updater: (prev: TaskItem[]) => TaskItem[]) => void;
  // Persist a bottom-edge-resized body height (called once on drop). Reuses
  // the same `height` field the sticky-note view uses.
  onSetHeight: (id: string, height: number) => void;
}

export function TaskPromptView({
  tasks, addingId, removingId, canSend,
  onRenameCategory, onUpdateDesc, onRemove, onSend, onReorder, onSetHeight,
}: TaskPromptViewProps) {
  const t = useT();
  // Right-click cut/copy/paste/select menu for the category-title input.
  // (PromptBody has its own instance for the prompt body.)
  const { menu: titleCtxMenu, openMenu: openTitleCtxMenu } = useTextContextMenu();

  // Collapsed categories (keyed by title string), persisted so the fold
  // survives tab switches / restarts.
  const [collapsed, setCollapsed] = useState<Set<string>>(() => {
    try { return new Set<string>(JSON.parse(localStorage.getItem(COLLAPSED_KEY) || '[]')); }
    catch { return new Set<string>(); }
  });
  const toggleCollapse = (cat: string) => setCollapsed(prev => {
    const next = new Set(prev);
    if (next.has(cat)) next.delete(cat); else next.add(cat);
    try { localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...next])); } catch { /* Best-effort operation; failure is non-fatal. */ }
    return next;
  });

  // Category-title inline edit. `editingTitle` holds the OLD title being
  // renamed ('' when naming a brand-new category). Committing an empty name
  // on a NEW category removes the phantom item so it can't linger unnamed.
  const [editingTitle, setEditingTitle] = useState<string | null>(null);
  const [editValue, setEditValue] = useState('');
  const editInputRef = useRef<HTMLInputElement>(null);
  // The last FAB-added item id - after the user names a new category we
  // auto-focus its body so they can write the prompt without a second click.
  const newItemRef = useRef<string | null>(null);

  // Which card body is in edit mode (by item id). null = all bodies static.
  const [editingBody, setEditingBody] = useState<string | null>(null);

  // Whether the "pick existing category" dropdown is open while editing a title.
  const [titleMenuOpen, setTitleMenuOpen] = useState(false);

  // A freshly FAB-added item kicks its header into edit, pre-filled with a
  // default name (selected, so typing replaces it). The picker does NOT
  // auto-open - the user opens it manually via ▼.
  /* eslint-disable react-hooks/set-state-in-effect -- addingId is an explicit parent signal to enter the new category editor. */
  useEffect(() => {
    if (addingId) {
      newItemRef.current = addingId;
      setEditingTitle('');
      setEditValue(t('task.default_title'));
    }
  }, [addingId, t]);
  /* eslint-enable react-hooks/set-state-in-effect */
  // Focus the title input on edit. New category -> select-all (typing
  // replaces the default); rename -> caret at end (tweak-friendly).
  useEffect(() => {
    if (editingTitle === null) return;
    requestAnimationFrame(() => {
      const el = editInputRef.current;
      if (!el) return;
      el.focus();
      if (editingTitle === '') {
        el.select();
      } else {
        const len = el.value.length;
        el.selectionStart = len;
        el.selectionEnd = len;
      }
    });
  }, [editingTitle]);

  // Close the category dropdown on outside click (anything not the menu, the
  // toggle, or the input itself). The option/toggle handlers use onMouseDown
  // + preventDefault to keep input focus, so this only fires for genuine
  // outside clicks.
  useEffect(() => {
    if (!titleMenuOpen) return;
    const onDown = (e: MouseEvent) => {
      const target = e.target as HTMLElement;
      if (!target.closest('.task-prompt-category-menu, .task-prompt-title-toggle, .task-prompt-category-input')) {
        setTitleMenuOpen(false);
      }
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [titleMenuOpen]);

  const commitTitle = (oldTitle: string) => {
    const trimmed = editValue.trim();
    if (trimmed && trimmed !== oldTitle) {
      onRenameCategory(oldTitle, trimmed);
      // Named a brand-new category -> drop straight into its body so the
      // user can write the prompt (create-then-write flow, no extra click).
      if (oldTitle === '' && newItemRef.current) setEditingBody(newItemRef.current);
    } else if (!trimmed && oldTitle === '') {
      // New category left unnamed -> drop the phantom so it doesn't persist.
      const phantom = tasks.find(it => it.title === '');
      if (phantom) onRemove(phantom.id);
    } else if (trimmed === oldTitle) {
      // Unchanged - keep the category as-is (no rename, no merge).
    } else if (!trimmed && oldTitle !== '') {
      // Renaming an existing category to empty -> cancel (don't wipe it).
    }
    setEditingTitle(null);
    setEditValue('');
    setTitleMenuOpen(false);
  };
  const cancelTitle = (oldTitle: string) => {
    if (oldTitle === '') {
      const phantom = tasks.find(it => it.title === '');
      if (phantom) onRemove(phantom.id);
    }
    setEditingTitle(null);
    setEditValue('');
    setTitleMenuOpen(false);
  };

  // Named categories feed the picker dropdown (every category except the one
  // being edited, so the list offers real retarget/merge targets).
  const allCategories = Array.from(new Set(tasks.map(it => it.title).filter(Boolean)));

  // Group by title, preserving first-seen order. The '' group (an unnamed
  // new category) lands wherever its item first appears.
  const groups: { category: string; items: TaskItem[] }[] = [];
  const index = new Map<string, number>();
  for (const it of tasks) {
    let gi = index.get(it.title);
    if (gi === undefined) {
      gi = groups.length;
      groups.push({ category: it.title, items: [] });
      index.set(it.title, gi);
    }
    groups[gi].items.push(it);
  }

  // ── Drag (ghost + highlight-bar drop indicator, same as the note view) ──
  const listRef = useRef<HTMLDivElement>(null);
  const ghostRef = useRef<HTMLDivElement | null>(null);
  const dragStartedRef = useRef(false);
  const dropTargetRef = useRef<DropLine | null>(null);
  const [dragId, setDragId] = useState<string | null>(null);
  const [dropLine, setDropLine] = useState<DropLine | null>(null);
  // Live preview height while dragging a card's bottom-edge resize handle;
  // persisted via onSetHeight only on pointer-up (one write per resize).
  const [resizing, setResizing] = useState<{ id: string; height: number } | null>(null);

  const handlePointerDown = (e: React.PointerEvent, id: string) => {
    const target = e.target as HTMLElement;
    // Never start a drag from an interactive control. The resting body has
    // pointer-events:none so it never swallows the event - the card handles
    // the drag. An EDITING body (pointer-events:auto) is skipped here so the
    // caret works.
    if (target.closest('button') || target.closest('input')) return;
    if (target.closest('textarea')) {
      const ta = target.closest('textarea') as HTMLTextAreaElement;
      if (!ta.readOnly) return; // editing body -> let the caret work
    }
    const cardEl = target.closest('.task-prompt-card') as HTMLElement;
    if (!cardEl) return;

    const rect = cardEl.getBoundingClientRect();
    const offsetX = e.clientX - rect.left;
    const offsetY = e.clientY - rect.top;
    const startX = e.clientX;
    const startY = e.clientY;
    dragStartedRef.current = false;
    dropTargetRef.current = null;

    const THRESHOLD = 6;
    const LONG_PRESS_MS = 400;

    // Body is now click-to-edit, so a drag must be told apart from a click:
    // start only on an actual move past THRESHOLD, or on a held press (long
    // press). Ghost creation is extracted so both paths share it.
    const startDrag = (cx: number, cy: number) => {
      if (dragStartedRef.current) return;
      dragStartedRef.current = true;
      setDragId(id);
      const ghost = cardEl.cloneNode(true) as HTMLDivElement;
      ghost.className = 'task-prompt-card-ghost';
      // cloneNode copies attributes but not the live textarea `.value`
      // (React drives it as a property), so the clone's body would be
      // blank. Copy it across so the drag preview keeps the user's text.
      const srcBody = cardEl.querySelector('textarea');
      const ghostBody = ghost.querySelector('textarea');
      if (srcBody && ghostBody) ghostBody.value = srcBody.value;
      // Cap the ghost's body to ~2 lines so a long prompt's drag preview
      // stays compact (the full text isn't needed while dragging).
      if (ghostBody) {
        ghostBody.style.height = '42px';
        ghostBody.style.maxHeight = '42px';
        ghostBody.style.overflow = 'hidden';
      }
      ghost.style.width = `${rect.width}px`;
      ghost.style.left = `${cx - offsetX}px`;
      ghost.style.top = `${cy - offsetY}px`;
      document.body.appendChild(ghost);
      ghostRef.current = ghost;
    };

    // Held still for LONG_PRESS_MS -> drag (long-press), so a quick click
    // never moves the card. Cleared on pointerup.
    const longPressTimer = window.setTimeout(() => startDrag(startX, startY), LONG_PRESS_MS);

    const onMove = (me: PointerEvent) => {
      if (!dragStartedRef.current) {
        if (Math.abs(me.clientX - startX) < THRESHOLD && Math.abs(me.clientY - startY) < THRESHOLD) return;
        startDrag(me.clientX, me.clientY);
      }
      if (ghostRef.current) {
        ghostRef.current.style.left = `${me.clientX - offsetX}px`;
        ghostRef.current.style.top = `${me.clientY - offsetY}px`;
      }
      if (!listRef.current) return;
      const cards = listRef.current.querySelectorAll<HTMLElement>('[data-prompt-id]');
      // Find the insertion target (which card, before/after).
      let best: DropLine | null = null;
      for (const card of cards) {
        const cid = card.dataset.promptId!;
        if (cid === id) continue;
        const r = card.getBoundingClientRect();
        if (me.clientY >= r.top && me.clientY < r.bottom) {
          best = { id: cid, position: me.clientY < r.top + r.height / 2 ? 'before' : 'after' };
          break;
        }
      }
      dropTargetRef.current = best;
      // Drop indicator: a highlight bar at the target card's edge (CSS
      // .drop-before/.drop-after). Only re-render when the target changes.
      setDropLine(prev =>
        prev?.id === best?.id && prev?.position === best?.position ? prev : best
      );
    };

    const onUp = () => {
      window.clearTimeout(longPressTimer);
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      if (ghostRef.current) { ghostRef.current.remove(); ghostRef.current = null; }
      const drop = dropTargetRef.current;
      if (dragStartedRef.current && drop && drop.id !== id) {
        onReorder(prev => {
          const dragged = prev.find(it => it.id === id);
          const targetItem = prev.find(it => it.id === drop.id);
          if (!dragged || !targetItem) return prev;
          // Cross-category drag adopts the target's category (title), exactly
          // like the note view adopts the target's status - only the grouping
          // dimension differs. Same-category drags just reorder.
          const moved = dragged.title === targetItem.title
            ? dragged
            : { ...dragged, title: targetItem.title };
          const without = prev.filter(it => it.id !== id);
          const targetIdx = without.findIndex(it => it.id === drop.id);
          if (targetIdx === -1) return prev;
          const insertAt = drop.position === 'before' ? targetIdx : targetIdx + 1;
          without.splice(insertAt, 0, moved);
          return without;
        });
      } else if (!dragStartedRef.current) {
        // Quick click (no move, no long-press) -> edit the body directly,
        // replacing the old pencil affordance.
        setEditingBody(id);
      }
      setDragId(null);
      setDropLine(null);
      dropTargetRef.current = null;
      dragStartedRef.current = false;
    };

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  };

  // Bottom-edge resize: drag the card's bottom border to set a fixed body
  // height (text clips when shrunk below content). Mirrors the note view.
  const handleResizeDown = (e: React.PointerEvent, id: string) => {
    e.preventDefault();
    e.stopPropagation(); // don't let the card's reorder gesture start
    const handle = e.currentTarget as HTMLElement;
    const body = handle.closest('.task-prompt-card')?.querySelector('.task-prompt-body') as HTMLElement | null;
    const startHeight = body ? body.getBoundingClientRect().height : BODY_MIN_HEIGHT;
    const startY = e.clientY;
    // Pointer capture keeps move/up/cancel coming to this element even if the
    // pointer leaves the window or focus is stolen (WebView2).
    try { handle.setPointerCapture(e.pointerId); } catch { /* Best-effort operation; failure is non-fatal. */ }
    const onMove = (me: PointerEvent) => {
      const next = Math.max(BODY_MIN_HEIGHT, Math.min(BODY_MAX_HEIGHT, startHeight + (me.clientY - startY)));
      setResizing({ id, height: Math.round(next) });
    };
    const finish = () => {
      handle.removeEventListener('pointermove', onMove);
      handle.removeEventListener('pointerup', finish);
      handle.removeEventListener('pointercancel', finish);
      setResizing(cur => {
        if (cur && cur.id === id) onSetHeight(id, cur.height);
        return null;
      });
    };
    handle.addEventListener('pointermove', onMove);
    handle.addEventListener('pointerup', finish);
    handle.addEventListener('pointercancel', finish);
  };

  if (tasks.length === 0) {
    return (
      <div className="task-prompt-list task-prompt-list--empty">
        <TaskEmptyState />
      </div>
    );
  }

  return (
    <div ref={listRef} className={`task-prompt-list${dragId ? ' dragging-active' : ''}`}>
      {groups.map(group => {
        const isCollapsed = collapsed.has(group.category);
        const isEditingTitle = editingTitle === group.category;
        // Every category except the one being edited - real retarget/merge
        // targets for the picker dropdown (unfiltered, unlike <datalist>).
        const otherCategories = allCategories.filter(c => c !== group.category);
        return (
          <section key={group.category || '__new__'} className="task-prompt-group" data-category={group.category}>
            <div
              className="task-prompt-header"
              // Clicking the resting header (the name, the caret, the count)
              // folds/unfolds the group. During title edit, leave clicks alone
              // so the user can interact with the input without collapsing.
              onClick={() => { if (!isEditingTitle) toggleCollapse(group.category); }}
            >
              <span className="task-prompt-caret" aria-hidden="true">
                <svg
                  width="10" height="10" viewBox="0 0 24 24" fill="none"
                  stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round"
                  style={{ transform: isCollapsed ? 'rotate(-90deg)' : 'none' }}
                >
                  <polyline points="6 9 12 15 18 9" />
                </svg>
              </span>

              {isEditingTitle ? (
                <>
                  <input
                    ref={editInputRef}
                    className="task-prompt-category-input"
                    value={editValue}
                    placeholder={t('task.prompt.category_placeholder')}
                    onClick={e => e.stopPropagation()}
                    onChange={e => setEditValue(e.target.value)}
                    onKeyDown={e => {
                      if (e.nativeEvent.isComposing) return;
                      if (e.key === 'Enter') { e.preventDefault(); commitTitle(group.category); }
                      if (e.key === 'Escape') cancelTitle(group.category);
                    }}
                    onBlur={() => commitTitle(group.category)}
                    onContextMenu={(e) => openTitleCtxMenu(e, setEditValue)}
                  />
                  <button
                    className="task-prompt-title-toggle"
                    // onMouseDown + preventDefault keeps focus on the input so
                    // onBlur doesn't commit before the toggle opens the menu.
                    onMouseDown={e => e.preventDefault()}
                    onClick={e => { e.stopPropagation(); setTitleMenuOpen(o => !o); }}
                    aria-label="pick existing category"
                  >
                    <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                      <polyline points="6 9 12 15 18 9" />
                    </svg>
                  </button>
                  {titleMenuOpen && otherCategories.length > 0 && (
                    <div className="task-prompt-category-menu">
                      {otherCategories.map(c => (
                        <button
                          key={c}
                          className="task-prompt-category-option"
                          onMouseDown={e => { e.preventDefault(); setEditValue(c); setTitleMenuOpen(false); }}
                        >
                          {c}
                        </button>
                      ))}
                    </div>
                  )}
                  <button
                    className="task-prompt-tick"
                    onMouseDown={e => { e.preventDefault(); commitTitle(group.category); }}
                    aria-label="confirm"
                  >
                    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                      <polyline points="20 6 9 17 4 12" />
                    </svg>
                  </button>
                </>
              ) : (
                <>
                  <span className="task-prompt-category-name">
                    {group.category || t('task.prompt.new_category')}
                  </span>
                  <button
                    className="task-prompt-pencil"
                    onClick={e => { e.stopPropagation(); setEditingTitle(group.category); setEditValue(group.category); }}
                    aria-label="edit category"
                  >
                    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M17 3a2.828 2.828 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5L17 3z" />
                    </svg>
                  </button>
                  <span className="task-prompt-count">({group.items.length})</span>
                </>
              )}
            </div>

            {!isCollapsed && group.items.map(item => {
              const hasBody = !!(item.description && item.description.trim());
              const isEditingBody = editingBody === item.id;
              const lineBefore = dropLine?.id === item.id && dropLine.position === 'before';
              const lineAfter = dropLine?.id === item.id && dropLine.position === 'after';
              return (
                <div
                  key={item.id}
                  data-prompt-id={item.id}
                  className={[
                    'task-prompt-card',
                    dragId === item.id && 'dragging',
                    removingId === item.id && 'removing',
                    lineBefore && 'drop-before',
                    lineAfter && 'drop-after',
                  ].filter(Boolean).join(' ')}
                  style={{ touchAction: 'none' }}
                  onPointerDown={e => handlePointerDown(e, item.id)}
                >
                  <PromptBody
                    value={item.description || ''}
                    placeholder={t('task.prompt.body_placeholder')}
                    editable={isEditingBody}
                    height={resizing?.id === item.id ? resizing.height : (item.height ?? null)}
                    onChange={next => onUpdateDesc(item.id, next)}
                    onDone={() => setEditingBody(null)}
                  />
                  <div className="task-prompt-actions">
                    <button className="task-prompt-btn task-prompt-send" onClick={e => { e.stopPropagation(); onSend(item); }} disabled={!canSend || !hasBody} aria-label="send">
                      <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" stroke="none"><polygon points="5 3 21 12 5 21" /></svg>
                    </button>
                    <button className="task-prompt-btn task-prompt-delete" onClick={e => { e.stopPropagation(); onRemove(item.id); }} aria-label="delete">
                      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                        <polyline points="3 6 5 6 21 6" /><path d="M19 6l-2 14a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L5 6" /><path d="M10 11v6" /><path d="M14 11v6" /><path d="M9 6V4a2 2 0 0 1 2-2h2a2 2 0 0 1 2 2v2" />
                      </svg>
                    </button>
                  </div>
                  <div
                    className={`task-prompt-resize${resizing?.id === item.id ? ' resizing' : ''}`}
                    onPointerDown={e => handleResizeDown(e, item.id)}
                  />
                </div>
              );
            })}
          </section>
        );
      })}
      {titleCtxMenu}
    </div>
  );
}

// ─── Prompt body - a textarea that's readOnly at rest, editable on demand ─────
// Resting: readOnly + pointer-events:none + user-select:none, so it reads as
// static display text, pointer events pass through to the card (drag works by
// grabbing the body), and the cursor is the card's default arrow (NOT the
// textarea's I-beam). Editing: pointer-events:auto + caret at the end. Same
// element in both modes -> zero layout shift; the only visible change is the
// caret. Auto-grows to fit content, with NoteBody's zero-width guard (the
// right panel mounts collapsed at width 0 then animates open).

interface PromptBodyProps {
  value: string;
  placeholder: string;
  editable: boolean;
  // Manual body height (px) - when set, the body is FIXED at this height and
  // text clips; null = auto-grow to fit. Set by the bottom-edge resize handle.
  height: number | null;
  onChange: (next: string) => void;
  onDone: () => void;
}

const BODY_MIN_HEIGHT = 84;
const BODY_MAX_HEIGHT = 600;

function PromptBody({ value, placeholder, editable, height, onChange, onDone }: PromptBodyProps) {
  const ref = useRef<HTMLTextAreaElement>(null);
  // Right-click cut/copy/paste/select menu (same one Gambit/terminal use).
  const { menu: ctxMenu, openMenu: openCtxMenu } = useTextContextMenu();

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    let lastW = -1;
    const fit = () => {
      if (el.offsetWidth === 0) return; // collapsed / not laid out - wait for width
      if (height != null) {
        // Manually resized: FIXED height, text clips (overflow hidden).
        el.style.height = `${Math.max(BODY_MIN_HEIGHT, height)}px`;
      } else {
        el.style.height = 'auto';
        el.style.height = `${Math.max(BODY_MIN_HEIGHT, el.scrollHeight)}px`;
      }
      lastW = el.offsetWidth;
    };
    fit();
    const ro = new ResizeObserver(() => {
      if (el.offsetWidth !== 0 && el.offsetWidth !== lastW) fit();
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [value, height]);

  // Entering edit mode: focus and park the caret at the end so typing
  // continues from the tail (matches shell/REPL recall UX).
  useEffect(() => {
    if (editable && ref.current) {
      const el = ref.current;
      el.focus();
      const len = el.value.length;
      el.selectionStart = len;
      el.selectionEnd = len;
    }
  }, [editable]);

  return (
    <>
    <textarea
      ref={ref}
      className={`task-prompt-body${editable ? ' editable' : ''}`}
      value={value}
      placeholder={placeholder}
      readOnly={!editable}
      rows={1}
      onChange={e => onChange(e.target.value)}
      // In edit mode, stop propagation so dragging text doesn't move the
      // card. At rest, pointer-events:none (CSS) means this handler doesn't
      // fire - events pass straight to the card.
      onPointerDown={e => { if (editable) e.stopPropagation(); }}
      onContextMenu={(e) => openCtxMenu(e, onChange)}
      onBlur={onDone}
      onKeyDown={e => {
        if (e.nativeEvent.isComposing) return;
        if (e.key === 'Escape') { e.preventDefault(); onDone(); }
      }}
    />
    {ctxMenu}
    </>
  );
}
