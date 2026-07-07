// Coffee CLI — Global App State (React Context)

import { createContext, useContext, useReducer } from 'react';
import type { ReactNode } from 'react';

// ─── Types ───────────────────────────────────────────────────────────────────

export type ToolType = 'claude' | 'qwen' | 'installer' | 'hermes' | 'opencode' | 'mimocode' | 'openclaw' | 'codex' | 'antigravity' | 'pi' | 'crush' | 'aider' | 'kimicode' | 'goose' | 'copilot' | 'terminal' | 'remote' | 'two-split' | 'three-split' | 'four-split' | null;

/**
 * Tab status shown as an animated 9-dot glyph. Three states only —
 * Claude Code is the only CLI we drive a real status machine for.
 *
 *   idle       — ready for input (green Wave-Double)
 *   working    — LLM generating / tool call in flight (orange Snake-CCW)
 *   wait_input — permission prompt blocking, user must confirm (blue Ripple)
 *
 * CSS classes are `status-idle / -working / -waiting`
 * (the `wait_input → waiting` rename happens at render time).
 */
export type AgentStatus = 'idle' | 'working' | 'wait_input';

// Theme: color palette (orthogonal to shape)
export type ThemeColor =
  | 'dark' | 'light' | 'cappuccino' | 'sakura' | 'lavender' | 'mint'
  | 'obsidian' | 'cobalt' | 'moss'
  // Vibrant batch — saturated accents on tinted-dark bases (crimson is the
  // Spider-Man hero, intended to pair with the carbon shape).
  | 'crimson' | 'sunset' | 'amber' | 'emerald' | 'teal' | 'indigo' | 'fuchsia';
// Theme: shape form (orthogonal to color)
export type ThemeShape = 'soft' | 'slab' | 'sharp' | 'glass' | 'panel' | 'carbon';
// Icon theme: visual style for file/folder icons in the explorer.
// 8 themes, each with genuinely distinct folder silhouette + file icon style.
// Fetched upstream (6): material, vscode-icons, catppuccin-mocha, devicon, fluent, symbols
// Self-authored (2): outline (line-frame), coffee (Coffee CLI brand)
export type IconTheme =
  | 'outline' | 'material' | 'vscode-icons' | 'catppuccin-mocha'
  | 'devicon' | 'fluent' | 'symbols' | 'coffee';

// Unified one-hand hotkey SCHEME for the three chrome toggles — left panel,
// Gambit (妙手), right panel — on three adjacent keys. The user picks one of
// four presets in Settings → 妙手; default Alt+QWE. Each scheme is a single
// modifier + three keys mapped left/gambit/right, so a whole hand rests on the
// row. We match on `e.code` (physical key) because Alt on macOS rewrites
// `e.key` into the typed glyph (œ/∑/´) — only the code is stable — and every
// handler preventDefaults to cancel that glyph AND stop the combo reaching a
// focused terminal. Ctrl+QWE is offered but collides with core terminal editing
// (Ctrl+W/E/Q); it's opt-in, same "you chose it" contract as the old alt-*.
// Literal `Ctrl`/`Alt` (not ⌃/⌥) for the mass-market audience.
export const HOTKEY_SCHEMES = [
  { code: 'alt-qwe',  mod: 'Alt',  modifier: 'alt',
    keys: { left: { key: 'Q', eventCode: 'KeyQ' }, gambit: { key: 'W', eventCode: 'KeyW' }, right: { key: 'E', eventCode: 'KeyE' } } },
  { code: 'ctrl-qwe', mod: 'Ctrl', modifier: 'ctrl',
    keys: { left: { key: 'Q', eventCode: 'KeyQ' }, gambit: { key: 'W', eventCode: 'KeyW' }, right: { key: 'E', eventCode: 'KeyE' } } },
  { code: 'alt-123',  mod: 'Alt',  modifier: 'alt',
    keys: { left: { key: '1', eventCode: 'Digit1' }, gambit: { key: '2', eventCode: 'Digit2' }, right: { key: '3', eventCode: 'Digit3' } } },
  { code: 'ctrl-123', mod: 'Ctrl', modifier: 'ctrl',
    keys: { left: { key: '1', eventCode: 'Digit1' }, gambit: { key: '2', eventCode: 'Digit2' }, right: { key: '3', eventCode: 'Digit3' } } },
] as const;

export type HotkeyScheme = typeof HOTKEY_SCHEMES[number]['code'];
export type HotkeyAction = 'left' | 'gambit' | 'right';

