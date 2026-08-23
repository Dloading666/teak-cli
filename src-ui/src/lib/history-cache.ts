// History cache — app-level singleton for session history list.
//
// Why this exists:
//   - get_native_history parses up to N jsonl/json files; doing it lazily on
//     HistoryBoard mount makes the tab feel frozen on first open.
//   - Instead, App.tsx prefetches on startup and the result is stored here,
//     so switching to the History tab is instantaneous from the user's POV.
//
// The store follows the useSyncExternalStore contract so React subscribers
// re-render automatically when status/sessions change.
//
// Refresh strategy (issue: "会话记录列表始终是第一次打开软件时的,要重启才能看到新的"):
//   `prefetchHistory` is idempotent (no-ops after the first load) — it does NOT
//   re-fetch. `refreshHistory` is the non-idempotent counterpart: debounced +
//   throttled + inFlight-guarded, it silently swaps in fresh data without
//   flipping to the loading skeleton (no flicker). Triggers wired up across
//   the app:
//     • Explorer — clicking the 会话记录 tab (re-entering the History view).
//     • App.tsx `initHistoryAutoRefresh` — window-foreground (alt-tab back) +
//       a 60s background poll while foregrounded.
//   `refreshHistory` no-ops until the initial prefetch has run (status ===
//   'idle'), so users who never open the History tab pay zero polling cost.

import { commands, isTauri } from '../tauri';
import type { NativeSessionTitle, SavedSession } from '../tauri';
import { onWindowForeground, onWindowBackground } from './window-focus-filter';

type HistoryStatus = 'idle' | 'loading' | 'ready' | 'error';

interface HistoryState {
  sessions: SavedSession[];
  status: HistoryStatus;
}

let state: HistoryState = { sessions: [], status: 'idle' };
const listeners = new Set<() => void>();

// Refresh plumbing — see refreshHistory / doFetch.
let inFlight = false;
let lastFetchAt = 0;
let refreshTimer: ReturnType<typeof setTimeout> | null = null;
/** Skip a refresh within this window of the last fetch — multiple triggers
 *  (tab click + foreground + poll) routinely fire within a second of each
 *  other and one fetch is enough. */
const REFRESH_THROTTLE_MS = 2000;
/** Coalesce rapid triggers into a single fetch. */
const REFRESH_DEBOUNCE_MS = 400;
/** Background poll interval while the window is foregrounded. */
const POLL_INTERVAL_MS = 60_000;

function emit() {
  for (const l of listeners) l();
}

function sortByMtime(list: SavedSession[]): SavedSession[] {
  const copy = [...list];
  copy.sort((a, b) => {
    let ams = Date.parse(a.saved_at);
    if (isNaN(ams)) {
      const n = Number(a.saved_at);
      if (!isNaN(n)) ams = n < 1e11 ? n * 1000 : n;
    }
    let bms = Date.parse(b.saved_at);
    if (isNaN(bms)) {
      const n = Number(b.saved_at);
      if (!isNaN(n)) bms = n < 1e11 ? n * 1000 : n;
    }
    return (bms || 0) - (ams || 0);
  });
  return copy;
}

/// Perform a fetch. `isRefresh = false` is the initial prefetch (show loading +
/// clear the list so the skeleton reads as "loading"). `isRefresh = true`
/// keeps the previous sessions + `ready` status visible and swaps silently
/// when the fetch lands — no flicker. Guarded by `inFlight` (no concurrent
/// fetches) and a throttle (no re-fetch within REFRESH_THROTTLE_MS of the
/// last one). Refresh is a no-op until the first prefetch has run.
function doFetch(isRefresh: boolean) {
  if (inFlight) return;
  if (isRefresh && state.status === 'idle') return; // never opened History
  if (isRefresh && Date.now() - lastFetchAt < REFRESH_THROTTLE_MS) return;
  inFlight = true;
  lastFetchAt = Date.now();
  if (!isRefresh) {
    state = { sessions: [], status: 'loading' };
    emit();
  }
  commands.getNativeHistory()
    .then(sessions => {
      state = { sessions: sortByMtime(sessions || []), status: 'ready' };
      emit();
    })
    .catch(err => {
      console.error('[history-cache] fetch failed:', err);
      if (!isRefresh) {
        // Initial load failed — surface the error (skeleton → error state).
        state = { ...state, status: 'error' };
        emit();
      }
      // Refresh failed — keep the previous data visible (silent).
    })
    .finally(() => { inFlight = false; });
}

