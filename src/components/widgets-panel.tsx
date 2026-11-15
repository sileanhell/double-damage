import { Button } from "@kobalte/core/button";
import { ContextMenu } from "@kobalte/core/context-menu";
import { createSignal, For, type JSX, Show } from "solid-js";
import {
  closeWidget,
  isWidgetOpen,
  moveWidget,
  openWidget,
  openWidgets,
  WIDGET_IDS,
  type WidgetId,
} from "../lib/widgets";
import { locale, type MessageKey, t } from "../lib/i18n";
import { widgetsWidth } from "../lib/layout";
import { ChartIcon, ChevronRightIcon, CloseIcon, GripIcon, LayersIcon, PlusCircleIcon, TrashIcon } from "./icons";

const WIDGET_INFO: Record<WidgetId, { title: MessageKey; icon: (props: { class?: string }) => JSX.Element }> = {
  usage: { title: "widgets.usage", icon: ChartIcon },
  context: { title: "widgets.context", icon: LayersIcon },
};

const MENU_ITEM =
  "flex w-full cursor-pointer items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-sm text-white/80 outline-none transition-colors data-disabled:cursor-default data-highlighted:bg-white/[0.08] data-highlighted:text-white";
const MENU_BOX =
  "min-w-44 rounded-xl border border-white/10 bg-white/[0.06] p-1.5 shadow-2xl outline-none backdrop-blur-2xl";

function UsageLimits() {
  const num = (n: number) => n.toLocaleString(locale());
  const rows = () => [
    { label: t("usage.window5h"), used: `${num(3412)} / ${num(5000)}`, pct: 68, reset: t("usage.reset5h") },
    { label: t("usage.windowWeek"), used: `${num(41208)} / ${num(180000)}`, pct: 23, reset: t("usage.resetWeek") },
  ];
  return (
    <div class="space-y-3">
      <For each={rows()}>
        {(row) => (
          <div>
            <div class="flex items-baseline justify-between text-[13px]">
              <span class="text-white/80">{row.label}</span>
              <span class="text-white/40">{row.used}</span>
            </div>
            <div class="mt-1.5 h-1.5 overflow-hidden rounded-full bg-white/[0.07]">
              <div class="h-full rounded-full bg-white/70" style={{ width: `${row.pct}%` }} />
            </div>
            <div class="mt-1 text-[11px] text-white/35">{row.reset}</div>
          </div>
        )}
      </For>
    </div>
  );
}

const CTX_PARTS: { label: MessageKey; tokens: number; pct: number; color: string }[] = [
  { label: "context.history", tokens: 28_100, pct: 59, color: "bg-emerald-400/70" },
  { label: "context.system", tokens: 12_400, pct: 26, color: "bg-sky-400/70" },
  { label: "context.files", tokens: 4_200, pct: 9, color: "bg-amber-400/70" },
  { label: "context.tools", tokens: 2_500, pct: 6, color: "bg-violet-400/70" },
];

/** Compact token count in the current locale, e.g. 28.1K / 28,1 тыс. */
const tokens = (n: number) => n.toLocaleString(locale(), { notation: "compact", maximumFractionDigits: 1 });

function ContextUsage() {
  return (
    <div>
      <div class="space-y-1">
        <For each={CTX_PARTS}>
          {(part) => (
            <div class="flex items-center gap-2 text-[12px]">
              <span class={`size-2 shrink-0 rounded-full ${part.color}`} />
              <span class="min-w-0 flex-1 truncate text-white/60">{t(part.label)}</span>
              <span class="text-white/40">{tokens(part.tokens)}</span>
            </div>
          )}
        </For>
      </div>
      <div class="mt-2.5 flex items-baseline justify-between text-[13px]">
        <span class="text-white/80">{t("context.total", { used: tokens(47_200), max: tokens(200_000) })}</span>
        <span class="text-white/40">24%</span>
      </div>
      <div class="mt-1.5 flex h-1.5 overflow-hidden rounded-full bg-white/[0.07]">
        <For each={CTX_PARTS}>
          {(part) => <div class={`h-full ${part.color}`} style={{ width: `${part.pct}%` }} />}
        </For>
      </div>
    </div>
  );
}

const WIDGET_BODY: Record<WidgetId, () => ReturnType<typeof UsageLimits>> = {
  usage: UsageLimits,
  context: ContextUsage,
};