// Which chrome toggle a keydown maps to under the active scheme, or null. The
// caller MUST then preventDefault (cancels the macOS Alt-glyph + the control
// byte) AND stopPropagation (keeps the combo out of xterm's own keydown).
// metaKey is always rejected (macOS Cmd stays a system key); the modifier must
// match EXACTLY (an Alt-scheme ignores Ctrl-held combos and vice-versa) so the
// other modifier's shortcuts pass through untouched.
export function matchHotkeyScheme(e: KeyboardEvent, scheme: HotkeyScheme): HotkeyAction | null {
  const s = HOTKEY_SCHEMES.find(h => h.code === scheme) ?? HOTKEY_SCHEMES[0];
  if (e.metaKey) return null;
  const modOk = s.modifier === 'ctrl' ? (e.ctrlKey && !e.altKey) : (e.altKey && !e.ctrlKey);
  if (!modOk) return null;
  if (e.code === s.keys.left.eventCode) return 'left';
  if (e.code === s.keys.gambit.eventCode) return 'gambit';
  if (e.code === s.keys.right.eventCode) return 'right';
  return null;
}

// The three display combos for a scheme, e.g. { left: 'Alt + Q', gambit: 'Alt + W', right: 'Alt + E' }.
export function schemeLabels(scheme: HotkeyScheme): Record<HotkeyAction, string> {
  const s = HOTKEY_SCHEMES.find(h => h.code === scheme) ?? HOTKEY_SCHEMES[0];
  return {
    left: `${s.mod} + ${s.keys.left.key}`,
    gambit: `${s.mod} + ${s.keys.gambit.key}`,
    right: `${s.mod} + ${s.keys.right.key}`,
  };
}

/// One pane inside a multi-agent Tab. `paneIdx` is 1-indexed (1..4)
/// matching the user-visible badge and the MCP session id suffix —
/// sessionId = `${tabId}::pane-${paneIdx}`. The Rust MCP server's
/// list_panes returns the same ids, so when the user says "pane 2"
/// a CLI's MCP call can target it verbatim.
export interface MultiAgentPane {
  paneIdx: number;
  tool: ToolType;
  toolData?: string;
  agentStatus?: AgentStatus;
  // Per-pane working directory. Only used by the four-split (independent quad) tab
  // where each pane can run in its own project. Multi-agent panes ignore this
  // and use the tab-level folderPath (all 4 panes share one workspace because
  // they coordinate via MCP against that workspace's config).
  folderPath?: string | null;
  // Sentinel Protocol (opt-in per pane). When true, TierTerminal scans the
  // PTY output stream of this pane for the marker `[COFFEE-DONE:pane<N>]`
  // that the user instructs their agent to emit on task completion. On a
  // match, completionTs is set to Date.now() — the pane number badge
  // renders a small green dot while the timestamp is fresh.
  sentinelEnabled?: boolean;
  completionTs?: number;
}

/// State attached to a Tab with `tool === 'multi-agent'`. All four panes
/// are peers — there is no primary/worker distinction — so this type is
/// deliberately minimal. Each pane's CLI and toolData live on
/// `MultiAgentPane`; focus tracking happens inside `<MultiAgentGrid/>`.
interface MultiAgentState {
  panes: MultiAgentPane[];
  // Independent split (`*-split`) only: which pane the user last focused.
  // Drives left Explorer + right Changes target — without it the file panels
  // can't tell which pane's project they should reflect. Multi-agent tabs
  // (`*-agent`, shared folder) ignore this field.
  focusedPaneIdx?: number | null;
}

export interface TerminalSession {
  id: string;
  tool: ToolType;
  toolData?: string;  // Extra context for the tool (e.g. SSH connection JSON for remote)
  folderPath: string | null;
  /// Live terminal title set by the tool via OSC 0/2 (e.g. Claude Code's
  /// conversation summary). When set, the tab shows this instead of the cwd
  /// basename — matching how other terminals display the tool's own title.
  toolTitle?: string;
  restartKey?: number;
  /// When set, this tab was opened from a history "Continue this session"
  /// action. TierTerminal's mount effect passes it to tierTerminalStart,
  /// which spawns the tool with its `--resume <token>` flag instead of a
  /// fresh launch. Cleared on any subsequent SET_TERMINAL_TOOL.
  resumeToken?: string;
  isHidden?: boolean;
  agentStatus?: AgentStatus;
  gambitDraft?: string;    // Unsent textarea content, preserved across tab switches
  /// When present, this Tab renders as a 2×2+ pane grid instead of a
  /// single terminal. See docs/MULTI-AGENT-ARCHITECTURE.md §5.7 and §7.
  multiAgent?: MultiAgentState;
}

