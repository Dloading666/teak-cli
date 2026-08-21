// Per-project session order for the left rail.
//
// Live PTY tabs are the rows. The rail is a chat list — newest-first by
// default, with a per-project override after the user drag-reorders. Keys are
// terminal ids, grouped by the same project key HistoryBoard uses
// (normalized cwd / openclaw / __none__).

import { prefGet, prefSet } from './prefs';

const STORAGE_KEY = 'session-nav-order';

type OrderMap = Record<string, string[]>;

let order: OrderMap = load();
const listeners = new Set<() => void>();

function load(): OrderMap {
  try {
    const raw = prefGet(STORAGE_KEY);
    if (!raw) return {};
    const obj = JSON.parse(raw) as unknown;
    if (!obj || typeof obj !== 'object' || Array.isArray(obj)) return {};
    const out: OrderMap = {};
    for (const [key, value] of Object.entries(obj as Record<string, unknown>)) {
      if (!Array.isArray(value)) continue;
      const ids = value.filter((id): id is string => typeof id === 'string' && id.length > 0);
      if (ids.length > 0) out[key] = ids;
    }
    return out;
  } catch {
    return {};
  }
}

function persist(): void {
  try {
    prefSet(STORAGE_KEY, JSON.stringify(order));
  } catch {
    // quota / private mode — keep the in-memory map for this session
  }
}

function emit(): void {
  for (const listener of listeners) listener();
}

export function setGroupOrder(groupKey: string, ids: string[]): void {
  const next = { ...order };
  if (ids.length === 0) delete next[groupKey];
  else next[groupKey] = ids;
  if (JSON.stringify(next[groupKey] ?? []) === JSON.stringify(order[groupKey] ?? [])) return;
  order = next;
  persist();
  emit();
}

export function getGroupOrder(groupKey: string): string[] | undefined {
  return order[groupKey];
}

export function subscribeNavOrder(listener: () => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

export function getNavOrderSnapshot(): OrderMap {
  return order;
}

/** Seed the in-memory map from a restored snapshot. Skips when the user
 *  already has a local order so a later disk load cannot clobber a drag. */
export function hydrateNavOrder(next: OrderMap): void {
  if (!next || Object.keys(next).length === 0) return;
  if (Object.keys(order).length > 0) return;
  order = { ...next };
  persist();
  emit();
}
