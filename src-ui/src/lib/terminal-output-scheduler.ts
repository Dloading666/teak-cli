// terminal-output-scheduler.ts — Single shared cooperative output scheduler.
//
// BEFORE this existed, every TierTerminal's onOutput handler called
// `xterm.write(data)` directly on every PTY chunk. xterm.js gives each
// Terminal its own internal WriteBuffer timer (~12ms), so with N tabs each
// producing output, N timers competed on the shared renderer main thread and
// starved the focused terminal — the "3rd tab hangs" symptom. Orca's own
// comment describes this verbatim ("non-focused panes can produce output
// continuously. Letting every pane call xterm.write immediately schedules one
// xterm WriteBuffer timer per pane, which starves the focused terminal on the
// shared renderer thread").
//
// This module is the Teak CLI port of Orca's `pane-terminal-output-scheduler`:
//   • ONE module-global drain timer for ALL background tabs (not one per tab).
//   • Foreground (active) tab writes immediately via xterm.write — xterm's
//     own WriteBuffer paces it, and there's only ONE fg tab so no competition.
//   • Background tabs buffer into a per-session queue, drained at a 16ms
//     interval, max 2 writes / 8ms time-budget per tick (cooperative — yields
//     so a bg tab running `yes` cannot pin the UI).
//   • 2MB LOSSY cap per background tab: once exceeded, chunks are dropped and a
//     one-time in-order warning is enqueued. This is the correct trade for our
//     AI-CLI use case — a backgrounded agent MUST keep running (backpressure to
//     the child would stall Claude Code's own stdout write), and dropped
//     terminal display is acceptable because the agent's full conversation is
//     preserved in its own JSONL session file (--resume recovers it).
//
// Visibility routing is driven by the same IntersectionObserver that flips
// `setSessionActive` to the Rust backend (TierTerminal.tsx) — setActive(true)
// also triggers a bounded resume drain (Phase 3) so switching back to a tab
// that ran a build doesn't beachball for 2s parsing megabytes.
//
// References: D:/Coffee-CLI/reference/orca/src/renderer/src/lib/pane-manager/
// pane-terminal-output-scheduler.ts (~1100 lines; this is a simplified port —
// drops Orca's BSU/ESU ?2026 synchronized-output cursor-show stripping, which
// we don't need yet).

import type { Terminal } from '@xterm/xterm';

// ── Tunables ──────────────────────────────────────────────────────────────────

/** Per-background-tab byte cap. Exceeding → drop + one-time in-order warning. */
const MAX_BG_QUEUE = 2 * 1024 * 1024; // 2 MB
/** Interval between background drain ticks (single shared timer). */
const BG_DRAIN_INTERVAL_MS = 16;
/** Max term.write() calls per background session per drain tick. */
const MAX_WRITES_PER_DRAIN = 2;
/** Cooperative time budget per drain tick across ALL sessions — yield after. */
const DRAIN_BUDGET_MS = 8;
/** On tab activation, sync-flush up to this many bytes for immediate content. */
const RESUME_FLUSH_SYNC = 256 * 1024; // 256 KB

/** One-time in-order warning when the 2MB cap is hit. Pushed as a chunk so it
 *  lands at the correct position (where drops started) in the buffer. */
const DROP_WARNING =
  '\r\n\x1b[33m[Teak CLI: 后台输出积压超过 2MB,已丢弃部分历史输出。切回此 tab 查看实时输出,完整对话见 session 文件]\x1b[0m\r\n';

// ── Per-session state ────────────────────────────────────────────────────────

interface SessionQueue {
  term: Terminal;
  /** FIFO of buffered PTY chunks awaiting term.write while backgrounded. */
  chunks: string[];
  /** Total bytes currently in `chunks` (for the 2MB cap check). */
  bytes: number;
  /** True once the 2MB cap has been hit — guards the one-time warning. */
  dropped: boolean;
  /** Foreground (write immediately) vs background (buffer + drain). */
  isActive: boolean;
}

const sessions = new Map<string, SessionQueue>();

/** Single shared drain timer across all background sessions. null = idle. */
let sharedDrainTimer: ReturnType<typeof setTimeout> | null = null;

// ── Public API ───────────────────────────────────────────────────────────────

export function registerSession(id: string, term: Terminal): void {
  sessions.set(id, {
    term,
    chunks: [],
    bytes: 0,
    dropped: false,
    // Defaults ACTIVE (foreground). This is intentionally ASYMMETRIC with the
    // Rust side's `is_tab_active` default (false): the Rust default throttles
    // IPC emit rate (slow = safe on observer failure), while the frontend
    // default of "write immediately" is safe on observer failure — a missed
    // observer just means the tab keeps writing directly (the original
    // behavior, no 2MB drop risk). A backgrounded tab briefly writes directly
    // until the observer fires false (~one frame), then buffers — negligible
    // cost for the safety of never dropping output on an observer race.
    isActive: true,
  });
}

export function unregisterSession(id: string): void {
  sessions.delete(id);
  // If the shared timer is now idle (no bg sessions left), cancel it.
  if (sharedDrainTimer !== null && !hasBgWork()) {
    clearTimeout(sharedDrainTimer);
    sharedDrainTimer = null;
  }
}

