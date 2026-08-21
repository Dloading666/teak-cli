import { useState, useEffect, useMemo, useRef, useSyncExternalStore } from 'react';
import type { CSSProperties, PointerEvent as ReactPointerEvent } from 'react';
import { useT } from '../../i18n/useT';
import { useAppState } from '../../store/app-state';
import type { SavedSession } from '../../tauri';
import { commands } from '../../tauri';
import { getToolDisplayName } from '../../lib/tool-info';
import { subscribeRenamed, getRenamedSnapshot, setCustomName } from '../../lib/renamed-sessions';
import {
  prefetchHistory,
  refreshHistory,
  subscribeHistory,
  getHistorySnapshot,
} from '../../lib/history-cache';
import { SessionContextMenu, type SessionCtxMenuState } from './SessionContextMenu';
import {
  applySessionOrder,
  attachHistoryToLive,
  liveStatus,
  moveInOrder,
  normCwd,
  pathBasename,
} from '../../lib/session-nav';
import {
  getNavOrderSnapshot,
  setGroupOrder,
  subscribeNavOrder,
} from '../../lib/session-nav-order';
import type { TerminalSession } from '../../store/app-state';
import HERMES_DATA_URL from '../../icons-inline/hermes.png?inline';
import OPENCODE_DATA_URL from '../../icons-inline/opencode.png?inline';
import KIMICODE_DATA_URL from '../../icons-inline/kimicode.png?inline';
import PI_SVG from '../../icons-inline/pi.svg?raw';
import GROK_SVG from '../../icons-inline/grok.svg?raw';
import OPENCLAW_SVG from '../../icons-inline/openclaw.svg?raw';
import './HistoryBoard.css';

const TOOL_ICON_SRC: Record<string, string> = {
  claude:      '/icons/tools/claude.svg',
  codex:       '/icons/tools/codex.svg',
  qwen:        '/icons/tools/qwen.svg',
  antigravity: '/icons/tools/antigravity.svg',
  hermes:      HERMES_DATA_URL,
  opencode:    OPENCODE_DATA_URL,
  kimicode:    KIMICODE_DATA_URL,
  mimocode:    '/icons/tools/mimocode.svg',
  kilo:        '/icons/tools/kilo.svg',
};

const getToolIcon = (tool: string) => {
  if (tool === 'pi' || tool === 'grok') {
    const svg = tool === 'pi' ? PI_SVG : GROK_SVG;
    return (
      <span
        aria-hidden
        className="nav-tool-icon"
        dangerouslySetInnerHTML={{ __html: svg }}
      />
    );
  }
  if (tool === 'openclaw') {
    return (
      <span
        aria-hidden
        className="nav-tool-icon"
        dangerouslySetInnerHTML={{ __html: OPENCLAW_SVG }}
      />
    );
  }
  const src = TOOL_ICON_SRC[tool];
  if (!src) {
    return (
      <svg className="nav-tool-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
        <polyline points="4 17 10 11 4 5"/>
        <line x1="12" y1="19" x2="20" y2="19"/>
      </svg>
    );
  }
  const extra = (tool === 'hermes' || tool === 'opencode' || tool === 'kilo')
    ? { borderRadius: 'var(--radius-xs)', objectFit: 'cover' as const }
    : tool === 'kimicode' ? { borderRadius: 'var(--radius-xs)', objectFit: 'contain' as const }
    : {};
  return <img src={src} alt="" className="nav-tool-icon" style={extra} />;
};

interface NavRow {
  key: string;
  saved: SavedSession;
  live: TerminalSession;
}

interface ProjectGroup {
  key: string;
  cwd: string;
  label: string;
  rows: NavRow[];
}

