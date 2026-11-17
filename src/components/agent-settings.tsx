import { TextField } from "@kobalte/core/text-field";
import { Tooltip } from "@kobalte/core/tooltip";
import { createEffect, createMemo, createSignal, For, type JSX, on, onCleanup, Show } from "solid-js";
import {
  EDIT_METHODS,
  type EditMethod,
  autoCompact,
  COMPACT_THRESHOLDS,
  type CompactThreshold,
  compactThreshold,
  editMethod,
  maxSteps,
  projectInstructionsEnabled,
  STEP_LIMITS,
  setAutoCompact,
  setCompactThreshold,
  setMaxSteps,
  promptAddition,
  setEditMethod,
  setProjectInstructionsEnabled,
  setPromptAddition,
} from "../lib/agent-settings";
import { bashAvailable } from "../lib/bash";
import { type MessageKey, t } from "../lib/i18n";
import { PLACEHOLDERS, type PromptContext, promptBlocks } from "../lib/system-prompt";
import { Dropdown } from "./dropdown";
import { Toggle } from "./toggle";

const METHOD_LABEL: Record<EditMethod, MessageKey> = {
  str_replace: "agent.edit.str_replace",
  hashline: "agent.edit.hashline",
  apply_patch: "agent.edit.apply_patch",
  search_replace: "agent.edit.search_replace",
};
const METHOD_HINT: Record<EditMethod, MessageKey> = {
  str_replace: "agent.edit.str_replaceHint",
  hashline: "agent.edit.hashlineHint",
  apply_patch: "agent.edit.apply_patchHint",
  search_replace: "agent.edit.search_replaceHint",
};

const SAVE_DELAY = 300;

function Section(props: { title: string; children: JSX.Element }) {
  return (
    <section class="mt-8 first:mt-0">
      <h2 class="text-sm font-medium text-white/45">{props.title}</h2>
      <div class="mt-3 rounded-2xl border border-white/10 bg-white/[0.03] p-5">{props.children}</div>
    </section>
  );
}

function EditMethodSelect() {
  const options = () => EDIT_METHODS.map((value) => ({ value, label: t(METHOD_LABEL[value]) }));
  return (
    <div>
      <div class="flex items-center justify-between gap-4">
        <div>
          <div class="text-base font-medium">{t("agent.edit.title")}</div>
          <div class="mt-1 text-sm text-white/45">{t("agent.edit.hint")}</div>
        </div>
        <Dropdown
          variant="outline"
          placement="bottom-start"
          title={t("agent.edit.title")}
          value={editMethod()}
          options={options()}
          onChange={setEditMethod}
        />
      </div>
      <p class="mt-3 rounded-lg border border-white/10 bg-black/20 px-3 py-2 text-sm text-white/60">
        {t(METHOD_HINT[editMethod()])}
      </p>
    </div>
  );
}

function StepLimitSelect() {
  const options = () =>
    STEP_LIMITS.map((limit) => ({
      value: limit === null ? "none" : String(limit),
      label: limit === null ? t("agent.steps.none") : t("agent.steps.count", { count: limit }),
    }));
  return (
    <div class="flex items-center justify-between gap-4">
      <div>
        <div class="text-base font-medium">{t("agent.steps.title")}</div>
        <div class="mt-1 text-sm text-white/45">{t("agent.steps.hint")}</div>
      </div>
      <Dropdown
        variant="outline"
        placement="bottom-start"
        title={t("agent.steps.title")}
        value={maxSteps() === null ? "none" : String(maxSteps())}
        options={options()}
        onChange={(value) => setMaxSteps(value === "none" ? null : (Number(value) as (typeof STEP_LIMITS)[number]))}
      />
    </div>
  );
}

/** A `{{name}}` placeholder: a chip whose tooltip says what it becomes. */
function PlaceholderChip(props: { name: string }) {
  const info = () => PLACEHOLDERS[props.name];
  return (
    <Show when={info()} fallback={`{{${props.name}}}`}>
      {(placeholder) => (
        <Tooltip openDelay={150} gutter={6}>
          <Tooltip.Trigger
            as="span"
            tabIndex={0}
            class="cursor-help rounded-md border border-accent/40 bg-accent/15 px-1 py-px text-accent outline-none focus-visible:ring-2 focus-visible:ring-edge-active"
          >
            {`{{${props.name}}}`}
          </Tooltip.Trigger>
          <Tooltip.Portal>
            <Tooltip.Content class="z-50 max-w-sm rounded-xl border border-white/10 bg-white/[0.06] p-3 text-white shadow-2xl backdrop-blur-2xl transition-opacity duration-150 data-closed:opacity-0 data-expanded:opacity-100">
              <div class="text-sm text-white/85">{t(placeholder().description)}</div>
              <div class="mt-2 text-xs text-white/40">{t("agent.prompt.example")}</div>
              <pre class="mt-1 font-mono text-xs whitespace-pre-wrap text-white/70">{placeholder().example}</pre>
            </Tooltip.Content>
          </Tooltip.Portal>
        </Tooltip>
      )}
    </Show>
  );
}

