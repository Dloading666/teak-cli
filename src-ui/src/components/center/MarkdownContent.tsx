import { useEffect, useState } from 'react';
import ReactMarkdown from 'react-markdown';
import type { Components } from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { tokenizeByLang, getShikiTheme, type LineTokens } from '../../lib/shiki';
import { useDataAttr } from '../../lib/use-data-attr';

function CodeBlock({ code, lang }: { code: string; lang: string | null }) {
  const dataTheme = useDataAttr('data-theme');
  const [tokens, setTokens] = useState<LineTokens[] | null>(null);

  useEffect(() => {
    if (!lang) return;
    let cancelled = false;
    tokenizeByLang(code, lang, getShikiTheme(dataTheme)).then(value => {
      if (!cancelled) setTokens(value);
    });
    return () => { cancelled = true; };
  }, [code, lang, dataTheme]);

  return (
    <pre className="md-code"><code>
      {tokens && lang
        ? tokens.map((line, i) => (
            <div key={i} className="md-code-line">
              {line.map((token, j) => <span key={j} style={{ color: token.color }}>{token.content}</span>)}
            </div>
          ))
        : code}
    </code></pre>
  );
}

// Stable component identities keep syntax-highlighted blocks mounted when a
// live transcript appends another message. Recreating this object per render
// makes ReactMarkdown remount every code block and repeat Shiki tokenization.
const MARKDOWN_COMPONENTS: Components = {
  code({ className, children, ...props }) {
    const fenced = /language-(\w+)/.exec(className || '');
    const text = String(children).replace(/\n$/, '');
    if (!fenced && !String(children).includes('\n')) {
      return <code className="md-inline-code" {...props}>{children}</code>;
    }
    return <CodeBlock code={text} lang={fenced?.[1] ?? null} />;
  },
  a({ children, href, ...props }) {
    return <a href={href} target="_blank" rel="noreferrer noopener" {...props}>{children}</a>;
  },
};

export function MarkdownContent({ content }: { content: string }) {
  return (
    <div className="md-body">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={MARKDOWN_COMPONENTS}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}
