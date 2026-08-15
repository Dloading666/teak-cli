// ActiveGambit.tsx — app-level host for the docked compose panel.
//
// Gambit is a global overlay docked at the bottom of the center panel, and
// its Send target is always the currently active tab. To keep it isolated
// from per-tab re-renders (xterm output, agent status events, etc.), it
// lives at the App level instead of inside any TierTerminal.
//
// This wrapper:
// - Reads the active tab's gambit state (open / draft) from the reducer
// - Wires Send through the registry so the text ends up in the right xterm
// - Hands a stable set of props to the memoized Gambit component so parent
//   re-renders don't ripple into it.
//
// Visibility is global (state.gambitOpen) so the panel doesn't flicker
// in/out when the user switches tabs. Draft content remains per-tab —
// switching tabs swaps what's shown inside the (still-open) panel so
// text can't be misdirected to the wrong terminal.

import { useCallback, useEffect } from 'react';
import { useAppState, isSplitTool, paneSessionId, matchHotkeyScheme } from '../../store/app-state';
import { getTabActions } from '../../lib/tab-actions';
import { getFocusedPane } from '../../lib/pane-focus';
import { getToolIcon } from './CenterPanel';
import { Gambit } from './Gambit';

// Last path segment of a folder, Windows ("\") and POSIX ("/") safe. Local copy
// (CenterPanel has the same helper) — tiny, not worth a shared util yet.
function basename(p: string): string {
  const trimmed = p.replace(/[\\/]+$/, '');
  if (!trimmed) return '/';
  const parts = trimmed.split(/[\\/]/);
  return parts[parts.length - 1] || trimmed;
}

export function ActiveGambit() {
  const { state, dispatch } = useAppState();
  const activeId = state.activeTerminalId;
  const activeSession = activeId
    ? state.terminals.find(t => t.id === activeId)
    : undefined;

  const gambitOpen = state.gambitOpen;
  const gambitDraft = activeSession?.gambitDraft ?? '';
  // Active tab's working-folder name — the footer label telling the user which
  // workspace Send would land in. Recomputes on tab switch (activeSession
  // changes), so a long-lived open Gambit can't misdirect a prompt. '' when the
  // tab has no folder (blank terminal) → the label hides.
  const workspaceName =
    activeSession?.folderPath && activeSession.tool
      ? basename(activeSession.folderPath)
      : '';
  // The active tab's tool glyph — the same icon its chrome-tab shows, so the
  // Gambit workspace chip reads as "tool + directory" (mirrors the tab's
  // icon+name pairing). null session → undefined → Gambit hides the icon.
  const toolIcon = activeSession ? getToolIcon(activeSession.tool) : undefined;

  const handleDraftChange = useCallback((draft: string) => {
    if (!activeId) return;
    dispatch({ type: 'SET_GAMBIT_DRAFT', id: activeId, draft });
  }, [dispatch, activeId]);

  const handleClose = useCallback(() => {
    dispatch({ type: 'TOGGLE_GAMBIT' });
  }, [dispatch]);

  // Route Send to the correct xterm. For a plain single-terminal tab the
  // sessionId is just activeId. For a multi-pane tab, no xterm registers
  // under activeId itself — each pane registers under a suffixed id and
  // Gambit has to pick one. Two families of multi-pane tabs exist:
  //
  //   - Orchestrated multi-agent (`multi-agent` / `two-agent` /
  //     `three-agent`, rendered by MultiAgentGrid) uses the `::pane-N`
  //     suffix; backend treats that prefix as "hands-free mode" and
  //     injects auto-approve flags.
  //
  //   - Independent split (`two-split` / `three-split` / `four-split`,
  //     rendered by FourSplitGrid) uses the `::split-N` suffix; each
  //     pane is a plain user-interactive PTY with no auto-approve.
  //
  // Both write to the same `pane-focus` registry on click (tab-scoped
  // 1..N), so routing only has to pick the right prefix.
  //
  // If no pane has been focused yet, return false so Gambit preserves
  // the draft rather than dropping text into the void.
  const handleSend = useCallback((text: string): boolean => {
    if (!activeId) return false;
    const tool = activeSession?.tool ?? null;
    let targetId = activeId;
    if (isSplitTool(tool)) {
      const paneIdx = getFocusedPane(activeId);
      if (!paneIdx) return false;
      targetId = paneSessionId(activeId, paneIdx, 'split');
    }
    const actions = getTabActions(targetId);
    if (!actions) return false;
    return actions.paste(text);
  }, [activeId, activeSession?.tool]);

  // Global open/close hotkey (settings → 妙手). Registered in the CAPTURE
  // phase on document so it fires BEFORE the focused xterm's own keydown —
  // preventDefault then stops the combo (e.g. Ctrl+~) from leaking a control
  // byte into the terminal. NOT gated on gambitOpen: ActiveGambit stays mounted
  // app-wide even while the panel is closed, so this ONE listener drives all
  // three chrome toggles (left panel / Gambit / right panel) under the active
  // scheme. Auto-repeat events are still suppressed (preventDefault) but don't
  // re-toggle, so holding a key neither flickers nor leaks a byte.
  const scheme = state.hotkeyScheme;
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      // Match FIRST — do NOT gate on `e.isComposing` up here. A matched combo
      // always holds Alt or Ctrl, which a Chinese/Japanese IME composition never
      // does, so a match is unambiguously a deliberate hotkey; and non-matching
      // keys fall straight through untouched, so IME input is unaffected either
      // way. An `if (e.isComposing) return` ahead of the match would silently
      // drop macOS DEAD keys — Option+E (accent ´, the default Alt+QWE "right
      // panel" key) can report isComposing=true on its keydown in WebKit, which
      // would swallow the toggle on Mac.
      const action = matchHotkeyScheme(e, scheme);
      if (!action) return;
      // Suppress the combo for EVERY matching event — including auto-repeat —
      // so a held key never leaks a byte into the xterm during the ~1 frame
      // before the toggle lands. Only the initial (non-repeat) press acts.
      e.preventDefault();
      e.stopPropagation();
      if (e.repeat) return;
      dispatch({
        type: action === 'left' ? 'TOGGLE_LEFT_PANEL'
            : action === 'right' ? 'TOGGLE_RIGHT_PANEL'
            : 'TOGGLE_GAMBIT',
      });
    };
    document.addEventListener('keydown', onKeyDown, true);
    return () => document.removeEventListener('keydown', onKeyDown, true);
  }, [scheme, dispatch]);

  // Persist the open/closed choice across restarts. Gambit defaults to open on
  // first launch (see app-state initializer) but the user's toggle sticks —
  // cc-gambit-open is the single source that initializer reads back.
  useEffect(() => {
    try { localStorage.setItem('cc-gambit-open', state.gambitOpen ? '1' : '0'); } catch {}
  }, [state.gambitOpen]);

  if (!gambitOpen || !activeId) return null;

  return (
    <Gambit
      sessionId={activeId}
      draft={gambitDraft}
      workspaceName={workspaceName}
      toolIcon={toolIcon}
      onDraftChange={handleDraftChange}
      onClose={handleClose}
      onSend={handleSend}
      leftPanelHidden={state.leftPanelHidden}
      rightPanelHidden={state.rightPanelHidden}
    />
  );
}
