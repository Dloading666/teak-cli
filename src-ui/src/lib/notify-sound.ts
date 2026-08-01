// notify-sound.ts — audible cue when an agent finishes a turn or starts
// waiting for permission, so the user doesn't have to keep watching the
// terminal to know it's done.
//
// Signal source: Redux store's agentStatus, which is the SAME source the
// dynamic island uses. Only Claude, Codex, and Grok populate it, directly from
// their native terminal titles.
//
// Reading the store keeps sound transitions identical to the visible native
// title state and avoids an independent event path.
//
// Sounds are synthesized with WebAudio — no audio assets, no WebView2
// permission prompts. Two distinct chimes:
//   done — rising two-note (E5 → A5), "turn finished"
//   wait — lower double-beep (A4 ×2), "needs your input"
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

// Codex uses its activity title while starting up (including MCP init), then
// returns to idle before the user has submitted a turn. Keep that first cycle
// visually accurate but silent; normal notification behavior begins after the
// first idle title confirms startup is complete.
const codexSoundReady = new Set<string>();

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

function tone(ac: AudioContext, freq: number, start: number, dur: number, peak: number) {
  const osc = ac.createOscillator();
  const gain = ac.createGain();
  osc.type = 'sine';
  osc.frequency.value = freq;
  gain.gain.setValueAtTime(0, start);
  gain.gain.linearRampToValueAtTime(peak, start + 0.02);
  gain.gain.exponentialRampToValueAtTime(0.0001, start + dur);
  osc.connect(gain).connect(ac.destination);
  osc.start(start);
  osc.stop(start + dur + 0.05);
}

/** Play one of the two notification chimes. Exported so Settings can offer
 *  a "preview" button per kind. */
export function playNotifySound(kind: NotifyKind) {
  const ac = audioCtx();
  if (!ac) return;
  const t0 = ac.currentTime + 0.01;
  if (kind === 'done') {
    tone(ac, 659.25, t0, 0.18, 0.08);       // E5
    tone(ac, 880.0, t0 + 0.12, 0.30, 0.08); // A5 — rising resolve
  } else {
    tone(ac, 440.0, t0, 0.12, 0.07);        // A4
    tone(ac, 440.0, t0 + 0.18, 0.12, 0.07); // A4 — insistent double-beep
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
  for (const id of codexSoundReady) {
    if (!currentCodexIds.has(id)) codexSoundReady.delete(id);
  }

  // Check all terminals for transitions
  for (const terminal of nativeTerminals) {
    const currentStatus = terminal.agentStatus;
    if (!currentStatus) continue;

    const prev = prevStatus.get(terminal.id);

    // Update tracking
    prevStatus.set(terminal.id, currentStatus);

    // Codex's first working -> idle transition is CLI initialization, not a
    // completed user turn. Arm sounds at that idle boundary and suppress all
    // startup transitions; the dynamic island remains unaffected.
    if (terminal.tool === 'codex' && !codexSoundReady.has(terminal.id)) {
      if (currentStatus === 'idle') codexSoundReady.add(terminal.id);
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
