import { Accordion } from "@kobalte/core/accordion";
import { Button } from "@kobalte/core/button";
import { Collapsible } from "@kobalte/core/collapsible";
import { TextField } from "@kobalte/core/text-field";
import { createEffect, createResource, createSignal, For, type JSX, on, onCleanup, Show } from "solid-js";
import { formatBytes, formatCount } from "../lib/format";
import { type HfModel, type HfModelFile, modelFiles, searchModels } from "../lib/huggingface";
import { t } from "../lib/i18n";
import {
  deleteLocalModel,
  downloadModel,
  downloadTaskId,
  hasLocalModel,
  importLocalModel,
  isProjector,
  type LocalModel,
  localModels,
  modelKey,
  modelSetSize,
} from "../lib/local-models";
import { forgetLaunchSettings } from "../lib/launch";
import { forgetModelDevice } from "../lib/model-device";
import { llamaServer, providersForFile, providersForRepo } from "../lib/providers";
import { isCancelled, task, tasksOfKind } from "../lib/tasks";
import { ChevronDownIcon, DownloadIcon, HeartIcon, SearchIcon, SlidersIcon, TrashIcon, UploadIcon } from "./icons";
import { FitBadge } from "./fit-badge";
import { LaunchSettingsPanel } from "./launch-settings";
import { ProviderBadges } from "./provider-badges";
import { TaskProgress } from "./task-progress";

const CARD = "rounded-2xl border border-white/10 bg-white/[0.03]";
const SMALL_BUTTON =
  "flex h-8 shrink-0 items-center gap-1.5 rounded-lg border border-white/10 bg-white/[0.03] px-2.5 text-[13px] text-white/85 transition-colors hover:border-white/20 hover:bg-white/[0.07] hover:text-white disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:border-white/10 disabled:hover:bg-white/[0.03] disabled:hover:text-white/85";

const basename = (path: string) => path.split("/").pop() ?? path;
/** A model counts as a file that some provider can run, so only those are offered for download. */
const runnable = (path: string) => providersForFile(basename(path)).length > 0;

/** "Local models" tab: what's in storage, manual import, and Hugging Face search/download. */
export function LocalModelsSettings() {
  return (
    <>
      <InstalledModels />
      <HuggingFaceSearch />
    </>
  );
}

function SectionHeader(props: { title: string; children?: JSX.Element }) {
  return (
    <div class="flex items-center justify-between gap-4">
      <h2 class="text-sm font-medium text-white/45">{props.title}</h2>
      {props.children}
    </div>
  );
}

// --- storage -------------------------------------------------------------------

function InstalledModels() {
  const [error, setError] = createSignal("");
  // An import has no label until a file is picked, so it stays hidden while the dialog is open.
  const running = () => tasksOfKind("download", "import").filter((item) => item.label);

  const pick = async () => {
    setError("");
    try {
      await importLocalModel({ filterName: t("models.filterGguf"), allFilesName: t("models.filterAll") });
    } catch (reason) {
      if (isCancelled(reason)) return;
      setError(reason === "exists" ? t("models.exists") : t("models.importFailed", { reason: String(reason) }));
    }
  };

  return (
    <section>
      <SectionHeader title={t("models.mine")}>
        <Button onClick={pick} class={SMALL_BUTTON}>
          <UploadIcon class="size-3.5" />
          {t("models.import")}
        </Button>
      </SectionHeader>

      <Show when={error()}>
        <p class="mt-3 text-sm text-red-400">{error()}</p>
      </Show>

      <div class={`mt-3 divide-y divide-white/[0.06] ${CARD}`}>
        <For each={running()}>
          {(item) => (
            <div class="px-5 py-3.5">
              <TaskProgress task={item} />
            </div>
          )}
        </For>
        <For each={localModels()}>{(model) => <InstalledModelRow model={model} />}</For>
        <Show when={localModels().length === 0 && running().length === 0}>
          <p class="px-5 py-6 text-center text-sm text-white/40">{t("models.empty")}</p>
        </Show>
      </div>
    </section>
  );
}

