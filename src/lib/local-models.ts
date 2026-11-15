import { invoke } from "@tauri-apps/api/core";
import { createSignal } from "solid-js";
import { runTask } from "./tasks";

/** A model file in the app's own storage. */
export type LocalModel = { name: string; size: number; modified: number };

const [localModels, setLocalModels] = createSignal<LocalModel[]>([]);
export { localModels };

export const hasLocalModel = (name: string) => localModels().some((model) => model.name === name);

const SHARD = /^(.*)-\d{5}-of-(\d{5})\.gguf$/i;

/** Size of the whole model a file belongs to: a split GGUF counts all of its local parts. */
export function modelSetSize(name: string): number {
  const shard = name.match(SHARD);
  if (!shard) return localModels().find((model) => model.name === name)?.size ?? 0;
  return localModels()
    .filter((model) => {
      const other = model.name.match(SHARD);
      return other?.[1] === shard[1] && other[2] === shard[2];
    })
    .reduce((sum, model) => sum + model.size, 0);
}

/** One key for a whole model: the parts of a split GGUF share it. */
export function modelKey(name: string): string {
  const shard = name.match(SHARD);
  return shard ? `${shard[1]}.gguf` : name;
}

/** Multimodal projectors ride along with a model; they don't run on their own. */
export const isProjector = (name: string) => /(^|\/)mmproj/i.test(name);

export async function refreshLocalModels() {
  try {
    setLocalModels(await invoke<LocalModel[]>("models_list"));
  } catch (error) {
    console.error(error);
  }
}

export async function deleteLocalModel(name: string) {
  await invoke("model_delete", { name });
  await refreshLocalModels();
}

/**
 * Opens the native file picker and copies the chosen file into storage.
 * Resolves to false when the picker was dismissed. Rejects with "exists" for a duplicate name.
 */
export async function importLocalModel(labels: { filterName: string; allFilesName: string }): Promise<boolean> {
  const id = `import:${crypto.randomUUID()}`;
  try {
    const model = await runTask({ id, kind: "import", label: "" }, () =>
      invoke<LocalModel | null>("model_import", { id, ...labels }),
    );
    return model !== null;
  } finally {
    await refreshLocalModels();
  }
}

export const downloadTaskId = (repo: string, files: readonly string[]) => `download:${repo}/${files[0] ?? ""}`;

/** Downloads `files` (paths inside the repo) from Hugging Face into storage. */
export async function downloadModel(repo: string, files: readonly string[], total: number | undefined, name?: string) {
  const id = downloadTaskId(repo, files);
  const label = name ?? files[0]?.split("/").pop() ?? repo;
  try {
    await runTask({ id, kind: "download", label }, () =>
      invoke("model_download", { id, repo, files: [...files], total: total ?? null }),
    );
  } finally {
    await refreshLocalModels();
  }
}

void refreshLocalModels();