// ─── State Shape ─────────────────────────────────────────────────────────────

export interface AppState {
  // UI
  currentTheme: ThemeColor;
  currentShape: ThemeShape;
  currentLang: string;
  iconTheme: IconTheme;

  // Background wallpaper
  bgPath: string;
  bgType: 'image' | 'video' | 'none';
  // Wallpaper image opacity, 0-100 (percent). 100 = fully visible, 0 =
  // fully transparent (image not visible). Default 70 — leaves the
  // theme's base color partially visible underneath so foreground text
  // stays legible on busy wallpapers without a black overlay (the
  // overlay was the previous design and clashed with themed colors).
  wallpaperOpacity: number;

  // Terminal foreground color override ('' = use theme default)
  termColorScheme: string;
  // Terminal font family override ('' = bundled CascadiaMono + fallbacks)
  termFont: string;

  // Terminals
  terminals: TerminalSession[];
  activeTerminalId: string | null;

  // Gambit (global floating compose window). Visibility is app-wide so the
  // panel doesn't appear/disappear when switching tabs; only the draft is
  // per-tab (stored on TerminalSession.gambitDraft).
  gambitOpen: boolean;

  // Personalization settings modal (opened by the titlebar gear). App-level
  // so the titlebar gear and the App-mounted modal share one flag.
  settingsOpen: boolean;

  // Gambit compose-box send key. true → Enter sends (Shift/Ctrl/Cmd+Enter =
  // newline); false → Ctrl/Cmd+Enter sends (Enter = newline). Chosen in the
  // settings modal because the muscle-memory split is per-user / per-OS.
  gambitEnterToSend: boolean;

  // Unified hotkey scheme (settings → 妙手): one of HOTKEY_SCHEMES, driving the
  // three chrome toggles — left panel / Gambit / right panel — via a global
  // capture-phase listener in ActiveGambit.
  hotkeyScheme: HotkeyScheme;

  // IDE-style layout toggles driven from titlebar controls.
  // Default both panels visible — matches first-time user expectation.
  leftPanelHidden: boolean;
  rightPanelHidden: boolean;

  // Multi-agent pane arrangement. 'grid' = 2×2 quadrant (default),
  // 'columns' = 1×4 vertical strip. Only takes effect inside a tab
  // whose tool is 'multi-agent'; other tabs ignore it.
  multiAgentLayout: 'grid' | 'columns';

  // Right-panel task board form. 'list' = compact to-do checklist (default),
  // 'note' = big sticky-note cards (traffic-light status + timestamp + roomy
  // text area). Same task data either way — purely a presentation choice made
  // in the settings modal. Default 'list' so existing users see no change.
  taskViewMode: 'list' | 'note';
}

// ─── Tab tool predicates ────────────────────────────────────────────────────

const SPLIT_TOOLS: ReadonlySet<ToolType> = new Set<ToolType>(['two-split', 'three-split', 'four-split']);
export const isSplitTool = (t: ToolType): boolean => SPLIT_TOOLS.has(t);

// `kind` is a backend protocol contract: `::pane-N` triggers hands-free flag
// injection (yolo / skip-permissions) for coordinated multi-agent; `::split-N`
// leaves them off so each pane prompts as a normal interactive PTY.
export const paneSessionId = (tabId: string, paneIdx: number, kind: 'split' | 'pane'): string =>
  `${tabId}::${kind}-${paneIdx}`;

// ─── Diff context resolver ──────────────────────────────────────────────────
// Split tabs route file-stats to the focused pane's own session+folder.
// Multi-agent and regular tabs use the tab itself. `null` = no diff target.
interface DiffContext {
  sessionId: string;
  folderPath: string;
  tool: ToolType;
}

export function resolveDiffContext(session: TerminalSession | null | undefined): DiffContext | null {
  if (!session) return null;
  if (isSplitTool(session.tool)) {
    const focusedIdx = session.multiAgent?.focusedPaneIdx ?? null;
    if (focusedIdx == null) return null;
    const pane = session.multiAgent?.panes.find(p => p.paneIdx === focusedIdx);
    if (!pane?.tool || !pane.folderPath) return null;
    return {
      sessionId: paneSessionId(session.id, pane.paneIdx, 'split'),
      folderPath: pane.folderPath,
      tool: pane.tool,
    };
  }
  if (!session.folderPath) return null;
  return { sessionId: session.id, folderPath: session.folderPath, tool: session.tool };
}