function InstalledModelRow(props: { model: LocalModel }) {
  const [busy, setBusy] = createSignal(false);
  const remove = async () => {
    setBusy(true);
    try {
      await deleteLocalModel(props.model.name);
      // A split model keeps its choice while other parts are still there.
      if (!localModels().some((model) => modelKey(model.name) === modelKey(props.model.name))) {
        forgetModelDevice(props.model.name);
        forgetLaunchSettings(props.model.name);
      }
    } catch (error) {
      console.error(error);
      setBusy(false);
    }
  };
  const launchable = () => !!llamaServer()?.installed && runnable(props.model.name) && !isProjector(props.model.name);
  return (
    <Collapsible>
      <div class="flex items-center gap-4 px-5 py-3">
        <div class="min-w-0 flex-1">
          <div class="truncate font-mono text-[13px] text-white/90" title={props.model.name}>
            {props.model.name}
          </div>
          <div class="mt-1 flex items-center gap-3">
            <span class="text-xs text-white/40 tabular-nums">{formatBytes(props.model.size)}</span>
            <ProviderBadges providers={providersForFile(props.model.name)} />
            <Show when={runnable(props.model.name) && !isProjector(props.model.name)}>
              <FitBadge size={modelSetSize(props.model.name)} />
            </Show>
          </div>
        </div>
        <Show when={launchable()}>
          <Collapsible.Trigger
            title={t("launch.open")}
            aria-label={t("launch.open")}
            class="flex size-8 shrink-0 items-center justify-center rounded-lg text-white/45 transition-colors hover:bg-white/10 hover:text-white data-expanded:bg-white/10 data-expanded:text-white"
          >
            <SlidersIcon class="size-4" />
          </Collapsible.Trigger>
        </Show>
        <Button
          title={t("common.delete")}
          aria-label={t("models.deleteNamed", { name: props.model.name })}
          disabled={busy()}
          onClick={remove}
          class="flex size-8 shrink-0 items-center justify-center rounded-lg text-white/40 transition-colors hover:bg-red-500/10 hover:text-red-300 disabled:opacity-50"
        >
          <TrashIcon class="size-4" />
        </Button>
      </div>
      <Show when={launchable()}>
        <Collapsible.Content class="border-t border-white/[0.06] bg-black/10 px-5 py-4">
          <LaunchSettingsPanel model={props.model.name} />
        </Collapsible.Content>
      </Show>
    </Collapsible>
  );
}

// --- Hugging Face ----------------------------------------------------------------

function HuggingFaceSearch() {
  const [input, setInput] = createSignal("");
  const [query, setQuery] = createSignal("");

  // Debounce typing so every keystroke doesn't hit the Hub.
  createEffect(
    on(input, (value) => {
      const timer = setTimeout(() => setQuery(value.trim()), 350);
      onCleanup(() => clearTimeout(timer));
    }),
  );

  let controller: AbortController | undefined;
  const [results] = createResource(query, (value) => {
    controller?.abort();
    controller = new AbortController();
    return searchModels(value, controller.signal);
  });

  return (
    <section class="mt-8">
      <SectionHeader title="Hugging Face" />
      <TextField value={input()} onChange={setInput} class="relative mt-3">
        <SearchIcon class="pointer-events-none absolute top-1/2 left-3.5 size-4 -translate-y-1/2 text-white/40" />
        <TextField.Input
          type="search"
          aria-label={t("models.searchLabel")}
          placeholder={t("models.searchPlaceholder")}
          autocomplete="off"
          spellcheck={false}
          class="h-10 w-full rounded-xl border border-white/10 bg-white/[0.03] pr-4 pl-10 text-sm text-white outline-none transition-colors placeholder:text-white/35 focus:border-white/25 [&::-webkit-search-cancel-button]:hidden"
        />
      </TextField>

      <div class={`mt-3 overflow-hidden ${CARD}`}>
        <Show
          when={!results.error}
          fallback={<p class="px-5 py-6 text-center text-sm text-red-400">{t("models.searchFailed")}</p>}
        >
          <Show
            when={results.latest?.length || results.loading}
            fallback={<p class="px-5 py-6 text-center text-sm text-white/40">{t("search.nothing")}</p>}
          >
            <Show when={results.loading && !results.latest}>
              <p class="px-5 py-6 text-center text-sm text-white/40">{t("models.searching")}</p>
            </Show>
            <Accordion collapsible class="divide-y divide-white/[0.06]">
              <For each={results.latest ?? []}>{(model) => <SearchResult model={model} />}</For>
            </Accordion>
          </Show>
        </Show>
      </div>
    </section>
  );
}

