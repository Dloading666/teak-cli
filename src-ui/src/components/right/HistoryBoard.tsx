import { useState, useEffect, useMemo, useRef, useSyncExternalStore } from 'react';
import type { CSSProperties, PointerEvent as ReactPointerEvent } from 'react';
import { useT } from '../../i18n/useT';
import { useAppState } from '../../store/app-state';
import type { SavedSession } from '../../tauri';
import { commands } from '../../tauri';
import { getToolDisplayName } from '../../lib/tool-info';
import { subscribeRenamed, getRenamedSnapshot, setCustomName, lookupCustomName } from '../../lib/renamed-sessions';
import {
  prefetchHistory,
  refreshHistory,
  subscribeHistory,
  getHistorySnapshot,
  applyNativeTitlePatches,
} from '../../lib/history-cache';
import { SessionContextMenu, type SessionCtxMenuState } from './SessionContextMenu';
import {
  applySessionOrder,
  attachHistoryToLive,
  isGenericSessionName,
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
import {
  getPinnedSnapshot,
  pinSession,
  sessionPinKeys,
  subscribePinned,
  unpinSession,
} from '../../lib/pinned-sessions';
import {
  getHiddenSnapshot,
  hideSession,
  subscribeHidden,
} from '../../lib/hidden-sessions';
import { isPersistableTool } from '../../lib/open-sessions';
import type { TerminalSession, ToolType } from '../../store/app-state';
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
  live?: TerminalSession;
}

interface ProjectGroup {
  key: string;
  cwd: string;
  label: string;
  rows: NavRow[];
  kind: 'pinned' | 'project';
}

/** Resolve a tokenless live tab only when native history gives us a
 * high-confidence, unique match. This is deliberately stricter than the
 * ordinary navigation overlay: a wrong match here would persistently pin the
 * wrong conversation. */
