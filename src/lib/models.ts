import { createSignal } from "solid-js";
import { type CatalogModel, catalog, catalogModel, catalogModels } from "./gateway";
import { type MessageKey, t } from "./i18n";
import { storage } from "./storage";

/**
 * The model the next prompt goes to, and how hard it should think. Models come from the
 * gateway's catalog; the reasoning options follow what the model can do:
 * - toggle: thinking on / off (`enable_thinking`),
 * - effort: the model's own levels (`reasoning_effort`),
 * - none / always: nothing to choose.
 */

export type ModelId = string;

export type EffortLevel = { value: string };

const KNOWN_EFFORTS = ["off", "on", "minimal", "low", "medium", "high"] as const;

/** Label for an effort level; unknown levels (whatever a model's template offers) show as-is. */
export function effortLabel(level: EffortLevel): string {
  return (KNOWN_EFFORTS as readonly string[]).includes(level.value)
    ? t(`effort.${level.value}` as MessageKey)
    : level.value;
}

/** Reasoning levels a model offers, weakest first; empty when there's nothing to choose. */
export function effortLevels(model: CatalogModel | undefined): EffortLevel[] {
  const caps = model?.capabilities;
  if (!caps) return [];
  if (caps.reasoning === "toggle") return [{ value: "off" }, { value: "on" }];
  if (caps.reasoning === "effort") return caps.effortLevels.map((value) => ({ value }));
  return [];
}

function defaultEffortFor(model: CatalogModel | undefined): string | undefined {
  const levels = effortLevels(model);
  if (levels.length === 0) return undefined;
  if (model?.capabilities.reasoning === "toggle") return "on";
  return levels.find((level) => level.value === "medium")?.value ?? levels[Math.floor(levels.length / 2)].value;
}

const MODEL_KEY = "dd:model";
const EFFORT_KEY = "dd:efforts";

const [modelId, setModelId] = createSignal<ModelId | null>(storage.getItem(MODEL_KEY));

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

/** The chosen model, or the first available one when the choice is gone (deleted, not loaded yet). */
export function currentModel(): CatalogModel | undefined {
  const chosen = catalogModel(modelId() ?? undefined);
  return chosen ?? catalogModels()[0];
}

/** True while the catalog hasn't been read yet (so "no models" isn't shown too early). */
export const modelsLoading = () => catalog() === undefined;

export function setModel(id: ModelId) {
  if (!catalogModel(id)) return;
  setModelId(id);
  storage.setItem(MODEL_KEY, id);
}

/** Current model's reasoning effort, or undefined when it has no levels. */
export function effort(model: CatalogModel | undefined = currentModel()): string | undefined {
  const levels = effortLevels(model);
  const stored = model && efforts()[model.id];
  return levels.some((level) => level.value === stored) ? stored : defaultEffortFor(model);
}

export function setEffort(value: string) {
  const model = currentModel();
  if (!model || !effortLevels(model).some((level) => level.value === value)) return;
  const next = { ...efforts(), [model.id]: value };
  setEfforts(next);
  storage.setItem(EFFORT_KEY, JSON.stringify(next));
}
