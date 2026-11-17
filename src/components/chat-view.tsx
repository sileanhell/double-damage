import { Button } from "@kobalte/core/button";
import { Collapsible } from "@kobalte/core/collapsible";
import { type Accessor, createEffect, createMemo, createSignal, For, Index, Match, on, onCleanup, Show, Switch } from "solid-js";
import { answerStepLimit, stepLimitIn, type TurnPhase, turnIn } from "../lib/chat";
import type { Block, Message, ThinkingBlock, ToolUseBlock } from "../lib/chat-types";
import type { CompactionTrigger } from "../lib/compaction";
import { thinkingDisplay } from "../lib/chat-settings";
import { catalogModel } from "../lib/gateway";
import { locale, type MessageKey, t } from "../lib/i18n";
import { activeSessionId, messages } from "../lib/sessions";
import { ChevronDownIcon, PaperclipIcon } from "./icons";
import { Markdown } from "./markdown";
import { ToolCard, type ToolResult } from "./tool-card";

/** Messages of the open chat; follows the bottom while a reply streams in, unless scrolled up. */
export function ChatView() {
  let scroller!: HTMLDivElement;
  const [pinned, setPinned] = createSignal(true);
  const list = () => {
    const id = activeSessionId();
    return id ? messages(id) : [];
  };

  /** Each tool call's result, by tool_use id (results arrive in the next user message). */
  const results = createMemo(() => {
    const map = new Map<string, ToolResult>();
    for (const message of list()) {
      for (const block of message.content) {
        if (block.type === "tool_result") map.set(block.tool_use_id, { block, meta: message.tools?.[block.tool_use_id] });
      }
    }
    return map;
  });

  const atBottom = () => scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 40;

  // New content: stay at the bottom if we were there. Messages are updated in place while they
  // stream (the store reconciles), so track how much there is, not the objects.
  createEffect(
    on(
      () => {
        const last = list()[list().length - 1];
        const size = last?.content.reduce(
          (sum, block) => sum + (block.type === "text" ? block.text.length : block.type === "thinking" ? block.thinking.length : 1),
          0,
        );
        return [list().length, last?.content.length, size, last?.error] as const;
      },
      () => {
        if (pinned()) queueMicrotask(() => scroller.scrollTo({ top: scroller.scrollHeight }));
      },
    ),
  );
  // Switching chats jumps to the end.
  createEffect(
    on(activeSessionId, () => {
      setPinned(true);
      queueMicrotask(() => scroller.scrollTo({ top: scroller.scrollHeight }));
    }),
  );

  return (
    <div ref={scroller} onScroll={() => setPinned(atBottom())} class="thin-scroll min-h-0 flex-1 overflow-y-auto">
      <div class="mx-auto flex w-full max-w-3xl flex-col gap-6 px-6 py-6">
        {/* Rebuilt per chat, so per-message state (an open thinking block) doesn't carry over to
            whatever message sits at the same position in the next chat. Inside, Index rather than
            For: a streaming reply may be replaced by a new object, and its view must survive that. */}
        <Show when={activeSessionId()} keyed>
          <Index each={list()}>
            {(message) => (
              <Show when={!message().compaction} fallback={<CompactionDivider message={message()} />}>
              <Show
                when={message().role === "user"}
                fallback={<AssistantMessage message={message()} results={results()} />}
              >
                {/* Tool results are shown on their calls' cards, not as messages of their own. */}
                <Show when={message().content.some((block) => block.type === "text") || message().attachments?.length}>
                  <UserMessage message={message()} />
                </Show>
              </Show>
              </Show>
            )}
          </Index>
          <StepLimitQuestion sessionId={activeSessionId()!} />
        </Show>
      </div>
    </div>
  );
}

const TRIGGER_LABEL: Record<CompactionTrigger, MessageKey> = {
  manual: "compact.manual",
  auto: "compact.auto",
  overflow: "compact.overflow",
};