/** Submenu listing hidden widgets so they can be re-added. */
function AddWidgetSub() {
  const hidden = () => WIDGET_IDS.filter((id) => !isWidgetOpen(id));
  return (
    <ContextMenu.Sub overlap>
      <ContextMenu.SubTrigger class={MENU_ITEM}>
        <PlusCircleIcon class="size-3.5 shrink-0 text-white/50" />
        <span class="min-w-0 flex-1">{t("widgets.add")}</span>
        <ChevronRightIcon class="size-3.5 shrink-0 text-white/40" />
      </ContextMenu.SubTrigger>
      <ContextMenu.Portal>
        <ContextMenu.SubContent class={MENU_BOX}>
          <Show
            when={hidden().length > 0}
            fallback={
              <ContextMenu.Item disabled class={`${MENU_ITEM} text-white/35`}>
                {t("widgets.allOpen")}
              </ContextMenu.Item>
            }
          >
            <For each={hidden()}>
              {(id) => {
                const Info = WIDGET_INFO[id];
                return (
                  <ContextMenu.Item onSelect={() => openWidget(id)} class={MENU_ITEM}>
                    <Info.icon class="size-3.5 shrink-0 text-white/50" />
                    <span class="min-w-0 flex-1">{t(Info.title)}</span>
                  </ContextMenu.Item>
                );
              }}
            </For>
          </Show>
        </ContextMenu.SubContent>
      </ContextMenu.Portal>
    </ContextMenu.Sub>
  );
}

type Dnd = {
  dragging: () => WidgetId | null;
  dy: () => number;
  els: Map<WidgetId, HTMLElement>;
  start: (id: WidgetId, event: PointerEvent) => void;
};

/** One widget card: grab anywhere to drag, right-click opens close/add actions. */
function WidgetCard(props: { id: WidgetId; dnd: Dnd }) {
  const Body = WIDGET_BODY[props.id];
  return (
    <ContextMenu>
      <ContextMenu.Trigger
        ref={(el) => props.dnd.els.set(props.id, el)}
        onPointerDown={(event) => props.dnd.start(props.id, event)}
        style={{
          transform:
            props.dnd.dragging() === props.id ? `translateY(${props.dnd.dy()}px)` : undefined,
          "background-color": props.dnd.dragging() === props.id ? "rgb(255 255 255 / 0.08)" : undefined,
        }}
        class={`block w-full cursor-pointer touch-none rounded-xl border border-white/10 bg-white/[0.03] text-left outline-none select-none transition-[background-color,box-shadow] duration-200 hover:bg-white/[0.05] active:cursor-grabbing data-expanded:bg-white/[0.05] ${
          props.dnd.dragging() === props.id ? "relative z-10 backdrop-blur-2xl" : ""
        }`}
      >
        <div class="flex items-center gap-2 px-3 pt-2.5 pb-1 text-xs font-medium tracking-wider text-white/45 uppercase">
          <GripIcon class="size-3.5 shrink-0 text-white/25" />
          <span class="min-w-0 flex-1 truncate">{t(WIDGET_INFO[props.id].title)}</span>
        </div>
        <div class="px-3 pt-1 pb-3">
          <Body />
        </div>
      </ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenu.Content class={MENU_BOX}>
          <AddWidgetSub />
          <ContextMenu.Separator class="-mx-1.5 my-1.5 h-px border-0 bg-white/10" />
          <ContextMenu.Item
            onSelect={() => closeWidget(props.id)}
            class="flex w-full cursor-pointer items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-sm text-red-300/80 outline-none transition-colors data-highlighted:bg-red-500/10 data-highlighted:text-red-300"
          >
            <TrashIcon class="size-3.5 shrink-0" />
            <span class="min-w-0 flex-1">{t("common.delete")}</span>
          </ContextMenu.Item>
        </ContextMenu.Content>
      </ContextMenu.Portal>
    </ContextMenu>
  );
}

