import type { AgentStatus } from '../store/app-state';

// Grok Build's default title config is:
// action-required, spinner, activity, session-name, grok.
// These frames come directly from upstream's TitleManager.
const GROK_ACTIVITY_FRAMES = new Set([
  '⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏',
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
  if (/^(?:Thinking|Responding|Compacting|Waiting|Running tool|Recording)$/i.test(part)) return true;
  if (/^Running:\s+/i.test(part)) return true;
  if (/^Retrying\s*\(\d+\/\d+\)$/i.test(part)) return true;
  if (/^(?:Waiting for response|Waiting on subagent|Waiting on task output|Waiting on tasks|Waiting on plan approval|Sleeping)…?$/i.test(part)) return true;
  // Tool descriptions (`Read foo.ts…`). A leftover ellipsis on the session
  // name after the turn ends must not keep the rail spinning.
  if (part.endsWith('…') && !/[/~]/.test(part) && !/^\d/.test(part)) return true;
  return false;
}
