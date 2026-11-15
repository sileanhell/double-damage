import { storage } from "./storage";
import { createSignal } from "solid-js";

/** A persisted panel width, stored as a percentage of the viewport width. */
function createPanelWidth(key: string, min: number, max: number, fallback: number) {
  const clamp = (value: number) => Math.min(max, Math.max(min, value));
  const stored = Number.parseFloat(storage.getItem(key) ?? "");
  const [width, setWidth] = createSignal(Number.isFinite(stored) ? clamp(stored) : fallback);

  const set = (percent: number) => {
    const next = clamp(percent);
    setWidth(next);
    storage.setItem(key, next.toFixed(2));
  };
  return [width, set] as const;
}

export const SIDEBAR_MIN = 16;
export const SIDEBAR_MAX = 45;
export const SIDEBAR_DEFAULT = 22;

/** Sidebar width as a percentage of the viewport width. */
export const [sidebarWidth, setSidebarWidth] = createPanelWidth(
  "dd:sidebar-width",
  SIDEBAR_MIN,
  SIDEBAR_MAX,
  SIDEBAR_DEFAULT,
);

export const WIDGETS_MIN = 16;
export const WIDGETS_MAX = 40;
export const WIDGETS_DEFAULT = 22;

/** Widgets panel width as a percentage of the viewport width. */
export const [widgetsWidth, setWidgetsWidth] = createPanelWidth(
  "dd:widgets-width",
  WIDGETS_MIN,
  WIDGETS_MAX,
  WIDGETS_DEFAULT,
);
