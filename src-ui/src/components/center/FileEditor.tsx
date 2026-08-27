// Workspace file editor. VS Code-style work surface: current source,
// syntax highlight, git gutter, minimap, find/go-to-line, wrap, autosave.

import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { diffLines } from 'diff';
import { commands } from '../../tauri';
import { useT } from '../../i18n/useT';
import { clipboardWrite } from '../../lib/clipboard';
import { useDataAttr } from '../../lib/use-data-attr';
import { tokenizeFile, getShikiTheme, type LineTokens } from '../../lib/shiki';
import './FileEditor.css';

const MAX_BYTES = 1_000_000;
const SAVE_MS = 450;
const HIGHLIGHT_MS = 80;
const WRAP_EXTS = new Set(['md', 'mdx', 'txt', 'rst', 'log', 'csv']);

type SaveStatus = 'loading' | 'saved' | 'saving' | 'error' | 'too_large' | 'binary';
type GutterKind = 'add' | 'mod' | 'del';
type Marks = { add: Set<number>; mod: Set<number>; del: Set<number> };
type FindHit = { start: number; end: number; line: number; col: number };

const LANG_LABEL: Record<string, string> = {
  ts: 'TypeScript', tsx: 'TypeScript React', js: 'JavaScript', jsx: 'JavaScript React',
  mjs: 'JavaScript', cjs: 'JavaScript', mts: 'TypeScript', cts: 'TypeScript',
  py: 'Python', rs: 'Rust', go: 'Go', java: 'Java', rb: 'Ruby', php: 'PHP',
  css: 'CSS', scss: 'SCSS', sass: 'Sass', less: 'Less', html: 'HTML', json: 'JSON',
  md: 'Markdown', mdx: 'MDX', yml: 'YAML', yaml: 'YAML', toml: 'TOML',
  sh: 'Shell', bash: 'Shell', zsh: 'Shell', sql: 'SQL', vue: 'Vue',
  svelte: 'Svelte', xml: 'XML', svg: 'SVG', c: 'C', h: 'C', cpp: 'C++',
  cc: 'C++', cxx: 'C++', hpp: 'C++', cs: 'C#', kt: 'Kotlin', swift: 'Swift',
};

const LINE_COMMENT: Record<string, string> = {
  ts: '//', tsx: '//', js: '//', jsx: '//', mjs: '//', cjs: '//', mts: '//', cts: '//',
  rs: '//', go: '//', java: '//', c: '//', h: '//', cpp: '//', cc: '//', cxx: '//',
  hpp: '//', cs: '//', kt: '//', swift: '//', php: '//', scss: '//', sass: '//', less: '//',
  py: '#', rb: '#', sh: '#', bash: '#', zsh: '#', yml: '#', yaml: '#', toml: '#',
  sql: '--',
};

const FILE_META: Record<string, { color: string; fg: string; short: string }> = {
  ts: { color: '#3178c6', fg: '#fff', short: 'TS' },
  tsx: { color: '#3178c6', fg: '#fff', short: 'TX' },
  js: { color: '#c6b000', fg: '#1e1e1e', short: 'JS' },
  jsx: { color: '#00b3cc', fg: '#1e1e1e', short: 'JX' },
  mjs: { color: '#c6b000', fg: '#1e1e1e', short: 'JS' },
  json: { color: '#c6b000', fg: '#1e1e1e', short: '{}' },
  md: { color: '#519aba', fg: '#fff', short: 'MD' },
  mdx: { color: '#519aba', fg: '#fff', short: 'MD' },
  rs: { color: '#dea584', fg: '#1e1e1e', short: 'RS' },
  py: { color: '#3572a5', fg: '#fff', short: 'PY' },
  css: { color: '#563d7c', fg: '#fff', short: '#' },
  scss: { color: '#c6538c', fg: '#fff', short: '$' },
  html: { color: '#e34c26', fg: '#fff', short: '<>' },
  go: { color: '#00add8', fg: '#fff', short: 'GO' },
  yml: { color: '#a074c4', fg: '#fff', short: 'Y' },
  yaml: { color: '#a074c4', fg: '#fff', short: 'Y' },
  toml: { color: '#9c4221', fg: '#fff', short: 'T' },
  sh: { color: '#4eaa25', fg: '#fff', short: '$' },
  bash: { color: '#4eaa25', fg: '#fff', short: '$' },
  zsh: { color: '#4eaa25', fg: '#fff', short: '$' },
  vue: { color: '#41b883', fg: '#fff', short: 'V' },
  svelte: { color: '#ff3e00', fg: '#fff', short: 'S' },
  svg: { color: '#e37933', fg: '#fff', short: '◇' },
  xml: { color: '#e37933', fg: '#fff', short: '<>' },
  sql: { color: '#336791', fg: '#fff', short: 'Q' },
  java: { color: '#b07219', fg: '#fff', short: 'J' },
  kt: { color: '#a97bff', fg: '#fff', short: 'K' },
  swift: { color: '#e37933', fg: '#fff', short: 'S' },
  c: { color: '#555555', fg: '#fff', short: 'C' },
  h: { color: '#555555', fg: '#fff', short: 'H' },
  cpp: { color: '#004482', fg: '#fff', short: 'C+' },
  cs: { color: '#178600', fg: '#fff', short: 'C#' },
  php: { color: '#4f5d95', fg: '#fff', short: 'P' },
  rb: { color: '#cc342d', fg: '#fff', short: 'RB' },
  txt: { color: '#6e7681', fg: '#fff', short: 'TXT' },
  png: { color: '#d16d9e', fg: '#fff', short: 'PNG' },
  jpg: { color: '#d16d9e', fg: '#fff', short: 'JPG' },
  jpeg: { color: '#d16d9e', fg: '#fff', short: 'JPG' },
  gif: { color: '#d16d9e', fg: '#fff', short: 'GIF' },
  webp: { color: '#d16d9e', fg: '#fff', short: 'WBP' },
  bmp: { color: '#d16d9e', fg: '#fff', short: 'BMP' },
  ico: { color: '#d16d9e', fg: '#fff', short: 'ICO' },
  avif: { color: '#d16d9e', fg: '#fff', short: 'AV' },
  heic: { color: '#d16d9e', fg: '#fff', short: 'HE' },
  tif: { color: '#d16d9e', fg: '#fff', short: 'TIF' },
  tiff: { color: '#d16d9e', fg: '#fff', short: 'TIF' },
  mp4: { color: '#c45c26', fg: '#fff', short: 'MP4' },
  mov: { color: '#c45c26', fg: '#fff', short: 'MOV' },
  webm: { color: '#c45c26', fg: '#fff', short: 'WM' },
  mkv: { color: '#c45c26', fg: '#fff', short: 'MKV' },
  m4v: { color: '#c45c26', fg: '#fff', short: 'M4V' },
  avi: { color: '#c45c26', fg: '#fff', short: 'AVI' },
  mp3: { color: '#3d8bfd', fg: '#fff', short: 'MP3' },
  wav: { color: '#3d8bfd', fg: '#fff', short: 'WAV' },
  flac: { color: '#3d8bfd', fg: '#fff', short: 'FL' },
  m4a: { color: '#3d8bfd', fg: '#fff', short: 'M4A' },
  aac: { color: '#3d8bfd', fg: '#fff', short: 'AAC' },
  ogg: { color: '#3d8bfd', fg: '#fff', short: 'OGG' },
  pdf: { color: '#e2574c', fg: '#fff', short: 'PDF' },
  ttf: { color: '#7c6aed', fg: '#fff', short: 'TTF' },
  otf: { color: '#7c6aed', fg: '#fff', short: 'OTF' },
  woff: { color: '#7c6aed', fg: '#fff', short: 'WF' },
  woff2: { color: '#7c6aed', fg: '#fff', short: 'WF' },
};

