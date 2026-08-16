import type { ToolType } from '../store/app-state';

const CHAT_TOOLS = new Set<ToolType>([
  'claude', 'codex', 'qwen', 'antigravity', 'pi', 'hermes',
  'opencode', 'mimocode', 'kilo', 'grok', 'kimicode',
]);

export function supportsConversationTool(tool: ToolType | null | undefined): boolean {
  return Boolean(tool && CHAT_TOOLS.has(tool));
}
