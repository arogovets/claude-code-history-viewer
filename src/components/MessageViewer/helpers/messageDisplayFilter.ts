/**
 * Role / content-type display filter shared by the message list and export.
 *
 * Extracted from the MessageViewer `displayMessages` memo so that export can
 * apply the exact same filtering to a freshly fetched FULL session (the store
 * may only hold a paginated window of it).
 */

import type { ClaudeMessage, ClaudeSystemMessage } from "../../../types";
import type { MessageFilter } from "../../../store/slices/filterSlice";
import { extractClaudeMessageContent, hasCommandTags } from "../../../utils/messageUtils";
import { filterMessagesByCategory } from "./messageCategories";

export const COMMAND_SYSTEM_SUBTYPES = new Set([
  "compact_boundary",
  "microcompact_boundary",
  "local_command",
  "stop_hook_summary",
  "turn_duration",
]);

export function isCommandMessage(msg: ClaudeMessage): boolean {
  if (msg.type === "command") return true;
  if (msg.subtype && COMMAND_SYSTEM_SUBTYPES.has(msg.subtype)) return true;
  const sysMsg = msg as ClaudeSystemMessage;
  if (sysMsg.compactMetadata || sysMsg.microcompactMetadata) return true;
  const content = extractClaudeMessageContent(msg);
  if (content && hasCommandTags(content)) return true;
  if (Array.isArray(msg.content)) {
    return msg.content.some((item: unknown) => {
      if (!item || typeof item !== "object") return false;
      const typed = item as Record<string, unknown>;
      if (typed.type === "command") return true;
      if (typed.type === "text" && typeof typed.text === "string" && hasCommandTags(typed.text)) return true;
      return false;
    });
  }
  return false;
}

function hasVisibleContent(
  msg: ClaudeMessage,
  contentTypes: MessageFilter["contentTypes"],
): boolean {
  // Command subtypes and dedicated command events (compact_boundary, microcompact_boundary, local_command, etc.)
  if (
    msg.type === "command" ||
    (msg.subtype && COMMAND_SYSTEM_SUBTYPES.has(msg.subtype)) ||
    Boolean((msg as ClaudeSystemMessage).compactMetadata) ||
    Boolean((msg as ClaudeSystemMessage).microcompactMetadata)
  ) {
    return contentTypes.commands;
  }
  if (msg.type === "system" && msg.subtype === "system_prompt") {
    return contentTypes.text;
  }
  if (msg.type === "summary") {
    return contentTypes.text;
  }

  const content = extractClaudeMessageContent(msg);
  const isCommand = content ? hasCommandTags(content) : false;
  const hasText = !isCommand && contentTypes.text && !!content;
  const hasCommand = isCommand && contentTypes.commands;
  const hasContentArray = Array.isArray(msg.content) && msg.content.some((item: unknown) => {
    if (!item || typeof item !== "object") return false;
    const typed = item as Record<string, unknown>;
    const t = typed.type as string;
    if (t === "text") {
      const isItemCommand = typeof typed.text === "string" && hasCommandTags(typed.text);
      return isItemCommand ? contentTypes.commands : contentTypes.text;
    }
    if (t === "thinking" || t === "redacted_thinking") return contentTypes.thinking;
    if (t === "tool_use" || t === "tool_result" || t === "server_tool_use"
      || t === "web_search_tool_result" || t === "mcp_tool_use" || t === "mcp_tool_result"
      || t === "web_fetch_tool_result" || t === "code_execution_tool_result"
      || t === "bash_code_execution_tool_result" || t === "text_editor_code_execution_tool_result"
      || t === "tool_search_tool_result") return contentTypes.toolCalls;
    if (t === "command") return contentTypes.commands;
    return true; // image, document, search_result — always show
  });
  const msgRecord = msg as unknown as Record<string, unknown>;
  const hasLegacyTool = contentTypes.toolCalls && !!(msgRecord.toolUse || msgRecord.toolUseResult);
  return hasText || hasCommand || hasContentArray || hasLegacyTool;
}

export function applyMessageDisplayFilter(
  messages: ClaudeMessage[],
  messageFilter: MessageFilter,
): ClaudeMessage[] {
  const { roles, contentTypes } = messageFilter;
  const allRoles = roles.user && roles.assistant;
  const allContent = contentTypes.text && contentTypes.thinking && contentTypes.toolCalls && contentTypes.commands;
  const parallelTaskFilteredMessages = filterMessagesByCategory(
    messages,
    "parallel-task",
    contentTypes.parallelTasks,
  );
  if (allRoles && allContent) return parallelTaskFilteredMessages;

  return parallelTaskFilteredMessages.filter((msg) => {
    const msgRecord = msg as unknown as Record<string, unknown>;
    const isUser = msg.type === "user" || (msg.type !== "system" && msg.type !== "assistant" && msgRecord.role === "user");
    const isAssistant = msg.type === "assistant" || (msg.type !== "system" && msg.type !== "user" && msgRecord.role === "assistant");

    // Role filter & content type filter
    if (isUser) {
      if (!roles.user) return false;
      if (!allContent && !hasVisibleContent(msg, contentTypes)) return false;
      return true;
    }
    if (isAssistant) {
      if (!roles.assistant) return false;
      if (!allContent && !hasVisibleContent(msg, contentTypes)) return false;
      return true;
    }

    // Non-user, non-assistant messages (system, command, summary, etc.)
    // If filtering by role (e.g. user-only or assistant-only), hide system/other messages
    if (!roles.user || !roles.assistant) {
      return false;
    }

    // Check visible content according to active content type filters
    return hasVisibleContent(msg, contentTypes);
  });
}
