import { Button } from "@kobalte/core/button";
import { createSignal, Show } from "solid-js";
import { BASH_TASK, bashStatus, installBash, latestBash, uninstallBash } from "../lib/bash";
import { t } from "../lib/i18n";
import { isCancelled, task } from "../lib/tasks";
import { TaskProgress } from "./task-progress";

const PRIMARY =
  "h-8 shrink-0 rounded-lg bg-accent px-3 text-[13px] font-medium text-accent-fg transition hover:brightness-110 disabled:opacity-60";
const SECONDARY =
  "h-8 shrink-0 rounded-lg border border-white/10 bg-white/[0.03] px-3 text-[13px] text-white/85 transition-colors hover:border-white/20 hover:bg-white/[0.07] hover:text-white disabled:opacity-50";
const DANGER =
  "h-8 shrink-0 rounded-lg border border-red-400/30 px-3 text-[13px] font-medium text-red-300/90 transition-colors hover:bg-red-500/10 hover:text-red-300 disabled:opacity-60";

/** Bash module card: installing it gives the model the `bash` tool. */
export function BashSettings() {
  const [error, setError] = createSignal("");
  const [removing, setRemoving] = createSignal(false);
  const installing = () => task(BASH_TASK);
  const update = () => {
    const latest = latestBash();
    const current = bashStatus()?.version;
    return latest && current && latest !== current ? latest : undefined;
  };

  const install = async () => {
    setError("");
    try {
      await installBash();
    } catch (reason) {
      if (!isCancelled(reason)) setError(t("bash.installFailed", { reason: String(reason) }));
    }
  };

  const uninstall = async () => {
    setError("");
    setRemoving(true);
    try {
      await uninstallBash();
    } catch (reason) {
      setError(t("bash.uninstallFailed", { reason: String(reason) }));
    } finally {
      setRemoving(false);
    }
  };

  return (
    <div>
      <div class="flex items-center justify-between gap-4">
        <div class="min-w-0">
          <div class="flex items-center gap-2">
            <span class="text-base font-medium">Bash</span>
            <Show when={bashStatus()?.installed}>
              <span class="rounded-md bg-emerald-400/10 px-1.5 py-0.5 text-[11px] text-emerald-300/90">
                {t("bash.installed")}
                <Show when={bashStatus()?.version}>{(version) => ` · ${version()}`}</Show>
              </span>
            </Show>
          </div>
          <p class="mt-1 text-sm text-white/45">{t("bash.hint")}</p>
          <p class="mt-1 text-xs text-white/35">{t("bash.details")}</p>
        </div>

        <Show when={bashStatus() && !installing()}>
          <div class="flex shrink-0 gap-2">
            <Show
              when={bashStatus()?.installed}
              fallback={
                <Button onClick={install} class={PRIMARY}>
                  {t("bash.install")}
                </Button>
              }
            >
              <Show when={update()}>
                {(version) => (
                  <Button onClick={install} class={SECONDARY}>
                    {t("bash.update", { version: version() })}
                  </Button>
                )}
              </Show>
              <Button onClick={uninstall} disabled={removing()} class={DANGER}>
                {t("bash.uninstall")}
              </Button>
            </Show>
          </div>
        </Show>
      </div>

      <Show when={installing()}>
        {(running) => (
          <div class="mt-4">
            <TaskProgress task={running()} label={t("bash.installing", { version: running().label === BASH_TASK ? "" : running().label }).trim()} />
          </div>
        )}
      </Show>

      <Show when={error()}>
        <p class="mt-3 text-sm text-red-400">{error()}</p>
      </Show>
    </div>
  );
}
