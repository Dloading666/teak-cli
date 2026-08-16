import { useEffect, useRef } from 'react';
import { createPortal } from 'react-dom';
import { useT } from '../../i18n/useT';

export interface TermContextMenuState {
  x: number;
  y: number;
  hasSelection: boolean;
}

/** Shared read-only surface menu used by both xterm and ConversationView. */
export function TermContextMenu({ menu, onClose, onCopy, onPaste, onSelectAll }: {
  menu: TermContextMenuState;
  onClose: () => void;
  onCopy: () => void;
  onPaste: () => void;
  onSelectAll: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const isMac = navigator.platform.toUpperCase().includes('MAC');
  const t = useT();
  const mod = isMac ? '⌘' : 'Ctrl';

  useEffect(() => {
    const close = (event: MouseEvent) => {
      if (ref.current && !ref.current.contains(event.target as Node)) onClose();
    };
    const closeKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    // Delay so the triggering mousedown does not immediately close the menu.
    const timer = window.setTimeout(() => {
      document.addEventListener('mousedown', close);
      document.addEventListener('keydown', closeKey);
    }, 0);
    return () => {
      window.clearTimeout(timer);
      document.removeEventListener('mousedown', close);
      document.removeEventListener('keydown', closeKey);
    };
  }, [onClose]);

  const left = Math.max(4, Math.min(menu.x, window.innerWidth - 164));
  const top = Math.max(4, Math.min(menu.y, window.innerHeight - 116));

  return createPortal(
    <div ref={ref} className="term-ctx-menu" style={{ left, top }}>
      <button
        type="button"
        className={`term-ctx-item${menu.hasSelection ? '' : ' disabled'}`}
        onMouseDown={(event) => {
          event.preventDefault();
          if (menu.hasSelection) onCopy();
        }}
      >
        <span>{t('menu.copy')}</span><kbd>{mod}+C</kbd>
      </button>
      <button
        type="button"
        className="term-ctx-item"
        onMouseDown={(event) => {
          event.preventDefault();
          onPaste();
        }}
      >
        <span>{t('menu.paste')}</span><kbd>{mod}+V</kbd>
      </button>
      <div className="term-ctx-sep" />
      <button
        type="button"
        className="term-ctx-item"
        onMouseDown={(event) => {
          event.preventDefault();
          onSelectAll();
        }}
      >
        <span>{t('menu.select_all')}</span><kbd>{mod}+A</kbd>
      </button>
    </div>,
    document.body,
  );
}
