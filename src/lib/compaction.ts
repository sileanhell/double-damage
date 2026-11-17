import type { Block, Message, TextBlock, ToolResultBlock, ToolUseBlock } from "./chat-types";

/**
 * Context compaction, after rho's design (crates/rho/src/compaction*.rs). Tiers, cheapest first:
 *
 * 1. Elision: old tool results (≥ 1 KiB, outside the first turn and the recent tail) are replaced
 *    with a one-line stub. Calls and results stay paired; only the result text changes. If that
 *    alone reaches the target, no model request is made.
 * 2. Text summary by the chat's own model. The history is split into: the first user turn (kept
 *    verbatim when small), one summary, the latest user message (restated when it would otherwise
 *    only be summarized) and the recent tail; tool calls are never split from their results. An
 *    earlier summary is updated, not summarized again.
 *
 * Nothing is deleted: the chat keeps every message. A compaction is a marker message whose
 * `compaction` says what the model sees from then on; `contextItems` builds that view.
 */

export type CompactionTrigger = "manual" | "auto" | "overflow";

export type Compaction = {
  trigger: CompactionTrigger;
  /** False while it runs, and when it failed (then the marker has `error`). */
  done: boolean;
  /** What the user asked to keep (`/compact <focus>`). */
  focus?: string;
  /** Replaces everything between `keep` and `tailStart`. */
  summary?: string;
  /** First-turn messages kept verbatim before the summary. */
  keep: string[];
  /** A user message restated right after the summary. */
  latestUser?: string;
  /** First message kept verbatim after the summary; from there on everything is sent. */
  tailStart?: string;
  /** tool_use ids whose results the model sees as a stub. */
  elided: string[];
  /** Estimated context before and after, in tokens. */
  before?: number;
  after?: number;
};

/** One message as the model sees it (before roles are merged). `id` is the stored message's. */
export type Item = { id: string; role: "user" | "assistant"; content: Block[]; summary?: boolean };

const SUMMARY_RESERVE_MIN = 512;
const SUMMARY_RESERVE_MAX = 8192;
/** Largest first user turn / latest user message kept verbatim; larger ones are summarized. */
const VERBATIM_USER_MAX = 2048;
/** Results below this size stay verbatim: eliding them saves little. */
const MIN_ELIDED_CHARS = 1024;
const ARGUMENT_SUMMARY_CHARS = 80;

// --- the model's view of a chat ----------------------------------------------------------------

const SUMMARY_HEADER: Record<CompactionTrigger, string> = {
  manual: "Manual compaction summary of earlier conversation, for model context only. It is not a new user message.",
  auto: "Automatic compaction summary of earlier conversation, for model context only. It is not a new user message.",
  overflow: "Automatic compaction summary of earlier conversation, for model context only. It is not a new user message.",
};

const resultText = (block: ToolResultBlock) =>
  typeof block.content === "string" ? block.content : block.content.map((part) => part.text).join("\n");

/** The latest finished compaction before `end` (exclusive), with its index. */
export function latestCompaction(list: Message[], end = list.length): { index: number; compaction: Compaction } | undefined {
  for (let i = end - 1; i >= 0; i--) {
    const compaction = list[i].compaction;
    if (compaction?.done) return { index: i, compaction };
  }
  return undefined;
}

/**
 * What the model sees of `list` (messages before `exclude`, which is skipped): the latest
 * compaction's view, then everything after it. No thinking, no failed replies without calls.
 */
