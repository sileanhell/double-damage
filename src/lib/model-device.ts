import { createSignal } from "solid-js";
import { modelKey } from "./local-models";
import { llamaDevices } from "./providers";
import { storage } from "./storage";

/** Where each local model runs: a llama-server device id ("CUDA0", "Vulkan1", …) or CPU. */
const KEY = "dd:model-device";
export const CPU = "cpu";

function load(): Record<string, string> {
  try {
    const parsed: unknown = JSON.parse(storage.getItem(KEY) ?? "{}");
    return parsed && typeof parsed === "object" ? (parsed as Record<string, string>) : {};
  } catch {
    return {};
  }
}

const [choices, setChoices] = createSignal(load());

function save(next: Record<string, string>) {
  setChoices(next);
  if (Object.keys(next).length === 0) storage.removeItem(KEY);
  else storage.setItem(KEY, JSON.stringify(next));
}

/** The chosen device, or the first GPU (CUDA comes first) when nothing valid was chosen. */
export function modelDevice(name: string): string {
  const devices = llamaDevices();
  const chosen = choices()[modelKey(name)];
  if (chosen === CPU || devices.some((device) => device.id === chosen)) return chosen;
  return devices[0]?.id ?? CPU;
}

export function setModelDevice(name: string, device: string) {
  save({ ...choices(), [modelKey(name)]: device });
}

export function forgetModelDevice(name: string) {
  const { [modelKey(name)]: _, ...rest } = choices();
  save(rest);
}
