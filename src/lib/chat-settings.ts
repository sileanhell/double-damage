import { createSignal } from "solid-js";
import { storage } from "./storage";

/** How a model's reasoning shows in the chat: folded away (click to read) or open. */
export type ThinkingDisplay = "collapsed" | "expanded";

const THINKING_KEY = "dd:thinking-display";

const [thinkingDisplay, setThinkingDisplaySignal] = createSignal<ThinkingDisplay>(
  storage.getItem(THINKING_KEY) === "expanded" ? "expanded" : "collapsed",
);
export { thinkingDisplay };

export function setThinkingDisplay(value: ThinkingDisplay) {
  setThinkingDisplaySignal(value);
  storage.setItem(THINKING_KEY, value);
}