export function contextItems(list: Message[], exclude?: string): Item[] {
  const messages = list.filter((message) => message.id !== exclude);
  const calls = new Map<string, ToolUseBlock>();
  for (const message of messages) for (const block of message.content) if (block.type === "tool_use") calls.set(block.id, block);

  const latest = latestCompaction(messages);
  const elided = new Set(latest?.compaction.elided ?? []);
  const item = (message: Message | undefined): Item | undefined => {
    if (!message || message.compaction) return undefined;
    // Thinking isn't sent back: local templates drop it anyway and it would eat the context.
    let content = message.content.filter((block) => block.type !== "thinking");
    if (message.error && !content.some((block) => block.type === "tool_use")) return undefined;
    if (message.attachments?.length) content = [...content, { type: "text", text: attachmentsText(message.attachments) }];
    if (content.length === 0) return undefined;
    if (elided.size > 0) {
      content = content.map((block) =>
        block.type === "tool_result" && elided.has(block.tool_use_id) ? { ...block, content: stubText(block, calls.get(block.tool_use_id)) } : block,
      );
    }
    return { id: message.id, role: message.role, content };
  };

  if (!latest) return messages.map(item).filter((entry): entry is Item => !!entry);

  const { compaction, index } = latest;
  const byId = new Map(messages.map((message) => [message.id, message]));
  const out: Item[] = [];
  for (const id of compaction.keep) {
    const entry = item(byId.get(id));
    if (entry) out.push(entry);
  }
  if (compaction.summary) {
    out.push({
      id: messages[index].id,
      role: "user",
      summary: true,
      content: [
        { type: "text", text: SUMMARY_HEADER[compaction.trigger] },
        { type: "text", text: compaction.summary },
      ],
    });
  }
  const latestUser = item(byId.get(compaction.latestUser ?? ""));
  if (latestUser) out.push(latestUser);
  const tail = compaction.tailStart ? messages.findIndex((message) => message.id === compaction.tailStart) : -1;
  for (const message of messages.slice(tail >= 0 ? tail : index + 1)) {
    const entry = item(message);
    if (entry) out.push(entry);
  }
  return out;
}

/** `C:\Users\me\a.pdf` → `/c/Users/me/a.pdf`, the way the tools and bash show paths. */
export const unixPath = (path: string) =>
  path.replace(/\\/g, "/").replace(/^([A-Za-z]):/, (_, drive: string) => `/${drive.toLowerCase()}`).replace(/\/+$/, "") || "/";

/** How a message's attached files reach the model: their paths. */
const attachmentsText = (files: string[]) =>
  `<attached_files>\nAttached files (read them with read_file if needed):\n${files.map((file) => `- ${unixPath(file)}`).join("\n")}\n</attached_files>`;

/** `items` with the results of `ids` replaced by stubs. */
export function withElided(items: Item[], ids: string[]): Item[] {
  if (ids.length === 0) return items;
  const set = new Set(ids);
  const calls = new Map<string, ToolUseBlock>();
  for (const item of items) for (const block of item.content) if (block.type === "tool_use") calls.set(block.id, block);
  return items.map((item) =>
    item.content.some((block) => block.type === "tool_result" && set.has(block.tool_use_id))
      ? {
          ...item,
          content: item.content.map((block) =>
            block.type === "tool_result" && set.has(block.tool_use_id) ? { ...block, content: stubText(block, calls.get(block.tool_use_id)) } : block,
          ),
        }
      : item,
  );
}

function stubText(result: ToolResultBlock, call: ToolUseBlock | undefined): string {
  const name = call ? [call.name, argumentSummary(call.input)].filter(Boolean).join(" ") : "unknown";
  const status = result.is_error ? "error" : "ok";
  return `[elided tool result: ${name} · ${status} · ${resultText(result).length} chars. It is no longer in context; run the tool again if you need it.]`;
}

function argumentSummary(input: unknown): string {
  if (!input || typeof input !== "object") return "";
  const summary = Object.entries(input as Record<string, unknown>)
    .filter(([, value]) => ["string", "number", "boolean"].includes(typeof value))
    .map(([key, value]) => `${key}=${String(value)}`)
    .join(" ")
    .split(/\s+/)
    .join(" ");
  return summary.length <= ARGUMENT_SUMMARY_CHARS ? summary : `${summary.slice(0, ARGUMENT_SUMMARY_CHARS - 1)}…`;
}

// --- token estimates ------------------------------------------------------------------------

/** Rough local estimate (≈ 4 chars per token), scaled by `ratio` to match the model's counts. */
export function blockChars(block: Block): number {
  switch (block.type) {
    case "text":
      return block.text.length;
    case "thinking":
      return 0;
    case "tool_use":
      return block.name.length + JSON.stringify(block.input ?? {}).length + 20;
    case "tool_result":
      return resultText(block).length + 20;
  }
}

export const estimateTokens = (chars: number, ratio = 1) => Math.ceil((chars / 4) * ratio);

const itemTokens = (item: Item, ratio: number) =>
  estimateTokens(item.content.reduce((sum, block) => sum + blockChars(block), 16), ratio);

export const itemsTokens = (items: Item[], ratio: number) => items.reduce((sum, item) => sum + itemTokens(item, ratio), 0);

/** Tokens kept free for the summary itself. */
export const summaryReserve = (target: number) =>
  target <= 0 ? 0 : Math.min(target, Math.max(SUMMARY_RESERVE_MIN, Math.min(SUMMARY_RESERVE_MAX, Math.floor(target / 10))));