function SearchResult(props: { model: HfModel }) {
  const providers = () => providersForRepo(props.model);
  return (
    <Accordion.Item value={props.model.id}>
      <Accordion.Header>
        <Accordion.Trigger class="group flex w-full items-center gap-4 px-5 py-3 text-left outline-none transition-colors hover:bg-white/[0.03] focus-visible:bg-white/[0.05] data-expanded:bg-white/[0.03]">
          <div class="min-w-0 flex-1">
            <div class="truncate text-sm text-white/90">{props.model.id}</div>
            <div class="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-white/40">
              <span class="flex items-center gap-1 tabular-nums">
                <DownloadIcon class="size-3" />
                {formatCount(props.model.downloads)}
              </span>
              <span class="flex items-center gap-1 tabular-nums">
                <HeartIcon class="size-3" />
                {formatCount(props.model.likes)}
              </span>
              <Show when={props.model.pipelineTag}>{(tag) => <span>{tag()}</span>}</Show>
              <ProviderBadges providers={providers()} />
            </div>
          </div>
          <ChevronDownIcon class="size-4 shrink-0 text-white/40 transition-transform group-data-expanded:rotate-180" />
        </Accordion.Trigger>
      </Accordion.Header>
      <Accordion.Content class="border-t border-white/[0.06] bg-black/10">
        <Show
          when={providers().length > 0}
          fallback={<p class="px-5 py-4 text-sm text-white/40">{t("models.unsupportedRepo")}</p>}
        >
          <RepoFiles repo={props.model.id} />
        </Show>
      </Accordion.Content>
    </Accordion.Item>
  );
}

/** Loaded only once the result is expanded (Accordion unmounts closed content). */
function RepoFiles(props: { repo: string }) {
  const [files] = createResource(
    () => props.repo,
    (repo) => modelFiles(repo, runnable),
  );
  return (
    <Show
      when={!files.error}
      fallback={<p class="px-5 py-4 text-sm text-red-400">{t("models.filesFailed")}</p>}
    >
      <Show when={files()} fallback={<p class="px-5 py-4 text-sm text-white/40">{t("models.loadingFiles")}</p>}>
        {(list) => (
          <Show
            when={list().length > 0}
            fallback={<p class="px-5 py-4 text-sm text-white/40">{t("models.noFiles")}</p>}
          >
            <ul class="py-1.5">
              <For each={list()}>{(file) => <RepoFileRow repo={props.repo} file={file} />}</For>
            </ul>
          </Show>
        )}
      </Show>
    </Show>
  );
}

function RepoFileRow(props: { repo: string; file: HfModelFile }) {
  const [error, setError] = createSignal("");
  const running = () => task(downloadTaskId(props.repo, props.file.files));
  const downloaded = () => props.file.files.every((path) => hasLocalModel(basename(path)));

  const download = async () => {
    setError("");
    try {
      await downloadModel(props.repo, props.file.files, props.file.size || undefined, props.file.name);
    } catch (reason) {
      if (!isCancelled(reason)) setError(t("models.downloadFailed", { reason: String(reason) }));
    }
  };

  return (
    <li class="px-5 py-2">
      <div class="flex items-center gap-4">
        <div class="min-w-0 flex-1">
          <div class="truncate font-mono text-[13px] text-white/85" title={props.file.files.join("\n")}>
            {props.file.name}
          </div>
          <div class="mt-0.5 flex items-center gap-2 text-xs text-white/40 tabular-nums">
            <span>
              {formatBytes(props.file.size)}
              <Show when={props.file.files.length > 1}>
                {` · ${t("models.parts", { count: props.file.files.length })}`}
              </Show>
            </span>
            <Show when={!isProjector(props.file.name)}>
              <FitBadge size={props.file.size} />
            </Show>
          </div>
        </div>
        {/* Progress itself is shown under "My models". */}
        <Show
          when={running() || !downloaded()}
          fallback={<span class="shrink-0 text-xs text-emerald-300/80">{t("models.downloaded")}</span>}
        >
          <Button onClick={download} disabled={!!running()} class={SMALL_BUTTON}>
            <DownloadIcon class="size-3.5" />
            {running() ? t("models.downloading") : t("models.download")}
          </Button>
        </Show>
      </div>
      <Show when={error()}>
        <p class="mt-1.5 text-xs text-red-400">{error()}</p>
      </Show>
    </li>
  );
}
