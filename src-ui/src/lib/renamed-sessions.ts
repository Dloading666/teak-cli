// Renamed sessions - localStorage-backed custom title table.
//
// Same local-marker design as pinned-sessions.ts / hidden-sessions.ts:
// Teak only *reads* each CLI tool's session data for the 会话记录 list,
// so a rename is a per-device override layered on top of the tool's own
// auto-generated title — never a write into the tool's session files
// (those formats are per-tool and risky to mutate). Keyed `${tool}:${id}`
// like the other two tables. HistoryBoard renders the custom name when
// present, falls back to the auto title otherwise; search matches the
// displayed (custom) name.
//
// External-store shape mirrors pinned-sessions.ts so HistoryBoard
// subscribes via useSyncExternalStore and re-renders the instant a rename
// lands.

import { prefGetWith, prefSet } from './prefs';

const STORAGE_KEY = 'renamed-sessions';
const LEGACY_KEY = 'coffee:renamed-sessions';

let names: Record<string, string> = load();
const listeners = new Set<() => void>();

function load(): Record<string, string> {
  try {
    const raw = prefGetWith(STORAGE_KEY, LEGACY_KEY);
    if (!raw) return {};
    const obj = JSON.parse(raw);
    if (!obj || typeof obj !== 'object' || Array.isArray(obj)) return {};
    const out: Record<string, string> = {};
    for (const [k, v] of Object.entries(obj)) {
      if (typeof v === 'string' && v.trim()) out[k] = v;
    }
    return out;
  } catch {
    return {};
  }
}

function persist(): void {
  try {
    prefSet(STORAGE_KEY, JSON.stringify(names));
  } catch {
    // quota exceeded / localStorage disabled - rename stays in-memory for this session
  }
}

function emit(): void {
  for (const l of listeners) l();
}

function key(tool: string, id: string): string {
  return `${tool}:${id}`;
}

export function getCustomName(tool: string, id: string): string | undefined {
  return names[key(tool, id)];
}

/** Prefer the CLI session token so a leftover tab-id label from a previous
 *  conversation cannot stick after `--resume` forks or `/new`. Fall back to
 *  the Teak tab id only when this tab has no live token yet. */
export function lookupCustomName(
  tool: string,
  id: string,
  resumeToken?: string | null,
): string | undefined {
  const token = resumeToken?.trim();
  if (token) return names[key(tool, token)];
  return names[key(tool, id)];
}

/** Set (or replace) a custom title. Empty / whitespace-only clears the
 *  override, reverting the card to the tool's auto title. */
export function setCustomName(tool: string, id: string, name: string | null): void {
  const k = key(tool, id);
  const trimmed = (name ?? '').trim();
  if (trimmed) {
    if (names[k] === trimmed) return;
    names = { ...names, [k]: trimmed };
  } else {
    if (!(k in names)) return;
    const next = { ...names };
    delete next[k];
    names = next;
  }
  persist();
  emit();
}

/** useSyncExternalStore subscribe. */
export function subscribeRenamed(listener: () => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

/** useSyncExternalStore getSnapshot. Returns a stable reference that only
 *  swaps when a rename lands (immutable update), so React doesn't loop. */
export function getRenamedSnapshot(): Record<string, string> {
  return names;
}
