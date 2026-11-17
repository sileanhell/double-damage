import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createResource, createRoot, createSignal } from "solid-js";
import { type LaunchPlan, type LaunchSpec, modelInfo, planLaunch, runningUsage } from "./launch";
import { localModels } from "./local-models";
import { hardware } from "./hardware";
import { llamaDevices, llamaDevicesState, llamaServer } from "./providers";
import { storage } from "./storage";

// --- types (mirror src-tauri/src/gateway) -----------------------------------------

export type GatewayInfo = { url: string | null; port: number; token: string; error: string | null };

export type Reasoning = "none" | "toggle" | "effort" | "always";
export type Capabilities = { tools: boolean; reasoning: Reasoning; effortLevels: string[]; confirmed: boolean };

export type Sampling = {
  temperature: number | null;
  topK: number | null;
  topP: number | null;
  minP: number | null;
  repeatPenalty: number | null;
  repeatLastN: number | null;
};

export type ModelState = "unloaded" | "loading" | "loaded" | "failed";

export type CatalogModel = {
  id: string;
  name: string;
  provider: string;
  state: ModelState;
  file: string | null;
  size: number | null;
  contextLength: number | null;
  loadedContext: number | null;
  capabilities: Capabilities;
  sampling: Sampling | null;
  error: string | null;
};

export type CatalogProvider = {
  id: string;
  name: string;
  status: "ready" | "not-installed";
  models: CatalogModel[];
};

export type Catalog = { providers: CatalogProvider[] };

export type InstanceState = { kind: "loading" } | { kind: "ready" } | { kind: "failed"; error: string };

export type Instance = {
  id: string;
  spec: LaunchSpec;
  state: InstanceState;
  startedAt: number;
  lastUsed: number;
  active: number;
  caps: unknown;
};

const inTauri = "__TAURI_INTERNALS__" in window;

// --- gateway endpoint ---------------------------------------------------------------

const [gatewayInfo, setGatewayInfo] = createSignal<GatewayInfo>();
/** Where the gateway listens and its token; undefined until the backend answers. */
export { gatewayInfo };

export async function refreshGatewayInfo() {
  if (!inTauri) return;
  setGatewayInfo(await invoke<GatewayInfo>("gateway_info"));
}

/** Restarts the endpoint on the port saved in settings (`GATEWAY_PORT_KEY`). */
export async function restartGateway() {
  setGatewayInfo(await invoke<GatewayInfo>("gateway_restart"));
}

export async function regenerateGatewayToken() {
  setGatewayInfo(await invoke<GatewayInfo>("gateway_regenerate_token"));
}

export const GATEWAY_PORT_KEY = "dd:gateway-port";
export const DEFAULT_GATEWAY_PORT = 11488;

// The listener starts in the background at launch; ask until it has settled.
void (async () => {
  for (let i = 0; i < 20; i++) {
    await refreshGatewayInfo().catch(console.error);
    if (gatewayInfo()?.url || gatewayInfo()?.error) return;
    await new Promise((r) => setTimeout(r, 150));
  }
})();

// --- running models -----------------------------------------------------------------

const [instances, setInstances] = createSignal<Instance[]>([]);
/** Model servers that are loading, ready or failed. */
export { instances };

if (inTauri) {
  void invoke<Instance[]>("llama_instances").then(setInstances).catch(console.error);
  void listen<Instance[]>("llama-instances", (event) => setInstances(event.payload)).catch(console.error);
}

export const instance = (id: string) => instances().find((item) => item.id === id);

// --- catalog ------------------------------------------------------------------------

/** Changes only when models appear, go away or change state (not on every request). */
const instanceShape = () =>
  instances()
    .map((item) => `${item.id}:${item.state.kind}`)
    .join("|");

const [catalog, { refetch: refetchCatalog }] = createRoot(() => {
  const [resource, actions] = createResource(
    () => (inTauri ? `${instanceShape()}#${localModels().length}#${llamaServer()?.installed}` : false),
    () => invoke<Catalog>("gateway_catalog"),
  );
  return [resource, actions] as const;
});
/** Every provider with its models; undefined until loaded. */
export { catalog, refetchCatalog };

export const catalogModels = (): CatalogModel[] => catalog()?.providers.flatMap((provider) => provider.models) ?? [];
export const catalogModel = (id: string | undefined) => (id ? catalogModels().find((model) => model.id === id) : undefined);

// --- loading ------------------------------------------------------------------------

/** What to do when a model doesn't fit next to the ones already loaded. */
export type MemoryChoice = "unload" | "together" | "cancel";

export type MemoryConflict = {
  model: CatalogModel;
  /** Loaded models that would have to go. */
  blocking: Instance[];
  /** Loading alongside is possible, with this many layers on the GPU instead of `aloneLayers`. */
  together?: { gpuLayers: number; totalLayers: number };
  aloneLayers: number;
  /** Doesn't fit at all next to the others. */
  noRoom: boolean;
  resolve: (choice: MemoryChoice) => void;
};

const [memoryConflict, setMemoryConflict] = createSignal<MemoryConflict>();
/** A pending question for the user; the app shows it as a dialog and calls `resolve`. */
export { memoryConflict };

function askMemory(conflict: Omit<MemoryConflict, "resolve">): Promise<MemoryChoice> {
  return new Promise((resolve) =>
    setMemoryConflict({
      ...conflict,
      resolve: (choice) => {
        setMemoryConflict(undefined);
        resolve(choice);
      },
    }),
  );
}