/** Kick off the background fetch. Idempotent — second call while loading or
 *  after ready is a no-op. Safe to call from App mount and from HistoryBoard. */
export function prefetchHistory(): void {
  if (!isTauri) return;
  if (state.status === 'loading' || state.status === 'ready') return;
  doFetch(false);
}

/** Force a re-fetch of the history list (non-idempotent). Debounced so a
 *  burst of triggers (tab click + foreground + poll landing together)
 *  collapses into one fetch; throttled so we don't re-fetch more often than
 *  every REFRESH_THROTTLE_MS. No-op until the initial prefetch has run, so
 *  users who never open the History tab aren't polled. Safe to call from
 *  Explorer's tab onClick and from the window-foreground / poll listeners. */
export function refreshHistory(): void {
  if (!isTauri) return;
  if (refreshTimer) clearTimeout(refreshTimer);
  refreshTimer = setTimeout(() => {
    refreshTimer = null;
    doFetch(true);
  }, REFRESH_DEBOUNCE_MS);
}

export function subscribeHistory(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function getHistorySnapshot(): HistoryState {
  return state;
}

/** Patch live Grok titles from peek_native_session_titles without a full rescan. */
export function applyNativeTitlePatches(patches: NativeSessionTitle[]): void {
  if (!patches.length || state.status !== 'ready') return;
  const byToken = new Map(
    patches
      .filter((p) => p.token && p.name)
      .map((p) => [p.token, p] as const),
  );
  if (byToken.size === 0) return;
  let changed = false;
  const sessions = state.sessions.map((session) => {
    const token = session.session_token;
    if (!token) return session;
    const patch = byToken.get(token);
    if (!patch) return session;
    if (session.name === patch.name && Boolean(session.title_is_manual) === patch.title_is_manual) {
      return session;
    }
    changed = true;
    return { ...session, name: patch.name, title_is_manual: patch.title_is_manual };
  });
  if (!changed) return;
  state = { sessions, status: 'ready' };
  emit();
}

/// Wire up the background auto-refresh triggers. Call once from a
/// always-mounted component (App.tsx). Returns a cleanup that tears down
/// the listeners + interval. refreshHistory no-ops until the initial
/// prefetch has run, so this is safe to install unconditionally — users who
/// never open the History tab pay only the 60s setInterval tick (a function
/// call that early-returns).
export function initHistoryAutoRefresh(): () => void {
  if (!isTauri) return () => {};
  let pollTimer: ReturnType<typeof setInterval> | null = null;
  const startPoll = () => {
    if (pollTimer) return;
    pollTimer = setInterval(() => refreshHistory(), POLL_INTERVAL_MS);
  };
  const stopPoll = () => {
    if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
  };
  startPoll();
  // Alt-tab back: refresh immediately (user may have run sessions in another
  // window / external terminal while we were hidden) AND resume the poll.
  const unsubFg = onWindowForeground(() => {
    refreshHistory();
    startPoll();
  });
  // Window hidden: pause the poll (no point scanning for a window nobody is
  // looking at). onWindowBackground fires after the SETTLE_MS gate, so the
  // spurious start_dragging blur+focus pair on Windows doesn't flap it.
  const unsubBg = onWindowBackground(stopPoll);
  return () => {
    unsubFg();
    unsubBg();
    stopPoll();
  };
}
