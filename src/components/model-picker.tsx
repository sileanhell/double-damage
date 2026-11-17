import { Button } from "@kobalte/core/button";
import { Listbox } from "@kobalte/core/listbox";
import { Popover } from "@kobalte/core/popover";
import { Slider } from "@kobalte/core/slider";
import { createSignal, For, onCleanup, Show } from "solid-js";
import { t } from "../lib/i18n";
import type { CatalogModel } from "../lib/gateway";
import { catalog } from "../lib/gateway";
import {
  currentModel,
  type EffortLevel,
  effort,
  effortLabel,
  effortLevels,
  modelsLoading,
  setEffort,
  setModel,
} from "../lib/models";
import { CheckIcon, ChevronDownIcon, PenSquareIcon } from "./icons";

const TRIGGER =
  "flex h-8 items-center gap-1.5 rounded-lg border border-white/10 bg-white/[0.03] px-2.5 text-[13px] text-white/85 outline-none transition-colors hover:border-white/20 hover:bg-white/[0.07] hover:text-white focus-visible:border-white/25 data-expanded:border-white/20 data-expanded:bg-white/[0.07] data-expanded:text-white";

/** Model selector; the reasoning-effort slider lives inside and is built from the chosen model's levels. */
export function ModelPicker() {
  const [open, setOpen] = createSignal(false);
  const [view, setView] = createSignal<"effort" | "models">("effort");

  const levels = () => effortLevels(currentModel());
  const effortIndex = () => Math.max(0, levels().findIndex((level) => level.value === effort()));
  const currentEffort = () => {
    const level = levels()[effortIndex()];
    return level && effortLabel(level);
  };
  const modelName = () => currentModel()?.name ?? (modelsLoading() ? t("common.loading") : t("model.none"));

  return (
    <Popover
      open={open()}
      onOpenChange={(next) => {
        setOpen(next);
        if (!next) setView("effort");
      }}
      placement="top-start"
      gutter={8}
    >
      <Popover.Trigger title={t("model.trigger")} class={TRIGGER}>
        <span class="max-w-40 truncate">{modelName()}</span>
        <Show when={currentEffort()}>
          <span class="text-white/45">{currentEffort()}</span>
        </Show>
        <ChevronDownIcon class="size-3 transition-transform in-data-expanded:rotate-180" />
      </Popover.Trigger>

      <Popover.Portal>
        <Popover.Content class="z-50 w-64 origin-bottom-left rounded-2xl border border-white/10 bg-white/[0.06] p-2 text-white shadow-2xl backdrop-blur-2xl outline-none transition-[opacity,transform] duration-150 data-closed:scale-[0.97] data-closed:opacity-0 data-expanded:scale-100 data-expanded:opacity-100">
          <Show
            when={view() === "effort"}
            fallback={
              <ModelList
                onSelect={(id) => {
                  setModel(id);
                  setView("effort");
                }}
              />
            }
          >
            <div class="flex min-w-0 flex-col items-center">
              <Button
                title={t("model.choose")}
                class="flex max-w-full items-center gap-1.5 rounded-md px-1.5 text-[15px] font-medium text-white/90 outline-none transition-colors hover:text-white focus-visible:bg-white/10"
                onClick={() => setView("models")}
              >
                <span class="truncate">{modelName()}</span>
                <PenSquareIcon class="size-3.5 shrink-0 text-white/50" />
              </Button>
              <Show when={currentEffort()} fallback={<span class="text-[13px] text-white/45">{t("model.noReasoning")}</span>}>
                <span class="text-[13px] text-accent">{currentEffort()}</span>
              </Show>
            </div>

            <Show when={levels().length > 1}>
              <EffortSlider levels={levels()} index={effortIndex()} />
            </Show>
          </Show>
        </Popover.Content>
      </Popover.Portal>
    </Popover>
  );
}

const FILL_AFTER = 5000;
const GROW_DURATION = 6000;
const DEFLATE_DURATION = 1300;

/**
 * Keyframes for a balloon letting its air out: it collapses from `from` to normal size while
 * wobbling (alternating squash/stretch), twisting back and forth and darting around, all of which
 * die down as it empties. Uses the individual `scale`/`rotate`/`translate` properties so Kobalte's
 * own `transform` (the thumb's centring) is left alone.
 */
function deflateKeyframes(from: number): Keyframe[] {
  const steps = 48;
  return Array.from({ length: steps + 1 }, (_, i) => {
    const t = i / steps;
    const left = 1 - t; // air left: 1 -> 0
    const size = 1 + (from - 1) * left ** 2.4; // most air rushes out first, the rest dribbles
    const wobble = 0.22 * left * Math.sin(t * Math.PI * 16);
    const turn = 28 * left * Math.sin(t * Math.PI * 7);
    const dx = 10 * left * Math.sin(t * Math.PI * 11);
    const dy = 8 * left * Math.sin(t * Math.PI * 9);
    return {
      offset: t,
      scale: `${size * (1 + wobble)} ${size * (1 - wobble)}`,
      rotate: `${turn}deg`,
      translate: `${dx}px ${dy}px`,
    };
  });
}

