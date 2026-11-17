import { Button } from "@kobalte/core/button";
import { Dialog } from "@kobalte/core/dialog";
import { invoke } from "@tauri-apps/api/core";
import { createResource, createSignal, Show } from "solid-js";
import { closeDb, reopenDb } from "../lib/db";
import { instances, unloadModel } from "../lib/gateway";
import { t } from "../lib/i18n";
import { flushStorage } from "../lib/storage";
import { tasksOfKind } from "../lib/tasks";

type DataDirInfo = { path: string; isCustom: boolean };

const SECONDARY =
  "h-8 shrink-0 rounded-lg border border-white/10 bg-white/[0.03] px-3 text-[13px] text-white/85 transition-colors hover:border-white/20 hover:bg-white/[0.07] hover:text-white disabled:pointer-events-none disabled:opacity-50";
const DANGER =
  "h-8 shrink-0 rounded-lg border border-red-400/30 px-3 text-[13px] text-red-300/90 transition-colors hover:bg-red-500/10 hover:text-red-300 disabled:pointer-events-none disabled:opacity-50";

/**
 * Where the app keeps everything. A custom folder can be picked or removed (back to the folder
 * next to dd.exe); either way all data moves along and the window is recreated.
 */
export function DataDirSettings() {
  const [info] = createResource(() => invoke<DataDirInfo>("data_dir_info").catch(() => undefined));
  // A path = move to that custom folder; `null` = remove the custom folder; undefined = closed.
  const [target, setTarget] = createSignal<string | null | undefined>();
  const [error, setError] = createSignal("");
  const [applying, setApplying] = createSignal(false);

  // Recreating the window mid-download would leave files in the old folder.
  const busy = () => tasksOfKind("download", "import", "llama-server").length > 0;

  const pick = async () => {
    setError("");
    const path = await invoke<string | null>("data_dir_pick", { title: t("dataDir.pickTitle") }).catch((reason) => {
      setError(String(reason));
      return null;
    });
    if (path && path !== info()?.path) setTarget(path);
  };

  const apply = async () => {
    setApplying(true);
    setError("");
    try {
      await flushStorage();
      // Open files can't move: stop the model servers and close the chat database first.
      await Promise.all(instances().map((item) => unloadModel(item.id)));
      await closeDb();
      // On success this window is replaced by a new one, so the call normally never returns here;
      // it does when nothing had to move (the same folder), and then the page carries on.
      await invoke("data_dir_apply", { path: target() });
      reopenDb();
      setTarget(undefined);
    } catch (reason) {
      reopenDb();
      setError(t("dataDir.failed", { reason: String(reason) }));
      setTarget(undefined);
    } finally {
      setApplying(false);
    }
  };

  return (
    <div>
      <div class="text-base font-medium">{t("dataDir.title")}</div>
      <p class="mt-1 text-sm text-white/45">{t(info()?.isCustom ? "dataDir.hintCustom" : "dataDir.hint")}</p>

      <div class="mt-4 flex items-center gap-2">
        <div
          class="h-8 min-w-0 flex-1 truncate rounded-lg border border-white/10 bg-black/30 px-3 font-mono text-[13px] leading-8 text-white/80 select-text"
          title={info()?.path}
        >
          {info()?.path ?? "…"}
        </div>
        <Button onClick={pick} disabled={!info() || busy()} class={SECONDARY}>
          {t("dataDir.change")}
        </Button>
        <Show when={info()?.isCustom}>
          <Button onClick={() => setTarget(null)} disabled={busy()} class={DANGER}>
            {t("dataDir.remove")}
          </Button>
        </Show>
      </div>

      <Show when={busy()}>
        <p class="mt-2 text-xs text-amber-300/80">{t("dataDir.busy")}</p>
      </Show>
      <Show when={error()}>
        <p class="mt-2 text-sm text-red-400">{error()}</p>
      </Show>

      <Dialog open={target() !== undefined} onOpenChange={(open) => !open && !applying() && setTarget(undefined)}>
        <Dialog.Portal>
          <Dialog.Overlay class="fixed inset-0 z-50 bg-black/60 backdrop-blur-md" />
          <div class="fixed inset-0 z-50 flex justify-center px-6 pt-[20vh]">
            <Dialog.Content class="h-fit w-full max-w-md rounded-2xl border border-white/10 bg-white/[0.06] p-5 text-white shadow-2xl outline-none backdrop-blur-2xl">
              <Show
                when={target()}
                fallback={
                  <>
                    <Dialog.Title class="text-sm font-medium text-white/90">{t("dataDir.removeTitle")}</Dialog.Title>
                    <Dialog.Description class="mt-2 text-sm text-white/55">{t("dataDir.removeText")}</Dialog.Description>
                  </>
                }
              >
                {(path) => (
                  <>
                    <Dialog.Title class="text-sm font-medium text-white/90">{t("dataDir.confirmTitle")}</Dialog.Title>
                    <Dialog.Description class="mt-2 text-sm text-white/55">{t("dataDir.confirmText")}</Dialog.Description>
                    <div
                      class="mt-3 truncate rounded-lg bg-black/30 px-3 py-2 font-mono text-[13px] text-white/80"
                      title={path()}
                    >
                      {path()}
                    </div>
                  </>
                )}
              </Show>
              <p class="mt-3 text-xs text-white/40">{t("dataDir.moveHint")}</p>

              <div class="mt-5 flex justify-end gap-2">
                <Button
                  onClick={() => setTarget(undefined)}
                  disabled={applying()}
                  class="h-8 rounded-lg px-3 text-[13px] text-white/60 transition-colors hover:bg-white/[0.06] hover:text-white"
                >
                  {t("common.cancel")}
                </Button>
                <Button
                  onClick={apply}
                  disabled={applying()}
                  class="h-8 rounded-lg bg-accent px-3 text-[13px] font-medium text-accent-fg transition hover:brightness-110 disabled:opacity-60"
                >
                  {t("dataDir.apply")}
                </Button>
              </div>
            </Dialog.Content>
          </div>
        </Dialog.Portal>
      </Dialog>
    </div>
  );
}
