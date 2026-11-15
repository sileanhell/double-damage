import { Button } from "@kobalte/core/button";
import { createSignal, Show } from "solid-js";
import { t } from "../lib/i18n";
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
      await uninstallLlamaServer();
    } catch (reason) {
      setError(t("llama.uninstallFailed", { reason: String(reason) }));
    } finally {
      setRemoving(false);
    }
  };

  return (
    <div>
      <div class="flex items-center justify-between gap-4">
        <div class="min-w-0">
          <div class="flex items-center gap-2">
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
        </div>

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
    </div>
  );
}