// ─── Actions ─────────────────────────────────────────────────────────────────

type Action =
  | { type: 'SET_FOLDER'; path: string }
  | { type: 'CLEAR_FOLDER' }
  | { type: 'SET_THEME'; theme: ThemeColor }
  | { type: 'SET_SHAPE'; shape: ThemeShape }
  | { type: 'SET_ICON_THEME'; theme: IconTheme }
  | { type: 'SET_LANG'; lang: string }
  | { type: 'ADD_TERMINAL'; session: TerminalSession }
  | { type: 'REMOVE_TERMINAL'; id: string }
  | { type: 'REORDER_TERMINAL'; sessionId: string; beforeId: string | null }
  | { type: 'SET_ACTIVE_TERMINAL'; id: string | null }
  | { type: 'SET_TERMINAL_TOOL'; id: string; tool: ToolType; toolData?: string; resumeToken?: string }
  | { type: 'SET_TERMINAL_HIDDEN'; id: string; isHidden: boolean }
  | { type: 'RESTART_TERMINAL'; id: string; newId: string }
  | { type: 'OPEN_HYPER_AGENT_TAB' }
  | { type: 'SET_AGENT_STATUS'; id: string; status: AgentStatus }
  | { type: 'SET_BG'; path: string; bgType: 'image' | 'video' }
  | { type: 'CLEAR_BG' }
  | { type: 'SET_WALLPAPER_OPACITY'; opacity: number }
  | { type: 'SET_TERM_SCHEME'; scheme: string }
  | { type: 'SET_TERM_FONT'; font: string }
  | { type: 'TOGGLE_GAMBIT' }
  | { type: 'TOGGLE_SETTINGS' }
  | { type: 'SET_SETTINGS_OPEN'; open: boolean }
  | { type: 'SET_GAMBIT_ENTER_TO_SEND'; value: boolean }
  | { type: 'SET_HOTKEY_SCHEME'; value: HotkeyScheme }
  | { type: 'SET_GAMBIT_DRAFT'; id: string; draft: string }
  | { type: 'SET_PANE_TOOL'; tabId: string; paneIdx: number; tool: ToolType; toolData?: string; folderPath?: string | null }
  | { type: 'SET_PANE_SENTINEL'; tabId: string; paneIdx: number; enabled: boolean }
  | { type: 'SET_PANE_COMPLETION'; tabId: string; paneIdx: number; ts: number }
  | { type: 'SET_FOCUSED_PANE'; tabId: string; paneIdx: number | null }
  | { type: 'TOGGLE_LEFT_PANEL' }
  | { type: 'TOGGLE_RIGHT_PANEL' }
  | { type: 'SET_MULTI_AGENT_LAYOUT'; layout: 'grid' | 'columns' }
  | { type: 'SET_TASK_VIEW_MODE'; mode: 'list' | 'note' }
  | { type: 'SET_TAB_TITLE'; id: string; title: string };

// ─── Reducer ─────────────────────────────────────────────────────────────────

