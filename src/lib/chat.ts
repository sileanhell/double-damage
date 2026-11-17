import { createSignal } from "solid-js";
import {
  autoCompact,
  COMPACT_TARGET_PERCENT,
  compactThreshold,
  type EditMethod,
  editMethod,
  maxSteps,
} from "./agent-settings";
import { answerApproval, approvalMode, cancelApprovals, isAlwaysAllowed, needsApproval, requestApproval } from "./approval";
import type { Block, Message, Stats, ToolMeta, ToolResultBlock, ToolUseBlock, Usage } from "./chat-types";
import {
  type Compaction,
  type CompactionTrigger,
  contextItems,
  elide,
  estimateTokens,
  type Item,
  itemsTokens,
  latestCompaction,
  type Partition,
  partition,
  SUMMARY_SYSTEM_PROMPT,
  sessionSummaryInstruction,
  summaryCompaction,
  summaryReserve,
  summaryText,
  transcriptSummaryRequest,
  unixPath,
  withElided,
} from "./compaction";
import { type CatalogModel, ensureLoaded, gatewayInfo, LoadCancelled, memoryConflict } from "./gateway";
import { launchSettings } from "./launch";
import { effort } from "./models";
import {
  appendMessage,
  messages,
  sessionWorkspace,
  setSessionModel,
  setSessionWorkspace,
  submitPrompt,
  updateMessage,
} from "./sessions";
import { projectInstructions, systemPrompt } from "./system-prompt";
import { runTool, sensitivePaths, TOOL_KIND, type ToolDef, type ToolName, toolDefs } from "./tools";

/**
 * Runs chat turns. A turn sends the conversation to the gateway (Anthropic Messages API) and
 * streams the reply; when the model calls tools, they run (asking the user first where the
 * approval mode says so), their results go back, and the model continues: one assistant message
 * per step, until it answers without calling a tool. Each chat runs its own turns, one at a time;
 * different chats run side by side (llama-server queues requests to the same model itself).
 */

export type TurnPhase = "queued" | "loading-model" | "waiting" | "streaming" | "tools" | "paused" | "compacting";

type Turn = {
  /** Stable for the whole turn. */
  id: string;
  sessionId: string;
  /** The assistant message being written now (a new one per step). */
  messageId: string;
  phase: TurnPhase;
  abort: AbortController;
};

const [turns, setTurns] = createSignal<Turn[]>([]);

/** The turn running or queued in a session, if any. */
export const turnIn = (sessionId: string | null) =>
  sessionId ? turns().find((turn) => turn.sessionId === sessionId) : undefined;

const patchTurn = (turn: Turn, patch: Partial<Turn>) => {
  Object.assign(turn, patch);
  setTurns((all) => all.map((item) => (item.id === turn.id ? { ...turn } : item)));
};
const setPhase = (turn: Turn, phase: TurnPhase) => patchTurn(turn, { phase });
const dropTurn = (turn: Turn) => setTurns((all) => all.filter((item) => item.id !== turn.id));

// --- live tool state ----------------------------------------------------------------------

export type ToolRun = { status: "approval" | "running"; output: string };
const [toolRuns, setToolRuns] = createSignal<Record<string, ToolRun>>({});
/** A tool call that's waiting for the user or running, with its live output. */
export const toolRun = (toolUseId: string): ToolRun | undefined => toolRuns()[toolUseId];
const setToolRun = (id: string, run: ToolRun | undefined) =>
  setToolRuns(({ [id]: _, ...rest }) => (run ? { ...rest, [id]: run } : rest));

// --- step limit ---------------------------------------------------------------------------

type LimitQuestion = { count: number; resolve: (go: boolean) => void };
const [limits, setLimits] = createSignal<Record<string, LimitQuestion>>({});
/** Steps taken when a chat's turn paused at the step limit (undefined when it isn't paused). */
export const stepLimitIn = (sessionId: string) => limits()[sessionId]?.count;

/** The user's answer to "continue?" at the step limit. */
export function answerStepLimit(sessionId: string, go: boolean) {
  const entry = limits()[sessionId];
  if (!entry) return;
  setLimits(({ [sessionId]: _, ...rest }) => rest);
  entry.resolve(go);
}

/** Per chat: the turn that runs last; a new one starts after it. */
const chains = new Map<string, Promise<void>>();

/**
 * Sends `text` to `model` in the open chat (a new chat is created if none is open). `folders`
 * become the new chat's workspace.
 */
export function send(text: string, model: CatalogModel, folders?: string[], files: string[] = []) {
  const { sessionId } = submitPrompt(text, files);
  setSessionModel(sessionId, model.id);
  if (folders?.length) setSessionWorkspace(sessionId, [...new Set([...sessionWorkspace(sessionId), ...folders])]);
  const reply = newReply(model);
  appendMessage(sessionId, reply);

  const turn: Turn = { id: crypto.randomUUID(), sessionId, messageId: reply.id, phase: "queued", abort: new AbortController() };
  setTurns((all) => [...all, turn]);
  const next: Promise<void> = (chains.get(sessionId) ?? Promise.resolve())
    .then(() => run(turn, model))
    .catch(console.error)
    .finally(() => {
      dropTurn(turn);
      if (chains.get(sessionId) === next) chains.delete(sessionId);
    });
  chains.set(sessionId, next);
}

