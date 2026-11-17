import { createSignal } from "solid-js";
import { storage } from "./storage";

/** How the model edits files: which edit tool (and format) the agent loop offers. */
export type EditMethod = "str_replace" | "hashline" | "apply_patch" | "search_replace";
export const EDIT_METHODS: EditMethod[] = ["str_replace", "hashline", "apply_patch", "search_replace"];

const EDIT_KEY = "dd:edit-method";
const INSTRUCTIONS_KEY = "dd:project-instructions";
const ADDITIONS_KEY = "dd:prompt-additions";

const readEditMethod = (): EditMethod => {
  const stored = storage.getItem(EDIT_KEY);
  return (EDIT_METHODS as string[]).includes(stored ?? "") ? (stored as EditMethod) : "str_replace";
};

const [editMethod, setEditMethodSignal] = createSignal<EditMethod>(readEditMethod());
export { editMethod };

export function setEditMethod(method: EditMethod) {
  setEditMethodSignal(method);
  storage.setItem(EDIT_KEY, method);
}

/** Load AGENTS.md / CLAUDE.md from the attached folders into the system prompt. */
const [projectInstructionsEnabled, setInstructionsSignal] = createSignal(storage.getItem(INSTRUCTIONS_KEY) !== "0");
export { projectInstructionsEnabled };

export function setProjectInstructionsEnabled(on: boolean) {
  setInstructionsSignal(on);
  storage.setItem(INSTRUCTIONS_KEY, on ? "1" : "0");
}

/** How many steps (model replies) a turn may take before asking whether to go on; null = no limit. */
export const STEP_LIMITS = [25, 50, 100, 200, 500, null] as const;
export type StepLimit = (typeof STEP_LIMITS)[number];
const STEPS_KEY = "dd:max-steps";
const DEFAULT_STEPS: StepLimit = 100;

const readSteps = (): StepLimit => {
  const stored = storage.getItem(STEPS_KEY);
  if (stored === "none") return null;
  const value = Number(stored);
  return (STEP_LIMITS as readonly (number | null)[]).includes(value) && stored !== null ? (value as StepLimit) : DEFAULT_STEPS;
};

const [maxSteps, setMaxStepsSignal] = createSignal<StepLimit>(readSteps());
export { maxSteps };

export function setMaxSteps(limit: StepLimit) {
  setMaxStepsSignal(limit);
  storage.setItem(STEPS_KEY, limit === null ? "none" : String(limit));
}

/** Compact automatically once the context is this full (percent of the model's window). */
export const COMPACT_THRESHOLDS = [50, 60, 70, 80, 90] as const;
export type CompactThreshold = (typeof COMPACT_THRESHOLDS)[number];
/** What compaction shrinks the context to (percent of the window). */
export const COMPACT_TARGET_PERCENT = 50;
const AUTO_COMPACT_KEY = "dd:auto-compact";
const THRESHOLD_KEY = "dd:compact-threshold";

const [autoCompact, setAutoCompactSignal] = createSignal(storage.getItem(AUTO_COMPACT_KEY) !== "0");
export { autoCompact };
export function setAutoCompact(on: boolean) {
  setAutoCompactSignal(on);
  storage.setItem(AUTO_COMPACT_KEY, on ? "1" : "0");
}

const readThreshold = (): CompactThreshold => {
  const value = Number(storage.getItem(THRESHOLD_KEY));
  return (COMPACT_THRESHOLDS as readonly number[]).includes(value) ? (value as CompactThreshold) : 80;
};
const [compactThreshold, setThresholdSignal] = createSignal<CompactThreshold>(readThreshold());
export { compactThreshold };
export function setCompactThreshold(value: CompactThreshold) {
  setThresholdSignal(value);
  storage.setItem(THRESHOLD_KEY, String(value));
}

const readAdditions = (): Record<string, string> => {
  try {
    const parsed: unknown = JSON.parse(storage.getItem(ADDITIONS_KEY) ?? "{}");
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    return Object.fromEntries(Object.entries(parsed).filter((entry): entry is [string, string] => typeof entry[1] === "string"));
  } catch {
    return {};
  }
};

const [additions, setAdditions] = createSignal<Record<string, string>>(readAdditions());

/** The user's own text appended to the system prompt for `provider` ("" when none). */
export const promptAddition = (provider: string): string => additions()[provider] ?? "";

export function setPromptAddition(provider: string, text: string) {
  const next = { ...additions() };
  if (text.trim()) next[provider] = text;
  else delete next[provider];
  setAdditions(next);
  storage.setItem(ADDITIONS_KEY, JSON.stringify(next));
}