function reducer(state: AppState, action: Action): AppState {
  switch (action.type) {
    case 'SET_FOLDER':
      // Persist as the "last folder" so a fresh launch lands here instead
      // of the C-drive default. Read back in getInitialState().
      try { localStorage.setItem('cc-folder', action.path); } catch {}
      return {
        ...state,
        terminals: state.terminals.map(t => t.id === state.activeTerminalId ? { ...t, folderPath: action.path } : t)
      };
    case 'CLEAR_FOLDER':
      try { localStorage.removeItem('cc-folder'); } catch {}
      return {
        ...state,
        terminals: state.terminals.map(t => t.id === state.activeTerminalId ? { ...t, folderPath: null } : t)
      };
    case 'SET_TAB_TITLE':
      // OSC 0/2 title from the tool (xterm onTitleChange). Skip if the session
      // is gone (late dispatch after dispose) or unchanged (onTitleChange can
      // fire frequently) — both avoid a redundant CenterPanel re-render.
      if (!state.terminals.some(t => t.id === action.id)) return state;
      if (state.terminals.some(t => t.id === action.id && t.toolTitle === action.title)) return state;
      return { ...state, terminals: state.terminals.map(t => t.id === action.id ? { ...t, toolTitle: action.title } : t) };
    case 'SET_THEME':
      return { ...state, currentTheme: action.theme };
    case 'SET_SHAPE':
      return { ...state, currentShape: action.shape };
    case 'SET_ICON_THEME':
      return { ...state, iconTheme: action.theme };
    case 'SET_LANG':
      return { ...state, currentLang: action.lang };
    case 'ADD_TERMINAL':
      return { 
        ...state, 
        terminals: [...state.terminals, action.session],
        activeTerminalId: action.session.id 
      };
    case 'REMOVE_TERMINAL': {
      let newTerminals = state.terminals.filter(t => t.id !== action.id);
      let newActiveId = state.activeTerminalId;
      
      if (newTerminals.length === 0) {
        const defaultId = crypto.randomUUID();
        const folderPath = state.terminals.length > 0 ? state.terminals[0].folderPath : null;
        newTerminals = [{ id: defaultId, tool: null, folderPath }];
        newActiveId = defaultId;
      } else if (state.activeTerminalId === action.id) {
         newActiveId = newTerminals[newTerminals.length - 1].id;
      }
      return { ...state, terminals: newTerminals, activeTerminalId: newActiveId };
    }
    case 'REORDER_TERMINAL': {
      // Move `sessionId`'s tab so that it sits immediately before
      // `beforeId` in the array. `beforeId === null` means "drop at end".
      // Used by browser-style tab reordering: pointer-down a tab, drag
      // horizontally, drop wherever you want it. CenterPanel does the
      // pixel-math; the reducer just handles the array surgery.
      const t = state.terminals;
      const fromIdx = t.findIndex(x => x.id === action.sessionId);
      if (fromIdx < 0) return state;
      const without = t.filter(x => x.id !== action.sessionId);
      const insertIdx = action.beforeId
        ? without.findIndex(x => x.id === action.beforeId)
        : without.length;
      if (insertIdx < 0) return state;
      const moved = t[fromIdx];
      const next = [...without.slice(0, insertIdx), moved, ...without.slice(insertIdx)];
      // No-op detection: skip dispatch round-trip when the order didn't
      // actually change (e.g., user dragged 1px and dropped, or dropped
      // back into the same gap).
      if (next.every((x, i) => x.id === t[i].id)) return state;
      return { ...state, terminals: next };
    }
    case 'SET_ACTIVE_TERMINAL':
      return { ...state, activeTerminalId: action.id };
    case 'SET_TERMINAL_TOOL':
      return {
        ...state,
        terminals: state.terminals.map(t => t.id === action.id ? { ...t, tool: action.tool, toolData: action.toolData, resumeToken: action.resumeToken, toolTitle: undefined } : t)
      };
    case 'SET_TERMINAL_HIDDEN':
      return {
        ...state,
        terminals: state.terminals.map(t => t.id === action.id ? { ...t, isHidden: action.isHidden } : t)
      };
    case 'RESTART_TERMINAL':
      return {
        ...state,
        terminals: state.terminals.map(t =>
          t.id === action.id ? { ...t, id: action.newId, toolTitle: undefined } : t
        ),
        activeTerminalId: state.activeTerminalId === action.id ? action.newId : state.activeTerminalId
      };
    case 'SET_AGENT_STATUS':
      return {
        ...state,
        terminals: state.terminals.map(t => t.id === action.id ? { ...t, agentStatus: action.status } : t)
      };
    case 'SET_BG':
      return { ...state, bgPath: action.path, bgType: action.bgType };
    case 'CLEAR_BG':
      return { ...state, bgPath: '', bgType: 'none' };
    case 'SET_WALLPAPER_OPACITY':
      return { ...state, wallpaperOpacity: Math.max(0, Math.min(100, action.opacity)) };
    case 'SET_TERM_SCHEME':
      return { ...state, termColorScheme: action.scheme };
    case 'SET_TERM_FONT':
      return { ...state, termFont: action.font };
    case 'TOGGLE_GAMBIT':
      return { ...state, gambitOpen: !state.gambitOpen };
    case 'TOGGLE_SETTINGS':
      return { ...state, settingsOpen: !state.settingsOpen };
    case 'SET_SETTINGS_OPEN':
      return { ...state, settingsOpen: action.open };
    case 'SET_GAMBIT_ENTER_TO_SEND':
      return { ...state, gambitEnterToSend: action.value };
    case 'SET_HOTKEY_SCHEME':
      return { ...state, hotkeyScheme: action.value };
    case 'SET_GAMBIT_DRAFT':
      return {
        ...state,
        terminals: state.terminals.map(t => t.id === action.id ? { ...t, gambitDraft: action.draft } : t)
      };
    case 'SET_PANE_TOOL': {
      // Seed a MultiAgentState lazily on the first pane selection so
      // quadrant tabs don't need a separate enable-step — point of entry
      // is the user clicking a CLI button in any empty pane slot.
      return {
        ...state,
        terminals: state.terminals.map(t => {
          if (t.id !== action.tabId) return t;
          const existing = t.multiAgent?.panes
            ?? ([1, 2, 3, 4].map(i => ({ paneIdx: i, tool: null as ToolType })) as MultiAgentPane[]);
          const panes = existing.map(p =>
            p.paneIdx === action.paneIdx
              ? {
                  ...p,
                  tool: action.tool,
                  toolData: action.toolData,
                  // Only overwrite folderPath when the action explicitly
                  // carries one. Clearing a pane (tool=null without folderPath)
                  // wipes the pane back to empty state, so we also null out
                  // the stored folder to avoid ghost state.
                  folderPath: action.folderPath !== undefined
                    ? action.folderPath
                    : (action.tool === null ? null : p.folderPath),
                }
              : p
          );
          return { ...t, multiAgent: { ...t.multiAgent, panes } };
        }),
      };
    }
    case 'SET_PANE_SENTINEL': {
      return {
        ...state,
        terminals: state.terminals.map(t => {
          if (t.id !== action.tabId) return t;
          const existing = t.multiAgent?.panes
            ?? ([1, 2, 3, 4].map(i => ({ paneIdx: i, tool: null as ToolType })) as MultiAgentPane[]);
          const panes = existing.map(p =>
            p.paneIdx === action.paneIdx ? { ...p, sentinelEnabled: action.enabled } : p
          );
          return { ...t, multiAgent: { ...t.multiAgent, panes } };
        }),
      };
    }
    case 'SET_PANE_COMPLETION': {
      return {
        ...state,
        terminals: state.terminals.map(t => {
          if (t.id !== action.tabId) return t;
          if (!t.multiAgent) return t;
          const panes = t.multiAgent.panes.map(p =>
            p.paneIdx === action.paneIdx ? { ...p, completionTs: action.ts } : p
          );
          return { ...t, multiAgent: { ...t.multiAgent, panes } };
        }),
      };
    }
    case 'SET_FOCUSED_PANE': {
      const tab = state.terminals.find(t => t.id === action.tabId);
      if (!tab) return state;
      if ((tab.multiAgent?.focusedPaneIdx ?? null) === action.paneIdx) return state;
      return {
        ...state,
        terminals: state.terminals.map(t => {
          if (t.id !== action.tabId) return t;
          const ma = t.multiAgent
            ?? { panes: [1, 2, 3, 4].map(i => ({ paneIdx: i, tool: null as ToolType })) as MultiAgentPane[] };
          return { ...t, multiAgent: { ...ma, focusedPaneIdx: action.paneIdx } };
        }),
      };
    }
    case 'TOGGLE_LEFT_PANEL': {
      const next = !state.leftPanelHidden;
      try { localStorage.setItem('cc-left-hidden', next ? '1' : '0'); } catch {}
      return { ...state, leftPanelHidden: next };
    }
    case 'TOGGLE_RIGHT_PANEL': {
      const next = !state.rightPanelHidden;
      try { localStorage.setItem('cc-right-hidden', next ? '1' : '0'); } catch {}
      return { ...state, rightPanelHidden: next };
    }
    case 'SET_MULTI_AGENT_LAYOUT': {
      try { localStorage.setItem('cc-ma-layout', action.layout); } catch {}
      return { ...state, multiAgentLayout: action.layout };
    }
    case 'SET_TASK_VIEW_MODE': {
      try { localStorage.setItem('cc-task-view', action.mode); } catch {}
      return { ...state, taskViewMode: action.mode };
    }
    default:
      return state;
  }
}

