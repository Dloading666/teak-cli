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
    .split(/\s+[-–—]\s+/)
    .map((part) => part.trim())
    .filter(Boolean);

  const actionRequired = parts.some((part) => ACTION_REQUIRED_RE.test(part));
  // A leftover spinner frame after the turn ends is common — Grok stops
  // sending titles and leaves ⠋ in OSC 0. Only a live activity label means
  // the agent is still working.
  const working = parts.some((part) => isActivityLabel(part));

  const stableParts = parts.filter((part) => (
    part.toLowerCase() !== 'grok'
    && !ACTION_REQUIRED_RE.test(part)
    && !GROK_ACTIVITY_FRAMES.has(part)
    && !isActivityLabel(part)
    && !isTurnTimer(part)
  ));

  return {
    status: actionRequired ? 'wait_input' : working ? 'working' : 'idle',
    displayTitle: stableParts.join(' - ') || 'Grok Build',
  };
}

function isTurnTimer(part: string): boolean {
  return /^(?:\d+[hms](?:\s*\d+[ms])?|\d+:\d{2}|Worked for\b.*)$/i.test(part);
}

function isActivityLabel(part: string): boolean {
  // Bare "Waiting" is the idle prompt (waiting for the user), not a turn.
  if (/^(?:Thinking|Responding|Compacting|Running tool)$/i.test(part)) return true;
  if (/^Running:\s+/i.test(part)) return true;
  if (/^Retrying\s*\(\d+\/\d+\)$/i.test(part)) return true;
  if (/^(?:Waiting for response|Waiting on subagent|Waiting on task output|Waiting on tasks|Waiting on plan approval|Sleeping)…?$/i.test(part)) return true;
  // Tool descriptions (`Read foo.ts…`). Session names and leftover path-like
  // titles must not keep the rail spinning after the turn ends.
  if (part.endsWith('…') && /^[A-Z][A-Za-z].{0,80}…$/.test(part) && !/[/~]/.test(part)) return true;
  return false;
}
