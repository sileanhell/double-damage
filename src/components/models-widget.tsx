import { Button } from "@kobalte/core/button";
import { createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { formatBytes } from "../lib/format";
import { catalogModel, idleMinutes, type Instance, instances, unloadModel } from "../lib/gateway";
import { locale, t } from "../lib/i18n";
import { modelInfo, runningUsage } from "../lib/launch";
import { CloseIcon } from "./icons";

const STATE_DOT: Record<Instance["state"]["kind"], string> = {
  loading: "animate-pulse bg-amber-400",
  ready: "bg-emerald-400",
  failed: "bg-red-400",
};

/** Models in memory: where they run, what they take, when they'll be unloaded; unload by hand. */
export function ModelsWidget() {
  const [now, setNow] = createSignal(Date.now());
  const timer = setInterval(() => setNow(Date.now()), 15_000);
  onCleanup(() => clearInterval(timer));

  return (
    <Show when={instances().length > 0} fallback={<p class="text-xs text-white/35">{t("loaded.empty")}</p>}>
      <ul class="space-y-2.5">
        <For each={instances()}>{(item) => <Row item={item} now={now()} />}</For>
      </ul>
    </Show>
  );
}

function Row(props: { item: Instance; now: number }) {
  const name = () => catalogModel(props.item.id)?.name ?? props.item.id.split("/").pop() ?? props.item.id;
  const [usage] = createResource(
    () => props.item.spec,
    async (spec) => runningUsage(await modelInfo(spec.file), spec.file, spec),
  );
  const ctx = () => props.item.spec.ctx.toLocaleString(locale(), { notation: "compact" });
  const where = () => props.item.spec.device ?? t("models.cpu");

  const status = () => {
    const state = props.item.state;
    if (state.kind === "loading") return t("loaded.loading");
    if (state.kind === "failed") return t("loaded.failed");
    if (props.item.active > 0) return t("loaded.busy");
    const limit = idleMinutes();
    if (limit <= 0) return t("loaded.idle");
    const left = Math.max(1, Math.ceil((props.item.lastUsed + limit * 60_000 - props.now) / 60_000));
    return t("loaded.unloadIn", { minutes: left });
  };

  return (
    <li class="text-[12px]">
      <div class="flex items-center gap-2">
        <span class={`size-1.5 shrink-0 rounded-full ${STATE_DOT[props.item.state.kind]}`} />
        <span class="min-w-0 flex-1 truncate text-[13px] text-white/80" title={props.item.id}>
          {name()}
        </span>
        <Button
          title={t("loaded.unload")}
          aria-label={t("loaded.unloadNamed", { name: name() })}
          // The card is draggable; keep the press to ourselves so the click lands.
          onPointerDown={(event: PointerEvent) => event.stopPropagation()}
          onClick={() => void unloadModel(props.item.id).catch(console.error)}
          class="flex size-6 shrink-0 items-center justify-center rounded-md text-white/40 transition-colors hover:bg-white/[0.08] hover:text-white"
        >
          <CloseIcon class="size-3.5" />
        </Button>
      </div>
      <div class="mt-0.5 flex flex-wrap gap-x-2 pl-3.5 text-white/40 tabular-nums">
        <span>{where()}</span>
        <span>{t("loaded.ctx", { ctx: ctx() })}</span>
        <Show when={usage()}>
          {(used) => (
            <span>
              {used().vram > 0 ? `${formatBytes(used().vram)} VRAM` : ""}
              {used().vram > 0 && used().ram > 0.2 * 1024 ** 3 ? " + " : ""}
              {used().ram > 0.2 * 1024 ** 3 || used().vram === 0 ? `${formatBytes(used().ram)} RAM` : ""}
            </span>
          )}
        </Show>
      </div>
      <div
        class="mt-0.5 pl-3.5"
        classList={{ "text-red-300/80": props.item.state.kind === "failed", "text-white/30": props.item.state.kind !== "failed" }}
        title={props.item.state.kind === "failed" ? props.item.state.error : undefined}
      >
        {status()}
      </div>
    </li>
  );
}