// ─── Initial State ────────────────────────────────────────────────────────────

const VALID_THEMES: ThemeColor[] = [
  'dark', 'light', 'cappuccino', 'sakura', 'lavender', 'mint',
  'obsidian', 'cobalt', 'moss',
  'crimson', 'sunset', 'amber', 'emerald', 'teal', 'indigo', 'fuchsia',
];
const VALID_SHAPES: ThemeShape[] = ['soft', 'slab', 'sharp', 'glass', 'panel', 'carbon'];
const VALID_ICON_THEMES: IconTheme[] = [
  'outline', 'material', 'vscode-icons', 'catppuccin-mocha',
  'devicon', 'fluent', 'symbols', 'coffee',
];

function getInitialState(): AppState {
  let theme: ThemeColor = 'dark';
  // Default 'carbon' — matches the index.html pre-paint script (also carbon)
  // so there's no soft→carbon flash on mount. Carbon shares Soft's radii
  // (16/8/9999), so the rounded + titlebar-tab showcase is preserved; it just
  // layers the hex mesh + translucent chrome on top. Screenshots skew heavily
  // to Carbon despite the old soft default = users actively choose it, so it's
  // the right out-of-box. KEEP index.html's pre-paint default in sync with
  // this value or first paint flashes.
  let shape: ThemeShape = 'carbon';
  let iconTheme: IconTheme = 'devicon';
  let lang = 'zh-CN';
  let folderPath: string | null = null;

  try {
    const savedTheme = localStorage.getItem('cc-theme') as ThemeColor | null;
    if (savedTheme && VALID_THEMES.includes(savedTheme)) theme = savedTheme;
  } catch {}

  try {
    const savedShape = localStorage.getItem('cc-shape') as ThemeShape | null;
    if (savedShape && VALID_SHAPES.includes(savedShape)) shape = savedShape;
  } catch {}

  try {
    const savedIconTheme = localStorage.getItem('cc-icon-theme') as IconTheme | null;
    if (savedIconTheme && VALID_ICON_THEMES.includes(savedIconTheme)) iconTheme = savedIconTheme;
  } catch {}

  try { folderPath = localStorage.getItem('cc-folder'); } catch {}

  try {
    const savedLang = localStorage.getItem('cc-lang');
    if (savedLang) lang = savedLang;
  } catch {}

  // No factory-default wallpaper — the bundled /wallpapers/default.png
  // didn't load reliably across platforms (Linux WebKit asset URL
  // resolution diverges from Windows/macOS WebView2/WKWebView), so a
  // chunk of new users saw a black panel and assumed wallpaper was
  // broken. Default is now an empty wallpaper; users who want one pick
  // their own via the theme menu.
  let bgPath = '';
  let bgType: 'image' | 'video' | 'none' = 'none';
  let termColorScheme = '';
  let termFont = '';
  let wallpaperOpacity = 70;
  // Default Enter-to-send; only opt-out if the user explicitly stored 'false'.
  let gambitEnterToSend = true;
  // Default hotkey scheme = Alt+QWE (left=Q / Gambit=W / right=E) — one-hand
  // ergonomics + a memorable, purpose-built feel. matchHotkeyScheme + each
  // handler's preventDefault cancel the macOS Alt dead-key glyphs (œ/∑/´). The
  // Ctrl+QWE preset is opt-in despite clashing with terminal Ctrl+W/E/Q.
  // Overridden only by a stored valid scheme.
  let hotkeyScheme: HotkeyScheme = 'alt-qwe';
  try {
    const storedPath = localStorage.getItem('cc-bg-path');
    const storedType = localStorage.getItem('cc-bg-type') as 'image' | 'video' | 'none' | null;

    // Migration: clear legacy seeded /wallpapers/default.png from
    // existing installs so they don't keep trying to load a file we
    // no longer ship. Anything else (user-picked) is preserved.
    if (storedPath && storedPath.startsWith('/wallpapers/')) {
      bgPath = '';
      bgType = 'none';
      try {
        localStorage.removeItem('cc-bg-path');
        localStorage.removeItem('cc-bg-type');
        localStorage.removeItem('cc-bg-init');
      } catch {}
    } else {
      bgPath = storedPath || '';
      bgType = storedType || 'none';
    }

    termColorScheme = localStorage.getItem('cc-term-scheme') || '';
    termFont = localStorage.getItem('cc-term-font') || '';
    gambitEnterToSend = localStorage.getItem('cc-gambit-enter-send') !== 'false';
    const storedScheme = localStorage.getItem('cc-hotkey-scheme');
    if (storedScheme && HOTKEY_SCHEMES.some(h => h.code === storedScheme)) {
      hotkeyScheme = storedScheme as HotkeyScheme;
    }
    // New key (post-refactor): wallpaper opacity, 0-100, larger = more
    // visible. Old key was `cc-wallpaper-dim` (0-80, larger = darker
    // overlay). On first load after upgrade, fall back to the legacy
    // key with `opacity ≈ 100 - dim` so the user's perceived brightness
    // stays close to what they had set, then write the new key.
    const savedOpacity = localStorage.getItem('cc-wallpaper-opacity');
    if (savedOpacity !== null) {
      const n = parseInt(savedOpacity, 10);
      if (!Number.isNaN(n) && n >= 0 && n <= 100) wallpaperOpacity = n;
    } else {
      const savedDim = localStorage.getItem('cc-wallpaper-dim');
      if (savedDim !== null) {
        const n = parseInt(savedDim, 10);
        if (!Number.isNaN(n) && n >= 0 && n <= 80) {
          wallpaperOpacity = Math.max(0, Math.min(100, 100 - n));
        }
        try { localStorage.removeItem('cc-wallpaper-dim'); } catch {}
      }
    }
  } catch {}

  const defaultTerminalId = crypto.randomUUID();

  let leftPanelHidden = false;
  let rightPanelHidden = false;
  let multiAgentLayout: 'grid' | 'columns' = 'grid';
  // Default 'note' (big sticky-note view) — the welcome guide + the whole
  // task-board UX are tuned for it. A saved pref in localStorage (written by
  // the SET_TASK_VIEW_MODE toggle) still wins for returning users. Previously
  // 'list', only flipped to 'note' by the TaskBoard first-launch seed — which
  // was fragile (cleared localStorage or a removed seed fell back to list).
  let taskViewMode: 'list' | 'note' = 'note';
  try {
    leftPanelHidden = localStorage.getItem('cc-left-hidden') === '1';
    rightPanelHidden = localStorage.getItem('cc-right-hidden') === '1';
    const savedLayout = localStorage.getItem('cc-ma-layout');
    if (savedLayout === 'columns' || savedLayout === 'grid') multiAgentLayout = savedLayout;
    const savedTaskView = localStorage.getItem('cc-task-view');
    if (savedTaskView === 'list' || savedTaskView === 'note') taskViewMode = savedTaskView;
  } catch {}

  return {
    currentTheme: theme,
    currentShape: shape,
    iconTheme,
    currentLang: lang,
    bgPath,
    bgType,
    wallpaperOpacity,
    termColorScheme,
    termFont,
    terminals: [{ id: defaultTerminalId, tool: null, folderPath }],
    activeTerminalId: defaultTerminalId,
    gambitOpen: false,
    settingsOpen: false,
    gambitEnterToSend,
    hotkeyScheme,
    leftPanelHidden,
    rightPanelHidden,
    multiAgentLayout,
    taskViewMode,
  };
}