type PreviewKind = 'image' | 'video' | 'audio' | 'pdf' | 'font';

const IMAGE_EXT = new Set([
  'png', 'jpg', 'jpeg', 'jfif', 'gif', 'webp', 'bmp', 'ico', 'svg',
  'avif', 'tif', 'tiff', 'heic', 'heif', 'apng',
]);
const VIDEO_EXT = new Set(['mp4', 'webm', 'mov', 'm4v', 'mkv', 'ogv', 'avi']);
const AUDIO_EXT = new Set(['mp3', 'wav', 'ogg', 'flac', 'm4a', 'aac', 'opus', 'oga', 'weba']);
const PDF_EXT = new Set(['pdf']);
const FONT_EXT = new Set(['ttf', 'otf', 'woff', 'woff2']);

function previewKindOf(ext: string): PreviewKind | null {
  if (IMAGE_EXT.has(ext)) return 'image';
  if (VIDEO_EXT.has(ext)) return 'video';
  if (AUDIO_EXT.has(ext)) return 'audio';
  if (PDF_EXT.has(ext)) return 'pdf';
  if (FONT_EXT.has(ext)) return 'font';
  return null;
}

export function FileKindIcon({ name, size = 16 }: { name: string; size?: number }) {
  const ext = name.split('.').pop()?.toLowerCase() ?? '';
  const meta = FILE_META[ext] ?? { color: '#6e7681', fg: '#fff', short: (ext.slice(0, 2) || '·').toUpperCase() };
  return (
    <span
      className="file-kind-icon"
      style={{ width: size, height: size, background: meta.color, color: meta.fg, fontSize: size < 15 ? 7 : 8 }}
      title={ext || name}
      aria-hidden
    >
      {meta.short}
    </span>
  );
}

function fileExt(path: string): string {
  return path.replace(/\\/g, '/').split('/').pop()?.split('.').pop()?.toLowerCase() ?? '';
}

function detectIndent(text: string): { size: number; tabs: boolean } {
  let tabs = 0;
  let two = 0;
  let four = 0;
  for (const line of text.split('\n').slice(0, 120)) {
    if (line.startsWith('\t')) {
      tabs += 1;
      continue;
    }
    const m = line.match(/^( +)/);
    if (!m) continue;
    const n = m[1].length;
    if (n % 4 === 0) four += 1;
    else if (n % 2 === 0) two += 1;
  }
  if (tabs > four && tabs > two) return { size: 4, tabs: true };
  return { size: four >= two ? 4 : 2, tabs: false };
}

function cursorAt(text: string, pos: number): { line: number; col: number } {
  let line = 1;
  let col = 1;
  const end = Math.max(0, Math.min(pos, text.length));
  for (let i = 0; i < end; i += 1) {
    if (text.charCodeAt(i) === 10) {
      line += 1;
      col = 1;
    } else {
      col += 1;
    }
  }
  return { line, col };
}

function lineSpan(text: string, start: number, end: number): { from: number; to: number } {
  const from = cursorAt(text, start).line;
  const to = cursorAt(text, end > start ? end - 1 : end).line;
  return { from, to };
}

function leadingCols(line: string, tabSize: number): number {
  let n = 0;
  for (let i = 0; i < line.length; i += 1) {
    const ch = line[i];
    if (ch === ' ') n += 1;
    else if (ch === '\t') n += tabSize - (n % tabSize);
    else break;
  }
  return n;
}

function countRows(value: string): number {
  const rows = value.split('\n');
  if (rows[rows.length - 1] === '') rows.pop();
  return rows.length;
}

function classifyMarks(oldText: string, newText: string): Marks {
  const add = new Set<number>();
  const mod = new Set<number>();
  const del = new Set<number>();
  if (!oldText) {
    const n = Math.max(1, newText.split('\n').length);
    for (let i = 1; i <= n; i += 1) add.add(i);
    return { add, mod, del };
  }
  let newLine = 1;
  const parts = diffLines(oldText, newText);
  for (let i = 0; i < parts.length; i += 1) {
    const part = parts[i];
    const rows = countRows(part.value);
    if (part.added && parts[i - 1]?.removed) {
      for (let k = 0; k < rows; k += 1) mod.add(newLine++);
    } else if (part.added) {
      for (let k = 0; k < rows; k += 1) add.add(newLine++);
    } else if (part.removed) {
      if (!parts[i + 1]?.added) del.add(newLine);
    } else {
      newLine += rows;
    }
  }
  return { add, mod, del };
}

