// notify-sound.ts — audible cue when an agent finishes a turn or starts
// waiting for permission, so the user doesn't have to keep watching the
// terminal to know it's done.
//
// Signal source: Redux store's agentStatus, which is the SAME source the
// dynamic island uses. Only Claude, Codex, and Grok populate it, directly from
// their native terminal titles.
//
// Reading the store keeps sound transitions identical to the visible native
// title state and avoids an independent event path. Codex additionally requires
// a local user-submission marker solely to exclude its startup title cycle.
//
// Sounds are synthesized with WebAudio — no audio assets, no WebView2
// permission prompts. The two chimes are ported from DeepSeek-Reasonix's
// synthesized sound set (https://github.com/esengine/DeepSeek-Reasonix):
//   done — "Generation complete": rising E6 → G6 → C7 triad
//   wait — "Awaiting response": A6 → E6 descending fifth
//
// User controls (Settings ▸ Sound, localStorage `cc-*` keys, both default ON):
//   cc-sound-done  — chime when a turn completes
//   cc-sound-wait  — chime on permission / input prompts
// (A "only when window unfocused" toggle was removed — it silently muted all
// chimes for single-window users, who are always focused on their one tab.)

import {
  supportsNativeAgentStatus,
  type AgentStatus,
  type ToolType,
} from '../store/app-state';

export type NotifyKind = 'done' | 'wait';

let ctx: AudioContext | null = null;

// Persistent across effect re-runs. initNotifySound is called from a useEffect
// that depends on state.terminals, which produces a new array on every
// SET_AGENT_STATUS — so the effect re-runs on every status change. If this Map
// were a function-local, it would be recreated empty each call, `prev` would
// always be undefined, and the transition detection below would never fire
// (i.e. no sound ever plays). Module scope keeps the last-seen status alive
// across calls.
const prevStatus = new Map<string, AgentStatus>();

// Codex emits real working/idle title transitions during startup. Seeing an
// initial idle title is not enough to distinguish that cycle from a completed
// turn, so Codex notifications stay muted until the user actually submits
// input in that terminal. This is local UI state, not a CLI hook.
const codexPromptSubmitted = new Set<string>();

/** Arm Codex notifications after a real terminal/Gambit submission. */
export function markNotifySoundPromptSubmitted(sessionId: string, tool: ToolType) {
  if (tool === 'codex') codexPromptSubmitted.add(sessionId);
}

/** Lazy singleton AudioContext. Created on first play (almost always after
 *  a user gesture, so autoplay policy is satisfied); resume() covers the
 *  suspended-state edge on stricter WebViews. */
function audioCtx(): AudioContext | null {
  try {
    if (!ctx) ctx = new AudioContext();
    if (ctx.state === 'suspended') ctx.resume().catch(() => {});
    return ctx;
  } catch {
    return null; // WebAudio unavailable — silence is an acceptable degrade
  }
}

/** Synth a single Reasonix-style note: a sine tone plus a quiet 4× overtone
 *  "shimmer" for sparkle (ported from DeepSeek-Reasonix's playSynthNote). */
function note(ac: AudioContext, freq: number, start: number, dur: number, peak: number) {
  const osc = ac.createOscillator();
  const gain = ac.createGain();
  osc.type = 'sine';
  osc.frequency.value = freq;
  gain.gain.setValueAtTime(0, start);
  gain.gain.linearRampToValueAtTime(peak, start + 0.002);
  gain.gain.exponentialRampToValueAtTime(0.001, start + dur);
  osc.connect(gain).connect(ac.destination);
  osc.start(start);
  osc.stop(start + dur);

  const shimmer = ac.createOscillator();
  const sGain = ac.createGain();
  shimmer.type = 'sine';
  shimmer.frequency.value = freq * 4;
  sGain.gain.setValueAtTime(0, start);
  sGain.gain.linearRampToValueAtTime(peak * 0.12, start + 0.002);
  sGain.gain.exponentialRampToValueAtTime(0.001, start + dur * 0.6);
  shimmer.connect(sGain).connect(ac.destination);
  shimmer.start(start);
  shimmer.stop(start + dur);
}

/** Play one of the two notification chimes. Exported so Settings can offer
 *  a "preview" button per kind. */
export function playNotifySound(kind: NotifyKind) {
  const ac = audioCtx();
  if (!ac) return;
  const t0 = ac.currentTime + 0.01;
  if (kind === 'done') {
    // Reasonix "Generation complete": rising E6 → G6 → C7 triad
    note(ac, 1318.5, t0, 0.20, 0.12);
    note(ac, 1568.0, t0 + 0.07, 0.22, 0.10);
    note(ac, 2093.0, t0 + 0.14, 0.30, 0.08);
  } else {
    // Reasonix "Awaiting response": A6 → E6 descending fifth
    note(ac, 1760.0, t0, 0.14, 0.10);
    note(ac, 1318.5, t0 + 0.09, 0.22, 0.08);
  }
}

function enabled(key: string): boolean {
  try {
    return localStorage.getItem(key) !== 'false';
  } catch { return true; }
}

/** Watch agentStatus changes and chime on meaningful transitions.
 *  Call this from a useEffect that depends on state.terminals (which contain
 *  agentStatus), so it fires whenever Redux updates any terminal's status.
 *  Returns a cleanup function. */
export function initNotifySound(
  terminals: Array<{ id: string; tool: ToolType; agentStatus?: AgentStatus }>,
): () => void {
  const nativeTerminals = terminals.filter(terminal => supportsNativeAgentStatus(terminal.tool));
  const currentNativeIds = new Set(nativeTerminals.map(terminal => terminal.id));
  const currentCodexIds = new Set(
    nativeTerminals.filter(terminal => terminal.tool === 'codex').map(terminal => terminal.id),
  );

  for (const id of prevStatus.keys()) {
    if (!currentNativeIds.has(id)) prevStatus.delete(id);
  }
  for (const id of codexPromptSubmitted) {
    if (!currentCodexIds.has(id)) codexPromptSubmitted.delete(id);
  }

  // Check all terminals for transitions
  for (const terminal of nativeTerminals) {
    const currentStatus = terminal.agentStatus;
    if (!currentStatus) continue;

    const prev = prevStatus.get(terminal.id);

    // Update tracking
    prevStatus.set(terminal.id, currentStatus);

    // Codex drives the island immediately, but startup transitions remain
    // silent until this session has received an actual user submission.
    if (terminal.tool === 'codex' && !codexPromptSubmitted.has(terminal.id)) {
      continue;
    }

    // Skip if no previous state or no change
    if (!prev || prev === currentStatus) continue;

    // Detect meaningful transitions
    const becameIdle = (prev === 'working' || prev === 'wait_input') && currentStatus === 'idle';
    const becameWaiting = currentStatus === 'wait_input' && prev !== 'wait_input';

    if (!becameIdle && !becameWaiting) continue;

    const kind: NotifyKind = becameIdle ? 'done' : 'wait';
    if (!enabled(kind === 'done' ? 'cc-sound-done' : 'cc-sound-wait')) continue;

    playNotifySound(kind);
  }

  // Cleanup: no-op. Live native-tab IDs are pruned at the start of each call.
  // Clearing here would run before every effect re-run and erase the previous
  // status needed for transition detection.
  return () => {};
}
