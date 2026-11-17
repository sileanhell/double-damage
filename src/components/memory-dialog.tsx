import { Button } from "@kobalte/core/button";
import { Dialog } from "@kobalte/core/dialog";
import { For, Show } from "solid-js";
import { catalogModel, memoryConflict } from "../lib/gateway";
import { t } from "../lib/i18n";

const OPTION =
  "flex w-full flex-col items-start gap-0.5 rounded-xl border border-white/10 bg-white/[0.03] px-3.5 py-2.5 text-left outline-none transition-colors hover:border-white/20 hover:bg-white/[0.07] focus-visible:border-white/30";

/**
 * Asks what to do when a model doesn't fit next to the ones already in memory: unload them,
 * load alongside with part of the model on the CPU (when that works at all), or cancel.
 */
export function MemoryDialog() {
  const conflict = memoryConflict;
  const names = () =>
    (conflict()?.blocking ?? []).map((item) => catalogModel(item.id)?.name ?? item.id.split("/").pop() ?? item.id);

  return (
    <Dialog open={!!conflict()} onOpenChange={(open) => !open && conflict()?.resolve("cancel")}>
      <Dialog.Portal>
        <Dialog.Overlay class="fixed inset-0 z-50 bg-black/60 backdrop-blur-md" />
        <div class="fixed inset-0 z-50 flex justify-center px-6 pt-[18vh]">
          <Dialog.Content class="h-fit w-full max-w-md rounded-2xl border border-white/10 bg-white/[0.06] p-5 text-white shadow-2xl outline-none backdrop-blur-2xl">
            <Show when={conflict()}>
              {(current) => (
                <>
                  <Dialog.Title class="text-sm font-medium text-white/90">
                    {t(current().noRoom ? "memory.noRoomTitle" : "memory.partialTitle", { model: current().model.name })}
                  </Dialog.Title>
                  <Dialog.Description class="mt-2 text-sm text-white/55">
                    {t(current().noRoom ? "memory.noRoomText" : "memory.partialText")}
                  </Dialog.Description>
                  <ul class="mt-2 space-y-0.5 text-sm text-white/75">
                    <For each={names()}>{(name) => <li class="truncate">· {name}</li>}</For>
                  </ul>

                  <div class="mt-4 space-y-2">
                    <Button class={OPTION} onClick={() => current().resolve("unload")}>
                      <span class="text-sm text-white/90">{t("memory.unload")}</span>
                      <span class="text-xs text-white/45">{t("memory.unloadHint")}</span>
                    </Button>
                    <Show when={current().together}>
                      {(together) => (
                        <Button class={OPTION} onClick={() => current().resolve("together")}>
                          <span class="text-sm text-white/90">{t("memory.together")}</span>
                          <span class="text-xs text-white/45">
                            {t("memory.togetherHint", {
                              count: together().gpuLayers,
                              total: together().totalLayers,
                              alone: current().aloneLayers,
                            })}
                          </span>
                        </Button>
                      )}
                    </Show>
                  </div>

                  <div class="mt-4 flex justify-end">
                    <Button
                      onClick={() => current().resolve("cancel")}
                      class="h-8 rounded-lg px-3 text-[13px] text-white/60 transition-colors hover:bg-white/[0.06] hover:text-white"
                    >
                      {t("common.cancel")}
                    </Button>
                  </div>
                </>
              )}
            </Show>
          </Dialog.Content>
        </div>
      </Dialog.Portal>
    </Dialog>
  );
}
