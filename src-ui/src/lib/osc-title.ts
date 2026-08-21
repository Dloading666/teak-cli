// Pull OSC 0/2 titles out of a raw PTY byte string.
//
// xterm's onTitleChange only fires after term.write(). Chat view hides the
// terminal (display:none), so the output scheduler treats it as background
// and can delay or drop those writes — the left-rail spinner then stays on
// after the agent is idle. Parsing the escape here is independent of xterm.

const OSC_TITLE_RE = /\x1b\](?:0|2);([^\x07\x1b]*)(?:\x07|\x1b\\)/g;

export function consumeOscTitles(buffer: string): { titles: string[]; rest: string } {
  const titles: string[] = [];
  OSC_TITLE_RE.lastIndex = 0;
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = OSC_TITLE_RE.exec(buffer)) !== null) {
    titles.push(match[1]);
    lastIndex = OSC_TITLE_RE.lastIndex;
  }

  let rest = buffer.slice(lastIndex);
  const esc = rest.lastIndexOf('\x1b');
  if (esc === -1) return { titles, rest: '' };

  const tail = rest.slice(esc);
  // Keep an incomplete OSC 0/2 prefix so a title split across PTY chunks
  // still assembles. Anything else is display data we don't need to carry.
  if (tail === '\x1b' || /^\x1b\](?:[02](?:;.*)?)?$/.test(tail)) {
    return { titles, rest: tail.length > 1024 ? tail.slice(-1024) : tail };
  }
  return { titles, rest: '' };
}
