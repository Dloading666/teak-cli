import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { AgentStatus, TerminalSession, ToolType } from '../../store/app-state';
import type { ChatSessionRead, SavedSession } from '../../tauri';
import { commands, isTauri } from '../../tauri';
import { bindAutoHideScrollbar } from '../../lib/auto-hide-scrollbar';
import { clipboardRead, clipboardReadImage, clipboardWrite } from '../../lib/clipboard';
import { getHistorySnapshot } from '../../lib/history-cache';
import { subscribeTerminalEvents } from '../../lib/pty-event-bus';
import {
  updateChatTranscript, normalizePrompt, transcriptHasPrompt,
  type ChatMessage, type ChatTranscriptState,
} from '../../lib/chat-transcript';
import { MarkdownContent } from './MarkdownContent';
import { TermContextMenu, type TermContextMenuState } from './TermContextMenu';
import './ConversationView.css';

interface ConversationViewProps {
  sessionId: string;
  tool: ToolType;
  folderPath: string | null;
  resumeToken?: string;
  startedAt?: number;
  pending?: TerminalSession['chatPending'];
  agentStatus?: AgentStatus;
  isActive: boolean;
  isVisible: boolean;
  onPendingResolved: () => void;
  onPasteToDraft: (text: string) => void;
  hasBg?: boolean;
  bgUrl?: string;
  bgType?: 'image' | 'video' | 'none';
  competingBindings?: Array<{ startedAt?: number; sentAt?: number }>;
}

interface ConversationCacheEntry {
  source: SavedSession | null;
  transcript: ChatTranscriptState;
  cursor: number | null;
  revision: string;
}

const conversationCache = new Map<string, ConversationCacheEntry>();
const sourceOwners = new Map<string, string>();
const CONVERSATION_CACHE_LIMIT = 12;
let historyRequest: Promise<SavedSession[]> | null = null;
let historyRequestForced = false;
let recentHistory: { sessions: SavedSession[]; fetchedAt: number } | null = null;
const HISTORY_CACHE_TTL_MS = 5_000;

function writeConversationCache(ownerKey: string, entry: ConversationCacheEntry) {
  conversationCache.delete(ownerKey);
  conversationCache.set(ownerKey, entry);
  while (conversationCache.size > CONVERSATION_CACHE_LIMIT) {
    const oldestKey = conversationCache.keys().next().value as string | undefined;
    if (!oldestKey) break;
    conversationCache.delete(oldestKey);
  }
}

/** Share the expensive native-history scan across every chat tab. A scan can
 * take seconds on large histories (and much longer in Rust debug builds), so
 * callers must join the same promise instead of starting overlapping scans. */
async function loadConversationHistory(force = false): Promise<SavedSession[]> {
  // A forced refresh that arrives behind a normal cached scan must wait for a
  // fresh producer. Re-check in a loop after every await: several chat tabs
  // can reach this branch in the same tick, and without the second check they
  // would all start their own full native-history walk once the older request
  // settles.
  while (historyRequest) {
    const inFlightWasForced = historyRequestForced;
    const sessions = await historyRequest;
    // A forced caller must not silently inherit an older cached request. Once
    // that request settles, loop so it either joins another tab's fresh scan
    // or becomes the single producer below.
    if (!force || inFlightWasForced) return sessions;
  }
  if (!force && recentHistory && Date.now() - recentHistory.fetchedAt < HISTORY_CACHE_TTL_MS) {
    return recentHistory.sessions;
  }
  historyRequestForced = force;
  historyRequest = commands.getNativeHistory(force)
    .then(sessions => {
      recentHistory = { sessions, fetchedAt: Date.now() };
      return sessions;
    })
    .finally(() => {
      historyRequest = null;
      historyRequestForced = false;
    });
  return historyRequest;
}

function normalizedPath(path: string): string {
  const normalized = path.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
  const worktreeMarker = normalized.indexOf('/.claude/worktrees/');
  return worktreeMarker >= 0 ? normalized.slice(0, worktreeMarker) : normalized;
}

function savedAtMs(value: string): number {
  const parsed = Date.parse(value);
  if (!Number.isNaN(parsed)) return parsed;
  const numeric = Number(value);
  if (!Number.isFinite(numeric) || numeric <= 0) return 0;
  return numeric < 1e11 ? numeric * 1000 : numeric;
}

