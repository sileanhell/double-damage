import { createResource, createRoot, createSignal } from "solid-js";
import { api, coreInfo, inTauri, onCoreEvent, post, restartCore } from "./core";
import { type FlashAttn, type KvType, type LaunchPlan, type LaunchSpec, modelInfo, planLaunch, runningUsage } from "./launch";
import { localModels } from "./local-models";
import { claudeStatus } from "./claude";
import { devinStatus } from "./devin";
import { hardware } from "./hardware";
import { llamaDevices, llamaDevicesState, llamaServer } from "./providers";
import { storage } from "./storage";
import { invoke } from "@tauri-apps/api/core";

// --- types (mirror core/src: catalog.rs, llama/runtime.rs, llama/files.rs) -------------

/** The local API: where dd-core listens and the token other apps use with it. */
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

/** A running model as the core reports it: every launch setting is optional there. */
type CoreInstance = Omit<Instance, "spec"> & {
  spec: {
    model: string;
    file: string;
    ctx: number | null;
    device: string | null;
    gpuLayers: number | null;
    kv: string | null;
    flashAttn: string | null;
    threads: number | null;
  };
  loadedContext: number | null;
};

/** A GGUF file in the models folder (`GET /dd/llama/models`). */
type ModelFile = {
  file: string;
  id: string;
  size: number;
  contextLength: number | null;
  capabilities: Capabilities;
  sampling: Sampling | null;
};

const isCpu = (device: string | null) => !device || device === "cpu" || device === "none";

/** The core's instance, in the shape the planner works with (CPU = no device). */
function fromCore(item: CoreInstance): Instance {
  const spec = item.spec;
  return {
    ...item,
    spec: {
      file: spec.file,
      ctx: spec.ctx ?? item.loadedContext ?? 0,
      device: isCpu(spec.device) ? null : spec.device,
      gpuLayers: spec.gpuLayers ?? (isCpu(spec.device) ? 0 : 999),
      kv: (spec.kv ?? "f16") as KvType,
      flashAttn: (spec.flashAttn ?? "auto") as FlashAttn,
      threads: spec.threads,
    },
  };
}

// --- gateway endpoint ---------------------------------------------------------------

/** Where the gateway listens and its token; undefined until the core has started (or failed). */
export const gatewayInfo = (): GatewayInfo | undefined => {
  const info = coreInfo();
  return info && { url: info.url, port: info.port, token: info.token, error: info.error };
};

export async function refreshGatewayInfo() {
  // Pushed by the shell (`core-info` events); nothing to ask.
}

/** Restarts the core on the port saved in settings (`GATEWAY_PORT_KEY`). */
export async function restartGateway() {
  await restartCore();
}

export async function regenerateGatewayToken() {
  const { token } = await post<{ token: string }>("/dd/token/regenerate");
  await invoke("core_set_token", { token });
}

export const GATEWAY_PORT_KEY = "dd:gateway-port";
export const DEFAULT_GATEWAY_PORT = 11488;

// --- running models -----------------------------------------------------------------

const [instances, setInstances] = createSignal<Instance[]>([]);
/** Model servers that are loading, ready or failed. */
export { instances };

if (inTauri) {
  // The event stream starts with the full list and sends it again after every change.
  onCoreEvent<CoreInstance[]>("instances", (list) => setInstances(list.map(fromCore)));
}

export const instance = (id: string) => instances().find((item) => item.id === id);

// --- catalog ------------------------------------------------------------------------

/** Changes only when models appear, go away or change state (not on every request). */
const instanceShape = () =>
  instances()
    .map((item) => `${item.id}:${item.state.kind}`)
    .join("|");

/** The core's catalog lists local models once they run; the models folder adds the rest. */
async function buildCatalog(): Promise<Catalog> {
  const [catalog, files] = await Promise.all([api<Catalog>("/dd/catalog"), api<ModelFile[]>("/dd/llama/models")]);
  const llama = catalog.providers.find((provider) => provider.id === "llama-server");
  if (llama) {
    const listed = new Set(llama.models.map((model) => model.id));
    for (const file of files) {
      if (listed.has(file.id)) continue;
      llama.models.push({
        id: file.id,
        name: file.id.replace(/^llama-server\//, ""),
        provider: "llama-server",
        state: "unloaded",
        file: file.file,
        size: file.size,
        contextLength: file.contextLength,
        loadedContext: null,
        capabilities: file.capabilities,
        sampling: file.sampling,
        error: null,
      });
    }
    llama.models.sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()));
  }
  return catalog;
}

const [catalog, { refetch: refetchCatalog }] = createRoot(() => {
  const [resource, actions] = createResource(
    () =>
      inTauri && coreInfo()?.url
        ? `${coreInfo()?.url}#${instanceShape()}#${localModels().length}#${llamaServer()?.installed}#${devinStatus()?.signedIn}:${devinStatus()?.models}#${claudeStatus()?.signedIn}:${claudeStatus()?.models}`
        : false,
    buildCatalog,
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
  // The core leaves out what isn't given; CPU has to be asked for.
  const spec = { ...plan.spec, model: plan.spec.file, device: plan.spec.device ?? "cpu" };
  return fromCore(await post<CoreInstance>("/dd/llama/load", spec));
}

export async function unloadModel(id: string) {
  await post("/dd/llama/unload", { id });
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
