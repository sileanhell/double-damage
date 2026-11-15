import { storage } from "./storage";
import { createSignal } from "solid-js";
import type { MessageKey } from "./i18n";

export type EffortLevel = { value: "minimal" | "low" | "medium" | "high" };

/** Translation key for an effort level's label. */
export const effortKey = (level: EffortLevel): MessageKey => `effort.${level.value}`;

export type ModelInfo = {
  value: string;
  label: string;
  /** Reasoning levels the model supports, from weakest to strongest. Empty if it can't reason. */
  efforts: readonly EffortLevel[];
  defaultEffort?: string;
};

const MINIMAL: EffortLevel = { value: "minimal" };
const LOW: EffortLevel = { value: "low" };
const MEDIUM: EffortLevel = { value: "medium" };
const HIGH: EffortLevel = { value: "high" };

// Placeholder catalog until the agent backend provides the real one.
export const MODELS: readonly ModelInfo[] = [
  { value: "gpt-5", label: "GPT-5", efforts: [MINIMAL, LOW, MEDIUM, HIGH], defaultEffort: "medium" },
  { value: "claude-sonnet-4.5", label: "Claude Sonnet 4.5", efforts: [LOW, MEDIUM, HIGH], defaultEffort: "medium" },
  { value: "gemini-2.5-pro", label: "Gemini 2.5 Pro", efforts: [LOW, HIGH], defaultEffort: "high" },
];

export type ModelId = string;

const MODEL_KEY = "dd:model";
const EFFORT_KEY = "dd:efforts";

const findModel = (id: string | null) => MODELS.find((item) => item.value === id);

const [modelId, setModelId] = createSignal<ModelId>(findModel(storage.getItem(MODEL_KEY))?.value ?? MODELS[0].value);

/** Effort chosen per model, so switching models keeps each one's own setting. */
const [efforts, setEfforts] = createSignal<Record<string, string>>(readEfforts());

function readEfforts(): Record<string, string> {
  try {
    const parsed: unknown = JSON.parse(storage.getItem(EFFORT_KEY) ?? "{}");
    return parsed && typeof parsed === "object" ? (parsed as Record<string, string>) : {};
  } catch {
    return {};
  }
}

export const model = modelId;
export const modelInfo = () => findModel(modelId()) ?? MODELS[0];

export function setModel(id: ModelId) {
  if (!findModel(id)) return;
  setModelId(id);
  storage.setItem(MODEL_KEY, id);
}

export function defaultEffort(info: ModelInfo = modelInfo()) {
  const levels = info.efforts;
  if (levels.length === 0) return undefined;
  return levels.find((level) => level.value === info.defaultEffort)?.value ?? levels[Math.floor(levels.length / 2)].value;
}

/** Current model's reasoning effort, or undefined when the model has no reasoning levels. */
export function effort(): string | undefined {
  const info = modelInfo();
  const stored = efforts()[info.value];
  return info.efforts.some((level) => level.value === stored) ? stored : defaultEffort(info);
}

export function setEffort(value: string) {
  const info = modelInfo();
  if (!info.efforts.some((level) => level.value === value)) return;
  const next = { ...efforts(), [info.value]: value };
  setEfforts(next);
  storage.setItem(EFFORT_KEY, JSON.stringify(next));
}
