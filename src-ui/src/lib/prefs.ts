/** Preference keys. New writes use `teak-*`; reads fall back to the old `cc-*`
 *  keys so a rename does not reset existing settings. */

const NS = 'teak-';
const LEGACY_NS = 'cc-';

export function prefGet(key: string): string | null {
  try {
    return localStorage.getItem(NS + key) ?? localStorage.getItem(LEGACY_NS + key);
  } catch {
    return null;
  }
}

export function prefSet(key: string, value: string): void {
  try {
    localStorage.setItem(NS + key, value);
  } catch { /* quota / private mode */ }
}

export function prefRemove(key: string): void {
  try {
    localStorage.removeItem(NS + key);
    localStorage.removeItem(LEGACY_NS + key);
  } catch { /* ignore */ }
}

/** Read teak-/cc- first, then any extra exact keys from the Coffee-era schema. */
export function prefGetWith(key: string, ...legacyExact: string[]): string | null {
  const v = prefGet(key);
  if (v != null) return v;
  try {
    for (const k of legacyExact) {
      const x = localStorage.getItem(k);
      if (x != null) return x;
    }
  } catch { /* ignore */ }
  return null;
}
