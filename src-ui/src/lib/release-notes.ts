// GitHub Release notes are authored as Markdown with optional multilingual
// <details> sections. The titlebar card is intentionally plain text: release
// bodies are remote/untrusted content and must never be injected as HTML.

interface ReleaseSection {
  label: string;
  body: string;
}

const LOCALE_MARKERS: Record<string, string[]> = {
  'zh-CN': ['简体中文'],
  'zh-TW': ['繁體中文', '繁体中文'],
  ja: ['日本語'],
  ko: ['한국어'],
  en: ['English'],
};

const FALLBACK_MARKERS = LOCALE_MARKERS.en;

function decodeEntities(value: string): string {
  return value
    .replace(/&amp;/gi, '&')
    .replace(/&lt;/gi, '<')
    .replace(/&gt;/gi, '>')
    .replace(/&quot;/gi, '"')
    .replace(/&#(?:39|x27);/gi, "'")
    .replace(/&nbsp;/gi, ' ');
}

function plainText(value: string): string {
  return decodeEntities(value)
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
    .replace(/<[^>]*>/g, '')
    .replace(/\*\*|__/g, '')
    .replace(/~~/g, '')
    .replace(/`/g, '')
    .replace(/\\([\\`*_{}[\]()#+.!-])/g, '$1')
    .replace(/\s+/g, ' ')
    .trim();
}

function detailsSections(markdown: string): ReleaseSection[] {
  const sections: ReleaseSection[] = [];
  const detailsRe = /<details(?:\s[^>]*)?>([\s\S]*?)<\/details>/gi;
  for (const match of markdown.matchAll(detailsRe)) {
    const block = match[1] ?? '';
    const summary = block.match(/<summary(?:\s[^>]*)?>([\s\S]*?)<\/summary>/i);
    sections.push({
      label: plainText(summary?.[1] ?? ''),
      body: summary ? block.replace(summary[0], '') : block,
    });
  }
  return sections;
}

function chooseSection(markdown: string, locale: string): string {
  const sections = detailsSections(markdown);
  if (sections.length === 0) return markdown;

  const markers = LOCALE_MARKERS[locale] ?? FALLBACK_MARKERS;
  const localized = sections.find((section) => markers.some((marker) => section.label.includes(marker)));
  if (localized) return localized.body;

  const english = sections.find((section) => FALLBACK_MARKERS.some((marker) => section.label.includes(marker)));
  return english?.body ?? sections[0].body;
}

function bulletItems(markdown: string): string[] {
  const items: string[] = [];
  let current = -1;
  for (const line of markdown.split(/\r?\n/)) {
    const bullet = line.match(/^\s*[-*+]\s+(.+)$/);
    if (bullet) {
      items.push(bullet[1]);
      current = items.length - 1;
      continue;
    }
    // Preserve wrapped Markdown bullet lines, but stop joining at headings,
    // blank lines, or raw HTML scaffolding.
    if (current >= 0 && /^\s{2,}\S/.test(line) && !/^\s*</.test(line)) {
      items[current] += ` ${line.trim()}`;
    } else if (!line.trim()) {
      current = -1;
    }
  }
  return items.map(plainText).filter(Boolean);
}

/** Return a compact, localized, plain-text summary for the update popover. */
export function releaseNoteItems(markdown: string, locale: string, limit = 4): string[] {
  if (!markdown.trim() || limit <= 0) return [];
  const section = chooseSection(markdown, locale);
  const bullets = bulletItems(section);
  if (bullets.length > 0) return bullets.slice(0, limit);

  // Graceful fallback for release bodies written as short paragraphs rather
  // than a list. Headings/details tags are structure, not user-facing copy.
  return section
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line && !/^#{1,6}\s/.test(line) && !/^<\/?(?:details|summary)/i.test(line))
    .map(plainText)
    .filter(Boolean)
    .slice(0, limit);
}