const newReply = (model: CatalogModel): Message => ({
  id: crypto.randomUUID(),
  role: "assistant",
  content: [],
  createdAt: Date.now(),
  model: model.id,
});

/** Stops the session's turn: a running reply keeps what it has so far, a queued one is dropped. */
export function stop(sessionId: string) {
  const turn = turnIn(sessionId);
  if (!turn) return;
  turn.abort.abort();
  // A queued turn would otherwise keep the chat busy until the queue reaches it.
  if (turn.phase === "queued") {
    finish(turn, { stopReason: "cancelled" });
    dropTurn(turn);
  }
  // Don't leave questions hanging for a turn nobody waits for any more.
  if (turn.phase === "loading-model") memoryConflict()?.resolve("cancel");
  cancelApprovals(sessionId);
  answerStepLimit(sessionId, false);
}

/** Rejects with an AbortError as soon as `signal` fires (the work itself carries on). */
function abortable<T>(work: Promise<T>, signal: AbortSignal): Promise<T> {
  return new Promise((resolve, reject) => {
    const onAbort = () => reject(new DOMException("aborted", "AbortError"));
    if (signal.aborted) return onAbort();
    signal.addEventListener("abort", onAbort, { once: true });
    work.then(resolve, reject).finally(() => signal.removeEventListener("abort", onAbort));
  });
}

const finish = (turn: Turn, patch: Partial<Message>) =>
  updateMessage(turn.sessionId, turn.messageId, (message) => ({ ...message, ...patch }), true);

export { unixPath };

type Setup = {
  context: number;
  roots: string[];
  /** Files attached to the chat's messages: readable wherever they are. */
  files: string[];
  tools: ToolDef[];
  /** Fixed for the turn: the tool schema the model got was built for it. */
  method: EditMethod;
  format: "native" | "text";
  system: string;
};

/** Loads the model if needed and works out what it gets. Undefined when the turn ended (finished already). */
async function prepare(turn: Turn, model: CatalogModel): Promise<Setup | undefined> {
  // Stopped while queued: `stop` has already finished it.
  if (turn.abort.signal.aborted) return undefined;

  // 1. The model must be in memory.
  let loadedContext = model.loadedContext;
  try {
    if (model.provider === "llama-server") {
      setPhase(turn, "loading-model");
      // Stopping doesn't cancel the load itself: the model will be there for the next prompt.
      const ready = await abortable(ensureLoaded(model), turn.abort.signal);
      loadedContext = ready.spec.ctx;
    }
  } catch (error) {
    if (error instanceof LoadCancelled || turn.abort.signal.aborted) finish(turn, { stopReason: "cancelled" });
    else finish(turn, { error: errorText(error) });
    return undefined;
  }
  if (turn.abort.signal.aborted) {
    finish(turn, { stopReason: "cancelled" });
    return undefined;
  }

  // 2. What the model gets: tools when the chat has folders, and the system prompt.
  const roots = sessionWorkspace(turn.sessionId);
  const files = [...new Set(messages(turn.sessionId).flatMap((message) => message.attachments ?? []))];
  const method = editMethod();
  // Without a folder, the model can still read the files attached to messages.
  const tools = roots.length > 0 ? toolDefs(method) : files.length > 0 ? toolDefs(method).filter((tool) => tool.name === "read_file") : [];
  const format = model.capabilities.tools ? "native" : "text";
  const instructions = roots.length > 0 ? await projectInstructions(roots) : [];
  const system = systemPrompt({
    provider: model.provider,
    model: model.name,
    workspace: roots.map(unixPath),
    tools: tools.map((tool) => tool.name),
    toolFormat: format,
    toolSchemas: format === "text" ? tools : undefined,
    editMethod: method,
    date: new Date().toISOString().slice(0, 10),
    os: "Windows",
    instructions,
    attachedFiles: files.length > 0,
  });
  return { context: loadedContext ?? 4096, roots, files, tools, method, format, system };
}

async function run(turn: Turn, model: CatalogModel) {
  const setup = await prepare(turn, model);
  if (!setup) return;

  // Steps: reply, run the tools it called, repeat.
  for (let steps = 1; ; steps++) {
    // Close to full: compact first (the reply is still empty, so it becomes the marker).
    if (autoCompact() && contextUsed(turn.sessionId, turn.messageId, setup) >= (setup.context * compactThreshold()) / 100) {
      await compactInTurn(turn, model, setup, "auto");
      if (turn.abort.signal.aborted) return finish(turn, { stopReason: "cancelled" });
    }
    let calls = await step(turn, model, setup);
    if (calls === "overflow") {
      // The model rejected the context as too long: compact once and try again.
      await compactInTurn(turn, model, setup, "overflow");
      if (turn.abort.signal.aborted) return finish(turn, { stopReason: "cancelled" });
      calls = await step(turn, model, setup, false);
    }
    if (calls === "overflow" || !calls || calls.length === 0 || turn.abort.signal.aborted) return;
    setPhase(turn, "tools");
    const stopped = await runCalls(turn, model, setup, calls);
    if (stopped) return;
    if (turn.abort.signal.aborted) return finish(turn, { stopReason: "cancelled" });

    // Every `limit` steps, ask before going on (a looping model would otherwise never stop).
    const limit = maxSteps();
    if (limit !== null && steps % limit === 0) {
      setPhase(turn, "paused");
      const go = await new Promise<boolean>((resolve) =>
        setLimits((all) => ({ ...all, [turn.sessionId]: { count: steps, resolve } })),
      );
      if (!go || turn.abort.signal.aborted) return finish(turn, { stopReason: "cancelled" });
    }
    nextReply(turn, model);
  }
}

