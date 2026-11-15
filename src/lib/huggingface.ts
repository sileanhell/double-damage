const API = "https://huggingface.co/api";

/** A model repo from the Hub search. */
export type HfModel = {
  id: string;
  downloads: number;
  likes: number;
  tags: string[];
  libraryName?: string;
  pipelineTag?: string;
};

/** One downloadable model: a single file, or every shard of a split GGUF. */
export type HfModelFile = { name: string; files: string[]; size: number };

type ApiModel = {
  id: string;
  downloads?: number;
  likes?: number;
  tags?: string[];
  library_name?: string;
  pipeline_tag?: string;
};
type ApiTreeEntry = { type: string; path: string; size?: number; lfs?: { size?: number } };

/** Pipelines of models you chat with; "" = the repo doesn't say (common for GGUF re-uploads). */
const CHAT_PIPELINES = new Set(["text-generation", "image-text-to-text", ""]);

/** The popular list barely moves within a day; it's cached in the webview's localStorage. */
const POPULAR_CACHE = "dd:hf-popular";
const POPULAR_TTL = 24 * 60 * 60 * 1000;

type PopularCache = { at: number; models: HfModel[] };

function readPopularCache(): PopularCache | undefined {
  try {
    const cache = JSON.parse(localStorage.getItem(POPULAR_CACHE) ?? "null") as PopularCache | null;
    return cache && Array.isArray(cache.models) ? cache : undefined;
  } catch {
    return undefined;
  }
}

/**
 * Hub search; an empty query gives the most popular GGUF chat models of the last 30 days
 * (the Hub's `downloads` counts the last 30 days), refreshed at most once a day.
 */
export async function searchModels(query: string, signal?: AbortSignal): Promise<HfModel[]> {
  if (query) return fetchModels({ search: query, limit: "30", sort: "downloads", direction: "-1" }, signal);

  const cache = readPopularCache();
  if (cache && Date.now() - cache.at < POPULAR_TTL) return cache.models;
  try {
    // Not the top of the whole Hub (embeddings, Whisper, GPT-2 — nothing that runs here).
    const models = (await fetchModels({ filter: "gguf", limit: "60", sort: "downloads", direction: "-1" }, signal))
      .filter((model) => CHAT_PIPELINES.has(model.pipelineTag ?? ""))
      .slice(0, 30);
    localStorage.setItem(POPULAR_CACHE, JSON.stringify({ at: Date.now(), models } satisfies PopularCache));
    return models;
  } catch (error) {
    // Offline: an old list beats an error.
    if (cache && !signal?.aborted) return cache.models;
    throw error;
  }
}

async function fetchModels(params: Record<string, string>, signal?: AbortSignal): Promise<HfModel[]> {
  const response = await fetch(`${API}/models?${new URLSearchParams(params)}`, { signal });
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  const body: ApiModel[] = await response.json();
  return body.map((model) => ({
    id: model.id,
    downloads: model.downloads ?? 0,
    likes: model.likes ?? 0,
    tags: model.tags ?? [],
    libraryName: model.library_name,
    pipelineTag: model.pipeline_tag,
  }));
}

const SHARD = /^(.*)-(\d{5})-of-(\d{5})\.gguf$/i;

/**
 * Files of a repo that `keep` accepts, with split GGUFs (`name-00001-of-00003.gguf`) merged
 * into a single entry so they are downloaded together.
 */
export async function modelFiles(repo: string, keep: (path: string) => boolean): Promise<HfModelFile[]> {
  const path = repo.split("/").map(encodeURIComponent).join("/");
  const response = await fetch(`${API}/models/${path}/tree/main?recursive=true`);
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  const entries: ApiTreeEntry[] = await response.json();

  const groups = new Map<string, HfModelFile>();
  for (const entry of entries) {
    if (entry.type !== "file" || !keep(entry.path)) continue;
    const size = entry.lfs?.size ?? entry.size ?? 0;
    const shard = entry.path.match(SHARD);
    const key = shard ? `${shard[1]}|${shard[3]}` : entry.path;
    const group = groups.get(key);
    if (group) {
      group.files.push(entry.path);
      group.size += size;
    } else {
      groups.set(key, { name: shard ? `${shard[1]}.gguf` : entry.path, files: [entry.path], size });
    }
  }
  return [...groups.values()]
    .map((group) => ({ ...group, files: group.files.sort() }))
    .sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }));
}