/** Block text with every placeholder rendered as a chip. */
function TemplateText(props: { text: string }) {
  // split with a capture group: even items are text, odd items are placeholder names
  const parts = () => props.text.split(/\{\{(\w+)\}\}/g);
  return (
    <pre class="thin-scroll mt-2 overflow-x-auto rounded-lg border border-white/10 bg-black/30 p-3 font-mono text-[12.5px] leading-5 whitespace-pre-wrap text-white/75 select-text">
      <For each={parts()}>{(part, index) => (index() % 2 === 1 ? <PlaceholderChip name={part} /> : part)}</For>
    </pre>
  );
}

function ProviderPrompt(props: { provider: string }) {
  const exampleCtx = (): PromptContext => {
    const cwd = PLACEHOLDERS.cwd.example;
    return {
      provider: props.provider,
      model: PLACEHOLDERS.model.example,
      workspace: [cwd],
      tools: ["read_file", "write", "edit", "list_dir", "glob", "grep", ...(bashAvailable() ? ["bash"] : [])],
      toolFormat: "native",
      editMethod: editMethod(),
      date: new Date().toISOString().slice(0, 10),
      os: "Windows",
      instructions: [{ file: `${cwd}/AGENTS.md`, content: "…" }],
    };
  };
  const blocks = createMemo(() => promptBlocks(exampleCtx()));

  // The addition is edited locally and saved shortly after typing stops.
  const [draft, setDraft] = createSignal(promptAddition(props.provider));
  let timer: number | undefined;
  let pending: { provider: string; text: string } | undefined;
  const flush = () => {
    clearTimeout(timer);
    if (pending) setPromptAddition(pending.provider, pending.text);
    pending = undefined;
  };
  createEffect(
    on(
      () => props.provider,
      (provider) => {
        flush();
        setDraft(promptAddition(provider));
      },
      { defer: true },
    ),
  );
  onCleanup(flush);
  const edit = (text: string) => {
    setDraft(text);
    pending = { provider: props.provider, text };
    clearTimeout(timer);
    timer = window.setTimeout(flush, SAVE_DELAY);
  };

  return (
    <div>
      <div class="text-sm text-white/45">{t("agent.prompt.baseHint")}</div>
      <div class="mt-4 space-y-4">
        <For each={blocks()}>
          {(block) => (
            <div>
              <div class="text-sm font-medium text-white/80">{block.title}</div>
              <TemplateText text={block.text} />
            </div>
          )}
        </For>
      </div>

      <TextField value={draft()} onChange={edit} class="mt-6">
        <TextField.Label class="block text-base font-medium">{t("agent.prompt.addition")}</TextField.Label>
        <TextField.Description class="mt-1 text-sm text-white/45">{t("agent.prompt.additionHint")}</TextField.Description>
        <TextField.TextArea
          autoResize
          rows={4}
          onBlur={flush}
          placeholder={t("agent.prompt.additionPlaceholder")}
          class="thin-scroll mt-3 max-h-96 min-h-24 w-full resize-none rounded-lg border border-white/10 bg-black/30 px-3 py-2 font-mono text-[13px] leading-5 text-white/85 outline-none placeholder:text-white/30 focus:border-white/25"
        />
      </TextField>
    </div>
  );
}

/** A provider's system prompt: the read-only base blocks plus the user's addition. */
export function ProviderPromptSettings(props: { provider: string }) {
  return (
    <div>
      <div class="text-base font-medium">{t("agent.prompt.section")}</div>
      <p class="mt-1 text-sm text-white/45">{t("agent.prompt.hint")}</p>
      <div class="mt-5">
        <ProviderPrompt provider={props.provider} />
      </div>
    </div>
  );
}

/** Agent behaviour: how the model edits files, project instructions, step limit and compaction. */
export function AgentSettings() {
  return (
    <>
      <Section title={t("agent.tools")}>
        <EditMethodSelect />
        <div class="mt-5 border-t border-white/10 pt-5">
          <Toggle
            label={t("agent.instructions")}
            description={t("agent.instructionsHint")}
            checked={projectInstructionsEnabled()}
            onChange={setProjectInstructionsEnabled}
          />
        </div>
        <div class="mt-5 border-t border-white/10 pt-5">
          <StepLimitSelect />
        </div>
      </Section>
      <Section title={t("agent.compact.section")}>
        <Toggle
          label={t("agent.compact.auto")}
          description={t("agent.compact.autoHint")}
          checked={autoCompact()}
          onChange={setAutoCompact}
        />
        <div class="mt-5 flex items-center justify-between gap-4 border-t border-white/10 pt-5" classList={{ "opacity-50": !autoCompact() }}>
          <div>
            <div class="text-base font-medium">{t("agent.compact.threshold")}</div>
            <div class="mt-1 text-sm text-white/45">{t("agent.compact.thresholdHint")}</div>
          </div>
          <Dropdown
            variant="outline"
            placement="bottom-start"
            title={t("agent.compact.threshold")}
            value={String(compactThreshold())}
            options={COMPACT_THRESHOLDS.map((value) => ({ value: String(value), label: `${value}%` }))}
            onChange={(value) => setCompactThreshold(Number(value) as CompactThreshold)}
          />
        </div>
      </Section>
    </>
  );
}