export function HistoryBoard() {
  const t = useT();
  const { state, dispatch } = useAppState();
  const renamed = useSyncExternalStore(subscribeRenamed, getRenamedSnapshot);
  const { sessions: historySessions } = useSyncExternalStore(
    subscribeHistory,
    getHistorySnapshot,
    getHistorySnapshot,
  );
  const navOrder = useSyncExternalStore(
    subscribeNavOrder,
    getNavOrderSnapshot,
    getNavOrderSnapshot,
  );
  const [renamingKey, setRenamingKey] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');
  const cancelRenameRef = useRef(false);
  const [ctxMenu, setCtxMenu] = useState<SessionCtxMenuState | null>(null);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [rowDrag, setRowDrag] = useState<{
    sessionId: string;
    groupKey: string;
    deltaY: number;
    fromIdx: number;
    targetIdx: number;
    slotHeight: number;
  } | null>(null);
  const rowDragSuppressClickRef = useRef(false);
  const dragCleanupRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    prefetchHistory();
    refreshHistory();
  }, []);

  const launchpadOpen = !!state.terminals.find(
    (term) => term.id === state.activeTerminalId && !term.tool,
  );

  const groups = useMemo((): ProjectGroup[] => {
    const claimed = new Set<string>();
    const liveTerms = state.terminals
      .filter((terminal) => !terminal.isHidden && terminal.tool && terminal.tool !== 'installer')
      .slice()
      .sort((a, b) => (b.startedAt ?? 0) - (a.startedAt ?? 0));
    const rows: NavRow[] = liveTerms.map((terminal) => ({
      key: terminal.id,
      saved: attachHistoryToLive(
        terminal,
        historySessions,
        claimed,
        getToolDisplayName(terminal.tool ?? ''),
      ),
      live: terminal,
    }));

    const buckets = new Map<string, NavRow[]>();
    const display = new Map<string, string>();
    for (const row of rows) {
      const tool = row.saved.tool ?? '';
      const raw = (row.saved.cwd ?? '').trim();
      let key: string;
      let cwd: string;
      if (tool === 'openclaw') {
        key = 'tool:openclaw';
        cwd = '';
      } else if (!raw) {
        key = '__none__';
        cwd = '';
      } else {
        key = normCwd(raw);
        cwd = raw.replace(/[\\/]+$/, '');
      }
      if (!display.has(key)) display.set(key, cwd);
      const list = buckets.get(key);
      if (list) list.push(row);
      else buckets.set(key, [row]);
    }

    const baseCount = new Map<string, number>();
    for (const cwd of display.values()) {
      if (!cwd) continue;
      const base = pathBasename(cwd);
      baseCount.set(base, (baseCount.get(base) ?? 0) + 1);
    }

    const result: ProjectGroup[] = [];
    for (const [key, list] of buckets) {
      const cwd = display.get(key) ?? '';
      let label: string;
      if (key === 'tool:openclaw') label = getToolDisplayName('openclaw');
      else if (key === '__none__') label = t('nav.uncategorized');
      else {
        const base = pathBasename(cwd);
        if ((baseCount.get(base) ?? 0) <= 1) label = base;
        else {
          const parent = pathBasename(cwd.slice(0, cwd.length - base.length).replace(/[\\/]+$/, ''));
          label = parent ? `${base} — ${parent}` : base;
        }
      }
      result.push({
        key,
        cwd,
        label,
        rows: applySessionOrder(
          list,
          navOrder[key],
          (row) => row.key,
          (row) => row.live.startedAt ?? 0,
        ),
      });
    }
    return result;
  }, [state.terminals, historySessions, navOrder, t]);

  useEffect(() => () => { dragCleanupRef.current?.(); }, []);

  // Grok and other CLIs do not echo a resume id on stdout. Copy the history
  // scanner's token onto the live tab so a restart can `--resume` it.
  useEffect(() => {
    for (const group of groups) {
      for (const row of group.rows) {
        const token = row.saved.session_token?.trim();
        if (!token || row.live.resumeToken === token) continue;
        dispatch({ type: 'SET_RESUME_TOKEN', id: row.live.id, token });
      }
    }
  }, [groups, dispatch]);

  const startRename = (saved: SavedSession) => {
    const k = `${saved.tool ?? ''}:${saved.id}`;
    cancelRenameRef.current = false;
    setRenamingKey(k);
    setRenameValue(renamed[k] ?? saved.name ?? '');
  };

  const openNewChat = (cwd?: string) => {
    const idle = state.terminals.find((term) => !term.tool && !term.isHidden);
    if (idle) {
      dispatch({ type: 'SET_ACTIVE_TERMINAL', id: idle.id });
      if (cwd) dispatch({ type: 'SET_FOLDER', path: cwd });
      return;
    }
    dispatch({
      type: 'ADD_TERMINAL',
      session: { id: crypto.randomUUID(), tool: null, folderPath: cwd ?? null },
    });
  };

  const openSession = (row: NavRow) => {
    dispatch({ type: 'SET_ACTIVE_TERMINAL', id: row.live.id });
  };

  const closeSession = (row: NavRow, e: React.MouseEvent) => {
    e.stopPropagation();
    dispatch({ type: 'REMOVE_TERMINAL', id: row.live.id });
  };

  const toggleCollapsed = (key: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const onSessionClickGuarded = (row: NavRow) => {
    if (rowDragSuppressClickRef.current) {
      rowDragSuppressClickRef.current = false;
      return;
    }
    openSession(row);
  };

  const onSessionPointerDown = (
    e: ReactPointerEvent<HTMLDivElement>,
    group: ProjectGroup,
    row: NavRow,
  ) => {
    if (e.button !== 0) return;
    const target = e.target as HTMLElement;
    if (target.closest('.nav-session-close, input, textarea, button')) return;
    if (renamingKey === `${row.saved.tool ?? ''}:${row.saved.id}`) return;

    const groupEl = e.currentTarget.closest('.nav-project');
    if (!groupEl) return;

    const rowEls = Array.from(groupEl.querySelectorAll<HTMLElement>('.nav-session[data-order-key]'));
    const positions = rowEls.map((el) => {
      const rect = el.getBoundingClientRect();
      return {
        sessionId: el.dataset.sessionId!,
        orderKey: el.dataset.orderKey!,
        center: rect.top + rect.height / 2,
        height: rect.height,
      };
    });
    const fromIdx = positions.findIndex((p) => p.sessionId === row.live.id);
    if (fromIdx < 0) return;
    const ownPos = positions[fromIdx];

    let slotHeight: number;
    if (fromIdx + 1 < positions.length) {
      slotHeight = positions[fromIdx + 1].center - ownPos.center;
    } else if (fromIdx > 0) {
      slotHeight = ownPos.center - positions[fromIdx - 1].center;
    } else {
      slotHeight = ownPos.height;
    }

    const startY = e.clientY;
    let started = false;
    const THRESHOLD = 6;

    const computeTargetIdx = (clientY: number): number => {
      const draggedCenter = ownPos.center + (clientY - startY);
      let count = 0;
      for (let i = 0; i < positions.length; i++) {
        if (i === fromIdx) continue;
        if (positions[i].center > draggedCenter) return count;
        count++;
      }
      return count;
    };

    const onMove = (ev: PointerEvent) => {
      const dy = ev.clientY - startY;
      if (!started && Math.abs(dy) < THRESHOLD) return;
      if (!started) {
        started = true;
        document.body.style.cursor = 'grabbing';
        document.body.style.userSelect = 'none';
      }
      ev.preventDefault();
      setRowDrag({
        sessionId: row.live.id,
        groupKey: group.key,
        deltaY: dy,
        fromIdx,
        targetIdx: computeTargetIdx(ev.clientY),
        slotHeight,
      });
    };

    const onUp = (ev: PointerEvent) => {
      finish();
      if (!started) return;
      rowDragSuppressClickRef.current = true;
      const swallow = (ce: MouseEvent) => {
        ce.stopImmediatePropagation();
        ce.preventDefault();
        document.removeEventListener('click', swallow, true);
        rowDragSuppressClickRef.current = false;
      };
      document.addEventListener('click', swallow, true);
      window.setTimeout(() => document.removeEventListener('click', swallow, true), 50);
      const targetIdx = computeTargetIdx(ev.clientY);
      const others = positions.filter((_, i) => i !== fromIdx);
      const beforeId = targetIdx < others.length ? others[targetIdx].orderKey : null;
      const visual = positions.map((p) => p.orderKey);
      setGroupOrder(group.key, moveInOrder(visual, row.key, beforeId));
      setRowDrag(null);
    };

    const onKey = (ev: KeyboardEvent) => {
      if (ev.key !== 'Escape') return;
      finish();
      setRowDrag(null);
    };

    const finish = () => {
      document.removeEventListener('pointermove', onMove);
      document.removeEventListener('pointerup', onUp);
      document.removeEventListener('keydown', onKey);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      dragCleanupRef.current = null;
    };

    dragCleanupRef.current?.();
    dragCleanupRef.current = () => {
      finish();
      setRowDrag(null);
    };
    document.addEventListener('pointermove', onMove, { passive: false });
    document.addEventListener('pointerup', onUp);
    document.addEventListener('keydown', onKey);
  };

  return (
    <>
      <button
        type="button"
        className={`nav-new-chat${launchpadOpen ? ' is-active' : ''}`}
        onClick={() => openNewChat()}
      >
        <svg className="nav-new-chat-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
          <path d="M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/>
          <path d="M18.375 2.625a1 1 0 0 1 3 3l-9.013 9.014a2 2 0 0 1-.853.505l-2.873.84a.5.5 0 0 1-.62-.62l.84-2.873a2 2 0 0 1 .506-.852z"/>
        </svg>
        {t('nav.new_chat')}
      </button>

      <div className={`project-session-nav${rowDrag ? ' is-reordering' : ''}`}>
        {groups.length === 0 && (
          <div className="nav-empty">
            <div className="nav-empty-title">{t('nav.empty')}</div>
            <div className="nav-empty-hint">{t('nav.empty_hint')}</div>
          </div>
        )}
        {groups.map((group) => {
          const isCollapsed = collapsed.has(group.key);
          return (
            <div key={group.key} className="nav-project">
              <div className="nav-project-header" onClick={() => toggleCollapsed(group.key)}>
                <svg
                  className={`nav-project-chevron${isCollapsed ? ' is-collapsed' : ''}`}
                  width="12"
                  height="12"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2.4"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                >
                  <path d="M6 9l6 6 6-6" />
                </svg>
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="nav-project-folder">
                  <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/>
                </svg>
                <span className="nav-project-name" title={group.cwd || group.label}>{group.label}</span>
                <button
                  type="button"
                  className="nav-project-add"
                  title={t('nav.new_chat')}
                  onClick={(e) => {
                    e.stopPropagation();
                    openNewChat(group.cwd || undefined);
                  }}
                >
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                    <line x1="12" y1="5" x2="12" y2="19"></line>
                    <line x1="5" y1="12" x2="19" y2="12"></line>
                  </svg>
                </button>
              </div>
              {!isCollapsed && group.rows.map((row, rowIdx) => {
                const session = row.saved;
                const sessionKey = `${session.tool ?? ''}:${session.id}`;
                const displayName = renamed[sessionKey] ?? session.name;
                const isRenaming = renamingKey === sessionKey;
                const active = row.live.id === state.activeTerminalId;
                const status = liveStatus(row.live);
                const statusKey = status === 'wait_input' ? 'waiting' : (status ?? 'idle');
                const isRunning = status === 'working' || Boolean(row.live.chatPending && status !== 'wait_input');
                const isDragging = rowDrag?.sessionId === row.live.id && rowDrag.groupKey === group.key;
                let siblingShift = 0;
                if (rowDrag && rowDrag.groupKey === group.key && !isDragging) {
                  const withoutIdx = rowIdx < rowDrag.fromIdx ? rowIdx : rowIdx - 1;
                  if (rowIdx < rowDrag.fromIdx && withoutIdx >= rowDrag.targetIdx) {
                    siblingShift = rowDrag.slotHeight;
                  } else if (rowIdx > rowDrag.fromIdx && withoutIdx < rowDrag.targetIdx) {
                    siblingShift = -rowDrag.slotHeight;
                  }
                }
                const dragStyle: CSSProperties | undefined = isDragging
                  ? { transform: `translateY(${rowDrag!.deltaY}px)` }
                  : siblingShift !== 0
                    ? { transform: `translateY(${siblingShift}px)` }
                    : undefined;
                return (
                  <div
                    key={row.key}
                    data-session-id={row.live.id}
                    data-order-key={row.key}
                    className={`nav-session${active ? ' is-active' : ''}${isRunning ? ' is-running' : ''}${isDragging ? ' is-dragging' : ''}`}
                    style={dragStyle}
                    aria-grabbed={isDragging}
                    onClick={() => onSessionClickGuarded(row)}
                    onPointerDown={(e) => onSessionPointerDown(e, group, row)}
                    onDragStart={(e) => e.preventDefault()}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      const x = e.clientX;
                      const y = e.clientY;
                      refreshHistory();
                      void (async () => {
                        let next = session;
                        try {
                          const liveToken = await commands.getTerminalSessionToken(row.live.id);
                          if (liveToken) {
                            const hist = getHistorySnapshot().sessions.find((s) => (
                              s.tool === session.tool && s.session_token === liveToken
                            ));
                            next = {
                              ...next,
                              session_token: liveToken,
                              file_path: hist?.file_path || next.file_path,
                            };
                          }
                        } catch { /* token capture is best-effort */ }
                        setCtxMenu({ session: next, x, y });
                      })();
                    }}
                  >
                    <span className={`nav-session-dot status-${statusKey}`} />
                    {getToolIcon(session.tool)}
                    <div className="nav-session-body">
                      {isRenaming ? (
                        <input
                          className="history-card-rename-input"
                          autoFocus
                          value={renameValue}
                          onChange={(e) => setRenameValue(e.target.value)}
                          onClick={(e) => e.stopPropagation()}
                          onContextMenu={(e) => e.stopPropagation()}
                          onKeyDown={(e) => {
                            if (e.key === 'Enter') {
                              e.preventDefault();
                              cancelRenameRef.current = false;
                              setCustomName(session.tool ?? '', session.id, renameValue);
                              setRenamingKey(null);
                            } else if (e.key === 'Escape') {
                              cancelRenameRef.current = true;
                              setRenamingKey(null);
                            }
                          }}
                          onBlur={() => {
                            if (!cancelRenameRef.current) {
                              setCustomName(session.tool ?? '', session.id, renameValue);
                            }
                            cancelRenameRef.current = false;
                            setRenamingKey(null);
                          }}
                          onFocus={(e) => e.target.select()}
                        />
                      ) : (
                        <span className="nav-session-title">{displayName}</span>
                      )}
                      <span className="nav-session-meta">{getToolDisplayName(session.tool)}</span>
                    </div>
                    <div className="nav-session-trailing">
                      {isRunning && (
                        <span
                          className="nav-session-spinner"
                          title={t('nav.status.working')}
                          aria-label={t('nav.status.working')}
                        />
                      )}
                      <button
                        type="button"
                        className="nav-session-close"
                        onClick={(e) => closeSession(row, e)}
                      >
                        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round">
                          <line x1="18" y1="6" x2="6" y2="18"></line>
                          <line x1="6" y1="6" x2="18" y2="18"></line>
                        </svg>
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          );
        })}
      </div>
      {ctxMenu && (
        <SessionContextMenu
          menu={ctxMenu}
          onClose={() => setCtxMenu(null)}
          onRename={startRename}
          onDelete={(saved) => {
            dispatch({ type: 'REMOVE_TERMINAL', id: saved.id });
          }}
        />
      )}
    </>
  );
}