// --- partition ---------------------------------------------------------------------------------

export type Partition = {
  items: Item[];
  /** items[0..firstEnd) stay verbatim before the summary. */
  firstEnd: number;
  /** Index of an earlier summary (updated, not summarized). */
  previousSummary?: number;
  /** Index of the user message restated after the summary. */
  latestUser?: number;
  /** items[recentStart..] stay verbatim. */
  recentStart: number;
};

/** A person's message (not tool results, not a summary). */
const isUserInput = (item: Item) =>
  item.role === "user" && !item.summary && item.content.some((block) => block.type === "text") && !item.content.some((block) => block.type === "tool_result");

/** Groups from `start`: a call with the results that answer it is one group. */
function groups(items: Item[], start: number, ratio: number) {
  const out: { start: number; tokens: number }[] = [];
  for (let i = start; i < items.length; ) {
    let end = i + 1;
    if (items[i].role === "assistant" && items[i].content.some((block) => block.type === "tool_use")) {
      while (end < items.length && items[end].role === "user" && items[end].content.some((block) => block.type === "tool_result")) end++;
    }
    out.push({ start: i, tokens: itemsTokens(items.slice(i, end), ratio) });
    i = end;
  }
  return out;
}

function tailStart(list: { start: number; tokens: number }[], budget: number): number | undefined {
  let start: number | undefined;
  let tokens = 0;
  for (let i = list.length - 1; i >= 0; i--) {
    const next = tokens + list[i].tokens;
    // The newest group always stays, even over budget.
    if (start !== undefined && next > budget) break;
    tokens = next;
    start = list[i].start;
  }
  return start;
}

function partitionWith(items: Item[], fixedExtra: number, target: number, ratio: number, keepUserTurns: boolean): Partition | undefined {
  let natural = 0;
  while (natural < items.length && isUserInput(items[natural])) natural++;
  if (itemsTokens(items.slice(0, natural), ratio) > VERBATIM_USER_MAX) natural = 0;
  // An earlier compaction put its summary right after the first turn.
  const previousSummary = items[natural]?.summary ? natural : undefined;
  const firstEnd = keepUserTurns ? natural : 0;
  const historyStart = previousSummary !== undefined ? previousSummary + 1 : firstEnd;
  const grouped = groups(items, historyStart, ratio);

  let fixed = fixedExtra + itemsTokens(items.slice(0, firstEnd), ratio);
  const budget = (fixedTokens: number) => Math.max(0, target - fixedTokens - summaryReserve(target));
  let recentStart = tailStart(grouped, budget(fixed));
  if (recentStart === undefined) return undefined;

  let latestUser: number | undefined;
  for (let i = items.length - 1; i >= historyStart; i--) {
    if (isUserInput(items[i])) {
      latestUser = i;
      break;
    }
  }
  if (latestUser !== undefined && !(keepUserTurns && latestUser < recentStart && itemTokens(items[latestUser], ratio) <= VERBATIM_USER_MAX)) {
    latestUser = undefined;
  }
  if (latestUser !== undefined) {
    fixed += itemTokens(items[latestUser], ratio);
    recentStart = tailStart(grouped, budget(fixed));
    if (recentStart === undefined) return undefined;
  }
  // Rewriting the earlier summary around a restated message alone removes nothing.
  let removes = false;
  for (let i = firstEnd; i < recentStart; i++) if (i !== previousSummary && i !== latestUser) removes = true;
  return removes ? { items, firstEnd, previousSummary, latestUser, recentStart } : undefined;
}

/** Splits `items` so the tail fits `target` next to what stays verbatim; undefined when nothing would be summarized. */
export function partition(items: Item[], fixedExtra: number, target: number, ratio: number): Partition | undefined {
  return partitionWith(items, fixedExtra, target, ratio, true) ?? partitionWith(items, fixedExtra, target, ratio, false);
}

/** Items the new summary covers, oldest first (the restated latest message included, for order). */
const summarized = (part: Partition) =>
  part.items.slice(part.firstEnd, part.recentStart).filter((_, offset) => part.firstEnd + offset !== part.previousSummary);

