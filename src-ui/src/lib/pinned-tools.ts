import type { ToolType } from '../store/app-state';

// ToolType includes `null` (an empty pane). A pane picker only ever offers a
// concrete tool, so work in the non-null subset.
type PaneToolKey = NonNullable<ToolType>;

// Tools eligible to fill a split pane — the launchpad's AI-CLIs plus the plain
// terminal (a pane CAN be a terminal). Excludes only Coffee 101 and the split
// views themselves (a pane can't be a nested split).
export const PANE_TOOL_KEYS: readonly PaneToolKey[] = [
  'claude', 'opencode', 'mimocode', 'openclaw', 'codex', 'antigravity', 'qwen',
  'hermes', 'pi', 'crush', 'aider', 'kimicode', 'goose', 'copilot', 'terminal',
];
const PANE_TOOL_SET = new Set<string>(PANE_TOOL_KEYS as readonly string[]);

// The user's pinned AI-CLI tools, in pinned order, read from the SAME
// `coffee_pinned_items` localStorage the launchpad ("选择工具") reads/writes —
// so the split-pane picker shows exactly what the user pinned instead of a
// hardcoded list. CenterPanel persists the pins (including the first-launch
// defaults) on mount, and FourSplitGrid only ever renders inside CenterPanel,
// so the key is already set by the time this runs. Falls back to the full
// AI-CLI set when the user has pinned none of them, so the picker is never empty.
export function getPinnedPaneToolKeys(): PaneToolKey[] {
  try {
    const raw = localStorage.getItem('coffee_pinned_items');
    if (raw) {
      const arr: unknown = JSON.parse(raw);
      if (Array.isArray(arr)) {
        const keys = arr
          .filter((id): id is string => typeof id === 'string' && id.startsWith('agent:'))
          .map((id) => id.slice('agent:'.length))
          .filter((k): k is PaneToolKey => PANE_TOOL_SET.has(k));
        if (keys.length > 0) return keys;
      }
    }
  } catch {}
  return [...PANE_TOOL_KEYS];
}
