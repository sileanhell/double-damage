import type { Compaction } from "./compaction";

/**
 * Chat messages as stored and shown. Content blocks follow the Anthropic Messages format, which
 * is also what the agent loop sends to the gateway, so nothing is lost converting back and forth.
 */

export type TextBlock = { type: "text"; text: string };
export type ThinkingBlock = { type: "thinking"; thinking: string; signature?: string };
export type ToolUseBlock = { type: "tool_use"; id: string; name: string; input: unknown };
export type ToolResultBlock = {
  type: "tool_result";
  tool_use_id: string;
  content: string | TextBlock[];
  is_error?: boolean;
};
export type Block = TextBlock | ThinkingBlock | ToolUseBlock | ToolResultBlock;

export type Usage = {
  /** Prompt tokens (whole context sent with this turn). */
  input: number;
  output: number;
};

/** Timing measured on our side for one assistant reply. */
export type Stats = {
  /** Request sent → first token, ms. */
  ttft?: number;
  /** First token → last token, ms. */
  generation?: number;
  /** Output tokens per second. */
  tps?: number;
  /** How long thinking took, ms (first thinking token → first non-thinking block). */
  thinking?: number;
};

/** What the UI shows about a finished tool call besides its result text (not sent to the model). */
export type ToolMeta = {
  /** Unified diff of what write / edit changed. */
  diff?: string;
  /** Files the call changed (unix paths). */
  files?: string[];
  exitCode?: number | null;
  timedOut?: boolean;
  durationMs?: number;
  /** The user declined it, with their reason if they gave one. */
  denied?: { reason?: string };
  /** Stopped by the user while it ran or waited. */
  cancelled?: boolean;
};

export type Message = {
  id: string;
  role: "user" | "assistant";
  content: Block[];
  createdAt: number;
  /** Gateway model id that wrote an assistant message. */
  model?: string;
  usage?: Usage;
  stats?: Stats;
  /** Anthropic stop reason: end_turn, max_tokens, tool_use, stop_sequence; "cancelled" when stopped. */
  stopReason?: string;
  /** Set when the reply failed. */
  error?: string;
  /** On a message carrying tool results: UI details per tool_use id. */
  tools?: Record<string, ToolMeta>;
  /** On a user message: files attached to it (absolute Windows paths); the model gets the paths. */
  attachments?: string[];
  /** Marks a context compaction (a divider in the chat; changes what the model sees from here on). */
  compaction?: Compaction;
};

/** Plain text of a message (text blocks only), e.g. for search and titles. */
export const messageText = (message: Pick<Message, "content">) =>
  message.content
    .filter((block): block is TextBlock => block.type === "text")
    .map((block) => block.text)
    .join("\n");
