// Workspace file editor. VS Code-style: current file, syntax highlight,
// line numbers, a thin git gutter, wrap so long lines stay readable,
// and autosave after each edit.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { diffLines } from 'diff';
import { commands } from '../../tauri';
import { useT } from '../../i18n/useT';
import { useDataAttr } from '../../lib/use-data-attr';
import { tokenizeFile, getShikiTheme, type LineTokens } from '../../lib/shiki';
import './FileEditor.css';

const MAX_BYTES = 1_000_000;
const SAVE_MS = 450;
const HIGHLIGHT_MS = 80;

type SaveStatus = 'loading' | 'saved' | 'saving' | 'error' | 'too_large' | 'binary';
type GutterKind = 'add' | 'mod';

interface FileEditorProps {
  path: string;
  repoRoot: string;
  rel: string;
  kind: 'uncommitted' | 'untracked' | 'committed';
}

function changedNewLines(oldText: string, newText: string): Set<number> {
  const out = new Set<number>();
  if (!oldText) {
    const n = Math.max(1, newText.split('\n').length);
    for (let i = 1; i <= n; i += 1) out.add(i);
    return out;
  }
  let newLine = 1;
  for (const part of diffLines(oldText, newText)) {
    const rows = part.value.split('\n');
    if (rows[rows.length - 1] === '') rows.pop();
    if (part.added) {
      for (let i = 0; i < rows.length; i += 1) out.add(newLine++);
    } else if (!part.removed) {
      newLine += rows.length;
    }
  }
  return out;
}