function EffortSlider(props: { levels: EffortLevel[]; index: number }) {
  const max = () => props.levels.length - 1;
  // Holding the thumb for FILL_AFTER makes it slowly grow over the whole app;
  // releasing lets it deflate back to normal like a balloon.
  // Both stages are Web Animations on the same element; a CSS transition here would fight the
  // deflation (once it finished, the half-done transition reappeared and kept growing).
  let thumb!: HTMLSpanElement;
  let pressed = false;
  let fillTimer: number | undefined;
  let animation: Animation | undefined;

  const stop = () => {
    clearTimeout(fillTimer);
    animation?.cancel();
    animation = undefined;
  };

  const press = () => {
    if (pressed) return;
    pressed = true;
    stop();
    fillTimer = window.setTimeout(() => {
      // Scale so the circle, centred on the thumb, reaches the farthest window corner.
      const rect = thumb.getBoundingClientRect();
      const cx = rect.left + rect.width / 2;
      const cy = rect.top + rect.height / 2;
      const dx = Math.max(cx, window.innerWidth - cx);
      const dy = Math.max(cy, window.innerHeight - cy);
      const full = (Math.hypot(dx, dy) * 2) / thumb.offsetWidth + 1;
      animation = thumb.animate([{ scale: 1 }, { scale: full }], {
        duration: GROW_DURATION,
        easing: "ease-in",
        fill: "forwards",
      });
    }, FILL_AFTER);
  };

  const release = () => {
    if (!pressed) return;
    pressed = false;
    clearTimeout(fillTimer);
    if (!animation) return;
    // Deflate from wherever the growth got to (it may still be under way).
    const current = Number.parseFloat(getComputedStyle(thumb).scale) || 1;
    animation.cancel();
    animation = thumb.animate(deflateKeyframes(current), { duration: DEFLATE_DURATION, easing: "linear" });
  };

  onCleanup(stop);

  return (
    <Slider
      class="mt-2 rounded-full bg-white/[0.06]"
      minValue={0}
      maxValue={max()}
      step={1}
      value={[props.index]}
      onChange={([index]) => {
        const level = props.levels[index];
        if (level) setEffort(level.value);
      }}
      getValueLabel={({ values }) => {
        const level = props.levels[values[0]];
        return level ? effortLabel(level) : "";
      }}
    >
      <Slider.Label class="sr-only">{t("model.effortLabel")}</Slider.Label>
      {/* Inset by the thumb radius so the thumb stays inside the pill at both ends;
          the fill's negative margins stretch it back out to the pill's edge. */}
      {/* Thumb/track capture the pointer while dragging, so losing capture marks the end of the press. */}
      <Slider.Track
        class="relative mx-3 h-6 cursor-pointer"
        onPointerDown={press}
        onLostPointerCapture={release}
        onPointerCancel={release}
      >
        <Slider.Fill class="absolute inset-y-0 -mx-3 rounded-full bg-accent" />
        <For each={props.levels}>
          {(_, i) => (
            <span
              aria-hidden="true"
              class="pointer-events-none absolute top-1/2 size-1 -translate-1/2 rounded-full"
              classList={{ "bg-white/60": i() <= props.index, "bg-white/30": i() > props.index }}
              style={{ left: `${(i() / max()) * 100}%` }}
            />
          )}
        </For>
        <Slider.Thumb
          // Kobalte centres the thumb with translateX(-50%), which `scale` also scales; origin-left is
          // the visual centre after that shift, so the thumb grows evenly in every direction.
          ref={thumb}
          onPointerDown={press}
          class="top-1 block size-4 origin-left rounded-full bg-white shadow-md outline-none"
        >
          <Slider.Input />
        </Slider.Thumb>
      </Slider.Track>
    </Slider>
  );
}

type Group = { name: string; models: CatalogModel[] };

const STATE_DOT: Record<CatalogModel["state"], string> = {
  loaded: "bg-emerald-400",
  loading: "animate-pulse bg-amber-400",
  failed: "bg-red-400",
  unloaded: "bg-white/20",
};

/** Models grouped by provider; a dot shows whether a local model is in memory. */
function ModelList(props: { onSelect: (id: string) => void }) {
  const groups = (): Group[] =>
    (catalog()?.providers ?? [])
      .filter((provider) => provider.models.length > 0)
      .map((provider) => ({ name: provider.name, models: provider.models }));

  return (
    <Show
      when={groups().length > 0}
      fallback={<p class="px-2.5 py-2 text-sm text-white/45">{t("model.empty")}</p>}
    >
      <Listbox<CatalogModel, Group>
        options={groups()}
        optionValue="id"
        optionTextValue="name"
        optionGroupChildren="models"
        value={currentModel() ? [currentModel()!.id] : []}
        onChange={(keys) => {
          const [id] = keys;
          if (id) props.onSelect(id);
        }}
        disallowEmptySelection
        shouldFocusOnHover
        autoFocus="first"
        class="thin-scroll max-h-80 overflow-y-auto outline-none"
        renderSection={(section) => (
          <Listbox.Section class="px-2.5 pt-2 pb-1 text-xs text-white/40 first:pt-1">
            {section.rawValue.name}
          </Listbox.Section>
        )}
        renderItem={(item) => (
          <Listbox.Item
            item={item}
            title={item.rawValue.state === "failed" ? (item.rawValue.error ?? undefined) : undefined}
            class="flex cursor-pointer items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-sm text-white/80 outline-none transition-colors data-highlighted:bg-white/10 data-highlighted:text-white"
            onClick={() => props.onSelect(item.rawValue.id)}
          >
            <span class={`size-1.5 shrink-0 rounded-full ${STATE_DOT[item.rawValue.state]}`} />
            <Listbox.ItemLabel class="min-w-0 flex-1 truncate">{item.rawValue.name}</Listbox.ItemLabel>
            <Listbox.ItemIndicator>
              <CheckIcon class="size-4 text-accent" />
            </Listbox.ItemIndicator>
          </Listbox.Item>
        )}
      />
    </Show>
  );
}