// ─── Context ─────────────────────────────────────────────────────────────────
//
// Two separate contexts so components that only need to dispatch (not read
// state) don't get re-rendered on every state change. This is what lets the
// React.memo'd TierTerminal skip re-renders when unrelated state updates fire.

const StateContext = createContext<AppState | null>(null);
const DispatchContext = createContext<React.Dispatch<Action> | null>(null);

// Kept for backward compatibility with existing consumers that read both
// state and dispatch from a single hook. New code should prefer the split
// hooks below.
const AppContext = createContext<{
  state: AppState;
  dispatch: React.Dispatch<Action>;
} | null>(null);

export function AppProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(reducer, undefined, getInitialState);
  // The combined-context value has to be recomputed whenever state changes,
  // so keeping the split contexts lets hot components subscribe only to the
  // half they care about.
  const combined = { state, dispatch };
  return (
    <DispatchContext.Provider value={dispatch}>
      <StateContext.Provider value={state}>
        <AppContext.Provider value={combined}>
          {children}
        </AppContext.Provider>
      </StateContext.Provider>
    </DispatchContext.Provider>
  );
}

export function useAppState() {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error('useAppState must be inside AppProvider');
  return ctx;
}

/**
 * Dispatch-only hook for components that don't need to read state.
 *
 * Components using this hook do NOT re-render when state changes — the
 * DispatchContext value (the dispatch function itself) is stable across
 * every render, so useContext never triggers a subscription update.
 *
 * Use this in any hot-path component (e.g. TierTerminal) that reads all of
 * its state via props and only needs to call dispatch() in event handlers.
 */
export function useAppDispatch(): React.Dispatch<Action> {
  const ctx = useContext(DispatchContext);
  if (!ctx) throw new Error('useAppDispatch must be inside AppProvider');
  return ctx;
}