/** Where the context was compacted: what happened, and the summary the model got instead. */
function CompactionDivider(props: { message: Message }) {
  const compaction = () => props.message.compaction!;
  const live = () => turnIn(activeSessionId())?.messageId === props.message.id;
  const status = () => {
    const c = compaction();
    if (live()) return { text: t("compact.running"), tone: "animate-pulse text-accent" };
    if (c.done) {
      const sizes = c.before !== undefined && c.after !== undefined ? t("compact.sizes", { before: compact(c.before), after: compact(c.after) }) : "";
      const what = c.summary ? t("compact.done") : t("compact.elided", { count: c.elided.length });
      return { text: [what, sizes].filter(Boolean).join(" · "), tone: "text-white/55" };
    }
    if (props.message.error) return { text: t("compact.failed", { reason: props.message.error }), tone: "text-red-300/80" };
    if (props.message.stopReason === "unchanged") return { text: t("compact.unchanged"), tone: "text-white/45" };
    return { text: t("compact.cancelled"), tone: "text-white/45" };
  };
  const [open, setOpen] = createSignal(false);
  return (
    <div class="flex flex-col gap-2">
      <div class="flex items-center gap-3 text-xs">
        <div class="h-px flex-1 bg-white/10" />
        <span class="shrink-0 text-white/35">{t(TRIGGER_LABEL[compaction().trigger])}</span>
        <span class={`min-w-0 truncate ${status().tone}`}>{status().text}</span>
        <div class="h-px flex-1 bg-white/10" />
      </div>
      <Show when={compaction().focus}>
        {(focus) => <p class="text-center text-xs text-white/40">{t("compact.focus", { focus: focus() })}</p>}
      </Show>
      <Show when={compaction().done && compaction().summary}>
        {(summary) => (
          <Collapsible open={open()} onOpenChange={setOpen} class="self-center">
            <Collapsible.Trigger class="group mx-auto flex items-center gap-1.5 rounded-lg px-2 py-1 text-xs text-white/45 outline-none transition-colors hover:text-white/80">
              {t("compact.summary")}
              <ChevronDownIcon class="size-3 transition-transform group-data-expanded:rotate-180" />
            </Collapsible.Trigger>
            <Collapsible.Content class="mt-2 w-full max-w-3xl rounded-xl border border-white/10 bg-white/[0.02] px-4 py-3 text-white/75">
              <Markdown text={summary()} />
            </Collapsible.Content>
          </Collapsible>
        )}
      </Show>
    </div>
  );
}

/** "The model has taken N steps. Continue?" when a turn reaches the step limit. */
function StepLimitQuestion(props: { sessionId: string }) {
  return (
    <Show when={stepLimitIn(props.sessionId)}>
      {(count) => (
        <div class="flex flex-wrap items-center gap-3 rounded-xl border border-amber-300/30 bg-amber-400/[0.06] px-4 py-3 text-sm text-amber-100">
          <span class="flex-1">{t("chat.stepLimit", { count: count() })}</span>
          <Button
            class="h-7 rounded-lg bg-accent px-2.5 text-[13px] font-medium text-accent-fg hover:brightness-110"
            onClick={() => answerStepLimit(props.sessionId, true)}
          >
            {t("chat.stepLimitContinue")}
          </Button>
          <Button
            class="h-7 rounded-lg border border-white/15 px-2.5 text-[13px] text-white/80 hover:bg-white/[0.07]"
            onClick={() => answerStepLimit(props.sessionId, false)}
          >
            {t("chat.stepLimitStop")}
          </Button>
        </div>
      )}
    </Show>
  );
}

function UserMessage(props: { message: Message }) {
  const hasText = () => props.message.content.some((block) => block.type === "text" && block.text);
  return (
    <div class="flex flex-col items-end gap-1.5">
      {/* Attached files are references: shown by name, the full path on hover. */}
      <Show when={props.message.attachments?.length}>
        <div class="flex max-w-[85%] flex-wrap justify-end gap-1.5">
          <For each={props.message.attachments}>
            {(path) => (
              <span
                title={path}
                class="flex max-w-56 items-center gap-1.5 rounded-lg border border-white/10 bg-white/5 px-2.5 py-1 text-xs text-white/75"
              >
                <PaperclipIcon class="size-3 shrink-0 text-white/45" />
                <span class="truncate">{path.split(/[\\/]/).pop()}</span>
              </span>
            )}
          </For>
        </div>
      </Show>
      <Show when={hasText()}>
        <div class="max-w-[85%] rounded-2xl border border-white/10 bg-white/[0.06] px-4 py-2.5 text-[15px] leading-6 whitespace-pre-wrap break-words text-white/90">
          <For each={props.message.content}>{(block) => (block.type === "text" ? block.text : null)}</For>
        </div>
      </Show>
    </div>
  );
}

const PHASE_LABEL: Record<TurnPhase, MessageKey> = {
  tools: "chat.tools",
  paused: "chat.paused",
  compacting: "chat.compacting",
  queued: "chat.queued",
  "loading-model": "chat.loadingModel",
  waiting: "chat.waiting",
  streaming: "chat.streaming",
};

function AssistantMessage(props: { message: Message; results: Map<string, ToolResult> }) {
  const turn = () => {
    const current = turnIn(activeSessionId());
    return current?.messageId === props.message.id ? current : undefined;
  };
  const live = () => !!turn();
  const hasText = () => props.message.content.some((block) => block.type !== "thinking");

  return (
    <div class="flex flex-col gap-2 text-[15px] leading-7 text-white/85">
      <Index each={props.message.content}>
        {(block, index) => (
          <BlockView
            block={block}
            message={props.message}
            results={props.results}
            live={live()}
            // Thinking is still going while it's the last block of a live reply.
            thinking={live() && index === props.message.content.length - 1}
          />
        )}
      </Index>

      <Show when={live() && !hasText()}>
        <PhaseIndicator phase={turn()!.phase} started={props.message.createdAt} />
      </Show>

      <Show when={props.message.error}>
        <p class="rounded-lg border border-red-400/25 bg-red-500/10 px-3 py-2 text-sm text-red-200 whitespace-pre-wrap">
          {props.message.error}
        </p>
      </Show>

      {/* A step that called tools isn't the answer yet: the stats go under the final reply
          (unless the turn stopped there). */}
      <Show
        when={
          !live() &&
          (!props.message.content.some((block) => block.type === "tool_use") || props.message.stopReason === "cancelled")
        }
      >
        <MessageFooter message={props.message} />
      </Show>
    </div>
  );
}

