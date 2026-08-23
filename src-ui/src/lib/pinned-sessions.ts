// Pinned sessions - localStorage-backed "置顶" marker table.
//
// Mirror of hidden-sessions.ts: Teak only reads each CLI tool's session
// data for the 会话记录 list, so "pinning" is a local per-device marker,
// not a write to the tool's own store. We keep a set of `${tool}:${id}`
// keys the user has pinned; HistoryBoard moves them into an independent
// Codex-style "置顶" shelf at the top of the rail. Pin/unpin happens via the
// session card's right-click context menu.
//
// `${tool}:${id}` guards against a uuid colliding across two tools' id
// spaces (same key scheme as hidden-sessions).
//
// External-store shape mirrors hidden-sessions.ts so HistoryBoard
// subscribes via useSyncExternalStore and re-renders the instant a pin
// lands.

import { prefGetWith, prefSet } from './prefs';

const STORAGE_KEY = 'pinned-sessions';
const LEGACY_KEY = 'coffee:pinned-sessions';

let pinned: Set<string> = load();
const listeners = new Set<() => void>();

function load(): Set<string> {
  try {
    const raw = prefGetWith(STORAGE_KEY, LEGACY_KEY);
    if (!raw) return new Set();
    const arr = JSON.parse(raw);
    if (!Array.isArray(arr)) return new Set();
    return new Set(arr.filter((k): k is string => typeof k === 'string'));
  } catch {
    return new Set();
  }
}

function persist(): void {
  try {
    prefSet(STORAGE_KEY, JSON.stringify([...pinned]));
  } catch {
    // quota exceeded / localStorage disabled - pin stays in-memory for this session
  }
}

function emit(): void {
  for (const l of listeners) l();
}

function key(tool: string, id: string): string {
  return `${tool}:${id}`;
}

/** Stable native identity first; live terminal id is a compatibility fallback. */
export function sessionPinKeys(
  tool: string,
  id: string,
  token?: string | null,
  aliases: string[] = [],
): string[] {
  const stable = token?.trim();
  const ids = [stable, id, ...aliases]
    .map((value) => value?.trim())
    .filter((value): value is string => Boolean(value));
  return [...new Set(ids.map((value) => key(tool, value)))];
}

export function pinSession(tool: string, id: string, token?: string | null, aliases: string[] = []): void {
  const [stable, ...legacy] = sessionPinKeys(tool, id, token, aliases);
  if (!stable) return;
  if (pinned.has(stable) && !legacy.some((candidate) => pinned.has(candidate))) return;
  const next = new Set(pinned);
  next.add(stable);
  // Migrate a pre-token/live-tab marker to the native session token. A token
  // survives closing and reopening the tab; a random terminal id may not.
  for (const candidate of legacy) next.delete(candidate);
  pinned = next;
  persist();
  emit();
}

export function unpinSession(tool: string, id: string, token?: string | null, aliases: string[] = []): void {
  const keys = sessionPinKeys(tool, id, token, aliases);
  if (!keys.some((candidate) => pinned.has(candidate))) return;
  const next = new Set(pinned);
  for (const candidate of keys) next.delete(candidate);
  pinned = next;
  persist();
  emit();
}

export function togglePin(tool: string, id: string, token?: string | null, aliases: string[] = []): void {
  if (isPinned(tool, id, token, aliases)) unpinSession(tool, id, token, aliases);
  else pinSession(tool, id, token, aliases);
}

export function isPinned(tool: string, id: string, token?: string | null, aliases: string[] = []): boolean {
  return sessionPinKeys(tool, id, token, aliases).some((candidate) => pinned.has(candidate));
}

/** useSyncExternalStore subscribe. */
export function subscribePinned(listener: () => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

/** useSyncExternalStore getSnapshot. Returns a stable Set reference that only
 *  swaps when a pin/unpin lands (immutable update), so React doesn't loop. */
export function getPinnedSnapshot(): Set<string> {
  return pinned;
}
