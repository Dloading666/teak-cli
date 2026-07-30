// notify-sound.ts — audible cue when an agent finishes a turn or starts
// waiting for permission, so the user doesn't have to keep watching the
// terminal to know it's done.
//
// Signal source: Redux store's agentStatus (driven by subscribeAgentStatus in
// App.tsx), which is the SAME source the dynamic island uses. This ensures
// audio notifications match the visual status exactly — if the island says
// "idle", the chime plays; if it says "wait_input", the beep plays.
//
// Previous design used a separate raw listen() to agent-status events, which
// duplicated the bus logic and caused false positives (e.g., playing "wait"
// sounds even when permissions were auto-approved). By reading the store
// instead, we inherit all the bus's deduplication, auto-idle fallback
// filtering, and state-transition logic.
//
// Sounds are synthesized with WebAudio — no audio assets, no WebView2
// permission prompts. Two distinct chimes:
//   done — rising two-note (E5 → A5), "turn finished"
//   wait — lower double-beep (A4 ×2), "needs your input"
//
// User controls (Settings ▸ Sound, localStorage `cc-*` keys, all default ON):
//   cc-sound-done            — chime when a turn completes
//   cc-sound-wait            — chime on permission / input prompts
//   cc-sound-only-unfocused  — only chime when the window is unfocused OR the
//                              finished tab isn't the one being viewed

import type { AgentStatus } from '../store/app-state';

export type NotifyKind = 'done' | 'wait';

let ctx: AudioContext | null = null;

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
    const val = localStorage.getItem(key);
    // cc-sound-only-unfocused defaults to OFF so users with 1 window hear the chime.
    // cc-sound-done / cc-sound-wait default to ON.
    if (key === 'cc-sound-only-unfocused') {
      return val === 'true';
    }
    return val !== 'false';
  } catch { return key !== 'cc-sound-only-unfocused'; }
}

/** Watch agentStatus changes and chime on meaningful transitions.
 *  Call this from a useEffect that depends on state.terminals (which contain
 *  agentStatus), so it fires whenever Redux updates any terminal's status.
 *  Returns a cleanup function. */
export function initNotifySound(
  terminals: Array<{ id: string; agentStatus?: AgentStatus }>,
  getActiveTabId: () => string | null,
): () => void {
  const prevStatus = new Map<string, AgentStatus>();

  // Check all terminals for transitions
  for (const terminal of terminals) {
    const currentStatus = terminal.agentStatus;
    if (!currentStatus) continue;

    const prev = prevStatus.get(terminal.id);

    // Update tracking
    prevStatus.set(terminal.id, currentStatus);

    // Skip if no previous state or no change
    if (!prev || prev === currentStatus) continue;

    // Detect meaningful transitions
    const becameIdle = (prev === 'working' || prev === 'wait_input') && currentStatus === 'idle';
    const becameWaiting = currentStatus === 'wait_input' && prev !== 'wait_input';

    if (!becameIdle && !becameWaiting) continue;

    const kind: NotifyKind = becameIdle ? 'done' : 'wait';
    if (!enabled(kind === 'done' ? 'cc-sound-done' : 'cc-sound-wait')) continue;

    // "Only when not looking": skip when the window is focused AND the
    // finished tab is the one on screen.
    if (enabled('cc-sound-only-unfocused') &&
        document.hasFocus() && terminal.id === getActiveTabId()) continue;

    playNotifySound(kind);
  }

  // Cleanup function (no-op for this sync implementation)
  return () => {
    prevStatus.clear();
  };
}
