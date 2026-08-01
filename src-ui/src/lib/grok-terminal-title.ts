import type { AgentStatus } from '../store/app-state';

// Grok Build's default title config is:
// action-required, spinner, activity, session-name, grok.
// These frames come directly from upstream's TitleManager.
const GROK_ACTIVITY_FRAMES = new Set([
  '⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧',
]);

const ACTION_REQUIRED_RE = /^⚠(?:\uFE0F)?\s*Action Required$/i;

export interface GrokTerminalTitleState {
  status: AgentStatus;
  /** Stable Coffee tab title with Grok's animated state components removed. */
  displayTitle: string;
}

/** Read Grok Build's native OSC 0 title as the authoritative agent state. */
export function parseGrokTerminalTitle(title: string): GrokTerminalTitleState {
  const parts = title
    .split(/\s+-\s+/)
    .map((part) => part.trim())
    .filter(Boolean);

  const actionRequired = parts.some((part) => ACTION_REQUIRED_RE.test(part));
  const working = parts.some((part) => (
    GROK_ACTIVITY_FRAMES.has(part) || isActivityLabel(part)
  ));

  const stableParts = parts.filter((part) => (
    part.toLowerCase() !== 'grok'
    && !ACTION_REQUIRED_RE.test(part)
    && !GROK_ACTIVITY_FRAMES.has(part)
    && !isActivityLabel(part)
  ));

  return {
    status: actionRequired ? 'wait_input' : working ? 'working' : 'idle',
    displayTitle: stableParts.join(' - ') || 'Grok Build',
  };
}

function isActivityLabel(part: string): boolean {
  return /^(?:Thinking|Responding|Compacting|Waiting|Running tool)$/i.test(part)
    || /^Running:\s+/i.test(part)
    || /^Retrying\s*\(\d+\/\d+\)$/i.test(part)
    || /^(?:Waiting for response|Waiting on subagent|Waiting on task output|Waiting on tasks|Sleeping)…?$/i.test(part)
    // Tool descriptions and named waits are rendered as `<subject>…`.
    || part.endsWith('…');
}