/** Route a PTY output chunk. Foreground → write immediately; background →
 *  buffer (2MB lossy). Called from TierTerminal's onOutput handler INSTEAD OF
 *  `term.write(data)` — all other per-chunk tracking (hasOutput, alt-screen,
 *  sentinel scan) stays in the handler and runs synchronously before this. */
export function enqueue(id: string, data: string): void {
  const q = sessions.get(id);
  if (!q) return;
  if (q.isActive) {
    // Foreground: xterm.write queues internally + paces via its own WriteBuffer
    // timer (~12ms). Only ONE tab is fg, so there's no cross-tab competition —
    // this is the Orca win (bg tabs don't each schedule their own timer).
    try { q.term.write(data); } catch { /* term disposed */ }
    return;
  }
  // Background: buffer with 2MB lossy cap.
  if (q.bytes + data.length > MAX_BG_QUEUE) {
    if (!q.dropped) {
      q.dropped = true;
      // Enqueue the warning IN-ORDER as a chunk so it lands at the position
      // where drops started, not out of order via a direct term.write. Small
      // (~150B) and one-time, so it doesn't meaningfully change the cap.
      q.chunks.push(DROP_WARNING);
      q.bytes += DROP_WARNING.length;
    }
    // Drop this chunk (lossy). The agent's own JSONL preserves the full
    // conversation; only the terminal DISPLAY loses this slice.
    return;
  }
  q.chunks.push(data);
  q.bytes += data.length;
  scheduleBgDrain();
}

/** Flip a session between foreground (immediate write) and background
 *  (buffered drain). On fg, runs the bounded resume drain (Phase 3) so a tab
 *  that accumulated output while hidden doesn't beachball on switch-back. */
export function setActive(id: string, active: boolean): void {
  const q = sessions.get(id);
  if (!q) return;
  q.isActive = active;
  if (active) {
    resumeDrain(q);
  }
}

// ── Internals ────────────────────────────────────────────────────────────────

function hasBgWork(): boolean {
  for (const q of sessions.values()) {
    if (!q.isActive && q.chunks.length > 0) return true;
  }
  return false;
}

function scheduleBgDrain(): void {
  if (sharedDrainTimer !== null) return; // already armed
  sharedDrainTimer = setTimeout(bgDrain, BG_DRAIN_INTERVAL_MS);
}

/** One drain tick: iterate ALL background sessions with pending chunks, write
 *  up to MAX_WRITES_PER_DRAIN chunks each, yield after DRAIN_BUDGET_MS. Re-arms
 *  itself if any session still has pending chunks. */
function bgDrain(): void {
  sharedDrainTimer = null;
  const started = performance.now();
  let anyRemaining = false;

  for (const q of sessions.values()) {
    if (q.isActive || q.chunks.length === 0) continue;

    let writes = 0;
    while (writes < MAX_WRITES_PER_DRAIN && q.chunks.length > 0) {
      const chunk = q.chunks.shift()!;
      q.bytes -= chunk.length;
      try { q.term.write(chunk); } catch { /* term disposed */ }
      writes++;
      // Cooperative: yield if the whole tick has overrun its budget. Remaining
      // chunks stay queued and get picked up by the next armed tick.
      if (performance.now() - started > DRAIN_BUDGET_MS) {
        if (q.chunks.length > 0) anyRemaining = true;
        break;
      }
    }
    if (q.chunks.length > 0) anyRemaining = true;
    // Tick-wide cooperative yield: if this session already blew the budget,
    // stop processing further bg sessions this tick (not just this one's
    // inner loop) so a tick can't overrun ~2×(N-1) writes with N backlogged
    // tabs. Remaining bg sessions are picked up by the next armed tick.
    if (performance.now() - started > DRAIN_BUDGET_MS) {
      anyRemaining = true;
      break;
    }
  }

  if (anyRemaining) scheduleBgDrain();
}

/** Phase 3: on tab activation, sync-flush up to RESUME_FLUSH_SYNC bytes for an
 *  immediate visible viewport, then write the rest via xterm.write (xterm's
 *  WriteBuffer paces the rest without blocking the main thread — streams in
 *  over ~1s for a large backlog). Order is preserved (FIFO from chunks). */
function resumeDrain(q: SessionQueue): void {
  let flushed = 0;
  while (q.chunks.length > 0 && flushed < RESUME_FLUSH_SYNC) {
    const chunk = q.chunks.shift()!;
    q.bytes -= chunk.length;
    try { q.term.write(chunk); } catch { /* term disposed */ }
    flushed += chunk.length;
  }
  if (q.chunks.length > 0) {
    // Join the residual into one write — xterm queues + paces it via its
    // internal WriteBuffer timer. New fg output arriving via enqueue() lands
    // after this residual (FIFO), so order is preserved.
    const rest = q.chunks.join('');
    q.chunks = [];
    q.bytes = 0;
    try { q.term.write(rest); } catch { /* term disposed */ }
  }
}