/** The compaction a summary of `part` makes. */
export function summaryCompaction(part: Partition, summary: string, base: Omit<Compaction, "summary" | "keep" | "latestUser" | "tailStart">): Compaction {
  return {
    ...base,
    summary,
    keep: part.items.slice(0, part.firstEnd).map((item) => item.id),
    latestUser: part.latestUser !== undefined ? part.items[part.latestUser].id : undefined,
    tailStart: part.items[part.recentStart]?.id,
  };
}

// --- tier 1: elision -------------------------------------------------------------------------

/**
 * tool_use ids to elide, oldest first, until the estimate reaches `target` (outside the first
 * turn and the recent tail). Empty when there's nothing worth eliding.
 */
export function elide(items: Item[], fixedExtra: number, target: number, ratio: number): { ids: string[]; tokens: number } {
  const part = partition(items, fixedExtra, target, ratio);
  let tokens = fixedExtra + itemsTokens(items, ratio);
  const ids: string[] = [];
  if (!part) return { ids, tokens };
  for (let i = part.firstEnd; i < part.recentStart && tokens > target; i++) {
    for (const block of items[i].content) {
      if (block.type !== "tool_result" || resultText(block).length < MIN_ELIDED_CHARS) continue;
      const saved = estimateTokens(resultText(block).length - 160, ratio);
      if (saved <= 0) continue;
      ids.push(block.tool_use_id);
      tokens -= saved;
    }
  }
  return { ids, tokens };
}

// --- tier 2: the summary request -------------------------------------------------------------------

export const SUMMARY_SYSTEM_PROMPT = `Summarize the compacted conversation history for continuation. The original transcript is stored separately; this summary replaces only older model context for an agent that will keep working on the task.

You may first think in an <analysis>...</analysis> block. It is removed before the summary is used.

Then write the summary with exactly these Markdown sections, in this order. Write "None" under a section with nothing to report.

## Original request
## Constraints and preferences
## Decisions and rationale
## Files touched and their current state
## Commands run and test results
## Errors and fixes
## Open tasks
## Exact next step

Keep exact paths, commands, identifiers, error text, and numbers. Be concise and factual. Do not invent progress that the transcript does not show.

Under commands, list only ones whose outcome still matters: builds, tests, checks. Leave out exploration such as reads, searches, and listings.`;

const PREVIOUS_SUMMARY_INSTRUCTION =
  "An earlier compaction already summarized the conversation before these turns. Update that summary with the new turns and return one complete summary in the same sections. Keep details that still matter, revise ones the new turns changed, and drop ones that no longer matter.";
const ORIGINAL_REQUEST_INSTRUCTION =
  "The user's original request stays verbatim in context after compaction. It is included for reference; summarize only what the turns below add.";
const SESSION_TASK_INSTRUCTION = `Stop working on the task. Do not call tools. Reply with the summary only.

The summary replaces one span of the conversation above, named below. That span will be deleted, so keep its paths, commands, identifiers, errors, and decisions. Where this message names a span, summarize that span rather than the whole conversation. Messages that stay verbatim after compaction should stay brief.`;
const SESSION_FIRST_TURN_INSTRUCTION = "The user's first message stays verbatim. Do not summarize that message; summarize the span named below.";
const SESSION_PREVIOUS_SUMMARY_INSTRUCTION =
  "An earlier compaction summary appears above. Update it from the span named below and return one complete summary in the same sections. Keep details that still matter, revise ones the newer turns changed, and drop ones that no longer matter.";
const FOCUS_INSTRUCTION =
  "The user asked this compaction to preserve the following. Give it priority in the summary and keep its details exact, while still filling every section.";

const focusSection = (focus: string) => `${FOCUS_INSTRUCTION}\n\n<focus>\n${focus}\n</focus>`;

function firstLine(text: string) {
  return text.split("\n").map((line) => line.trim()).find(Boolean) ?? "message";
}

/** Role plus a short marker pointing at one item, without copying it. */
function marker(item: Item): string {
  const label = item.summary ? "earlier compaction summary" : item.content.some((block) => block.type === "tool_result") ? "tool result" : item.role;
  const block = item.content[0];
  let text = "message";
  if (item.summary) text = firstLine((item.content[1] as TextBlock | undefined)?.text ?? "");
  else if (block?.type === "text") text = firstLine(block.text);
  else if (block?.type === "tool_use") text = `tool call ${block.name} (${block.id})`;
  else if (block?.type === "tool_result") text = `(${block.tool_use_id}) [${block.is_error ? "error" : "ok"}] ${firstLine(resultText(block))}`;
  return `${label}: ${text.slice(0, 120)}`;
}