/** Right-side column with rearrangeable widgets; sits in the layout like the left sidebar. */
export function WidgetsPanel(props: { onClose: () => void }) {
  const [dragId, setDragId] = createSignal<WidgetId | null>(null);
  const [dy, setDy] = createSignal(0);
  const els = new Map<WidgetId, HTMLElement>();

  /**
   * The widget the dragged card should sit before, judged by how far the card itself overlaps
   * its neighbours: a card swaps once the dragged one covers more than half of it.
   * Uses layout offsets (transform-free) so running FLIP animations don't cause jitter.
   */
  const hitTarget = (dragged: WidgetId): WidgetId | "end" => {
    const card = els.get(dragged);
    if (!card) return "end";
    const top = card.offsetTop + dy();
    const bottom = top + card.offsetHeight;
    let below = false; // whether we've passed the dragged card's current slot
    for (const id of openWidgets()) {
      if (id === dragged) {
        below = true;
        continue;
      }
      const el = els.get(id);
      if (!el) continue;
      const mid = el.offsetTop + el.offsetHeight / 2;
      // Above: stays above until the card's top rises past its middle.
      // Below: stays below until the card's bottom sinks past its middle.
      if (below ? bottom <= mid : top < mid) return id;
    }
    return "end";
  };

  /** Reorders and FLIP-animates every card except the dragged one to its new slot. */
  const shift = (id: WidgetId, before: WidgetId | undefined) => {
    const tops = new Map<WidgetId, number>();
    for (const wid of openWidgets()) tops.set(wid, els.get(wid)?.getBoundingClientRect().top ?? 0);
    moveWidget(id, before);
    for (const wid of openWidgets()) {
      if (wid === id) continue;
      const el = els.get(wid);
      const delta = (tops.get(wid) ?? 0) - (el?.getBoundingClientRect().top ?? 0);
      if (el && delta !== 0)
        el.animate([{ transform: `translateY(${delta}px)` }, { transform: "translateY(0px)" }], {
          duration: 200,
          easing: "ease-out",
        });
    }
  };

  const dnd: Dnd = {
    dragging: dragId,
    dy,
    els,
    start: (id, event) => {
      if (event.button !== 0) return;
      event.preventDefault();
      const card = event.currentTarget as HTMLElement;
      card.setPointerCapture(event.pointerId);
      const grabOffset = event.clientY - card.getBoundingClientRect().top;
      setDragId(id);
      setDy(0);

      const onMove = (move: PointerEvent) => {
        // The release can happen outside the window and never reach us; end on the next move.
        if ((move.buttons & 1) === 0) return onEnd();
        // Glue the card to the cursor: rect.top already includes the current translate.
        const glue = () => {
          const top = card.getBoundingClientRect().top - dy();
          setDy(move.clientY - grabOffset - top);
        };
        glue();

        const target = hitTarget(id);
        shift(id, target === "end" ? undefined : target);
        glue(); // re-glue if the reorder shifted the card's slot
      };
      let ended = false;
      const onEnd = () => {
        if (ended) return;
        ended = true;
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onEnd);
        window.removeEventListener("pointercancel", onEnd);
        if (card.hasPointerCapture(event.pointerId)) card.releasePointerCapture(event.pointerId);
        const settle = dy();
        setDragId(null);
        setDy(0);
        if (settle !== 0)
          card.animate([{ transform: `translateY(${settle}px)` }, { transform: "translateY(0px)" }], {
            duration: 150,
            easing: "ease-out",
          });
      };
      // Listen on window, not the card: if capture is lost (e.g. the cursor leaves the app),
      // moves still arrive once it's back over any part of the window.
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onEnd);
      window.addEventListener("pointercancel", onEnd);
    },
  };

  return (
    <aside
      class="flex h-full min-w-0 shrink-0 flex-col border-l border-white/[0.07] bg-white/[0.02] text-white"
      style={{ width: `${widgetsWidth()}vw` }}
    >
      <header class="flex h-12 shrink-0 items-center justify-between border-b border-white/[0.07] pr-2 pl-4">
        <span class="text-xs font-medium tracking-wider text-white uppercase">{t("widgets.title")}</span>
        <Button
          aria-label={t("widgets.close")}
          title={t("widgets.close")}
          onClick={props.onClose}
          class="flex size-8 items-center justify-center rounded-lg text-white/45 transition-colors hover:bg-white/[0.06] hover:text-white"
        >
          <CloseIcon class="size-4" />
        </Button>
      </header>

      <ContextMenu>
        <ContextMenu.Trigger class="thin-scroll block min-h-0 flex-1 space-y-3 overflow-y-auto p-4">
          <For each={openWidgets()}>{(id) => <WidgetCard id={id} dnd={dnd} />}</For>
          <Show when={openWidgets().length === 0}>
            <p class="flex h-32 items-center justify-center px-4 text-center text-xs text-white/35">
              {t("widgets.empty")}
            </p>
          </Show>
        </ContextMenu.Trigger>
        <ContextMenu.Portal>
          <ContextMenu.Content class={MENU_BOX}>
            <AddWidgetSub />
          </ContextMenu.Content>
        </ContextMenu.Portal>
      </ContextMenu>
    </aside>
  );
}
