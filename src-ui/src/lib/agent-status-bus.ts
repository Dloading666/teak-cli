// Agent Status Bus
//
// Listens to the `agent-status` Tauri event emitted by the Rust hook server
// (which in turn receives forwarded events from Claude Code / Qwen Code via
// the Python hook script, and from OpenCode / MiMo via the JS plugin). Each
// payload carries a tab_id and a status that is dispatched straight into
// AppState's agentStatus slot for that tab.
//
// Blue (wait_input) is driven by DIRECT signals only — no timeout heuristic:
//   Claude   — Notification hook subtype permission_prompt (forwarded by the
//              Rust hook server as wait_input).
//   OpenCode — permission.updated / question.asked bus events (JS plugin).
//   MiMo     — same as OpenCode (OpenCode fork, same plugin).
// Blue clears the moment the agent resumes (next PreToolUse/PostToolUse/Stop,
// user input, or the 30s auto-idle fallback) — we don't track approve/deny,
// just "waiting" vs "running again" (mirrors reference/open-vibe-island
// SessionState.swift's direct-event model).
//
// Previously a 3500ms "PreToolUse → no PostToolUse → infer wait_input"
// timeout lived here; it caused false blues during any tool call >3.5s (long
// Bash, file reads). Removed — the direct signals above are authoritative.

import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { AgentStatus } from '../store/app-state';

interface AgentStatusPayload {
  tab_id: string;
  tool: string;
  status: AgentStatus;
  event: string;
}

/** Fallback timer: any non-idle status that's gone this long without a
 *  follow-up event is assumed stale. Protects against hook drops and the
 *  "Claude finished but forgot to emit Stop" case that leaves the dot blue. */
const AUTO_IDLE_MS = 30_000;

/** Per-tab auto-idle timers (one per non-idle status). */
const idleTimers = new Map<string, number>();

/** Most recent emit function from the active subscription. Lets
 *  notifyUserInputSubmitted() route into the same pipeline as real
 *  hook events. Null before subscribe / after unsubscribe. */
let activeEmit: ((p: AgentStatusPayload) => void) | null = null;

/** Start / reset the auto-idle fallback for a given tab. */
function armAutoIdle(tabId: string, tool: string) {
  const existing = idleTimers.get(tabId);
  if (existing) clearTimeout(existing);
  const timer = window.setTimeout(() => {
    idleTimers.delete(tabId);
    if (activeEmit) {
      activeEmit({ tab_id: tabId, tool, status: 'idle', event: 'AutoIdleFallback' });
    }
  }, AUTO_IDLE_MS);
  idleTimers.set(tabId, timer);
}

/** Optimistic-update hook for CLIs that don't expose a "turn started"
 *  signal. Currently used only by Codex tabs (notify protocol only emits
 *  agent-turn-complete = idle). Claude and OpenCode have authoritative
 *  upstream signals (UserPromptSubmit hook / session.status busy) and
 *  must NOT call this — doing so caused 30 s false-positive working
 *  states on local slash commands like /help, /mcp, /clear. */
export function notifyUserInputSubmitted(tabId: string, tool: string) {
  if (!activeEmit) return;
  activeEmit({ tab_id: tabId, tool, status: 'working', event: 'UserSubmitted' });
  armAutoIdle(tabId, tool);
}

export function subscribeAgentStatus(
  onPayload: (payload: AgentStatusPayload) => void,
): () => void {
  let unlisten: UnlistenFn | null = null;
  let cancelled = false;
  activeEmit = onPayload;

  listen<AgentStatusPayload>('agent-status', (evt) => {
    const p = evt.payload;
    // Any real event resets the auto-idle clock; an `idle` status clears it.
    if (p.status === 'idle') {
      const it = idleTimers.get(p.tab_id);
      if (it) { clearTimeout(it); idleTimers.delete(p.tab_id); }
    } else {
      armAutoIdle(p.tab_id, p.tool);
    }
    onPayload(p);
  }).then((fn) => {
    if (cancelled) {
      fn();
    } else {
      unlisten = fn;
    }
  });

  return () => {
    cancelled = true;
    activeEmit = null;
    // Clean up every tab's auto-idle timer on unsubscribe.
    for (const timer of idleTimers.values()) clearTimeout(timer);
    idleTimers.clear();
    if (unlisten) unlisten();
  };
}