/**
 * The instruction appended to the chat's own history (the "session" request): the model has the
 * history in its prompt cache already, so only this suffix is new.
 */
export function sessionSummaryInstruction(part: Partition, focus?: string): string {
  const sections = [SUMMARY_SYSTEM_PROMPT, SESSION_TASK_INSTRUCTION];
  if (part.firstEnd > 0) sections.push(SESSION_FIRST_TURN_INSTRUCTION);
  if (part.previousSummary !== undefined) sections.push(SESSION_PREVIOUS_SUMMARY_INSTRUCTION);
  const lines: string[] = [];
  const kept = part.latestUser !== undefined ? part.items[part.latestUser] : undefined;
  if (kept) {
    lines.push(`This user message stays verbatim even though it sits among the turns being summarized. Mention it only briefly:\n${marker(kept)}`);
  }
  const start = summarized(part).find((item) => item !== kept);
  const tail = part.items[part.recentStart];
  if (start && tail) {
    lines.push(
      `Summarize from this message, inclusive, up to but not including the verbatim tail:\n\nDeleted span starts at:\n${marker(start)}\n\nVerbatim tail starts at:\n${marker(tail)}`,
    );
  } else if (start) {
    lines.push(`Summarize from this message through the end of the conversation:\n${marker(start)}`);
  } else {
    lines.push("Summarize the conversation above.");
  }
  sections.push(lines.join("\n\n"));
  if (focus) sections.push(focusSection(focus));
  return sections.join("\n\n");
}

/** A standalone request (own system prompt, the span rendered as text), for when the session request can't be used. */
export function transcriptSummaryRequest(part: Partition, focus: string | undefined, maxChars: number): { system: string; user: string } {
  const sections: string[] = [];
  if (part.firstEnd > 0) {
    sections.push(`${ORIGINAL_REQUEST_INSTRUCTION}\n\n<original-request>\n${render(part.items.slice(0, part.firstEnd))}\n</original-request>`);
  }
  if (part.previousSummary !== undefined) {
    const previous = (part.items[part.previousSummary].content[1] as TextBlock | undefined)?.text ?? "";
    sections.push(`${PREVIOUS_SUMMARY_INSTRUCTION}\n\n<previous-summary>\n${previous.trim()}\n</previous-summary>`);
  }
  let conversation = render(summarized(part));
  const room = maxChars - sections.join("\n\n").length - 2000;
  if (conversation.length > room && room > 0) {
    // Too big for the window even as text: keep the start and the end of the span.
    const half = Math.floor(room / 2);
    conversation = `${conversation.slice(0, half)}\n\n[… ${conversation.length - 2 * half} characters left out …]\n\n${conversation.slice(-half)}`;
  }
  sections.push(`<conversation>\n${conversation}\n</conversation>`);
  if (focus) sections.push(focusSection(focus));
  return { system: SUMMARY_SYSTEM_PROMPT, user: sections.join("\n\n") };
}

function render(items: Item[]): string {
  const names = new Map<string, string>();
  return items
    .map((item) => {
      if (item.summary) return `earlier compaction summary:\n${(item.content[1] as TextBlock | undefined)?.text ?? ""}`;
      const parts = item.content.map((block) => {
        if (block.type === "text") return block.text;
        if (block.type === "tool_use") {
          names.set(block.id, block.name);
          return `tool call ${block.name} (${block.id}):\n${JSON.stringify(block.input, null, 2)}`;
        }
        if (block.type === "tool_result") {
          return `tool result ${names.get(block.tool_use_id) ?? "unknown"} (${block.tool_use_id}) [${block.is_error ? "error" : "ok"}]:\n${resultText(block)}`;
        }
        return "";
      });
      return `${item.role}:\n${parts.filter(Boolean).join("\n")}`;
    })
    .join("\n\n");
}

/** The summary text of a reply, without `<analysis>` scratchpads; undefined if it called a tool or is empty. */
export function summaryText(content: Block[]): string | undefined {
  if (content.some((block) => block.type === "tool_use")) return undefined;
  let text = content.filter((block): block is TextBlock => block.type === "text").map((block) => block.text).join("");
  for (;;) {
    const open = text.indexOf("<analysis>");
    if (open < 0) break;
    const close = text.indexOf("</analysis>", open);
    text = close < 0 ? text.slice(0, open) : text.slice(0, open) + text.slice(close + "</analysis>".length);
  }
  text = text.trim();
  return text || undefined;
}