function sourceIdentityDistance(
  session: SavedSession,
  startedAt: number | undefined,
  sentAt?: number,
): number {
  const createdAt = session.created_at ? savedAtMs(session.created_at) : 0;
  if (createdAt <= 0) return Number.POSITIVE_INFINITY;
  const anchors = [startedAt, sentAt].filter((value): value is number => Boolean(value));
  if (anchors.length === 0) return Number.POSITIVE_INFINITY;
  return Math.min(...anchors.map(anchor => Math.abs(createdAt - anchor)));
}

function selectSources(
  sessions: SavedSession[], tool: ToolType, folderPath: string | null,
  resumeToken: string | undefined, pending: TerminalSession['chatPending'],
  startedAt: number | undefined, ownerKey: string,
  competingBindings: Array<{ startedAt?: number; sentAt?: number }>,
): SavedSession[] {
  const matchingTool = sessions.filter(session => session.tool === tool);
  if (resumeToken) {
    const resumed = matchingTool.find(session => session.session_token === resumeToken);
    return resumed ? [resumed] : [];
  }
  // startedAt is the stable identity boundary for this PTY. saved_at remains
  // the per-turn activity timestamp used below to prove a pending prompt has
  // actually reached the source.
  const bindingSince = startedAt ?? pending?.sentAt;
  if (!bindingSince) return [];
  const cwd = folderPath ? normalizedPath(folderPath) : '';
  const sameWorkspace = matchingTool.filter(session => {
    const owner = sourceOwners.get(session.id);
    if (owner && owner !== ownerKey) return false;
    if (cwd && normalizedPath(session.cwd) !== cwd) return false;
    return true;
  });

  // A live file's mtime normally advances with the prompt. Keep that fast
  // path, but retain a few same-workspace fallbacks: some CLIs flush their
  // index timestamp later than the transcript itself. The prompt-content
  // check in discover() is the final authority and prevents an older session
  // from being attached to this tab.
  // The transcript must have been written after this terminal/prompt began.
  // Prompt text alone is not an identity: common messages such as “你好” can
  // exist in many sessions in the same workspace. A forced history scan will
  // surface the current file as soon as its first write lands, so waiting is
  // safer than falling back to an older candidate.
  const recent = sameWorkspace.filter(session => savedAtMs(session.saved_at) >= bindingSince);
  const identityOwned = recent.filter(session => {
    const ownDistance = sourceIdentityDistance(session, startedAt, pending?.sentAt);
    if (!Number.isFinite(ownDistance)) return true;
    return !competingBindings.some(binding =>
      sourceIdentityDistance(session, binding.startedAt, binding.sentAt) < ownDistance
    );
  });
  // saved_at is an update timestamp and therefore cannot identify which of
  // two concurrently-running terminals created a source. Prefer the stable
  // creation timestamp nearest this PTY launch; retain saved_at only as a
  // fallback for history produced by older backends.
  identityOwned.sort((a, b) => {
    const aDistance = sourceIdentityDistance(a, startedAt, pending?.sentAt);
    const bDistance = sourceIdentityDistance(b, startedAt, pending?.sentAt);
    const aHasIdentity = Number.isFinite(aDistance);
    const bHasIdentity = Number.isFinite(bDistance);
    if (aHasIdentity !== bHasIdentity) return aHasIdentity ? -1 : 1;
    if (aHasIdentity && aDistance !== bDistance) return aDistance - bDistance;
    return Math.abs(savedAtMs(a.saved_at) - bindingSince) - Math.abs(savedAtMs(b.saved_at) - bindingSince);
  });
  return identityOwned.slice(0, pending ? 6 : 1);
}

function promptIndexSince(messages: ChatMessage[], prompt: string, baselineUserCount: number): number {
  const target = normalizePrompt(prompt);
  let userOrdinal = 0;
  let match = -1;
  for (let index = 0; index < messages.length; index += 1) {
    const message = messages[index];
    if (message.role !== 'user') continue;
    if (userOrdinal >= baselineUserCount && normalizePrompt(message.content) === target) {
      match = index;
    }
    userOrdinal += 1;
  }
  return match;
}

function hasAssistantAfterPrompt(
  messages: ChatMessage[], prompt: string, baselineUserCount = 0,
): boolean {
  const userIndex = promptIndexSince(messages, prompt, baselineUserCount);
  return userIndex >= 0 && messages.slice(userIndex + 1).some(message => message.role === 'assistant');
}

