import { Button } from "@kobalte/core/button";
import { Collapsible } from "@kobalte/core/collapsible";
import { TextField } from "@kobalte/core/text-field";
import { createEffect, createSignal, For, type JSX, Match, Show, Switch } from "solid-js";
import { approvalNote, awaitingApproval } from "../lib/approval";
import { answerApproval, toolRun } from "../lib/chat";
import type { ToolMeta, ToolResultBlock, ToolUseBlock } from "../lib/chat-types";
import { type MessageKey, t } from "../lib/i18n";
import { ChevronDownIcon } from "./icons";

export type ToolResult = { block: ToolResultBlock; meta?: ToolMeta };

type Input = Record<string, unknown>;
const str = (value: unknown) => (typeof value === "string" ? value : "");
const shortPath = (path: string) => path.split("/").filter(Boolean).slice(-3).join("/") || path;

/** One line saying what the call is about: the file, the command, the pattern. */
function summary(call: ToolUseBlock): string {
  const input = (call.input ?? {}) as Input;
  switch (call.name) {
    case "bash":
      // The command itself, never the model's own description of it.
      return str(input.command).replace(/\s+/g, " ").trim();
    case "glob":
    case "grep":
      return str(input.pattern) + (input.path ? ` · ${shortPath(str(input.path))}` : "");
    case "edit":
      if (input.patch) return [...str(input.patch).matchAll(/\*\*\* (?:Update|Add|Delete) File: (.+)/g)].map((m) => shortPath(m[1])).join(", ");
      return shortPath(str(input.path));
    default:
      return shortPath(str(input.path)) || "";
  }
}

const resultText = (block: ToolResultBlock) =>
  typeof block.content === "string" ? block.content : block.content.map((part) => part.text).join("\n");

const PRE = "thin-scroll max-h-80 overflow-auto rounded-lg bg-black/30 px-3 py-2 font-mono text-[12px] leading-5 whitespace-pre text-white/75";
/** Wraps everything: nothing in a command may hide past the right edge. */
const COMMAND = "thin-scroll max-h-80 overflow-auto rounded-lg bg-black/30 px-3 py-2 font-mono text-[12px] leading-5 whitespace-pre-wrap break-all text-white/85";

/** A tool call in the chat: what it does, its status, its result; asks for approval when needed. */
export function ToolCard(props: { call: ToolUseBlock; result?: ToolResult; live: boolean }) {
  const run = () => toolRun(props.call.id);
  const asking = () => awaitingApproval(props.call.id);
  const meta = () => props.result?.meta;
  const label = () => {
    const key = `tool.${props.call.name}` as MessageKey;
    const text = t(key);
    return text === key ? t("tool.unknown", { name: props.call.name }) : text;
  };
  const [open, setOpen] = createSignal(false);
  // Open while it needs the user; edits show their diff right away.
  createEffect(() => {
    if (asking()) setOpen(true);
  });

  const status = (): { text: string; tone: string } | undefined => {
    if (asking()) return { text: t("tool.waiting"), tone: "text-amber-300" };
    if (run()?.status === "running") return { text: t("tool.running"), tone: "text-accent animate-pulse" };
    const m = meta();
    if (m?.denied) return { text: m.denied.reason ? t("tool.deniedWith", { reason: m.denied.reason }) : t("tool.denied"), tone: "text-red-300/80" };
    if (m?.cancelled) return { text: t("tool.cancelled"), tone: "text-white/45" };
    if (m?.timedOut) return { text: t("tool.timedOut"), tone: "text-red-300/80" };
    if (m?.exitCode !== undefined && m.exitCode !== null && m.exitCode !== 0) return { text: t("tool.exit", { code: m.exitCode }), tone: "text-red-300/80" };
    if (props.result?.block.is_error) return { text: t("tool.failed"), tone: "text-red-300/80" };
    return undefined;
  };

  return (
    <Collapsible
      open={open()}
      onOpenChange={setOpen}
      class="rounded-xl border bg-white/[0.02] text-sm transition-colors"
      classList={{ "border-amber-300/30": asking(), "border-white/10": !asking() }}
    >
      <Collapsible.Trigger class="group flex w-full min-w-0 items-center gap-2 px-3 py-2 text-left outline-none">
        <span class="shrink-0 font-medium text-white/80">{label()}</span>
        <span class="min-w-0 flex-1 truncate font-mono text-[12px] text-white/45" title={summary(props.call)}>
          {summary(props.call)}
        </span>
        <Show when={status()}>{(s) => <span class={`shrink-0 truncate text-xs ${s().tone}`}>{s().text}</span>}</Show>
        <ChevronDownIcon class="size-3.5 shrink-0 text-white/40 transition-transform group-data-expanded:rotate-180" />
      </Collapsible.Trigger>
      <Collapsible.Content class="space-y-2 border-t border-white/[0.06] px-3 py-2.5">
        <Body call={props.call} result={props.result} />
        <Show when={asking()}>
          <Approval id={props.call.id} />
        </Show>
      </Collapsible.Content>
    </Collapsible>
  );
}

