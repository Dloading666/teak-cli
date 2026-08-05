// FrostBackdrop.tsx — Frost shape's frosted backdrop.
//
// The Frost shape is glass chrome + a genuinely blurred backdrop. CSS
// `backdrop-filter` cannot sample the OS desktop through a transparent
// WebView2 window, and native Acrylic (DWM) broke rounded corners + focus
// state (square corner layer + default frame reappearing), so we render a
// blurred copy of the current desktop wallpaper as an in-page layer instead.
// It sits behind the app content (z-index -1) and is clipped to #root's
// rounded corners, so Frost keeps the app's unified rounded corners with zero
// native window manipulation.

import { useEffect, useState } from 'react';
import { useAppState } from '../../store/app-state';
import { isFrostShape } from '../../lib/personalization';
import { commands } from '../../tauri';

export function FrostBackdrop() {
  const { state } = useAppState();
  const active = isFrostShape(state.currentShape);
  const [wallpaperUrl, setWallpaperUrl] = useState('');

  useEffect(() => {
    if (!active) {
      setWallpaperUrl('');
      return;
    }
    let cancelled = false;
    commands
      .getWallpaperPath()
      .then((p) => {
        if (cancelled || !p) return;
        // convertFileSrc via dynamic import (asset protocol) — same pattern as
        // the in-app wallpaper in CenterPanel.
        return import('@tauri-apps/api/core').then(({ convertFileSrc }) => {
          if (!cancelled) setWallpaperUrl(convertFileSrc(p));
        });
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [active]);

  // Not active, or wallpaper unresolved (non-Tauri dev / read failure) → Frost
  // degrades to plain transparent glass, which is already rounded.
  if (!active || !wallpaperUrl) return null;

  return (
    <>
      <div className="frost-backdrop" style={{ backgroundImage: `url("${wallpaperUrl}")` }} aria-hidden="true" />
      <div className="frost-tint" aria-hidden="true" />
    </>
  );
}