function findHits(text: string, q: string): FindHit[] {
  if (!q) return [];
  const out: FindHit[] = [];
  const lower = text.toLowerCase();
  const needle = q.toLowerCase();
  let from = 0;
  while (from < text.length && out.length < 2000) {
    const i = lower.indexOf(needle, from);
    if (i < 0) break;
    const { line, col } = cursorAt(text, i);
    out.push({ start: i, end: i + needle.length, line, col });
    from = i + Math.max(needle.length, 1);
  }
  return out;
}

function mapLines(text: string, selStart: number, selEnd: number, fn: (line: string, i: number) => string): { next: string; start: number; end: number } {
  const { from, to } = lineSpan(text, selStart, selEnd);
  const rows = text.split('\n');
  let deltaStart = 0;
  let deltaEnd = 0;
  for (let i = from - 1; i <= to - 1; i += 1) {
    const before = rows[i];
    rows[i] = fn(before, i);
    const d = rows[i].length - before.length;
    if (i === from - 1) deltaStart = d;
    deltaEnd += d;
  }
  return { next: rows.join('\n'), start: selStart + deltaStart, end: selEnd + deltaEnd };
}

function renderLine(
  line: string,
  toks: LineTokens | undefined,
  hits: { start: number; end: number }[],
  current: { start: number; end: number } | null,
): ReactNode {
  if (!line) return ' ';
  const bounds = new Set<number>([0, line.length]);
  for (const h of hits) {
    bounds.add(Math.max(0, Math.min(line.length, h.start)));
    bounds.add(Math.max(0, Math.min(line.length, h.end)));
  }
  if (current) {
    bounds.add(Math.max(0, Math.min(line.length, current.start)));
    bounds.add(Math.max(0, Math.min(line.length, current.end)));
  }
  const pts = [...bounds].sort((a, b) => a - b);
  const colors: { from: number; to: number; color?: string }[] = [];
  if (toks && toks.length > 0) {
    let p = 0;
    for (const tok of toks) {
      colors.push({ from: p, to: p + tok.content.length, color: tok.color });
      p += tok.content.length;
    }
  }
  const segs: { text: string; color?: string; hit: boolean; cur: boolean }[] = [];
  for (let i = 0; i < pts.length - 1; i += 1) {
    const a = pts[i];
    const b = pts[i + 1];
    if (a >= b) continue;
    const color = colors.find((c) => a >= c.from && a < c.to)?.color;
    const hit = hits.some((h) => a >= h.start && a < h.end);
    const cur = current ? a >= current.start && a < current.end : false;
    segs.push({ text: line.slice(a, b), color, hit, cur });
  }
  if (segs.length === 0) return ' ';
  return segs.map((s, j) => (
    <span
      key={j}
      className={s.cur ? 'fe-hit is-current' : s.hit ? 'fe-hit' : undefined}
      style={s.color ? { color: s.color } : undefined}
    >
      {s.text}
    </span>
  ));
}

interface FileEditorProps {
  path: string;
  repoRoot: string;
  rel: string;
  kind: 'uncommitted' | 'untracked' | 'committed';
}

export function FileEditor({ path, repoRoot, rel, kind }: FileEditorProps) {
  const preview = previewKindOf(fileExt(path));
  if (preview) return <MediaPreview path={path} rel={rel} kind={preview} />;
  return <TextFileEditor path={path} repoRoot={repoRoot} rel={rel} kind={kind} />;
}

function useAssetSrc(path: string): string {
  const [src, setSrc] = useState('');
  useEffect(() => {
    let cancelled = false;
    import('@tauri-apps/api/core').then(({ convertFileSrc }) => {
      if (!cancelled) setSrc(convertFileSrc(path));
    }).catch(() => {
      if (!cancelled) setSrc(`file:///${path.replace(/\\/g, '/')}`);
    });
    return () => { cancelled = true; };
  }, [path]);
  return src;
}

function PreviewChrome({
  path,
  rel,
  bar,
  children,
}: {
  path: string;
  rel: string;
  bar: ReactNode;
  children: ReactNode;
}) {
  const t = useT();
  const crumbs = useMemo(() => {
    const raw = (rel || path).replace(/\\/g, '/');
    return raw.split('/').filter(Boolean);
  }, [path, rel]);
  const copyPath = (value: string) => { void clipboardWrite(value); };

  return (
    <div className="file-editor file-preview">
      <div className="file-editor-bc" title={rel || path}>
        {crumbs.map((seg, i) => {
          const prefix = crumbs.slice(0, i + 1).join('/');
          const last = i === crumbs.length - 1;
          return (
            <span key={`${i}-${seg}`} className="file-editor-bc-item">
              {i > 0 && (
                <svg className="file-editor-bc-chevron" viewBox="0 0 16 16" width="12" height="12" aria-hidden>
                  <path fill="currentColor" d="M6.7 3.3a.75.75 0 0 1 1.06 0l4 4a.75.75 0 0 1 0 1.06l-4 4A.75.75 0 0 1 6.7 11.3L10.04 8 6.7 4.7a.75.75 0 0 1 0-1.4z"/>
                </svg>
              )}
              <button type="button" className={last ? 'file-editor-bc-file' : 'file-editor-bc-dir'} onClick={() => copyPath(prefix)} title={prefix}>
                {last ? <FileKindIcon name={seg} size={14} /> : (
                  <svg className="file-editor-bc-folder" viewBox="0 0 16 16" width="12" height="12" aria-hidden>
                    <path fill="currentColor" d="M1.75 2.5A.75.75 0 0 0 1 3.25v9.5c0 .41.34.75.75.75h12.5A.75.75 0 0 0 15 12.75v-7.5A.75.75 0 0 0 14.25 4.5H8.31l-.72-1.06a1.5 1.5 0 0 0-1.24-.69z"/>
                  </svg>
                )}
                {seg}
              </button>
            </span>
          );
        })}
      </div>
      {children}
      <div className="file-editor-bar">
        {bar}
        <span className="file-editor-bar-grow" />
        <button type="button" className="file-editor-bar-item" onClick={() => void commands.showInFolder(path)}>
          {t('menu.show_in_folder')}
        </button>
      </div>
    </div>
  );
}