function applySessionRead(
  current: ChatTranscriptState,
  read: ChatSessionRead,
): ChatTranscriptState {
  if (read.unchanged) return current;
  return read.append ? updateChatTranscript(read.data, current) : updateChatTranscript(read.data);
}

function ToolRow({ message }: { message: ChatMessage }) {
  const status = message.toolStatus ?? 'running';
  return (
    <details className="conversation-tool" open={status === 'failed'}>
      <summary>
        <span className={`conversation-tool-status conversation-tool-status--${status}`} aria-hidden="true" />
        <span>{message.toolName}</span>
        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <polyline points="9 18 15 12 9 6" />
        </svg>
      </summary>
      {message.content && <pre>{message.content}</pre>}
    </details>
  );
}

function MessageCopyButton({ copied, onCopy }: { copied: boolean; onCopy: () => void }) {
  return (
    <button
      type="button"
      className={`conversation-copy${copied ? ' conversation-copy--copied' : ''}`}
      aria-label={copied ? 'Copied' : 'Copy message'}
      onClick={onCopy}
    >
      {copied ? (
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <polyline points="5 13 9 17 19 7" />
        </svg>
      ) : (
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <rect x="8" y="8" width="11" height="11" rx="2" />
          <path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" />
        </svg>
      )}
    </button>
  );
}