export class LoadCancelled extends Error {
  constructor() {
    super("cancelled");
  }
}

const running = (item: Instance) => item.state.kind !== "failed";

/** The card behind a device id: CUDA0 and Vulkan0 can be the same GPU, so compare by card. */
const card = (id: string | null) => {
  const device = llamaDevices().find((item) => item.id === id);
  return device ? `${device.name}|${device.memory}` : id;
};

/** Memory the other loaded models take: video memory on `device`'s card, and RAM overall. */
async function reservedBy(others: Instance[], device: string | null) {
  let vram = 0;
  let ram = 0;
  for (const other of others) {
    const info = await modelInfo(other.spec.file).catch(() => undefined);
    if (!info) continue;
    const usage = runningUsage(info, other.spec.file, other.spec);
    if (device && other.spec.device && card(other.spec.device) === card(device)) vram += usage.vram;
    ram += usage.ram;
  }
  return { vram, ram };
}

const loads = new Map<string, Promise<Instance>>();

/**
 * Makes sure `model` is loaded and ready. Asks the user (via `memoryConflict`) when it doesn't
 * fit next to the models already loaded. Rejects with `LoadCancelled` if the user declines.
 */
export function ensureLoaded(model: CatalogModel): Promise<Instance> {
  const current = instance(model.id);
  if (current?.state.kind === "ready") return Promise.resolve(current);
  let pending = loads.get(model.id);
  if (!pending) {
    pending = load(model).finally(() => loads.delete(model.id));
    loads.set(model.id, pending);
  }
  return pending;
}

/** Polls `ready` until it holds or `timeout` ms pass. */
async function until(ready: () => boolean, timeout: number) {
  const end = Date.now() + timeout;
  while (!ready() && Date.now() < end) await new Promise((r) => setTimeout(r, 100));
  return ready();
}

async function load(model: CatalogModel): Promise<Instance> {
  if (!model.file) throw new Error(`${model.id} can't be loaded`);
  const file = model.file;
  // Planning needs the GPUs and RAM; without them a model would silently start on the CPU.
  if (!(await until(() => llamaDevicesState() !== "pending" && hardware() !== undefined, 30_000))) {
    throw new Error("timed out waiting for the list of GPUs");
  }
  if (llamaDevicesState() === "failed") throw new Error("llama-server couldn't list the GPUs (llama-server --list-devices failed)");
  const info = await modelInfo(file);
  const others = instances().filter((item) => item.id !== model.id && running(item));

  const alone = planLaunch(file, info);
  let plan: LaunchPlan = alone;
  if (others.length > 0) {
    const reserved = await reservedBy(others, alone.spec.device);
    const shared = planLaunch(file, info, reserved);
    // Fine as long as sharing costs nothing: it fits and keeps as many layers on the GPU.
    const sharesWell = shared.est.fits && shared.est.gpuLayers >= alone.est.gpuLayers;
    if (sharesWell) {
      plan = shared;
    } else {
      const blocking = others.filter(
        (item) =>
          !alone.spec.device || !shared.est.fits || (item.spec.device && card(item.spec.device) === card(alone.spec.device)),
      );
      const choice = await askMemory({
        model,
        blocking: blocking.length > 0 ? blocking : others,
        together:
          shared.est.fits && alone.spec.device
            ? { gpuLayers: shared.est.gpuLayers, totalLayers: shared.est.layers + 1 }
            : undefined,
        aloneLayers: alone.est.gpuLayers,
        noRoom: !shared.est.fits,
      });
      if (choice === "cancel") throw new LoadCancelled();
      if (choice === "together") plan = shared;
      else {
        for (const item of blocking.length > 0 ? blocking : others) await unloadModel(item.id);
        plan = alone;
      }
    }
  }
  return invoke<Instance>("llama_load", { spec: plan.spec });
}

export async function unloadModel(id: string) {
  await invoke("llama_unload", { id });
}

// --- idle unloading -----------------------------------------------------------------

const IDLE_KEY = "dd:llama-idle-minutes";
export const DEFAULT_IDLE_MINUTES = 10;

const readIdle = () => {
  const stored = Number(storage.getItem(IDLE_KEY));
  return storage.getItem(IDLE_KEY) !== null && Number.isFinite(stored) && stored >= 0 ? stored : DEFAULT_IDLE_MINUTES;
};
const [idleMinutes, setIdleMinutesSignal] = createSignal(readIdle());
/** Minutes without requests before a model is unloaded; 0 = never. */
export { idleMinutes };

export function setIdleMinutes(minutes: number) {
  setIdleMinutesSignal(minutes);
  storage.setItem(IDLE_KEY, String(minutes));
}

const IDLE_CHECK = 15_000;

if (inTauri) {
  setInterval(() => {
    const limit = idleMinutes() * 60_000;
    if (limit <= 0) return;
    const now = Date.now();
    for (const item of instances()) {
      if (item.state.kind === "ready" && item.active === 0 && now - item.lastUsed > limit && !loads.has(item.id)) {
        void unloadModel(item.id).catch(console.error);
      }
    }
  }, IDLE_CHECK);
}

// A failed start stays listed (with its error) until it's loaded again or dismissed in the
// loaded-models widget.