/** Starts a new assistant message for the next step. */
function nextReply(turn: Turn, model: CatalogModel) {
  const reply = newReply(model);
  appendMessage(turn.sessionId, reply);
  patchTurn(turn, { messageId: reply.id });
}

/**
 * One request and its streamed reply. Returns the tool calls it made (undefined on failure), or
 * "overflow" when the model rejected the context as too long (only with `allowOverflow`; the
 * message is left as it was so the caller can compact and retry).
 */
async function step(turn: Turn, model: CatalogModel, setup: Setup, allowOverflow = true): Promise<ToolUseBlock[] | undefined | "overflow"> {
  const gateway = gatewayInfo();
  if (!gateway?.url) {
    finish(turn, { error: gateway?.error ?? "gateway is not running" });
    return undefined;
  }

  setPhase(turn, "waiting");
  const body = requestBody(model, setup, history(turn, setup.format));
  const started = performance.now();
  let response: Response;
  try {
    response = await fetch(`${gateway.url}/v1/messages`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-api-key": gateway.token, "anthropic-version": "2023-06-01" },
      body: JSON.stringify(body),
      signal: turn.abort.signal,
    });
  } catch (error) {
    finish(turn, turn.abort.signal.aborted ? { stopReason: "cancelled" } : { error: errorText(error) });
    return undefined;
  }
  if (!response.ok || !response.body) {
    const error = await responseError(response);
    if (allowOverflow && isContextOverflow(response.status, error)) return "overflow";
    finish(turn, { error });
    return undefined;
  }

  setPhase(turn, "streaming");
  const reply = new ReplyBuilder(started);
  let flushQueued = false;
  const flush = (persist = false) => {
    flushQueued = false;
    const snapshot = reply.snapshot();
    if (persist && setup.format === "text" && snapshot.content) snapshot.content = parseTextCalls(snapshot.content);
    updateMessage(turn.sessionId, turn.messageId, (message) => ({ ...message, ...snapshot }), persist);
    return snapshot;
  };
  const scheduleFlush = () => {
    if (flushQueued) return;
    flushQueued = true;
    // At most one UI update per frame, however fast tokens arrive.
    requestAnimationFrame(() => flushQueued && flush());
  };

  try {
    for await (const event of sseEvents(response.body)) {
      reply.apply(event);
      scheduleFlush();
    }
  } catch (error) {
    if (!turn.abort.signal.aborted) reply.error = errorText(error);
  }
  if (turn.abort.signal.aborted) reply.stopReason = "cancelled";
  const final = flush(true);
  if (final.error || turn.abort.signal.aborted) return undefined;
  return (final.content ?? []).filter((block): block is ToolUseBlock => block.type === "tool_use");
}

/**
 * Runs the calls one by one, asking first where needed, and adds their results as one message.
 * Returns true when the turn was stopped (every call still gets a result, so the history stays
 * valid for the next prompt).
 */