function Body(props: { call: ToolUseBlock; result?: ToolResult }) {
  const input = () => (props.call.input ?? {}) as Input;
  const run = () => toolRun(props.call.id);
  const result = () => props.result;
  return (
    <Switch fallback={<Generic call={props.call} result={props.result} />}>
      <Match when={props.call.name === "bash"}>
        <Show when={str(input().description)}>
          {(description) => (
            <p class="text-xs text-white/45">
              <span class="text-white/30">{t("tool.byModel")}: </span>
              {description()}
            </p>
          )}
        </Show>
        <pre class={COMMAND}>
          <span class="text-white/40">$ </span>
          {str(input().command)}
        </pre>
        <Show when={run()?.output || (result() && resultText(result()!.block))}>
          {(text) => <pre class={PRE}>{text()}</pre>}
        </Show>
      </Match>
      <Match when={(props.call.name === "edit" || props.call.name === "write") && result()?.meta?.diff}>
        <Diff text={result()!.meta!.diff!} />
      </Match>
      <Match when={(props.call.name === "edit" || props.call.name === "write") && !result()}>
        <Proposed call={props.call} />
      </Match>
    </Switch>
  );
}

/** Before an edit has run (waiting for approval): what it means to change. */
function Proposed(props: { call: ToolUseBlock }) {
  const input = () => (props.call.input ?? {}) as Input;
  const lines = (text: string, sign: string) => text.split("\n").map((line) => `${sign}${line}`).join("\n");
  const text = () => {
    const i = input();
    if (props.call.name === "write") return lines(str(i.content), "+");
    if (i.old_string !== undefined) return `${lines(str(i.old_string), "-")}\n${lines(str(i.new_string), "+")}`;
    if (i.patch) return str(i.patch);
    if (i.blocks) return str(i.blocks);
    if (Array.isArray(i.edits)) {
      return (i.edits as Input[])
        .map((e) => {
          const lines = Array.isArray(e.lines) ? (e.lines as string[]).join("\n") : str(e.lines);
          const where = `@@ ${str(e.op)} ${str(e.pos)}${e.end ? `..${str(e.end)}` : ""} @@`;
          return e.op === "delete" ? where : `${where}\n${lines.split("\n").map((l) => `+${l}`).join("\n")}`;
        })
        .join("\n");
    }
    return JSON.stringify(i, null, 2);
  };
  return <Diff text={text()} />;
}

function Generic(props: { call: ToolUseBlock; result?: ToolResult }) {
  return (
    <>
      <pre class={COMMAND}>{JSON.stringify(props.call.input, null, 2)}</pre>
      <Show when={props.result}>{(result) => <pre class={PRE}>{resultText(result().block)}</pre>}</Show>
    </>
  );
}

const LINE_TONE = (line: string) =>
  line.startsWith("+++") || line.startsWith("---")
    ? "text-white/40"
    : line.startsWith("@@")
      ? "text-sky-300/70"
      : line.startsWith("+")
        ? "bg-emerald-400/10 text-emerald-200"
        : line.startsWith("-")
          ? "bg-red-400/10 text-red-200"
          : "text-white/60";

function Diff(props: { text: string }) {
  return (
    <div class={`${COMMAND} px-0`}>
      <For each={props.text.split("\n")}>{(line) => <div class={`px-3 ${LINE_TONE(line)}`}>{line || " "}</div>}</For>
    </div>
  );
}

const BUTTON = "h-7 rounded-lg px-2.5 text-[13px] transition-colors";

function Approval(props: { id: string }): JSX.Element {
  const [reason, setReason] = createSignal("");
  return (
    <div class="space-y-2 pt-1">
      <Show when={approvalNote(props.id)}>
        {(paths) => (
          <p class="rounded-lg border border-amber-300/30 bg-amber-400/10 px-2.5 py-1.5 text-xs text-amber-100">
            {t("tool.sensitive", { paths: paths() })}
          </p>
        )}
      </Show>
      <p class="text-xs text-amber-200/80">{t("tool.ask")}</p>
      <div class="flex flex-wrap items-center gap-2">
        <Button class={`${BUTTON} bg-accent font-medium text-accent-fg hover:brightness-110`} onClick={() => answerApproval(props.id, { allow: true })}>
          {t("tool.allow")}
        </Button>
        <Button
          class={`${BUTTON} border border-white/10 text-white/80 hover:bg-white/[0.07]`}
          onClick={() => answerApproval(props.id, { allow: true }, true)}
        >
          {t("tool.allowAlways")}
        </Button>
        <TextField value={reason()} onChange={setReason} class="min-w-40 flex-1">
          <TextField.Input
            placeholder={t("tool.denyReason")}
            aria-label={t("tool.denyReason")}
            onKeyDown={(event) => event.key === "Enter" && answerApproval(props.id, { allow: false, reason: reason() })}
            class="h-7 w-full rounded-lg border border-white/10 bg-black/30 px-2.5 text-[13px] text-white/80 outline-none placeholder:text-white/30 focus:border-white/25"
          />
        </TextField>
        <Button
          class={`${BUTTON} border border-red-400/30 text-red-300/90 hover:bg-red-500/10`}
          onClick={() => answerApproval(props.id, { allow: false, reason: reason() })}
        >
          {t("tool.deny")}
        </Button>
      </div>
    </div>
  );
}