export function FileEditor({ path, repoRoot, rel, kind }: FileEditorProps) {
  const t = useT();
  const dataTheme = useDataAttr('data-theme');
  const [draft, setDraft] = useState('');
  const [status, setStatus] = useState<SaveStatus>('loading');
  const [marks, setMarks] = useState<Set<number>>(new Set());
  const [tokens, setTokens] = useState<LineTokens[] | null>(null);
  const [cursorLine, setCursorLine] = useState(1);
  const baselineRef = useRef('');
  const isNewFileRef = useRef(false);
  const draftRef = useRef('');
  const savedRef = useRef('');
  const saveTimer = useRef<number | null>(null);
  const hlTimer = useRef<number | null>(null);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const saveGen = useRef(0);

  const lines = useMemo(() => draft.split('\n'), [draft]);
  const crumbs = useMemo(() => {
    const raw = (rel || path).replace(/\\/g, '/');
    return raw.split('/').filter(Boolean);
  }, [path, rel]);
  const gutterKind: GutterKind = isNewFileRef.current ? 'add' : 'mod';

  const syncCursor = (el: HTMLTextAreaElement) => {
    const pos = el.selectionStart ?? 0;
    let n = 1;
    for (let i = 0; i < pos; i += 1) {
      if (draftRef.current.charCodeAt(i) === 10) n += 1;
    }
    setCursorLine(n);
  };

  const highlight = useCallback((text: string) => {
    if (hlTimer.current != null) window.clearTimeout(hlTimer.current);
    hlTimer.current = window.setTimeout(() => {
      hlTimer.current = null;
      const theme = getShikiTheme(dataTheme);
      void tokenizeFile(text, path, theme).then((next) => {
        setTokens(next);
      });
    }, HIGHLIGHT_MS);
  }, [dataTheme, path]);

  const flushSave = useCallback(async (text: string) => {
    if (text === savedRef.current) {
      setStatus('saved');
      return;
    }
    const gen = ++saveGen.current;
    setStatus('saving');
    try {
      await commands.writeTextFile(path, text);
      if (gen !== saveGen.current) return;
      savedRef.current = text;
      setMarks(changedNewLines(baselineRef.current, text));
      setStatus('saved');
    } catch {
      if (gen !== saveGen.current) return;
      setStatus('error');
    }
  }, [path]);

  const scheduleSave = useCallback((text: string) => {
    if (saveTimer.current != null) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => {
      saveTimer.current = null;
      void flushSave(text);
    }, SAVE_MS);
  }, [flushSave]);

  useEffect(() => {
    let cancelled = false;
    setStatus('loading');
    setDraft('');
    setTokens(null);
    draftRef.current = '';
    savedRef.current = '';
    baselineRef.current = '';
    setMarks(new Set());
    (async () => {
      try {
        const working = await commands.readTextFile(path);
        if (cancelled) return;
        if (working == null) {
          setStatus('binary');
          return;
        }
        if (working.length > MAX_BYTES) {
          setStatus('too_large');
          return;
        }
        let baseline = '';
        if (kind !== 'untracked' && repoRoot && rel) {
          baseline = (await commands.gitShowFile(repoRoot, `HEAD:${rel}`)) ?? '';
        }
        if (cancelled) return;
        isNewFileRef.current = kind === 'untracked' || !baseline;
        baselineRef.current = baseline;
        draftRef.current = working;
        savedRef.current = working;
        setDraft(working);
        setMarks(changedNewLines(baseline, working));
        setStatus('saved');
        const theme = getShikiTheme(dataTheme);
        const next = await tokenizeFile(working, path, theme);
        if (!cancelled) setTokens(next);
        requestAnimationFrame(() => taRef.current?.focus());
      } catch {
        if (!cancelled) setStatus('error');
      }
    })();
    return () => {
      cancelled = true;
      if (saveTimer.current != null) {
        window.clearTimeout(saveTimer.current);
        saveTimer.current = null;
      }
      if (hlTimer.current != null) {
        window.clearTimeout(hlTimer.current);
        hlTimer.current = null;
      }
      const pending = draftRef.current;
      if (pending !== savedRef.current && pending.length <= MAX_BYTES) {
        void commands.writeTextFile(path, pending).catch(() => {});
      }
    };
  }, [path, repoRoot, rel, kind]);

  useEffect(() => {
    if (status === 'loading' || !draftRef.current) return;
    highlight(draftRef.current);
  }, [dataTheme, highlight, status]);

  useLayoutEffect(() => {
    const ta = taRef.current;
    if (!ta || status === 'loading') return;
    ta.style.height = 'auto';
    const min = ta.parentElement?.clientHeight ?? 0;
    ta.style.height = `${Math.max(ta.scrollHeight, min)}px`;
  }, [draft, status, tokens, lines.length]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 's') {
        e.preventDefault();
        if (saveTimer.current != null) {
          window.clearTimeout(saveTimer.current);
          saveTimer.current = null;
        }
        void flushSave(draftRef.current);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [flushSave]);

  const onChange = (value: string) => {
    draftRef.current = value;
    setDraft(value);
    setMarks(changedNewLines(baselineRef.current, value));
    setStatus('saving');
    scheduleSave(value);
    highlight(value);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key !== 'Tab') return;
    e.preventDefault();
    const el = e.currentTarget;
    const start = el.selectionStart;
    const end = el.selectionEnd;
    const next = `${draft.slice(0, start)}  ${draft.slice(end)}`;
    onChange(next);
    requestAnimationFrame(() => {
      el.selectionStart = el.selectionEnd = start + 2;
    });
  };

  const statusLabel =
    status === 'saving' ? (t('editor.saving') || '保存中…')
    : status === 'saved' ? (t('editor.saved') || '已自动保存')
    : status === 'error' ? (t('editor.save_error') || '保存失败')
    : '';
  const hlTokens = tokens && tokens.length + 1 >= lines.length ? tokens : null;

  if (status === 'loading') {
    return <div className="file-editor"><div className="file-editor-empty">{t('editor.loading') || '加载中…'}</div></div>;
  }
  if (status === 'too_large') {
    return <div className="file-editor"><div className="file-editor-empty">{t('editor.too_large') || '文件过大，无法在此编辑'}</div></div>;
  }
  if ((status === 'binary' || status === 'error') && !draft) {
    return <div className="file-editor"><div className="file-editor-empty">{t('editor.binary') || '无法作为文本打开此文件'}</div></div>;
  }

  return (
    <div className="file-editor">
      <div className="file-editor-bc">
        {crumbs.map((seg, i) => (
          <span key={`${i}-${seg}`} className="file-editor-bc-item">
            {i > 0 && <span className="file-editor-bc-sep" aria-hidden>/</span>}
            <span className={i === crumbs.length - 1 ? 'file-editor-bc-file' : 'file-editor-bc-dir'}>{seg}</span>
          </span>
        ))}
        <span className={`file-editor-status is-${status}`}>{statusLabel}</span>
      </div>
      <div className="file-editor-body">
        <div className="file-editor-gutter" aria-hidden>
          {lines.map((_, i) => (
            <div
              key={i}
              className={`file-editor-gutter-row${marks.has(i + 1) ? ` is-${gutterKind}` : ''}${cursorLine === i + 1 ? ' is-active' : ''}`}
            >
              {i + 1}
            </div>
          ))}
        </div>
        <div className="file-editor-main">
          <pre className="file-editor-hl" aria-hidden>
            {hlTokens
              ? hlTokens.map((lineToks, i) => (
                  <div key={i} className={`file-editor-hl-line${cursorLine === i + 1 ? ' is-active' : ''}`}>
                    {lineToks.length > 0
                      ? lineToks.map((tok, j) => (
                          <span key={j} style={{ color: tok.color }}>{tok.content}</span>
                        ))
                      : ' '}
                  </div>
                ))
              : lines.map((line, i) => (
                  <div key={i} className={`file-editor-hl-line${cursorLine === i + 1 ? ' is-active' : ''}`}>{line.length ? line : ' '}</div>
                ))}
          </pre>
          <textarea
            ref={taRef}
            className="file-editor-ta"
            value={draft}
            spellCheck={false}
            wrap="off"
            autoCapitalize="off"
            autoCorrect="off"
            autoComplete="off"
            onChange={(e) => onChange(e.target.value)}
            onKeyDown={(e) => {
              onKeyDown(e);
              requestAnimationFrame(() => { if (taRef.current) syncCursor(taRef.current); });
            }}
            onClick={(e) => syncCursor(e.currentTarget)}
            onKeyUp={(e) => syncCursor(e.currentTarget)}
            onSelect={(e) => syncCursor(e.currentTarget)}
            onBlur={() => {
              if (saveTimer.current != null) {
                window.clearTimeout(saveTimer.current);
                saveTimer.current = null;
              }
              void flushSave(draftRef.current);
            }}
          />
        </div>
      </div>
    </div>
  );
}