async function runCalls(turn: Turn, model: CatalogModel, setup: Setup, calls: ToolUseBlock[]): Promise<boolean> {
  const results: ToolResultBlock[] = [];
  const meta: Record<string, ToolMeta> = {};
  const offered = new Set(setup.tools.map((tool) => tool.name));
  const local = model.provider === "llama-server";
  let stopped = false;

  for (const call of calls) {
    const signal = turn.abort.signal;
    const result = (content: string, isError: boolean, extra: ToolMeta = {}) => {
      results.push({ type: "tool_result", tool_use_id: call.id, content, ...(isError ? { is_error: true } : {}) });
      meta[call.id] = extra;
    };
    if (stopped || signal.aborted) {
      stopped = true;
      result("The user stopped the turn before this tool ran.", true, { cancelled: true });
      continue;
    }
    if (!offered.has(call.name as ToolName)) {
      result(`Error: there is no tool named "${call.name}". Available: ${[...offered].join(", ") || "none"}.`, true);
      continue;
    }
    // Folders removed from the chat mid-turn are off limits from then on.
    const current = sessionWorkspace(turn.sessionId).map((folder) => folder.toLowerCase());
    const roots = setup.roots.filter((root) => current.includes(root.toLowerCase()));
    const filesOnly = setup.roots.length === 0 && call.name === "read_file" && setup.files.length > 0;
    if (roots.length === 0 && !filesOnly) {
      result("Error: the project folder was removed from this chat, so file tools are unavailable. Ask the user to attach it again.", true);
      continue;
    }

    // Edits where git or an editor would run the file count as running code.
    let kind = TOOL_KIND[call.name as ToolName];
    let key: string = call.name;
    let note: string | undefined;
    if (kind === "edit") {
      const risky = await sensitivePaths(call, roots, setup.method);
      if (risky.length > 0) {
        kind = "exec";
        key = "edit:sensitive";
        note = risky.join(", ");
      }
    }
    if (needsApproval(kind, approvalMode(local)) && !isAlwaysAllowed(turn.sessionId, key)) {
      setToolRun(call.id, { status: "approval", output: "" });
      const decision = await requestApproval(turn.sessionId, call.id, key, note);
      setToolRun(call.id, undefined);
      if (!decision.allow) {
        if (decision.cancelled || signal.aborted) {
          stopped = true;
          result("The user stopped the turn before this tool ran.", true, { cancelled: true });
        } else {
          const why = decision.reason?.trim();
          result(`The user declined this tool call${why ? `: ${why}` : "."} Don't retry it as is; adjust or ask.`, true, {
            denied: { reason: why || undefined },
          });
        }
        continue;
      }
    }

    setToolRun(call.id, { status: "running", output: "" });
    let output = "";
    try {
      const outcome = await runTool(call, roots, setup.method, setup.files, (chunk) => {
        output += chunk;
        setToolRun(call.id, { status: "running", output });
      }, signal);
      result(outcome.content, outcome.isError, outcome.meta);
      if (outcome.meta.cancelled) stopped = true;
    } catch {
      stopped = true;
      result("The user stopped this tool while it ran.", true, { cancelled: true });
    } finally {
      setToolRun(call.id, undefined);
    }
  }

  appendMessage(turn.sessionId, {
    id: crypto.randomUUID(),
    role: "user",
    content: results,
    createdAt: Date.now(),
    tools: meta,
  });
  if (stopped) finish(turn, { stopReason: "cancelled" });
  return stopped;
}

/** Answers a pending tool question from the chat (re-exported for the UI). */
export { answerApproval };

// --- compaction ----------------------------------------------------------------------------

/** llama-server (and Anthropic) wording for a prompt that doesn't fit the context. */
const isContextOverflow = (status: number, error: string) =>
  status === 400 && /exceed.*context|context (size|length|window)|too long|n_ctx|prompt is too long/i.test(error);

const fixedChars = (setup: Setup) => setup.system.length + JSON.stringify(setup.tools).length;

/**
 * Ratio between the model's token count and our estimate, from the latest reply that reported
 * its prompt size (1 when there's none yet).
 */
function calibration(list: Message[], setup: Setup): number {
  for (let i = list.length - 1; i >= 0; i--) {
    const message = list[i];
    if (message.compaction?.done) break;
    if (message.role !== "assistant" || !message.usage?.input) continue;
    const estimated = estimateTokens(fixedChars(setup)) + itemsTokens(contextItems(list.slice(0, i)), 1);
    return estimated > 0 ? Math.min(3, Math.max(0.5, message.usage.input / estimated)) : 1;
  }
  return 1;
}

/** How full the context is (tokens): the latest reply's own count, or the estimate after a compaction. */
function contextUsed(sessionId: string, exclude: string, setup: Setup): number {
  const list = messages(sessionId).filter((message) => message.id !== exclude);
  for (let i = list.length - 1; i >= 0; i--) {
    const message = list[i];
    if (message.compaction?.done && message.compaction.after !== undefined) {
      // Plus whatever came after the compaction.
      return message.compaction.after + itemsTokens(contextItems(list.slice(i + 1)), calibration(list, setup));
    }
    if (message.role === "assistant" && message.usage?.input) return message.usage.input + message.usage.output;
  }
  return 0;
}

/** Compacts the open chat now (`/compact [focus]`); runs in the chat's queue like a turn. */
export function compact(sessionId: string, model: CatalogModel, focus?: string) {
  const marker: Message = {
    id: crypto.randomUUID(),
    role: "user",
    content: [],
    createdAt: Date.now(),
    model: model.id,
    compaction: { trigger: "manual", done: false, focus: focus || undefined, keep: [], elided: [] },
  };
  appendMessage(sessionId, marker);
  const turn: Turn = { id: crypto.randomUUID(), sessionId, messageId: marker.id, phase: "queued", abort: new AbortController() };
  setTurns((all) => [...all, turn]);
  const next: Promise<void> = (chains.get(sessionId) ?? Promise.resolve())
    .then(async () => {
      const setup = await prepare(turn, model);
      if (setup) await compactAt(turn, model, setup, "manual", focus);
    })
    .catch(console.error)
    .finally(() => {
      dropTurn(turn);
      if (chains.get(sessionId) === next) chains.delete(sessionId);
    });
  chains.set(sessionId, next);
}

/** Mid-turn: the (still empty) reply becomes the marker, and a fresh reply follows it. */
async function compactInTurn(turn: Turn, model: CatalogModel, setup: Setup, trigger: CompactionTrigger) {
  updateMessage(turn.sessionId, turn.messageId, (message) => ({
    ...message,
    role: "user",
    content: [],
    compaction: { trigger, done: false, keep: [], elided: [] },
  }));
  await compactAt(turn, model, setup, trigger);
  if (!turn.abort.signal.aborted) nextReply(turn, model);
}

