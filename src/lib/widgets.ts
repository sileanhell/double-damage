import { storage } from "./storage";
import { createSignal } from "solid-js";

export type WidgetId = "models" | "usage" | "context";

export const WIDGET_IDS: WidgetId[] = ["models", "usage", "context"];

const KEY = "dd:widgets";
const PANEL_KEY = "dd:widgets-panel";
const MODELS_ADDED = "dd:widgets-models-added";

const [widgetsPanelOpen, setWidgetsPanelOpenSignal] = createSignal(storage.getItem(PANEL_KEY) === "1");
export { widgetsPanelOpen };

export function setWidgetsPanelOpen(open: boolean) {
  setWidgetsPanelOpenSignal(open);
  storage.setItem(PANEL_KEY, open ? "1" : "0");
}

function load(): WidgetId[] {
  try {
    const parsed = JSON.parse(storage.getItem(KEY) ?? "null");
    if (Array.isArray(parsed)) {
      const ids = parsed.filter((id): id is WidgetId => WIDGET_IDS.includes(id as WidgetId));
      // Widgets added in later versions show up once for people with a saved layout.
      if (storage.getItem(MODELS_ADDED) === null) {
        storage.setItem(MODELS_ADDED, "1");
        if (!ids.includes("models")) ids.unshift("models");
      }
      return ids;
    }
  } catch {
    // fall through to the default layout
  }
  return [...WIDGET_IDS];
}

const [openWidgets, setOpenWidgetsSignal] = createSignal<WidgetId[]>(load());
export { openWidgets };

function save(next: WidgetId[]) {
  setOpenWidgetsSignal(next);
  storage.setItem(KEY, JSON.stringify(next));
}

export const isWidgetOpen = (id: WidgetId) => openWidgets().includes(id);

export function openWidget(id: WidgetId) {
  if (!isWidgetOpen(id)) save([...openWidgets(), id]);
}

export function closeWidget(id: WidgetId) {
  save(openWidgets().filter((widget) => widget !== id));
}

/** Moves `id` to sit before `before`; `before === undefined` drops it at the end. */
export function moveWidget(id: WidgetId, before: WidgetId | undefined) {
  const rest = openWidgets().filter((widget) => widget !== id);
  const at = before === undefined ? -1 : rest.indexOf(before);
  rest.splice(at < 0 ? rest.length : at, 0, id);
  const current = openWidgets();
  if (rest.every((widget, index) => widget === current[index])) return;
  save(rest);
}
