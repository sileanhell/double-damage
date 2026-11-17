import { createResource, createRoot, createSignal } from "solid-js";
import { api, inTauri, onCoreEvent, post } from "./core";
import { runTask } from "./tasks";

export type ProviderId = "llama-server";

/** What a provider can run: judged from Hugging Face repo metadata and from local file names. */
export type Provider = {
  id: ProviderId;
  name: string;
  supportsRepo: (repo: { tags: readonly string[]; libraryName?: string }) => boolean;
  supportsFile: (fileName: string) => boolean;
};

const isGguf = (fileName: string) => fileName.toLowerCase().endsWith(".gguf");

/** Every known provider, installed or not. */
export const PROVIDERS: readonly Provider[] = [
  {
    id: "llama-server",
    name: "llama-server",
    supportsRepo: (repo) => repo.libraryName === "gguf" || repo.tags.includes("gguf"),
    supportsFile: isGguf,
  },
];

export const providersForRepo = (repo: Parameters<Provider["supportsRepo"]>[0]) =>
  PROVIDERS.filter((provider) => provider.supportsRepo(repo));
export const providersForFile = (fileName: string) => PROVIDERS.filter((provider) => provider.supportsFile(fileName));

// --- llama-server (installed and run by dd-core) -------------------------------

/** `backend`/`gpu`: the installed build, or the one an install would pick for this machine. */
export type LlamaServerStatus = {
  installed: boolean;
  version: string | null;
  backend?: string | null;
  gpu?: string | null;
};

export const LLAMA_SERVER_TASK = "llama-server";

const [llamaServer, setLlamaServer] = createSignal<LlamaServerStatus | undefined>();
/** Install state; undefined until the backend has answered. Installed means models can run through it. */
export { llamaServer };

export async function refreshLlamaServer() {
  try {
    setLlamaServer(await api<LlamaServerStatus>("/dd/llama"));
  } catch (error) {
    console.error(error);
    setLlamaServer({ installed: false, version: null });
  }
}

export async function installLlamaServer() {
  const status = await runTask({ id: LLAMA_SERVER_TASK, kind: "llama-server", label: "llama-server" }, () =>
    post<LlamaServerStatus>("/dd/llama/install"),
  );
  setLlamaServer(status);
}

export async function uninstallLlamaServer() {
  await post("/dd/llama/uninstall");
  await refreshLlamaServer();
}

if (inTauri) onCoreEvent("ready", () => void refreshLlamaServer());

/** A GPU as llama-server sees it through one backend (a card with CUDA and Vulkan is listed twice). */
export type LlamaDevice = { id: string; backend: string; name: string; memory: number };

const [devices] = createRoot(() =>
  createResource(
    () => {
      const status = llamaServer();
      return status?.installed ? `${status.version}:${status.backend}` : false;
    },
    // null = listing failed (told apart from "no GPUs").
    () => api<LlamaDevice[]>("/dd/llama/devices").catch((error): null => (console.error(error), null)),
  ),
);

/** GPUs the installed llama-server can run models on; empty when it isn't installed. */
export const llamaDevices = (): LlamaDevice[] => (llamaServer()?.installed ? (devices() ?? []) : []);

/**
 * Whether the device list is known: "ready", "pending" while llama-server is still listing them
 * (CUDA start-up can take seconds), "failed" when listing them didn't work.
 */
export function llamaDevicesState(): "ready" | "pending" | "failed" {
  const status = llamaServer();
  if (!status) return "pending";
  if (!status.installed) return "ready";
  if (devices.loading || devices.state === "unresolved") return "pending";
  return devices() === null ? "failed" : "ready";
}