function MediaPreview({ path, rel, kind }: { path: string; rel: string; kind: PreviewKind }) {
  const t = useT();
  const src = useAssetSrc(path);
  const [fit, setFit] = useState(true);
  const [failed, setFailed] = useState(false);
  const [meta, setMeta] = useState('');
  const kindLabel =
    kind === 'image' ? t('editor.image')
    : kind === 'video' ? t('editor.video')
    : kind === 'audio' ? t('editor.audio')
    : kind === 'pdf' ? t('editor.pdf')
    : t('editor.font');

  useEffect(() => {
    setFailed(false);
    setFit(true);
    setMeta('');
  }, [path]);

  const bar = (
    <>
      <span className="file-editor-bar-save">{kindLabel}{meta ? `  ·  ${meta}` : ''}</span>
      {kind === 'image' && (
        <button type="button" className="file-editor-bar-item" onClick={() => setFit((v) => !v)}>
          {fit ? t('editor.preview_actual') : t('editor.preview_fit')}
        </button>
      )}
    </>
  );

  if (!src) {
    return (
      <PreviewChrome path={path} rel={rel} bar={bar}>
        <div className="file-preview-stage"><div className="file-editor-skel" style={{ width: 180, height: 12 }} /></div>
      </PreviewChrome>
    );
  }

  if (failed) {
    return (
      <PreviewChrome path={path} rel={rel} bar={bar}>
        <div className="file-preview-stage">
          <div className="file-preview-unknown">
            <FileKindIcon name={path} size={36} />
            <div className="file-preview-unknown-name">{path.replace(/\\/g, '/').split('/').pop()}</div>
            <div>{t('editor.preview_error')}</div>
          </div>
        </div>
      </PreviewChrome>
    );
  }

  return (
    <PreviewChrome path={path} rel={rel} bar={bar}>
      {kind === 'image' && (
        <div className={`file-preview-stage file-preview-checker${fit ? '' : ' is-scroll'}`}>
          <img
            className={`file-preview-img${fit ? ' is-fit' : ' is-actual'}`}
            src={src}
            alt=""
            onError={() => setFailed(true)}
            onLoad={(e) => {
              const img = e.currentTarget;
              setMeta(`${img.naturalWidth} × ${img.naturalHeight}`);
            }}
          />
        </div>
      )}
      {kind === 'video' && (
        <div className="file-preview-stage file-preview-stage--media">
          <video
            className="file-preview-video"
            src={src}
            controls
            preload="metadata"
            onError={() => setFailed(true)}
            onLoadedMetadata={(e) => {
              const v = e.currentTarget;
              const d = Number.isFinite(v.duration) ? v.duration : 0;
              const mm = Math.floor(d / 60);
              const ss = Math.floor(d % 60).toString().padStart(2, '0');
              const dim = v.videoWidth && v.videoHeight ? `${v.videoWidth} × ${v.videoHeight} · ` : '';
              setMeta(`${dim}${mm}:${ss}`);
            }}
          />
        </div>
      )}
      {kind === 'audio' && (
        <div className="file-preview-stage">
          <div className="file-preview-audio-wrap">
            <FileKindIcon name={path} size={48} />
            <div className="file-preview-unknown-name">{path.replace(/\\/g, '/').split('/').pop()}</div>
            <audio
              className="file-preview-audio"
              src={src}
              controls
              preload="metadata"
              onError={() => setFailed(true)}
              onLoadedMetadata={(e) => {
                const d = e.currentTarget.duration;
                if (!Number.isFinite(d)) return;
                const mm = Math.floor(d / 60);
                const ss = Math.floor(d % 60).toString().padStart(2, '0');
                setMeta(`${mm}:${ss}`);
              }}
            />
          </div>
        </div>
      )}
      {kind === 'pdf' && (
        <div className="file-preview-stage">
          <iframe className="file-preview-pdf" src={src} title={rel || path} onError={() => setFailed(true)} />
        </div>
      )}
      {kind === 'font' && (
        <div className="file-preview-stage">
          <style>{`@font-face { font-family: "fe-preview-font"; src: url("${src}"); }`}</style>
          <div className="file-preview-font" style={{ fontFamily: 'fe-preview-font, var(--font)' }}>
            <div className="file-preview-font-sample">ABCDEFGHIJKLMNOPQRSTUVWXYZ</div>
            <div className="file-preview-font-sample">abcdefghijklmnopqrstuvwxyz</div>
            <div className="file-preview-font-sample">0123456789 .,;:!? @#$%</div>
            <div className="file-preview-font-sample">The quick brown fox jumps over the lazy dog.</div>
            <div className="file-preview-font-sample">汉字测试 あいうえお 한글</div>
          </div>
        </div>
      )}
    </PreviewChrome>
  );
}

function UnknownBinary({ path, rel }: { path: string; rel: string }) {
  const t = useT();
  const name = path.replace(/\\/g, '/').split('/').pop() || path;
  return (
    <PreviewChrome
      path={path}
      rel={rel}
      bar={<span className="file-editor-bar-save">{t('editor.no_preview')}</span>}
    >
      <div className="file-preview-stage">
        <div className="file-preview-unknown">
          <FileKindIcon name={name} size={36} />
          <div className="file-preview-unknown-name">{name}</div>
          <div>{t('editor.no_preview')}</div>
        </div>
      </div>
    </PreviewChrome>
  );
}