export function ConversationView({
  sessionId, tool, folderPath, resumeToken, startedAt, pending, agentStatus, isActive, isVisible,
  onPendingResolved, onPasteToDraft, hasBg, bgUrl, bgType, competingBindings = [],
}: ConversationViewProps) {
  const ownerKey = `${sessionId}:${tool ?? 'none'}:${resumeToken ?? 'fresh'}:${startedAt ?? 'unknown'}`;
  const cached = conversationCache.get(ownerKey);
  const initialTranscript = cached?.transcript ?? { messages: [], remainder: '', nextLineIndex: 0 };
  const [source, setSource] = useState<SavedSession | null>(cached?.source ?? null);
  const [messages, setMessages] = useState<ChatMessage[]>(initialTranscript.messages);
  const transcriptRef = useRef<ChatTranscriptState>(initialTranscript);
  const cursorRef = useRef<number | null>(cached?.cursor ?? null);
  const revisionRef = useRef(cached?.revision ?? '');
  const scrollRef = useRef<HTMLDivElement>(null);
  const threadRef = useRef<HTMLDivElement>(null);
  const pinnedRef = useRef(true);
  const onPendingResolvedRef = useRef(onPendingResolved);
  const [copiedMessageId, setCopiedMessageId] = useState<string | null>(null);
  const [ctxMenu, setCtxMenu] = useState<TermContextMenuState | null>(null);
  const navigationRef = useRef<HTMLElement>(null);
  const navigationScrollRef = useRef<HTMLDivElement>(null);
  const navigationTooltipId = useId();
  const [hoveredNavigationId, setHoveredNavigationId] = useState<string | null>(null);
  const [activeNavigationId, setActiveNavigationId] = useState<string | null>(null);
  const [navigationTooltipTop, setNavigationTooltipTop] = useState(0);
  const copyResetTimerRef = useRef<number | null>(null);
  const pendingBaselineRef = useRef<{ sentAt: number; userCount: number } | null>(null);
  const competingBindingsRef = useRef(competingBindings);
  competingBindingsRef.current = competingBindings;
  const competingBindingsKey = competingBindings
    .map(binding => `${binding.startedAt ?? 0}:${binding.sentAt ?? 0}`)
    .sort()
    .join('|');

  if (pending && pendingBaselineRef.current?.sentAt !== pending.sentAt) {
    pendingBaselineRef.current = {
      sentAt: pending.sentAt,
      userCount: messages.filter(message => message.role === 'user').length,
    };
  } else if (!pending && pendingBaselineRef.current) {
    pendingBaselineRef.current = null;
  }

  useEffect(() => {
    onPendingResolvedRef.current = onPendingResolved;
  }, [onPendingResolved]);

  useLayoutEffect(() => {
    if (isActive && isVisible) return;
    if (navigationRef.current?.contains(document.activeElement)) {
      (document.activeElement as HTMLElement).blur();
    }
    setHoveredNavigationId(null);
    setCtxMenu(null);
  }, [isActive, isVisible]);

  useEffect(() => {
    if (!source) return;
    sourceOwners.set(source.id, ownerKey);
    return () => {
      if (sourceOwners.get(source.id) === ownerKey) sourceOwners.delete(source.id);
    };
  }, [source, ownerKey]);

  useEffect(() => () => {
    if (copyResetTimerRef.current !== null) window.clearTimeout(copyResetTimerRef.current);
  }, []);

  const copyMessage = (messageId: string, content: string) => {
    void clipboardWrite(content).then(() => {
      if (copyResetTimerRef.current !== null) window.clearTimeout(copyResetTimerRef.current);
      setCopiedMessageId(messageId);
      copyResetTimerRef.current = window.setTimeout(() => {
        setCopiedMessageId(current => current === messageId ? null : current);
        copyResetTimerRef.current = null;
      }, 1_500);
    });
  };

  const closeCtxMenu = useCallback(() => setCtxMenu(null), []);

  const selectionText = (): string => {
    const root = scrollRef.current;
    const selection = window.getSelection();
    if (!root || !selection || selection.isCollapsed || selection.rangeCount === 0) return '';
    const range = selection.getRangeAt(0);
    return root.contains(range.commonAncestorContainer) ? selection.toString() : '';
  };

  const openContextMenu = (event: React.MouseEvent<HTMLDivElement>) => {
    event.preventDefault();
    event.stopPropagation();
    setCtxMenu({ x: event.clientX, y: event.clientY, hasSelection: Boolean(selectionText()) });
  };

  useEffect(() => {
    if (!isTauri) return;
    if (!isActive) return;
    if (source || (!resumeToken && !pending && !startedAt)) return;
    let cancelled = false;
    let timer: number | null = null;
    let capturedToken: string | undefined;
    let nextForcedScanAt = pending ? Date.now() + 400 : Number.POSITIVE_INFINITY;
    const bindFrom = async (sessions: SavedSession[]): Promise<boolean> => {
      const exactToken = resumeToken ?? capturedToken;
      const candidates = selectSources(
        sessions, tool, folderPath, exactToken, pending, startedAt, ownerKey,
        competingBindingsRef.current,
      );
      for (const candidate of candidates) {
        let read: ChatSessionRead;
        try {
          read = await commands.readChatSession(candidate);
        } catch (error) {
          // A stale index row or one partially-written transcript must not
          // prevent later candidates from binding to this live terminal.
          console.warn('[Conversation] skip unreadable candidate', candidate.id, error);
          continue;
        }
        if (cancelled) return false;
        const nextTranscript = applySessionRead(
          { messages: [], remainder: '', nextLineIndex: 0 }, read,
        );
        const nextMessages = nextTranscript.messages;
        const updatedAfterPrompt = !pending || savedAtMs(candidate.saved_at) >= pending.sentAt;
        if (pending && !updatedAfterPrompt) continue;
        if (pending && updatedAfterPrompt && !transcriptHasPrompt(nextMessages, pending.text)) continue;
        // Another pending view may have validated the same candidate while
        // this read was in flight. Claim synchronously after validation so two
        // same-workspace terminals can never attach to one transcript.
        const currentOwner = sourceOwners.get(candidate.id);
        if (currentOwner && currentOwner !== ownerKey) continue;

        sourceOwners.set(candidate.id, ownerKey);
        transcriptRef.current = nextTranscript;
        cursorRef.current = read.cursor;
        revisionRef.current = read.revision;
        writeConversationCache(ownerKey, {
          source: candidate, transcript: nextTranscript,
          cursor: read.cursor, revision: read.revision,
        });
        setMessages(nextMessages);
        setSource(candidate);
        const baselineState = pendingBaselineRef.current;
        const baseline = baselineState && baselineState.sentAt === pending?.sentAt
          ? baselineState.userCount
          : 0;
        if (pending && hasAssistantAfterPrompt(nextMessages, pending.text, baseline)) {
          onPendingResolvedRef.current();
        }
        return true;
      }
      return false;
    };
    const discover = async () => {
      try {
        // Claude and a few CLIs expose their freshly-created native session id
        // through PTY output. Prefer that authoritative identity whenever it
        // is available; creation-time matching below remains the fallback for
        // tools that never print a token.
        if (!resumeToken && !capturedToken) {
          capturedToken = await commands.getTerminalSessionToken(sessionId) ?? undefined;
        }
        // The app-wide history cache often already contains the source. Try it
        // first for resumed/pending turns; content validation below makes stale
        // metadata safe. A fresh direct-terminal session has no fingerprint,
        // so it deliberately goes through one fresh native scan instead.
        const snapshot = getHistorySnapshot().sessions;
        if ((resumeToken || pending) && snapshot.length > 0 && await bindFrom(snapshot)) {
          return;
        }
        // A prompt can create a new transcript after both frontend and Rust
        // history caches were populated. Bypass those caches after a short
        // grace period, then at most once every three seconds until bound.
        const now = Date.now();
        const forceScan = Boolean(pending) && now >= nextForcedScanAt;
        if (forceScan) nextForcedScanAt = now + 3_000;
        const sessions = await loadConversationHistory(forceScan || (!resumeToken && !pending));
        if (cancelled || await bindFrom(sessions)) return;
      } catch (error) {
        console.error('[Conversation] discover session failed', error);
      }
      // Recursive timeout (rather than setInterval) guarantees a slow native
      // scan finishes before another starts. A no-prompt fresh view gets one
      // attempt only; pending/resumed sessions may appear shortly after mount.
      if (!cancelled && (pending || resumeToken || agentStatus === 'working')) {
        timer = window.setTimeout(discover, pending ? 400 : 1200);
      }
    };
    void discover();
    return () => {
      cancelled = true;
      if (timer !== null) window.clearTimeout(timer);
    };
  }, [source, tool, folderPath, resumeToken, pending, startedAt, ownerKey, agentStatus, isActive, isVisible, competingBindingsKey, sessionId]);

  useEffect(() => {
    if (!isTauri) return;
    if (!isActive) return;
    if (!source) return;
    let cancelled = false;
    let pollTimer: number | null = null;
    let outputTimer: number | null = null;
    let unsubscribe: (() => void) | null = null;
    let reading = false;
    let readAgain = false;
    const active = Boolean(pending) || agentStatus === 'working';

    const scheduleOutputRefresh = () => {
      if (cancelled || outputTimer !== null) return;
      // Give the CLI a brief moment to flush JSONL/SQLite after emitting PTY
      // output. This makes chat feel streamed without rereading every chunk.
      outputTimer = window.setTimeout(() => {
        outputTimer = null;
        void refresh();
      }, 140);
    };

    const refresh = async () => {
      // PTY output can be very chunky. Coalesce overlapping notifications and
      // perform one trailing read instead of queueing full-transcript IPCs.
      if (reading) {
        readAgain = true;
        return;
      }
      reading = true;
      try {
        const read = await commands.readChatSession(
          source, cursorRef.current, revisionRef.current || undefined,
        );
        if (!cancelled && !read.unchanged) {
          const nextTranscript = applySessionRead(transcriptRef.current, read);
          const nextMessages = nextTranscript.messages;
          transcriptRef.current = nextTranscript;
          cursorRef.current = read.cursor;
          revisionRef.current = read.revision;
          writeConversationCache(ownerKey, {
            source, transcript: nextTranscript,
            cursor: read.cursor, revision: read.revision,
          });
          setMessages(nextMessages);
          const baselineState = pendingBaselineRef.current;
          const baseline = baselineState && baselineState.sentAt === pending?.sentAt
            ? baselineState.userCount
            : 0;
          if (pending && hasAssistantAfterPrompt(nextMessages, pending.text, baseline)) {
            onPendingResolvedRef.current();
          }
        }
      } catch (error) {
        console.error('[Conversation] read session failed', error);
      } finally {
        reading = false;
        if (!cancelled && readAgain) {
          readAgain = false;
          scheduleOutputRefresh();
        }
      }
    };

    const poll = async () => {
      await refresh();
      if (!cancelled) {
        // Some tools persist before/after their PTY write, and a few do not
        // emit output for every transcript mutation. Keep a quick active
        // fallback, then drop to a cheap idle heartbeat.
        pollTimer = window.setTimeout(poll, active ? 320 : 1200);
      }
    };

    void subscribeTerminalEvents(sessionId, {
      onOutput: scheduleOutputRefresh,
      onStatus: scheduleOutputRefresh,
    }).then(stop => {
      if (cancelled) stop();
      else unsubscribe = stop;
    }).catch(error => {
      console.error('[Conversation] subscribe terminal events failed', error);
    });
    void poll();
    return () => {
      cancelled = true;
      unsubscribe?.();
      if (pollTimer !== null) window.clearTimeout(pollTimer);
      if (outputTimer !== null) window.clearTimeout(outputTimer);
    };
  }, [source, pending, ownerKey, sessionId, agentStatus, isActive]);

  useEffect(() => {
    const element = scrollRef.current;
    if (!element) return;
    const onScroll = () => {
      pinnedRef.current = element.scrollHeight - element.scrollTop - element.clientHeight < 72;
    };
    element.addEventListener('scroll', onScroll, { passive: true });
    return () => element.removeEventListener('scroll', onScroll);
  }, []);

  useEffect(() => {
    const element = scrollRef.current;
    if (!element) return;
    return bindAutoHideScrollbar(element, { slim: true });
  }, []);

  // Gambit is a fixed dock whose live CSS height shrinks this scroll owner
  // while its top edge is being dragged. Unlike xterm (which refits through
  // its ResizeObserver), a normal overflow container preserves scrollTop when
  // its clientHeight decreases. That leaves the newest messages below the new
  // clipping edge and makes the dock appear to cover them. Follow the moving
  // bottom edge only while the conversation was already pinned; a user who
  // deliberately scrolled up to read history keeps their exact position.
  useLayoutEffect(() => {
    const element = scrollRef.current;
    if (!element || typeof ResizeObserver === 'undefined') return;
    let frame: number | null = null;
    const observer = new ResizeObserver(() => {
      if (!pinnedRef.current || frame !== null) return;
      frame = window.requestAnimationFrame(() => {
        frame = null;
        if (pinnedRef.current) element.scrollTop = element.scrollHeight;
      });
    });
    observer.observe(element);
    return () => {
      observer.disconnect();
      if (frame !== null) window.cancelAnimationFrame(frame);
    };
  }, []);

  const baselineState = pendingBaselineRef.current;
  const pendingBaseline = baselineState && baselineState.sentAt === pending?.sentAt
    ? baselineState.userCount
    : 0;
  const promptInTranscript = pending
    ? promptIndexSince(messages, pending.text, pendingBaseline) >= 0
    : false;
  const lastConversationalRole = [...messages].reverse().find(message =>
    message.role === 'user' || message.role === 'assistant'
  )?.role;
  const isThinking = pending
    ? !hasAssistantAfterPrompt(messages, pending.text, pendingBaseline)
    : agentStatus === 'working' && lastConversationalRole === 'user';
  const navigationMessages = useMemo(() => {
    const items = messages
      .filter(message => message.role === 'user')
      .map(message => ({ id: message.id, content: message.content }));
    if (pending && !promptInTranscript) {
      items.push({ id: `pending:${pending.sentAt}`, content: pending.text });
    }
    return items;
  }, [messages, pending, promptInTranscript]);
  const hoveredNavigationIndex = navigationMessages.findIndex(
    message => message.id === hoveredNavigationId,
  );
  const hoveredNavigationMessage = hoveredNavigationIndex >= 0
    ? navigationMessages[hoveredNavigationIndex]
    : null;

  useEffect(() => {
    if (navigationMessages.length < 2 || (
      hoveredNavigationId && !navigationMessages.some(message => message.id === hoveredNavigationId)
    )) {
      setHoveredNavigationId(null);
    }
  }, [hoveredNavigationId, navigationMessages]);
  // One conversation turn can contain several assistant narration fragments
  // around tool calls. The copy action belongs to the LAST assistant fragment
  // of EACH turn: either the assistant immediately preceding the next user
  // prompt, or the trailing assistant at the end of the current history. Walk
  // backwards over conversational roles so tool/reasoning rows do not split a
  // turn. While the agent is still working, withhold only the current trailing
  // candidate; summaries from completed earlier turns remain copyable.
  const assistantSummaryMessageIds = new Set<string>();
  let nextConversationalRole: 'user' | 'assistant' | null = null;
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message.role === 'assistant') {
      if (nextConversationalRole === null || nextConversationalRole === 'user') {
        assistantSummaryMessageIds.add(message.id);
      }
      nextConversationalRole = 'assistant';
    } else if (message.role === 'user') {
      nextConversationalRole = 'user';
    }
  }
  if (agentStatus === 'working' && (!pending || promptInTranscript)) {
    const lastUserIndex = messages.findLastIndex(message => message.role === 'user');
    const currentTurnAssistant = messages
      .slice(lastUserIndex + 1)
      .reverse()
      .find(message => message.role === 'assistant');
    if (currentTurnAssistant) {
      assistantSummaryMessageIds.delete(currentTurnAssistant.id);
    }
  }

  useLayoutEffect(() => {
    if (!pinnedRef.current) return;
    const element = scrollRef.current;
    if (element) element.scrollTop = element.scrollHeight;
  }, [messages, pending, isThinking]);

  useEffect(() => {
    const scroll = scrollRef.current;
    if (!scroll || navigationMessages.length < 2) {
      setActiveNavigationId(navigationMessages[0]?.id ?? null);
      return;
    }
    let frame: number | null = null;
    const updateActiveMessage = () => {
      frame = null;
      const scrollRect = scroll.getBoundingClientRect();
      const readingLine = scrollRect.top + Math.min(120, scrollRect.height * 0.24);
      let closestId = navigationMessages[0]?.id ?? null;
      let closestDistance = Number.POSITIVE_INFINITY;
      scroll.querySelectorAll<HTMLElement>('[data-conversation-navigation-id]').forEach(target => {
        const distance = Math.abs(target.getBoundingClientRect().top - readingLine);
        if (distance < closestDistance) {
          closestDistance = distance;
          closestId = target.dataset.conversationNavigationId ?? closestId;
        }
      });
      setActiveNavigationId(closestId);
    };
    const schedule = () => {
      if (frame === null) frame = window.requestAnimationFrame(updateActiveMessage);
    };
    const resizeObserver = typeof ResizeObserver === 'undefined'
      ? null
      : new ResizeObserver(schedule);
    updateActiveMessage();
    scroll.addEventListener('scroll', schedule, { passive: true });
    if (threadRef.current) resizeObserver?.observe(threadRef.current);
    return () => {
      scroll.removeEventListener('scroll', schedule);
      resizeObserver?.disconnect();
      if (frame !== null) window.cancelAnimationFrame(frame);
    };
  }, [navigationMessages]);

  useEffect(() => {
    const container = navigationScrollRef.current;
    if (!container || !activeNavigationId) return;
    const item = container.querySelector<HTMLElement>(
      `[data-conversation-navigation-target="${CSS.escape(activeNavigationId)}"]`,
    );
    if (!item) return;
    const itemTop = item.offsetTop;
    const itemBottom = itemTop + item.offsetHeight;
    if (itemTop < container.scrollTop) container.scrollTop = itemTop;
    else if (itemBottom > container.scrollTop + container.clientHeight) {
      container.scrollTop = itemBottom - container.clientHeight;
    }
  }, [activeNavigationId]);

  const jumpToNavigationMessage = (messageId: string) => {
    const scroll = scrollRef.current;
    const target = scroll?.querySelector<HTMLElement>(
      `[data-conversation-navigation-id="${CSS.escape(messageId)}"]`,
    );
    if (!scroll || !target) return;
    const top = target.getBoundingClientRect().top - scroll.getBoundingClientRect().top;
    pinnedRef.current = false;
    scroll.scrollTo({
      top: Math.max(0, scroll.scrollTop + top - 24),
      behavior: 'smooth',
    });
  };

  const showNavigationTooltip = (messageId: string, item: HTMLElement) => {
    const navigation = navigationRef.current;
    if (!navigation) return;
    const itemRect = item.getBoundingClientRect();
    const navigationRect = navigation.getBoundingClientRect();
    setHoveredNavigationId(messageId);
    setNavigationTooltipTop(itemRect.top + itemRect.height / 2 - navigationRect.top);
  };

  return (
    <div
      className={`conversation-view${hasBg && bgUrl ? ' conversation-view--has-bg' : ''}${navigationMessages.length > 1 ? ' conversation-view--has-navigation' : ''}`}
      onContextMenu={openContextMenu}
    >
      {hasBg && bgUrl && (
        <div className="conversation-background" aria-hidden="true">
          {bgType === 'video'
            ? <video src={bgUrl} autoPlay loop muted playsInline />
            : <img src={bgUrl} alt="" draggable={false} />}
        </div>
      )}
      {navigationMessages.length > 1 && (
        <nav
          ref={navigationRef}
          className="conversation-navigation"
          aria-label="对话快速定位"
          onMouseLeave={() => setHoveredNavigationId(null)}
        >
          <div
            ref={navigationScrollRef}
            className="conversation-navigation-scroll"
            onScroll={() => {
              if (!hoveredNavigationId) return;
              const item = navigationScrollRef.current?.querySelector<HTMLElement>(
                `[data-conversation-navigation-target="${CSS.escape(hoveredNavigationId)}"]`,
              );
              if (item) showNavigationTooltip(hoveredNavigationId, item);
            }}
          >
            {navigationMessages.map((message, index) => {
              const hoverDistance = hoveredNavigationIndex < 0
                ? undefined
                : Math.abs(index - hoveredNavigationIndex);
              return (
                <button
                  type="button"
                  key={message.id}
                  className="conversation-navigation-item"
                  data-conversation-navigation-target={message.id}
                  data-active={activeNavigationId === message.id ? 'true' : undefined}
                  data-hover-distance={hoverDistance !== undefined && hoverDistance <= 2
                    ? hoverDistance
                    : undefined}
                  aria-label={`跳转到第 ${index + 1} 轮提问`}
                  aria-current={activeNavigationId === message.id ? 'step' : undefined}
                  aria-describedby={hoveredNavigationId === message.id
                    ? navigationTooltipId
                    : undefined}
                  onMouseEnter={event => showNavigationTooltip(message.id, event.currentTarget)}
                  onFocus={event => showNavigationTooltip(message.id, event.currentTarget)}
                  onBlur={() => setHoveredNavigationId(null)}
                  onClick={() => jumpToNavigationMessage(message.id)}
                >
                  <span aria-hidden="true" />
                </button>
              );
            })}
          </div>
          {hoveredNavigationMessage && (
            <div
              id={navigationTooltipId}
              className="conversation-navigation-tooltip"
              role="tooltip"
              style={{ top: navigationTooltipTop }}
            >
              <span>{hoveredNavigationMessage.content.replace(/\s+/g, ' ').trim()}</span>
            </div>
          )}
        </nav>
      )}
      <div className="conversation-scroll" ref={scrollRef}>
        <div className="conversation-thread" ref={threadRef}>
        {messages.map(message => {
          if (message.role === 'tool') return <ToolRow key={message.id} message={message} />;
          if (message.role === 'reasoning') {
            return (
              <details key={message.id} className="conversation-reasoning">
                <summary><span className="conversation-reasoning-glyph">✦</span> 思考过程</summary>
                <MarkdownContent content={message.content} />
              </details>
            );
          }
          return (
            <article
              key={message.id}
              className={`conversation-message conversation-message--${message.role}`}
              data-conversation-navigation-id={message.role === 'user' ? message.id : undefined}
            >
              <div className="conversation-bubble"><MarkdownContent content={message.content} /></div>
              {(message.role === 'user' || assistantSummaryMessageIds.has(message.id)) && (
                <MessageCopyButton
                  copied={copiedMessageId === message.id}
                  onCopy={() => copyMessage(message.id, message.content)}
                />
              )}
            </article>
          );
        })}

        {pending && !promptInTranscript && (
          <article
            className="conversation-message conversation-message--user conversation-message--optimistic"
            data-conversation-navigation-id={`pending:${pending.sentAt}`}
          >
            <div className="conversation-bubble"><MarkdownContent content={pending.text} /></div>
            <MessageCopyButton
              copied={copiedMessageId === `pending:${pending.sentAt}`}
              onCopy={() => copyMessage(`pending:${pending.sentAt}`, pending.text)}
            />
          </article>
        )}

        {isThinking && (
          <div className="conversation-thinking" role="status" aria-live="polite">
            <span className="conversation-thinking-braille" aria-hidden="true" />
            <span className="conversation-thinking-text">正在思考中…</span>
          </div>
        )}
        </div>
      </div>
      {ctxMenu && (
        <TermContextMenu
          menu={ctxMenu}
          onClose={closeCtxMenu}
          onCopy={() => {
            const text = selectionText();
            if (text) void clipboardWrite(text);
            closeCtxMenu();
          }}
          onPaste={async () => {
            const imagePath = await clipboardReadImage();
            if (imagePath) {
              onPasteToDraft(imagePath);
              closeCtxMenu();
              return;
            }
            const text = await clipboardRead();
            if (text) onPasteToDraft(text);
            closeCtxMenu();
          }}
          onSelectAll={() => {
            const thread = threadRef.current;
            const selection = window.getSelection();
            if (thread && selection) {
              const range = document.createRange();
              range.selectNodeContents(thread);
              selection.removeAllRanges();
              selection.addRange(range);
            }
            closeCtxMenu();
          }}
        />
      )}
    </div>
  );
}
