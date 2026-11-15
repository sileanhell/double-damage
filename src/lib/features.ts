import { storage } from "./storage";
import { createSignal } from "solid-js";

const WIDGETS_KEY = "dd:feature-widgets";

/** Whether the widgets feature is on. When off, the widgets panel chunk is never loaded. */
const [widgetsEnabled, setWidgetsEnabledSignal] = createSignal(storage.getItem(WIDGETS_KEY) !== "0");
export { widgetsEnabled };

export function setWidgetsEnabled(enabled: boolean) {
  setWidgetsEnabledSignal(enabled);
  storage.setItem(WIDGETS_KEY, enabled ? "1" : "0");
}