function TextFileEditor({ path, repoRoot, rel, kind }: FileEditorProps) {
  const t = useT();
  const dataTheme = useDataAttr('data-theme');
  const [draft, setDraft] = useState('');
  const [status, setStatus] = useState<SaveStatus>('loading');
  const [marks, setMarks] = useState<Marks>({ add: new Set(), mod: new Set(), del: new Set() });
  const [tokens, setTokens] = useState<LineTokens[] | null>(null);
  const [cursor, setCursor] = useState({ line: 1, col: 1, selected: 0 });
  const [wrap, setWrap] = useState(() => WRAP_EXTS.has(fileExt(path)));
  const [findOpen, setFindOpen] = useState(false);
  const [findQ, setFindQ] = useState('');
  const [findI, setFindI] = useState(0);
  const [gotoOpen, setGotoOpen] = useState(false);
  const [gotoQ, setGotoQ] = useState('');
  const [copied, setCopied] = useState(false);
  const [view, setView] = useState({ top: 0, h: 1 });

  const baselineRef = useRef('');
  const isNewFileRef = useRef(false);
  const draftRef = useRef('');
  const savedRef = useRef('');
  const saveTimer = useRef<number | null>(null);
  const hlTimer = useRef<number | null>(null);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const findRef = useRef<HTMLInputElement>(null);
  const gotoRef = useRef<HTMLInputElement>(null);
  const saveGen = useRef(0);
  const copyTimer = useRef<number | null>(null);

  const ext = fileExt(path);
  const lines = useMemo(() => draft.split('\n'), [draft]);
  const crumbs = useMemo(() => {
    const raw = (rel || path).replace(/\\/g, '/');
    return raw.split('/').filter(Boolean);
  }, [path, rel]);
  const indent = useMemo(() => detectIndent(draft), [draft]);
  const langLabel = LANG_LABEL[ext] ?? (t('editor.plain_text') || 'Plain Text');
  const eol = draft.includes('\r\n') ? 'CRLF' : 'LF';
  const dirty = draft !== savedRef.current && status !== 'loading';
  const hits = useMemo(() => (findOpen ? findHits(draft, findQ) : []), [draft, findOpen, findQ]);
  const activeHit = hits.length > 0 ? hits[Math.min(findI, hits.length - 1)] : null;
  const hitsByLine = useMemo(() => {
    const map = new Map<number, { start: number; end: number }[]>();
    if (!findOpen || !findQ) return map;
    const lineStarts: number[] = [0];
    for (let i = 0; i < draft.length; i += 1) {
      if (draft.charCodeAt(i) === 10) lineStarts.push(i + 1);
    }
    for (const h of hits) {
      const lineStart = lineStarts[h.line - 1] ?? 0;
      const arr = map.get(h.line) ?? [];
      arr.push({ start: h.start - lineStart, end: h.end - lineStart });
      map.set(h.line, arr);
    }
    return map;
  }, [draft, findOpen, findQ, hits]);
  const gutterW = Math.max(3, String(lines.length).length) * 8 + 28;
  const hitsRef = useRef(hits);
  hitsRef.current = hits;
  const findIRef = useRef(findI);
  findIRef.current = findI;

  const syncCursor = (el: HTMLTextAreaElement) => {
    const start = el.selectionStart ?? 0;
    const end = el.selectionEnd ?? start;
    const { line, col } = cursorAt(draftRef.current, start);
    setCursor({ line, col, selected: Math.abs(end - start) });
  };

  const syncView = () => {
    const b = bodyRef.current;
    if (!b) return;
    const sh = Math.max(b.scrollHeight, 1);
    setView({ top: b.scrollTop / sh, h: Math.min(1, b.clientHeight / sh) });
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
      setMarks(classifyMarks(baselineRef.current, text));
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

  const apply = (next: string, selStart: number, selEnd: number) => {
    draftRef.current = next;
    setDraft(next);
    setMarks(classifyMarks(baselineRef.current, next));
    setStatus('saving');
    scheduleSave(next);
    highlight(next);
    requestAnimationFrame(() => {
      const el = taRef.current;
      if (!el) return;
      el.selectionStart = selStart;
      el.selectionEnd = selEnd;
      syncCursor(el);
    });
  };

  const jumpTo = (pos: number, end: number = pos, takeFocus = true) => {
    const el = taRef.current;
    if (!el) return;
    if (takeFocus) el.focus();
    el.selectionStart = pos;
    el.selectionEnd = end;
    syncCursor(el);
    const line = cursorAt(draftRef.current, pos).line;
    const node = bodyRef.current?.querySelectorAll('.file-editor-hl-line')[line - 1];
    node?.scrollIntoView({ block: 'center' });
  };

  const jumpHit = (index: number) => {
    const list = hitsRef.current;
    if (list.length === 0) return;
    const i = (index + list.length) % list.length;
    setFindI(i);
    jumpTo(list[i].start, list[i].end, false);
  };

  const copyPath = (value: string) => {
    void clipboardWrite(value);
    setCopied(true);
    if (copyTimer.current != null) window.clearTimeout(copyTimer.current);
    copyTimer.current = window.setTimeout(() => {
      copyTimer.current = null;
      setCopied(false);
    }, 1400);
  };

  useEffect(() => {
    let cancelled = false;
    setStatus('loading');
    setDraft('');
    setTokens(null);
    draftRef.current = '';
    savedRef.current = '';
    baselineRef.current = '';
    setMarks({ add: new Set(), mod: new Set(), del: new Set() });
    setFindOpen(false);
    setGotoOpen(false);
    setWrap(WRAP_EXTS.has(fileExt(path)));
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
        setMarks(classifyMarks(baseline, working));
        setStatus('saved');
        const theme = getShikiTheme(dataTheme);
        const next = await tokenizeFile(working, path, theme);
        if (!cancelled) setTokens(next);
        requestAnimationFrame(() => {
          taRef.current?.focus();
          syncView();
        });
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

  useEffect(() => {
    syncView();
    const onResize = () => syncView();
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, [draft, wrap, lines.length, status]);

  useEffect(() => {
    if (!findOpen || !findQ) return;
    const list = hitsRef.current;
    if (list.length === 0) return;
    setFindI(0);
    jumpTo(list[0].start, list[0].end, false);
  }, [findQ, findOpen]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const root = rootRef.current;
      if (!root || !root.contains(document.activeElement)) return;
      const mod = e.metaKey || e.ctrlKey;
      const key = e.key.toLowerCase();

      if (mod && key === 's') {
        e.preventDefault();
        if (saveTimer.current != null) {
          window.clearTimeout(saveTimer.current);
          saveTimer.current = null;
        }
        void flushSave(draftRef.current);
        return;
      }
      if (mod && key === 'f') {
        e.preventDefault();
        const el = taRef.current;
        const selected = el && el.selectionStart !== el.selectionEnd
          ? draftRef.current.slice(el.selectionStart, el.selectionEnd)
          : '';
        setGotoOpen(false);
        setFindOpen(true);
        if (selected && !selected.includes('\n')) setFindQ(selected);
        requestAnimationFrame(() => findRef.current?.select());
        return;
      }
      if (e.ctrlKey && !e.metaKey && key === 'g') {
        e.preventDefault();
        setFindOpen(false);
        setGotoOpen(true);
        setGotoQ(String(cursorAt(draftRef.current, taRef.current?.selectionStart ?? 0).line));
        requestAnimationFrame(() => gotoRef.current?.select());
        return;
      }
      if (findOpen && ((mod && key === 'g') || e.key === 'F3')) {
        e.preventDefault();
        const list = hitsRef.current;
        if (list.length === 0) return;
        const i = (findIRef.current + (e.shiftKey ? -1 : 1) + list.length) % list.length;
        setFindI(i);
        jumpTo(list[i].start, list[i].end, false);
        return;
      }
      if (e.altKey && key === 'z') {
        e.preventDefault();
        setWrap((w) => !w);
        return;
      }
      if (e.key === 'Escape') {
        if (findOpen || gotoOpen) {
          e.preventDefault();
          setFindOpen(false);
          setGotoOpen(false);
          taRef.current?.focus();
        }
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [findOpen, gotoOpen, flushSave]);

  const onChange = (value: string) => {
    draftRef.current = value;
    setDraft(value);
    setMarks(classifyMarks(baselineRef.current, value));
    setStatus('saving');
    scheduleSave(value);
    highlight(value);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    const el = e.currentTarget;
    const start = el.selectionStart;
    const end = el.selectionEnd;
    const text = draft;
    const pad = indent.tabs ? '\t' : ' '.repeat(indent.size);
    const mod = e.metaKey || e.ctrlKey;

    if (e.key === 'Tab') {
      e.preventDefault();
      if (start !== end || e.shiftKey) {
        const next = mapLines(text, start, end, (line) => {
          if (e.shiftKey) {
            if (line.startsWith('\t')) return line.slice(1);
            let n = 0;
            while (n < indent.size && line[n] === ' ') n += 1;
            return line.slice(n);
          }
          return pad + line;
        });
        apply(next.next, next.start, next.end);
      } else {
        apply(`${text.slice(0, start)}${pad}${text.slice(end)}`, start + pad.length, start + pad.length);
      }
      return;
    }

    if (e.key === 'Enter' && !e.nativeEvent.isComposing) {
      e.preventDefault();
      const lineStart = text.lastIndexOf('\n', start - 1) + 1;
      const before = text.slice(lineStart, start);
      const lead = before.match(/^[\t ]*/)?.[0] ?? '';
      const extra = /[{[(]\s*$/.test(before) ? pad : '';
      const insert = `\n${lead}${extra}`;
      apply(`${text.slice(0, start)}${insert}${text.slice(end)}`, start + insert.length, start + insert.length);
      return;
    }

    if (mod && e.key === '/') {
      e.preventDefault();
      const marker = LINE_COMMENT[ext];
      if (!marker) return;
      const { from, to } = lineSpan(text, start, end);
      const rows = text.split('\n');
      const slice = rows.slice(from - 1, to);
      const nonempty = slice.filter((s) => s.trim() !== '');
      const allOn = nonempty.length > 0 && nonempty.every((s) => s.trimStart().startsWith(marker));
      const next = mapLines(text, start, end, (line) => {
        if (line.trim() === '') return line;
        const lead = line.match(/^[\t ]*/)?.[0] ?? '';
        const rest = line.slice(lead.length);
        if (allOn) {
          const cut = rest.startsWith(`${marker} `) ? marker.length + 1 : marker.length;
          return lead + rest.slice(cut);
        }
        return `${lead}${marker} ${rest}`;
      });
      apply(next.next, next.start, next.end);
      return;
    }

    if (e.altKey && (e.key === 'ArrowUp' || e.key === 'ArrowDown')) {
      e.preventDefault();
      const dir = e.key === 'ArrowUp' ? -1 : 1;
      const { from, to } = lineSpan(text, start, end);
      const rows = text.split('\n');
      if (e.shiftKey) {
        const chunk = rows.slice(from - 1, to);
        const dest = dir === 1 ? to : from - 1;
        rows.splice(dest, 0, ...chunk);
        const next = rows.join('\n');
        let pos = 0;
        const destLine = dir === 1 ? to : from - 1;
        for (let i = 0; i < destLine; i += 1) pos += rows[i].length + 1;
        apply(next, pos, pos + chunk.join('\n').length);
        return;
      }
      if (dir === -1 && from === 1) return;
      if (dir === 1 && to === rows.length) return;
      const chunk = rows.splice(from - 1, to - from + 1);
      const dest = from - 1 + dir;
      rows.splice(dest, 0, ...chunk);
      const next = rows.join('\n');
      let pos = 0;
      for (let i = 0; i < dest; i += 1) pos += rows[i].length + 1;
      apply(next, pos, pos + chunk.join('\n').length);
    }
  };

  const selectLine = (lineNo: number) => {
    const text = draftRef.current;
    let start = 0;
    for (let i = 1; i < lineNo; i += 1) {
      const n = text.indexOf('\n', start);
      if (n < 0) {
        start = text.length;
        break;
      }
      start = n + 1;
    }
    let end = text.indexOf('\n', start);
    if (end < 0) end = text.length;
    else end += 1;
    jumpTo(start, end);
  };

  const gotoLine = (raw: string) => {
    const n = parseInt(raw.replace(/[^\d]/g, ''), 10);
    if (!Number.isFinite(n) || n < 1) return;
    const text = draftRef.current;
    const max = text.split('\n').length;
    const line = Math.min(n, max);
    let pos = 0;
    for (let i = 1; i < line; i += 1) {
      const idx = text.indexOf('\n', pos);
      if (idx < 0) break;
      pos = idx + 1;
    }
    setGotoOpen(false);
    jumpTo(pos);
  };

  const jumpMinimap = (clientY: number, target: HTMLElement) => {
    const rect = target.getBoundingClientRect();
    const frac = Math.max(0, Math.min(1, (clientY - rect.top) / Math.max(rect.height, 1)));
    const line = Math.max(0, Math.min(lines.length - 1, Math.floor(frac * lines.length)));
    const node = bodyRef.current?.querySelectorAll('.file-editor-hl-line')[line];
    node?.scrollIntoView({ block: 'center' });
  };

  const statusLabel = copied
    ? (t('editor.copied') || 'Path copied')
    : status === 'saving' ? (t('editor.saving') || 'Saving…')
    : status === 'saved' ? (t('editor.saved') || 'Saved')
    : status === 'error' ? (t('editor.save_error') || 'Save failed')
    : '';

  const gitLetter = kind === 'untracked' || isNewFileRef.current
    ? 'U'
    : (marks.add.size + marks.mod.size + marks.del.size) > 0 ? 'M' : '';
  const gitTitle = gitLetter === 'U'
    ? (t('editor.untracked') || 'Untracked')
    : gitLetter === 'M' ? (t('editor.modified') || 'Modified') : '';

  if (status === 'loading') {
    return (
      <div className="file-editor">
        <div className="file-editor-bc">
          <span className="file-editor-skel" style={{ width: 220 }} />
        </div>
        <div className="file-editor-work">
          <div className="file-editor-body">
            {Array.from({ length: 16 }, (_, i) => (
              <div key={i} className="file-editor-skel-row">
                <span className="file-editor-skel-num">{i + 1}</span>
                <span className="file-editor-skel" style={{ width: `${36 + ((i * 47) % 48)}%` }} />
              </div>
            ))}
          </div>
        </div>
        <div className="file-editor-bar" />
      </div>
    );
  }
  if (status === 'too_large') {
    return <div className="file-editor"><div className="file-editor-empty">{t('editor.too_large') || 'File too large to edit here'}</div></div>;
  }
  if ((status === 'binary' || status === 'error') && !draft) {
    return <UnknownBinary path={path} rel={rel} />;
  }

  const currentHitRange = activeHit
    ? { start: activeHit.col - 1, end: activeHit.col - 1 + (activeHit.end - activeHit.start) }
    : null;

  return (
    <div
      ref={rootRef}
      className={`file-editor${wrap ? ' is-wrap' : ' is-nowrap'}`}
      style={{
        ['--fe-indent' as string]: indent.tabs ? '4ch' : `${indent.size}ch`,
        ['--fe-gutter-w' as string]: `${gutterW}px`,
        ['--fe-tab' as string]: String(indent.size),
        ['--fe-rows' as string]: String(lines.length),
      }}
    >
      <div className="file-editor-bc" title={rel || path}>
        {crumbs.map((seg, i) => {
          const prefix = crumbs.slice(0, i + 1).join('/');
          const last = i === crumbs.length - 1;
          return (
            <span key={`${i}-${seg}`} className="file-editor-bc-item">
              {i > 0 && (
                <svg className="file-editor-bc-chevron" viewBox="0 0 16 16" width="12" height="12" aria-hidden>
                  <path fill="currentColor" d="M6.7 3.3a.75.75 0 0 1 1.06 0l4 4a.75.75 0 0 1 0 1.06l-4 4A.75.75 0 0 1 6.7 11.3L10.04 8 6.7 4.7a.75.75 0 0 1 0-1.4z"/>
                </svg>
              )}
              <button
                type="button"
                className={last ? 'file-editor-bc-file' : 'file-editor-bc-dir'}
                onClick={() => copyPath(prefix)}
                title={prefix}
              >
                {last ? <FileKindIcon name={seg} size={14} /> : (
                  <svg className="file-editor-bc-folder" viewBox="0 0 16 16" width="12" height="12" aria-hidden>
                    <path fill="currentColor" d="M1.75 2.5A.75.75 0 0 0 1 3.25v9.5c0 .41.34.75.75.75h12.5A.75.75 0 0 0 15 12.75v-7.5A.75.75 0 0 0 14.25 4.5H8.31l-.72-1.06a1.5 1.5 0 0 0-1.24-.69z"/>
                  </svg>
                )}
                {last && dirty && <span className="file-editor-dirty" />}
                {seg}
              </button>
            </span>
          );
        })}
      </div>

      <div className="file-editor-work">
        {(findOpen || gotoOpen) && (
          <div className="file-editor-widget">
            {findOpen && (
              <>
                <svg className="file-editor-widget-icon" viewBox="0 0 16 16" width="14" height="14" aria-hidden>
                  <path fill="currentColor" d="M10.68 11.74a6 6 0 1 1 1.06-1.06l3.04 3.04a.75.75 0 1 1-1.06 1.06zm-1.48-1.48a4.5 4.5 0 1 0-6.36-6.36 4.5 4.5 0 0 0 6.36 6.36"/>
                </svg>
                <input
                  ref={findRef}
                  className="file-editor-widget-input"
                  value={findQ}
                  placeholder={t('editor.find') || 'Find'}
                  spellCheck={false}
                  onChange={(e) => {
                    setFindQ(e.target.value);
                    setFindI(0);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') {
                      e.preventDefault();
                      jumpHit(findI + (e.shiftKey ? -1 : 1));
                    }
                  }}
                />
                <span className="file-editor-widget-count">
                  {findQ
                    ? (hits.length === 0
                      ? (t('editor.find_none') || 'No results')
                      : `${Math.min(findI, Math.max(hits.length - 1, 0)) + (hits.length ? 1 : 0)} / ${hits.length}`)
                    : ''}
                </span>
                <button type="button" className="file-editor-widget-btn" title={t('editor.find_prev') || 'Previous'} onClick={() => jumpHit(findI - 1)}>▲</button>
                <button type="button" className="file-editor-widget-btn" title={t('editor.find_next') || 'Next'} onClick={() => jumpHit(findI + 1)}>▼</button>
                <button type="button" className="file-editor-widget-btn" title="Esc" onClick={() => { setFindOpen(false); taRef.current?.focus(); }}>✕</button>
              </>
            )}
            {gotoOpen && (
              <>
                <input
                  ref={gotoRef}
                  className="file-editor-widget-input"
                  value={gotoQ}
                  placeholder={t('editor.goto') || 'Go to line'}
                  spellCheck={false}
                  onChange={(e) => setGotoQ(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') {
                      e.preventDefault();
                      gotoLine(gotoQ);
                    }
                  }}
                />
                <span className="file-editor-widget-count">{lines.length}</span>
                <button type="button" className="file-editor-widget-btn" onClick={() => gotoLine(gotoQ)}>↵</button>
              </>
            )}
          </div>
        )}

        <div
          ref={bodyRef}
          className="file-editor-body"
          onScroll={syncView}
        >
          <div className="file-editor-sheet">
            {lines.map((_, i) => {
              const n = i + 1;
              const kindMark: GutterKind | null = marks.add.has(n) ? 'add' : marks.mod.has(n) ? 'mod' : marks.del.has(n) ? 'del' : null;
              return (
                <div
                  key={`g${n}`}
                  className={`file-editor-gutter-row${kindMark ? ` is-${kindMark}` : ''}${cursor.line === n ? ' is-active' : ''}`}
                  style={{ gridRow: n }}
                  onMouseDown={(e) => {
                    e.preventDefault();
                    selectLine(n);
                  }}
                >
                  {n}
                </div>
              );
            })}
            {lines.map((line, i) => {
              const n = i + 1;
              const guides = leadingCols(line, indent.size);
              const lineHits = hitsByLine.get(n) ?? [];
              const current = activeHit && activeHit.line === n ? currentHitRange : null;
              return (
                <div
                  key={`h${n}`}
                  className={`file-editor-hl-line${cursor.line === n ? ' is-active' : ''}`}
                  style={{ gridRow: n }}
                >
                  {guides > 0 && <span className="file-editor-indent" style={{ width: `${guides}ch` }} />}
                  {renderLine(line, tokens?.[i], lineHits, current)}
                </div>
              );
            })}
            {!wrap && (
              <div
                className="file-editor-col80"
                style={{ gridColumn: 2, gridRow: `1 / ${lines.length + 2}` }}
                aria-hidden
              />
            )}
            <div
              className="file-editor-ta-layer"
              style={{ gridColumn: 2, gridRow: `1 / ${lines.length + 2}` }}
            >
              <textarea
                ref={taRef}
                className="file-editor-ta"
                value={draft}
                spellCheck={false}
                wrap={wrap ? 'soft' : 'off'}
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

        <Minimap
          lines={lines}
          marks={marks}
          current={cursor.line}
          view={view}
          light={dataTheme === 'light'}
          onJump={jumpMinimap}
        />
      </div>

      <div className="file-editor-bar">
        <span className={`file-editor-bar-git${gitLetter ? ` is-${gitLetter}` : ''}`} title={gitTitle}>{gitLetter}</span>
        <span className={`file-editor-bar-save is-${copied ? 'copied' : status}`}>{statusLabel}</span>
        <span className="file-editor-bar-grow" />
        <button
          type="button"
          className="file-editor-bar-item"
          onClick={() => {
            setFindOpen(false);
            setGotoOpen(true);
            setGotoQ(String(cursor.line));
            requestAnimationFrame(() => gotoRef.current?.select());
          }}
        >
          {cursor.selected > 0
            ? `Ln ${cursor.line}, Col ${cursor.col} (${cursor.selected} sel)`
            : `Ln ${cursor.line}, Col ${cursor.col}`}
        </button>
        <span className="file-editor-bar-item">
          {indent.tabs
            ? (t('editor.tab_size', { size: indent.size }) || `Tab Size: ${indent.size}`)
            : (t('editor.spaces', { size: indent.size }) || `Spaces: ${indent.size}`)}
        </span>
        <span className="file-editor-bar-item">UTF-8</span>
        <span className="file-editor-bar-item">{eol}</span>
        <button type="button" className="file-editor-bar-item" onClick={() => setWrap((w) => !w)}>
          {wrap ? (t('editor.wrap') || 'Wrap') : (t('editor.nowrap') || 'No Wrap')}
        </button>
        <span className="file-editor-bar-item file-editor-bar-lang">
          <FileKindIcon name={crumbs[crumbs.length - 1] ?? path} size={12} />
          {langLabel}
        </span>
      </div>
    </div>
  );
}

function Minimap({
  lines,
  marks,
  current,
  view,
  light,
  onJump,
}: {
  lines: string[];
  marks: Marks;
  current: number;
  view: { top: number; h: number };
  light: boolean;
  onJump: (clientY: number, target: HTMLElement) => void;
}) {
  const boxRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [h, setH] = useState(0);

  useEffect(() => {
    const el = boxRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setH(el.clientHeight));
    ro.observe(el);
    setH(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || h <= 0) return;
    const dpr = window.devicePixelRatio || 1;
    const w = 72;
    canvas.width = Math.floor(w * dpr);
    canvas.height = Math.floor(h * dpr);
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const n = Math.max(lines.length, 1);
    const rowH = h / n;
    const scale = Math.min(0.72, Math.max(0.28, rowH));
    for (let i = 0; i < lines.length; i += 1) {
      const line = lines[i];
      let lead = 0;
      while (lead < line.length && (line[lead] === ' ' || line[lead] === '\t')) lead += 1;
      const body = line.length - lead;
      if (body <= 0) continue;
      const y = i * rowH;
      const x = 4 + lead * 0.45;
      const bw = Math.min(w - 8 - x, body * 0.7);
      const n1 = i + 1;
      if (marks.add.has(n1)) ctx.fillStyle = '#3dd68c';
      else if (marks.mod.has(n1)) ctx.fillStyle = '#4fc1ff';
      else ctx.fillStyle = light ? 'rgba(60,60,60,0.35)' : 'rgba(212,212,212,0.38)';
      ctx.fillRect(x, y, Math.max(2, bw), Math.max(scale, 1.1));
    }
    if (current >= 1) {
      ctx.fillStyle = light ? 'rgba(0,0,0,0.08)' : 'rgba(255,255,255,0.10)';
      ctx.fillRect(0, (current - 1) * rowH, w, Math.max(rowH, 2));
    }
  }, [lines, marks, current, h, light]);

  return (
    <div
      ref={boxRef}
      className="file-editor-map"
      onMouseDown={(e) => {
        e.preventDefault();
        const target = e.currentTarget;
        onJump(e.clientY, target);
        const move = (ev: MouseEvent) => onJump(ev.clientY, target);
        const up = () => {
          window.removeEventListener('mousemove', move);
          window.removeEventListener('mouseup', up);
        };
        window.addEventListener('mousemove', move);
        window.addEventListener('mouseup', up);
      }}
      aria-hidden
    >
      <canvas ref={canvasRef} />
      <div
        className="file-editor-map-view"
        style={{ top: `${view.top * 100}%`, height: `${Math.max(view.h * 100, 4)}%` }}
      />
    </div>
  );
}
