// notify-sound.ts — audible cue when an agent finishes a turn or starts
// waiting for permission, so the user doesn't have to keep watching the
// terminal to know it's done.
//
// Signal source: the same `agent-status` Tauri event the status bus
// consumes (see agent-status-bus.ts). We deliberately do NOT use
// subscribeAgentStatus() here: its `activeEmit` is a singleton that every
// subscriber overwrites, and App.tsx's dispatch must stay the winner for
// Codex optimistic updates — so we open our own raw listen() instead.
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

import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { AgentStatus } from '../store/app-state';

interface AgentStatusPayload {
  tab_id: string;
  tool: string;
  status: AgentStatus;
  event: string;
}

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

/** Subscribe to agent-status events and chime on meaningful transitions.
 *  Returns an unsubscribe function. */
export function initNotifySound(getActiveTabId: () => string | null): () => void {
  const prevStatus = new Map<string, AgentStatus>();
  let unlisten: UnlistenFn | null = null;
  let cancelled = false;

  listen<AgentStatusPayload>('agent-status', (evt) => {
    const p = evt.payload;
    const prev = prevStatus.get(p.tab_id);
    prevStatus.set(p.tab_id, p.status);

    // The 30s stale-status fallback (AutoIdleFallback) is housekeeping, not
    // a real "agent finished" signal — never chime for it.
    const becameIdle =
      (prev === 'working' || prev === 'wait_input') &&
      p.status === 'idle' &&
      p.event !== 'AutoIdleFallback';
    // Dedupe repeated wait_input emits for the same prompt.
    const becameWaiting = p.status === 'wait_input' && prev !== 'wait_input';
    if (!becameIdle && !becameWaiting) return;

    const kind: NotifyKind = becameIdle ? 'done' : 'wait';
    if (!enabled(kind === 'done' ? 'cc-sound-done' : 'cc-sound-wait')) return;
    // "Only when not looking": skip when the window is focused AND the
    // finished tab is the one on screen.
    if (enabled('cc-sound-only-unfocused') &&
        document.hasFocus() && p.tab_id === getActiveTabId()) return;
    playNotifySound(kind);
  }).then((fn) => {
    if (cancelled) fn();
    else unlisten = fn;
  });

  return () => {
    cancelled = true;
    if (unlisten) unlisten();
  };
}