/** Runs the tiers for the marker `turn.messageId` and records the outcome on it. */
async function compactAt(turn: Turn, model: CatalogModel, setup: Setup, trigger: CompactionTrigger, focus?: string) {
  setPhase(turn, "compacting");
  const set = (compaction: Compaction, patch: Partial<Message> = {}) =>
    updateMessage(turn.sessionId, turn.messageId, (message) => ({ ...message, ...patch, compaction }), true);

  const list = messages(turn.sessionId);
  const index = list.findIndex((message) => message.id === turn.messageId);
  const earlier = list.slice(0, index);
  const items = contextItems(earlier);
  const ratio = calibration(earlier, setup);
  const fixed = estimateTokens(fixedChars(setup), ratio);
  const before = fixed + itemsTokens(items, ratio);
  const configured = Math.floor((setup.context * COMPACT_TARGET_PERCENT) / 100);
  // Manual and overflow compaction always remove something: at most half of what's there now.
  const target = trigger === "auto" ? configured : Math.min(configured, Math.floor(before / 2));
  const previous = latestCompaction(earlier)?.compaction;
  const base = { trigger, done: true, focus: focus || undefined, before };

  // Tier 1: elide old tool results; done if that's enough.
  const elision = elide(items, fixed, target, ratio);
  const elided = [...new Set([...(previous?.elided ?? []), ...elision.ids])];
  const view = {
    summary: previous?.summary,
    keep: previous?.keep ?? [],
    latestUser: previous?.latestUser,
    tailStart: previous ? previous.tailStart : items[0]?.id,
  };
  if (elision.ids.length > 0 && elision.tokens <= target) {
    return set({ ...base, ...view, elided, after: elision.tokens });
  }

  // Tier 2: a summary by the same model.
  const elidedItems = withElided(items, elision.ids);
  const plans: { part: Partition; elided: string[]; body: () => Record<string, unknown> }[] = [];
  const reserve = summaryReserve(target);
  const summaryOptions = (body: Record<string, unknown>) => {
    body.max_tokens = Math.max(1024, reserve);
    // Thinking would eat the summary's budget.
    if (model.capabilities.reasoning === "toggle") body.thinking = { type: "disabled" };
    else delete body.output_config;
    return body;
  };
  // a) The chat's own history plus an instruction: the model has that prefix cached already.
  //    Not when recovering from overflow (it just didn't fit), nor when it wouldn't fit now.
  const sessionPart = partition(items, fixed, target, ratio);
  if (trigger !== "overflow" && sessionPart) {
    const instruction = sessionSummaryInstruction(sessionPart, focus);
    if (before + estimateTokens(instruction.length, ratio) + reserve <= setup.context) {
      plans.push({
        part: sessionPart,
        elided: previous?.elided ?? [],
        body: () => {
          const api = toApi(items, setup.format);
          push(api, "user", [{ type: "text", text: instruction }]);
          return summaryOptions(requestBody(model, setup, api));
        },
      });
    }
  }
  // b) A standalone transcript of the span, without tools.
  const transcriptPart = partition(elidedItems, fixed, target, ratio);
  if (transcriptPart) {
    plans.push({
      part: transcriptPart,
      elided,
      body: () => {
        const maxChars = Math.max(4000, Math.floor(((setup.context - reserve) * 4) / ratio) - SUMMARY_SYSTEM_PROMPT.length);
        const request = transcriptSummaryRequest(transcriptPart, focus, maxChars);
        const body = requestBody(model, { ...setup, tools: [], system: request.system }, [
          { role: "user", content: [{ type: "text", text: request.user }] },
        ]);
        delete body.stop_sequences;
        return summaryOptions(body);
      },
    });
  }

  if (plans.length === 0) {
    // Nothing to summarize; maybe the elision still helped.
    if (elision.ids.length > 0) return set({ ...base, ...view, elided, after: elision.tokens });
    return set({ trigger, done: false, keep: [], elided: [], before }, { stopReason: "unchanged" });
  }

  let lastError = "";
  for (const plan of plans) {
    if (turn.abort.signal.aborted) break;
    const reply = await complete(plan.body(), turn.abort.signal);
    const text = reply.error ? undefined : summaryText(reply.content);
    if (!text) {
      lastError = reply.error ?? "the model returned no summary";
      continue;
    }
    const compaction = summaryCompaction(plan.part, text, { ...base, elided: plan.elided });
    set(compaction);
    // The estimate of what the model sees now.
    const now = messages(turn.sessionId);
    const at = now.findIndex((message) => message.id === turn.messageId);
    set({ ...compaction, after: fixed + itemsTokens(contextItems(now.slice(0, at + 1)), ratio) });
    return;
  }
  if (turn.abort.signal.aborted) return set({ trigger, done: false, keep: [], elided: [], before }, { stopReason: "cancelled" });
  set({ trigger, done: false, keep: [], elided: [], before }, { error: lastError });
}

