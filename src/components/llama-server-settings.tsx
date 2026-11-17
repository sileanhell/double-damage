import { Button } from "@kobalte/core/button";
import { Collapsible } from "@kobalte/core/collapsible";
import { createSignal, Show } from "solid-js";
import { DEFAULT_IDLE_MINUTES, idleMinutes, instances, setIdleMinutes, unloadModel } from "../lib/gateway";
import { t } from "../lib/i18n";
import { ProviderPromptSettings } from "./agent-settings";
import { Dropdown } from "./dropdown";
import { ChevronDownIcon } from "./icons";
import { installLlamaServer, LLAMA_SERVER_TASK, llamaServer, uninstallLlamaServer } from "../lib/providers";
import { isCancelled, task } from "../lib/tasks";
import { TaskProgress } from "./task-progress";

const PRIMARY =
  "h-8 shrink-0 rounded-lg bg-accent px-3 text-[13px] font-medium text-accent-fg transition hover:brightness-110 disabled:opacity-60";
const DANGER =
  "h-8 shrink-0 rounded-lg border border-red-400/30 px-3 text-[13px] font-medium text-red-300/90 transition-colors hover:bg-red-500/10 hover:text-red-300 disabled:opacity-60";

/** llama-server provider card: installing it is what makes GGUF models runnable. */
export function LlamaServerSettings() {
  const [error, setError] = createSignal("");
  const [removing, setRemoving] = createSignal(false);
  const installing = () => task(LLAMA_SERVER_TASK);

  const install = async () => {
    setError("");
    try {
      await installLlamaServer();
    } catch (reason) {
      if (!isCancelled(reason)) setError(t("llama.installFailed", { reason: String(reason) }));
    }
  };

  const uninstall = async () => {
    setError("");
    setRemoving(true);
    try {
      // Running servers keep the executable locked.
      await Promise.all(instances().map((item) => unloadModel(item.id)));
      await uninstallLlamaServer();
    } catch (reason) {
      setError(t("llama.uninstallFailed", { reason: String(reason) }));
    } finally {
      setRemoving(false);
    }
  };

  return (
    <Collapsible>
      <div class="flex items-center justify-between gap-4">
        <Collapsible.Trigger class="group min-w-0 flex-1 rounded-lg text-left outline-none focus-visible:ring-2 focus-visible:ring-edge-active">
          <div class="flex items-center gap-2">
            <ChevronDownIcon class="size-4 shrink-0 -rotate-90 text-white/40 transition-transform group-data-expanded:rotate-0" />
            <span class="text-base font-medium">llama-server</span>
            <Show when={llamaServer()?.installed}>
              <span class="rounded-md bg-emerald-400/10 px-1.5 py-0.5 text-[11px] text-emerald-300/90">
                {t("llama.installed")}
                <Show when={llamaServer()?.version}>{(version) => ` · ${version()}`}</Show>
                <Show when={llamaServer()?.backend}>{(backend) => ` · ${backend()}`}</Show>
              </span>
            </Show>
          </div>
          <p class="mt-1 text-sm text-white/45">{t("llama.hint")}</p>
          <Show when={llamaServer()?.backend ? llamaServer() : undefined}>
            {(status) => (
              <p class="mt-1 text-xs text-white/35">
                {t(status().installed ? "llama.buildInstalled" : "llama.buildPlanned", {
                  backend: status().backend ?? "",
                  gpu: status().gpu ?? t("llama.noGpu"),
                })}
              </p>
            )}
          </Show>
        </Collapsible.Trigger>

        <Show when={llamaServer() && !installing()}>
          <Show
            when={llamaServer()?.installed}
            fallback={
              <Button onClick={install} class={PRIMARY}>
                {t("llama.install")}
              </Button>
            }
          >
            <Button onClick={uninstall} disabled={removing()} class={DANGER}>
              {t("llama.uninstall")}
            </Button>
          </Show>
        </Show>
      </div>

      <Show when={installing()}>
        {(running) => (
          <div class="mt-4">
            <TaskProgress task={running()} label={t("llama.installing", {
                version: running().label === LLAMA_SERVER_TASK ? "" : running().label,
              }).trim()} />
          </div>
        )}
      </Show>

      <Show when={error()}>
        <p class="mt-3 text-sm text-red-400">{error()}</p>
      </Show>

      <Collapsible.Content class="mt-5 border-t border-white/[0.06] pt-4">
        <Show when={llamaServer()?.installed}>
          <IdleSetting />
          <div class="mt-5 border-t border-white/[0.06] pt-5" />
        </Show>
        <ProviderPromptSettings provider="llama-server" />
      </Collapsible.Content>
    </Collapsible>
  );
}

const IDLE_CHOICES = [5, 10, 15, 30, 60, 0];

/** How long a model may sit unused before it's unloaded to free memory. */
function IdleSetting() {
  const options = () =>
    IDLE_CHOICES.map((minutes) => ({
      value: String(minutes),
      label: minutes === 0 ? t("llama.idleNever") : t("llama.idleMinutes", { minutes }),
    }));
  return (
    <div class="flex items-center justify-between gap-4">
      <div>
        <div class="text-sm font-medium text-white/85">{t("llama.idle")}</div>
        <p class="mt-1 text-xs text-white/40">{t("llama.idleHint", { minutes: DEFAULT_IDLE_MINUTES })}</p>
      </div>
      <Dropdown
        variant="outline"
        placement="bottom-start"
        title={t("llama.idle")}
        value={String(idleMinutes())}
        options={options()}
        onChange={(value) => setIdleMinutes(Number(value))}
      />
    </div>
  );
}