function uniqueHistoryMatchForLive(
  live: TerminalSession,
  sessions: SavedSession[],
): SavedSession | undefined {
  const tool = live.tool ?? '';
  const cwdKey = live.folderPath ? normCwd(live.folderPath) : '';
  // Without a project boundary, title/time alone can collide across unrelated
  // workspaces. Leave the temporary marker unresolved instead of guessing.
  if (!cwdKey) return undefined;
  const candidates = sessions.filter((saved) => (
    saved.tool === tool
    && Boolean(saved.session_token)
    && Boolean(saved.cwd)
    && normCwd(saved.cwd) === cwdKey
  ));
  if (candidates.length === 0) return undefined;

  const fallbackName = getToolDisplayName(tool);
  const title = live.toolTitle?.trim() ?? '';
  if (title && !isGenericSessionName(title, fallbackName)) {
    const byTitle = candidates.filter((saved) => saved.name?.trim() === title);
    if (byTitle.length === 1) return byTitle[0];
  }

  const startedAt = live.startedAt ?? 0;
  if (!startedAt) return undefined;
  const fresh = candidates.filter((saved) => {
    if (!saved.created_at) return false;
    const createdAt = Date.parse(saved.created_at);
    if (!Number.isFinite(createdAt)) {
      const numeric = Number(saved.created_at);
      if (!Number.isFinite(numeric)) return false;
      const normalized = numeric < 1e11 ? numeric * 1000 : numeric;
      return normalized >= startedAt - 120_000;
    }
    return createdAt >= startedAt - 120_000;
  });
  return fresh.length === 1 ? fresh[0] : undefined;
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
  const pinned = useSyncExternalStore(
    subscribePinned,
    getPinnedSnapshot,
    getPinnedSnapshot,
  );
  const hidden = useSyncExternalStore(
    subscribeHidden,
    getHiddenSnapshot,
    getHiddenSnapshot,
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

  const grokTokensKey = state.terminals
    .filter((term) => term.tool === 'grok' && !term.isHidden && term.resumeToken)
    .map((term) => term.resumeToken as string)
    .sort()
    .join('|');

  useEffect(() => {
    if (!grokTokensKey) return;
    const tokens = grokTokensKey.split('|');
    let cancelled = false;
    const tick = () => {
      commands.peekNativeSessionTitles('grok', tokens)
        .then((rows) => {
          if (cancelled || !rows?.length) return;
          applyNativeTitlePatches(rows);
          for (const row of rows) {
            if (row.title_is_manual && row.token && row.name) {
              setCustomName('grok', row.token, row.name);
            }
          }
        })
        .catch(() => {});
    };
    tick();
    const id = window.setInterval(tick, 2000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [grokTokensKey]);

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

    const nativeIdByToken = new Map(
      historySessions
        .filter((saved) => saved.session_token)
        .map((saved) => [`${saved.tool}:${saved.session_token}`, saved.id] as const),
    );
    const markerKeys = (saved: SavedSession, liveId = saved.id) => {
      const nativeId = saved.session_token
        ? nativeIdByToken.get(`${saved.tool}:${saved.session_token}`)
        : undefined;
      return sessionPinKeys(
        saved.tool ?? '',
        liveId,
        saved.session_token,
        nativeId ? [nativeId] : [],
      );
    };
    const matchesMarker = (saved: SavedSession, liveId = saved.id) => (
      markerKeys(saved, liveId).some((candidate) => pinned.has(candidate))
    );
    const represented = new Set(rows.flatMap((row) => markerKeys(row.saved, row.key)));
    const pinnedRows: NavRow[] = rows
      .filter((row) => matchesMarker(row.saved, row.key))
      .map((row) => ({ ...row, key: `pinned:${markerKeys(row.saved, row.key)[0]}` }));
    for (const saved of historySessions) {
      if (!saved.session_token || !isPersistableTool(saved.tool)) continue;
      const identity = sessionPinKeys(saved.tool ?? '', saved.id, saved.session_token);
      if (!identity.some((candidate) => pinned.has(candidate))) continue;
      if (identity.some((candidate) => represented.has(candidate))) continue;
      if (hidden.has(`${saved.tool ?? ''}:${saved.id}`)) continue;
      pinnedRows.push({
        key: `pinned:${identity[0]}`,
        saved,
      });
    }

    const buckets = new Map<string, NavRow[]>();
    const display = new Map<string, string>();
    for (const row of rows) {
      if (matchesMarker(row.saved, row.key)) continue;
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
        kind: 'project',
        rows: applySessionOrder(
          list,
          navOrder[key],
          (row) => row.key,
          (row) => row.live?.startedAt ?? 0,
        ),
      });
    }
    if (pinnedRows.length > 0) {
      const recency = (row: NavRow) => {
        if (row.live?.startedAt) return row.live.startedAt;
        const value = row.saved.created_at ?? row.saved.saved_at;
        const parsed = Date.parse(value);
        if (!Number.isNaN(parsed)) return parsed;
        const numeric = Number(value);
        return Number.isNaN(numeric) ? 0 : numeric < 1e11 ? numeric * 1000 : numeric;
      };
      result.unshift({
        key: '@pinned',
        cwd: '',
        label: t('nav.pinned'),
        kind: 'pinned',
        rows: applySessionOrder(
          pinnedRows,
          navOrder['@pinned'],
          (row) => row.key,
          recency,
        ),
      });
    }
    return result;
  }, [state.terminals, historySessions, navOrder, pinned, hidden, t]);

  useEffect(() => () => { dragCleanupRef.current?.(); }, []);

  // Keep the tab's resume token pointed at the CLI session this row bound.
  // Grok never echoes an id, so this is the only writer. Claude can also
  // land here when OSC/ai-title identifies a newer conversation than the
  // first captured `--resume` id. Do not freeze auto titles into the
  // user-rename table — that made "你好" stick after the CLI moved on.
  // Manual Grok titles (`title_is_manual`) must overwrite a leftover overlay
  // so `/rename` shows up on the left rail.
  useEffect(() => {
    for (const group of groups) {
      for (const row of group.rows) {
        if (!row.live) continue;
        const token = row.saved.session_token?.trim();
        const tool = row.saved.tool ?? '';
        // Prefer the token already owned by the live tab. If history later
        // rebinds the row after `/new` or a fork, that newer overlay must not
        // steal a temporary pin that belonged to the previous conversation.
        const stableToken = row.live.resumeToken?.trim() || token;
        if (stableToken) {
          const nativeId = historySessions.find((saved) => (
            saved.tool === tool && saved.session_token === stableToken
          ))?.id;
          const aliases = nativeId ? [nativeId] : [];
          const legacyMarkers = sessionPinKeys(tool, row.live.id, null, aliases);
          // A brand-new terminal can be pinned before its CLI has exposed a
          // native token. As soon as history binds that token, replace the
          // temporary live-id/native-id marker with the stable token marker so
          // the item survives closing and reopening the terminal.
          if (legacyMarkers.some((candidate) => pinned.has(candidate))) {
            pinSession(tool, row.live.id, stableToken, aliases);
          }
        }
        if (token && row.live.resumeToken !== token) {
          dispatch({ type: 'SET_RESUME_TOKEN', id: row.live.id, token });
        }
        const native = row.saved.name?.trim();
        if (tool === 'grok' && row.saved.title_is_manual && native && token) {
          if (lookupCustomName(tool, row.live.id, token) !== native) {
            setCustomName(tool, token, native);
          }
          if (row.live.toolTitle !== native) {
            dispatch({ type: 'SET_TAB_TITLE', id: row.live.id, title: native });
          }
        }
      }
    }
  }, [groups, dispatch, historySessions, pinned]);

  const saveLabel = (saved: SavedSession, value: string) => {
    const trimmed = value.trim();
    setCustomName(saved.tool ?? '', saved.id, trimmed);
    const token = saved.session_token?.trim();
    if (token) {
      setCustomName(saved.tool ?? '', token, trimmed);
      commands.renameNativeSession(saved.tool ?? '', token, trimmed).catch(() => {});
    }
    dispatch({ type: 'SET_TAB_TITLE', id: saved.id, title: trimmed });
    refreshHistory();
  };

  const startRename = (saved: SavedSession) => {
    const k = `${saved.tool ?? ''}:${saved.id}`;
    cancelRenameRef.current = false;
    setRenamingKey(k);
    setRenameValue(lookupCustomName(saved.tool ?? '', saved.id, saved.session_token) ?? saved.name ?? '');
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
    if (row.live) {
      dispatch({ type: 'SET_ACTIVE_TERMINAL', id: row.live.id });
      return;
    }

    const token = row.saved.session_token?.trim();
    const tool = row.saved.tool;
    if (!token || !isPersistableTool(tool)) return;
    const existing = state.terminals.find((terminal) => (
      !terminal.isHidden && terminal.tool === tool && terminal.resumeToken === token
    ));
    if (existing) {
      dispatch({ type: 'SET_ACTIVE_TERMINAL', id: existing.id });
      return;
    }

    const folderPath = row.saved.cwd?.trim() || null;
    const idle = state.terminals.find((terminal) => !terminal.tool && !terminal.isHidden);
    if (idle) {
      dispatch({ type: 'SET_ACTIVE_TERMINAL', id: idle.id });
      if (folderPath) dispatch({ type: 'SET_FOLDER', path: folderPath });
      dispatch({
        type: 'SET_TERMINAL_TOOL',
        id: idle.id,
        tool: tool as ToolType,
        resumeToken: token,
      });
      if (row.saved.name?.trim()) {
        dispatch({ type: 'SET_TAB_TITLE', id: idle.id, title: row.saved.name.trim() });
      }
      return;
    }

    dispatch({
      type: 'ADD_TERMINAL',
      session: {
        id: crypto.randomUUID(),
        tool: tool as ToolType,
        folderPath,
        resumeToken: token,
        toolTitle: row.saved.name?.trim() || undefined,
        viewMode: 'terminal',
      },
    });
  };

  const closeSession = (row: NavRow, e: React.MouseEvent) => {
    e.stopPropagation();
    if (!row.live) return;
    const live = row.live;
    const tool = live.tool ?? '';
    const legacyMarker = sessionPinKeys(tool, live.id)[0];
    const needsStablePin = Boolean(
      legacyMarker
      && getPinnedSnapshot().has(legacyMarker)
      && !live.resumeToken?.trim()
      && !row.saved.session_token?.trim(),
    );

    // Close immediately from the user's point of view. For the rare case in
    // which a brand-new tokenless tab was pinned and then closed before the
    // normal history refresh bound it, retry the native history scan after the
    // CLI has had a chance to flush its session file. We only accept a unique,
    // high-confidence match, so concurrent sessions cannot be pinned by guess.
    dispatch({ type: 'REMOVE_TERMINAL', id: live.id });
    if (!needsStablePin) return;
    void (async () => {
      for (const delay of [0, 500, 1500]) {
        if (delay > 0) await new Promise((resolve) => window.setTimeout(resolve, delay));
        if (!legacyMarker || !getPinnedSnapshot().has(legacyMarker)) return;
        try {
          const latest = await commands.getNativeHistory(true);
          const matched = uniqueHistoryMatchForLive(live, latest ?? []);
          const token = matched?.session_token?.trim();
          if (!token) continue;
          pinSession(tool, live.id, token, matched?.id ? [matched.id] : []);
          refreshHistory();
          // refreshHistory is intentionally throttled; guarantee one scan
          // after that window so the newly flushed history row replaces the
          // just-closed live row in the pinned shelf.
          window.setTimeout(() => refreshHistory(), 2200);
          return;
        } catch { /* Retry: the CLI may still be flushing its history file. */ }
      }
    })();
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
    const fromIdx = positions.findIndex((p) => p.sessionId === row.key);
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
        sessionId: row.key,
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
          const isPinnedGroup = group.kind === 'pinned';
          const isCollapsed = !isPinnedGroup && collapsed.has(group.key);
          return (
            <div key={group.key} className={`nav-project${isPinnedGroup ? ' is-pinned-group' : ''}`}>
              {isPinnedGroup ? (
                <div className="nav-pinned-header">{group.label}</div>
              ) : (
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
              )}
              {!isCollapsed && group.rows.map((row, rowIdx) => {
                const session = row.saved;
                const sessionKey = `${session.tool ?? ''}:${session.id}`;
                const tokenForName = session.session_token ?? row.live?.resumeToken;
                const overlay = tokenForName
                  ? renamed[`${session.tool ?? ''}:${tokenForName}`]
                  : renamed[sessionKey];
                const displayName = (
                  session.title_is_manual && session.name?.trim()
                    ? session.name.trim()
                    : overlay
                ) ?? session.name;
                const isRenaming = renamingKey === sessionKey;
                const active = row.live?.id === state.activeTerminalId;
                const status = liveStatus(row.live);
                const statusKey = status === 'wait_input' ? 'waiting' : status;
                const isRunning = status === 'working';
                const isDragging = rowDrag?.sessionId === row.key && rowDrag.groupKey === group.key;
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
                    data-session-id={row.key}
                    data-order-key={row.key}
                    className={`nav-session${active ? ' is-active' : ''}${isRunning ? ' is-running' : ''}${!row.live ? ' is-history' : ''}${isDragging ? ' is-dragging' : ''}`}
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
                        if (row.live) {
                          try {
                            const liveToken = await commands.getTerminalSessionToken(row.live.id);
                            if (liveToken) {
                              const hist = getHistorySnapshot().sessions.find((s) => (
                                s.tool === session.tool && s.session_token === liveToken
                              ));
                              const bound = session.session_token?.trim();
                              const osc = row.live.toolTitle?.trim();
                              const liveMatchesTitle = Boolean(
                                osc && hist && (hist.name ?? '').trim() === osc,
                              );
                              if (!bound || bound === liveToken || liveMatchesTitle) {
                                next = {
                                  ...next,
                                  session_token: liveToken,
                                  file_path: hist?.file_path || next.file_path,
                                };
                                if (liveToken !== row.live.resumeToken) {
                                  dispatch({ type: 'SET_RESUME_TOKEN', id: row.live.id, token: liveToken });
                                }
                              }
                            }
                          } catch { /* token capture is best-effort */ }
                        }
                        const native = next.session_token
                          ? getHistorySnapshot().sessions.find((saved) => (
                            saved.tool === next.tool && saved.session_token === next.session_token
                          ))
                          : undefined;
                        setCtxMenu({
                          session: next,
                          x,
                          y,
                          pinAliases: native && native.id !== next.id ? [native.id] : undefined,
                        });
                      })();
                    }}
                  >
                    <span className={`nav-session-dot${statusKey ? ` status-${statusKey}` : ''}`} />
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
                              saveLabel(session, renameValue);
                              setRenamingKey(null);
                            } else if (e.key === 'Escape') {
                              cancelRenameRef.current = true;
                              setRenamingKey(null);
                            }
                          }}
                          onBlur={() => {
                            if (!cancelRenameRef.current) {
                              saveLabel(session, renameValue);
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
                    {row.live && (
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
                    )}
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
            const live = state.terminals.find((terminal) => terminal.id === saved.id);
            if (live) {
              dispatch({ type: 'REMOVE_TERMINAL', id: live.id });
            } else {
              hideSession(saved.tool ?? '', saved.id);
              unpinSession(saved.tool ?? '', saved.id, saved.session_token);
            }
          }}
        />
      )}
    </>
  );
}