/** Sends one request and collects the whole reply (no live updates). */
async function complete(body: Record<string, unknown>, signal: AbortSignal): Promise<{ content: Block[]; error?: string }> {
  const gateway = gatewayInfo();
  if (!gateway?.url) return { content: [], error: gateway?.error ?? "gateway is not running" };
  try {
    const response = await fetch(`${gateway.url}/v1/messages`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-api-key": gateway.token, "anthropic-version": "2023-06-01" },
      body: JSON.stringify(body),
      signal,
    });
    if (!response.ok || !response.body) return { content: [], error: await responseError(response) };
    const reply = new ReplyBuilder(performance.now());
    for await (const event of sseEvents(response.body)) reply.apply(event);
    const snapshot = reply.snapshot();
    return { content: snapshot.content ?? [], error: snapshot.error };
  } catch (error) {
    return { content: [], error: errorText(error) };
  }
}

// --- request ------------------------------------------------------------------------------

type ApiMessage = { role: "user" | "assistant"; content: Block[] };

const NOT_RUN = "Not run: the turn was stopped before this tool ran.";

/**
 * The conversation as the API takes it: no thinking, no failed or empty replies, roles
 * alternating, and every tool call followed by its result (a stop, a crash or a stream error can
 * leave a call without one).
 */
function history(turn: Turn, format: "native" | "text"): ApiMessage[] {
  return toApi(contextItems(messages(turn.sessionId), turn.messageId), format);
}

/** Items as API messages: roles merged, calls paired with results, text format if needed. */
function toApi(items: Item[], format: "native" | "text"): ApiMessage[] {
  let out: ApiMessage[] = [];
  for (const item of items) push(out, item.role, item.content);
  out = pairToolCalls(out);
  if (format === "native") return out;

  const names = new Map<string, string>();
  for (const message of out) for (const block of message.content) if (block.type === "tool_use") names.set(block.id, block.name);
  return out.map((message) => ({ role: message.role, content: asText(message.content, names) }));
}

/** Appends, merging into the last message when the role repeats. */
function push(list: ApiMessage[], role: ApiMessage["role"], content: Block[]) {
  const last = list[list.length - 1];
  if (last?.role === role) last.content.push(...content);
  else list.push({ role, content: [...content] });
}

/** Gives every tool_use a tool_result right after it, and drops results without a call. */
function pairToolCalls(list: ApiMessage[]): ApiMessage[] {
  const out: ApiMessage[] = [];
  for (let i = 0; i < list.length; i++) {
    const message = list[i];
    if (message.role === "user") {
      // Results belong to the assistant message right before; anything else is stray.
      const previous = out[out.length - 1];
      const ids = new Set(
        previous?.role === "assistant" ? previous.content.flatMap((block) => (block.type === "tool_use" ? [block.id] : [])) : [],
      );
      const content = message.content.filter((block) => block.type !== "tool_result" || ids.has(block.tool_use_id));
      if (content.length > 0) push(out, "user", content);
      continue;
    }
    push(out, "assistant", message.content);
    const calls = message.content.filter((block): block is ToolUseBlock => block.type === "tool_use");
    if (calls.length === 0) continue;
    const next = list[i + 1]?.role === "user" ? list[i + 1] : undefined;
    const results = (next?.content ?? []).filter((block): block is ToolResultBlock => block.type === "tool_result");
    const answered = new Set(results.map((block) => block.tool_use_id));
    const missing: ToolResultBlock[] = calls
      .filter((call) => !answered.has(call.id))
      .map((call) => ({ type: "tool_result", tool_use_id: call.id, content: NOT_RUN, is_error: true }));
    if (missing.length === 0) continue;
    if (next) {
      // Results first: the API wants them at the start of the user message.
      next.content = [...results, ...missing, ...next.content.filter((block) => block.type !== "tool_result")];
    } else {
      list.splice(i + 1, 0, { role: "user", content: missing });
    }
  }
  return out;
}

/** For models without native tool calling: calls and results as the text format of the prompt. */
function asText(content: Block[], toolNames: Map<string, string>): Block[] {
  const parts = content.map((block) => {
    if (block.type === "tool_use") {
      return `<tool_call>\n${JSON.stringify({ name: block.name, arguments: block.input })}\n</tool_call>`;
    }
    if (block.type === "tool_result") {
      const body = typeof block.content === "string" ? block.content : block.content.map((part) => part.text).join("\n");
      const name = toolNames.get(block.tool_use_id) ?? "tool";
      return `<tool_result name="${name}"${block.is_error ? ' error="true"' : ""}>\n${body}\n</tool_result>`;
    }
    return block.type === "text" ? block.text : "";
  });
  return [{ type: "text", text: parts.filter(Boolean).join("\n\n") }];
}

const OPEN_TAG = "<tool_call>";