function BlockView(props: {
  block: Accessor<Block>;
  message: Message;
  results: Map<string, ToolResult>;
  live: boolean;
  thinking: boolean;
}) {
  // `keyed={false}`-style matching on the type only, so the view isn't rebuilt per token.
  const type = () => props.block().type;
  return (
    <Switch>
      <Match when={type() === "text"}>
        <Markdown text={(props.block() as { text: string }).text} streaming={props.live} />
      </Match>
      <Match when={type() === "tool_use"}>
        <ToolCard
          call={props.block() as ToolUseBlock}
          result={props.results.get((props.block() as ToolUseBlock).id)}
          live={props.live}
        />
      </Match>
      <Match when={type() === "thinking"}>
        <Thinking
          block={props.block() as ThinkingBlock}
          active={props.thinking}
          duration={props.message.stats?.thinking}
        />
      </Match>
    </Switch>
  );
}

function Thinking(props: { block: ThinkingBlock; active: boolean; duration?: number }) {
  const [open, setOpen] = createSignal(thinkingDisplay() === "expanded");
  const label = () =>
    props.active
      ? t("chat.thinking")
      : props.duration
        ? t("chat.thought", { seconds: (props.duration / 1000).toLocaleString(locale(), { maximumFractionDigits: 1 }) })
        : t("chat.thoughtShort");

  return (
    <Collapsible open={open()} onOpenChange={setOpen}>
      <Collapsible.Trigger class="group flex items-center gap-1.5 rounded-md text-sm text-white/45 outline-none transition-colors hover:text-white/75 focus-visible:text-white/75">
        <span classList={{ "animate-pulse": props.active }}>{label()}</span>
        <ChevronDownIcon class="size-3.5 transition-transform group-data-expanded:rotate-180" />
      </Collapsible.Trigger>
      <Collapsible.Content class="mt-1.5 overflow-hidden border-l-2 border-white/10 pl-3 text-sm leading-6 whitespace-pre-wrap break-words text-white/45">
        {props.block.thinking.trim()}
      </Collapsible.Content>
    </Collapsible>
  );
}

/** "Loading model… 3 s" while a reply hasn't started. */
function PhaseIndicator(props: { phase: TurnPhase; started: number }) {
  const [now, setNow] = createSignal(Date.now());
  const timer = setInterval(() => setNow(Date.now()), 1000);
  onCleanup(() => clearInterval(timer));
  const seconds = () => Math.max(0, Math.floor((now() - props.started) / 1000));
  return (
    <div class="flex items-center gap-2 text-sm text-white/45">
      <span class="size-1.5 animate-pulse rounded-full bg-accent" />
      <span>{t(PHASE_LABEL[props.phase])}</span>
      <Show when={seconds() >= 2}>
        <span class="tabular-nums text-white/30">{t("chat.seconds", { seconds: seconds() })}</span>
      </Show>
    </div>
  );
}

const compact = (n: number) => n.toLocaleString(locale(), { notation: "compact", maximumFractionDigits: 1 });

/** Model, speed, tokens and how full the context is; or why the reply stopped. */
function MessageFooter(props: { message: Message }) {
  const model = () => catalogModel(props.message.model);
  const context = () => model()?.loadedContext ?? undefined;
  const used = () => (props.message.usage ? props.message.usage.input + props.message.usage.output : 0);

  const parts = () => {
    const out: string[] = [];
    const name = model()?.name ?? props.message.model?.split("/").pop();
    if (name) out.push(name);
    const stats = props.message.stats;
    if (stats?.tps) out.push(t("chat.tps", { value: stats.tps.toLocaleString(locale()) }));
    if (props.message.usage?.output) out.push(t("chat.tokens", { count: compact(props.message.usage.output) }));
    if (stats?.ttft !== undefined) out.push(t("chat.ttft", { ms: stats.ttft }));
    if (used() && context()) {
      out.push(t("chat.context", { used: compact(used()), max: compact(context()!), pct: Math.round((used() / context()!) * 100) }));
    }
    return out;
  };

  return (
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-white/30">
      <Show when={props.message.stopReason === "cancelled"}>
        <span class="text-white/50">{t("chat.stopped")}</span>
      </Show>
      <Show when={props.message.stopReason === "max_tokens"}>
        <span class="text-amber-300/80">{t("chat.maxTokens")}</span>
      </Show>
      <For each={parts()}>{(part) => <span class="tabular-nums">{part}</span>}</For>
    </div>
  );
}
