import { useState, useEffect, useMemo, useRef, useSyncExternalStore } from 'react';
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
import { attachHistoryToLive, liveStatus, normCwd, pathBasename } from '../../lib/session-nav';
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
  const [renamingKey, setRenamingKey] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');
  const cancelRenameRef = useRef(false);
  const [ctxMenu, setCtxMenu] = useState<SessionCtxMenuState | null>(null);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

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
      result.push({ key, cwd, label, rows: list });
    }
    return result;
  }, [state.terminals, historySessions, t]);

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

      <div className="project-session-nav">
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
              {!isCollapsed && group.rows.map((row) => {
                const session = row.saved;
                const sessionKey = `${session.tool ?? ''}:${session.id}`;
                const displayName = renamed[sessionKey] ?? session.name;
                const isRenaming = renamingKey === sessionKey;
                const active = row.live.id === state.activeTerminalId;
                const status = liveStatus(row.live);
                const statusKey = status === 'wait_input' ? 'waiting' : (status ?? 'idle');
                return (
                  <div
                    key={row.key}
                    className={`nav-session${active ? ' is-active' : ''}`}
                    onClick={() => openSession(row)}
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