/** Offsets covered by ``` fenced code blocks (an unclosed fence runs to the end). */
function fencedRanges(text: string): [number, number][] {
  const ranges: [number, number][] = [];
  let open: number | undefined;
  for (const match of text.matchAll(/^[ \t]*```/gm)) {
    if (open === undefined) open = match.index;
    else {
      ranges.push([open, match.index + match[0].length]);
      open = undefined;
    }
  }
  if (open !== undefined) ranges.push([open, text.length]);
  return ranges;
}

/** The JSON object starting at `from` (after whitespace), with where it ends; strings may hold braces and tags. */
function readObject(text: string, from: number): { value: unknown; end: number } | undefined {
  let i = from;
  while (i < text.length && /\s/.test(text[i])) i++;
  if (text[i] !== "{") return undefined;
  const start = i;
  let depth = 0;
  let inString = false;
  for (; i < text.length; i++) {
    const ch = text[i];
    if (inString) {
      if (ch === "\\") i++;
      else if (ch === '"') inString = false;
    } else if (ch === '"') inString = true;
    else if (ch === "{") depth++;
    else if (ch === "}" && --depth === 0) {
      try {
        return { value: JSON.parse(text.slice(start, i + 1)), end: i + 1 };
      } catch {
        return undefined;
      }
    }
  }
  return undefined;
}

function toCall(value: unknown): ToolUseBlock | undefined {
  if (!value || typeof value !== "object") return undefined;
  const { name, arguments: args, parameters, input } = value as Record<string, unknown>;
  if (typeof name !== "string" || !name) return undefined;
  let data = args ?? parameters ?? input ?? {};
  if (typeof data === "string") {
    try {
      data = JSON.parse(data);
    } catch {
      return undefined;
    }
  }
  return { type: "tool_use", id: `call_${crypto.randomUUID().slice(0, 8)}`, name, input: data };
}

/**
 * Splits `<tool_call>` blocks out of a text-format reply into tool_use blocks. Tags inside code
 * fences are just text (the model showing an example); a call the model didn't close still counts.
 */
export function parseTextCalls(content: Block[]): Block[] {
  const out: Block[] = [];
  for (const block of content) {
    if (block.type !== "text" || !block.text.includes(OPEN_TAG)) {
      out.push(block);
      continue;
    }
    const text = block.text;
    const fences = fencedRanges(text);
    const fenced = (at: number) => fences.some(([start, end]) => at >= start && at < end);
    let last = 0;
    let found = false;
    for (let pos = text.indexOf(OPEN_TAG); pos >= 0; pos = text.indexOf(OPEN_TAG, pos + 1)) {
      if (pos < last || fenced(pos)) continue;
      const parsed = readObject(text, pos + OPEN_TAG.length);
      const call = parsed && toCall(parsed.value);
      if (!parsed || !call) continue;
      const before = text.slice(last, pos).trim();
      if (before) out.push({ type: "text", text: before });
      out.push(call);
      found = true;
      const close = /^\s*(?:<\/tool_call>)?/.exec(text.slice(parsed.end))?.[0] ?? "";
      last = parsed.end + close.length;
    }
    const rest = text.slice(last).trim();
    // After a call, the rest is the model guessing the result: drop it.
    if (rest && !found) out.push({ type: "text", text: rest });
  }
  return out;
}

/** The request for one step: history, system prompt, tools, effort and sampling. */
function requestBody(model: CatalogModel, setup: Setup, messages: ApiMessage[]) {
  const { context } = setup;
  const body: Record<string, unknown> = {
    model: model.id,
    max_tokens: context,
    stream: true,
    system: setup.system,
    messages,
  };
  if (setup.tools.length > 0 && setup.format === "native") body.tools = setup.tools;
  // A text-format model that starts writing a result itself is cut off: results come from us.
  if (setup.tools.length > 0 && setup.format === "text") body.stop_sequences = ["<tool_result"];

  const level = effort(model);
  if (model.capabilities.reasoning === "toggle" && level) {
    body.thinking = level === "on" ? { type: "enabled", budget_tokens: Math.max(1024, Math.floor(context / 2)) } : { type: "disabled" };
  } else if (model.capabilities.reasoning === "effort" && level) {
    body.output_config = { effort: level };
  }

  // Sampling: the user's setting, else the model's own recommendation, else llama-server's default.
  if (model.file) {
    const chosen = launchSettings(model.file);
    const recommended = model.sampling;
    const pick = <K extends "temperature" | "topK" | "topP" | "minP" | "repeatPenalty">(key: K) =>
      chosen[key] ?? recommended?.[key] ?? undefined;
    const sampling = {
      temperature: pick("temperature"),
      top_k: pick("topK"),
      top_p: pick("topP"),
      min_p: pick("minP"),
      repeat_penalty: pick("repeatPenalty"),
    };
    for (const [key, value] of Object.entries(sampling)) if (value !== undefined && value !== null) body[key] = value;
  }
  return body;
}

// --- streaming --------------------------------------------------------------------------

type StreamEvent = { type: string; [key: string]: unknown };

/** Parses a server-sent event stream into its JSON `data` payloads. */
async function* sseEvents(body: ReadableStream<Uint8Array>): AsyncGenerator<StreamEvent> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      let boundary: number;
      while ((boundary = buffer.search(/\r?\n\r?\n/)) >= 0) {
        const chunk = buffer.slice(0, boundary);
        buffer = buffer.slice(boundary).replace(/^\r?\n\r?\n/, "");
        const data = chunk
          .split(/\r?\n/)
          .filter((line) => line.startsWith("data:"))
          .map((line) => line.slice(5).trimStart())
          .join("\n");
        if (!data || data === "[DONE]") continue;
        yield JSON.parse(data) as StreamEvent;
      }
    }
  } finally {
    reader.releaseLock();
  }
}

/** Accumulates Anthropic stream events into message content, usage and timing. */
class ReplyBuilder {
  /** Blocks by stream index; llama-server may close them out of order, so index is the key. */
  private blocks = new Map<number, Block & { partialJson?: string }>();
  private usage: Usage = { input: 0, output: 0 };
  private firstToken?: number;
  private firstThinking?: number;
  private thinkingEnd?: number;
  private lastToken?: number;
  /** Streamed deltas: about one token each, the count to fall back on when the reply is cut short. */
  private deltas = 0;
  stopReason?: string;
  error?: string;

  constructor(private started: number) {}

  apply(event: StreamEvent) {
    switch (event.type) {
      case "message_start": {
        const usage = (event.message as { usage?: { input_tokens?: number; cache_read_input_tokens?: number } })?.usage;
        this.usage.input = (usage?.input_tokens ?? 0) + (usage?.cache_read_input_tokens ?? 0);
        break;
      }
      case "content_block_start": {
        const block = event.content_block as Block;
        const index = event.index as number;
        if (block.type === "tool_use") this.blocks.set(index, { ...block, input: {}, partialJson: "" });
        else this.blocks.set(index, { ...block });
        break;
      }
      case "content_block_delta": {
        const index = event.index as number;
        const delta = event.delta as { type: string; text?: string; thinking?: string; partial_json?: string; signature?: string };
        const block = this.blocks.get(index);
        if (!block) break;
        const now = performance.now();
        if (delta.type === "text_delta" && block.type === "text") {
          block.text += delta.text ?? "";
          this.token(now, false);
        } else if (delta.type === "thinking_delta" && block.type === "thinking") {
          block.thinking += delta.thinking ?? "";
          this.token(now, true);
        } else if (delta.type === "signature_delta" && block.type === "thinking") {
          if (delta.signature) block.signature = (block.signature ?? "") + delta.signature;
        } else if (delta.type === "input_json_delta" && block.type === "tool_use") {
          block.partialJson = (block.partialJson ?? "") + (delta.partial_json ?? "");
          this.token(now, false);
        }
        break;
      }
      case "content_block_stop": {
        const block = this.blocks.get(event.index as number);
        if (block?.type === "tool_use" && block.partialJson !== undefined) {
          try {
            block.input = block.partialJson ? JSON.parse(block.partialJson) : {};
          } catch {
            block.input = { _raw: block.partialJson };
          }
          delete block.partialJson;
        }
        break;
      }
      case "message_delta": {
        const delta = event.delta as { stop_reason?: string };
        const usage = event.usage as { output_tokens?: number } | undefined;
        if (delta?.stop_reason) this.stopReason = delta.stop_reason;
        if (usage?.output_tokens !== undefined) this.usage.output = usage.output_tokens;
        break;
      }
      case "error": {
        const error = event.error as { message?: string } | undefined;
        this.error = error?.message ?? "stream error";
        break;
      }
    }
  }

  private token(now: number, thinking: boolean) {
    this.deltas++;
    this.firstToken ??= now;
    if (thinking) this.firstThinking ??= now;
    else if (this.firstThinking !== undefined) this.thinkingEnd ??= now;
    this.lastToken = now;
  }

  snapshot(): Partial<Message> {
    const content = [...this.blocks.entries()]
      .sort(([a], [b]) => a - b)
      .map(([, block]) => {
        const { partialJson: _, ...rest } = block as Block & { partialJson?: string };
        return rest as Block;
      });
    // A stopped reply never gets the final usage event.
    const usage = { ...this.usage, output: this.usage.output || this.deltas };
    const stats: Stats = {};
    if (this.firstToken !== undefined) {
      stats.ttft = Math.round(this.firstToken - this.started);
      const generation = (this.lastToken ?? this.firstToken) - this.firstToken;
      stats.generation = Math.round(generation);
      if (generation > 0 && usage.output > 1) stats.tps = Math.round(((usage.output - 1) / generation) * 10000) / 10;
    }
    if (this.firstThinking !== undefined) {
      stats.thinking = Math.round((this.thinkingEnd ?? this.lastToken ?? this.firstThinking) - this.firstThinking);
    }
    return {
      content,
      usage,
      stats,
      ...(this.stopReason ? { stopReason: this.stopReason } : {}),
      ...(this.error ? { error: this.error } : {}),
    };
  }
}

// --- errors -----------------------------------------------------------------------------

function errorText(error: unknown): string {
  if (error instanceof Error) return error.message;
  return typeof error === "string" ? error : JSON.stringify(error);
}

async function responseError(response: Response): Promise<string> {
  const text = await response.text().catch(() => "");
  try {
    const parsed = JSON.parse(text) as { error?: { message?: string } | string };
    const message = typeof parsed.error === "string" ? parsed.error : parsed.error?.message;
    if (message) return message;
  } catch {
    // not JSON
  }
  return text || `HTTP ${response.status}`;
}