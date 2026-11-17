import { invoke } from "@tauri-apps/api/core";
import { createSignal } from "solid-js";
import { hardware } from "./hardware";
import { modelKey, modelSetSize } from "./local-models";
import { modelDevice } from "./model-device";
import { llamaDevices } from "./providers";
import { storage } from "./storage";

/** What the GGUF header says about a model (see `gguf.rs`). */
export type ModelInfo = {
  architecture: string | null;
  contextLength: number | null;
  blockCount: number | null;
  embeddingLength: number | null;
  headCount: number | null;
  headCountKv: number[];
  keyLength: number | null;
  valueLength: number | null;
  slidingWindow: number | null;
  slidingWindowPattern: boolean[];
  kvLoraRank: number | null;
  ropeDimensionCount: number | null;
  vocabSize: number | null;
};

const SHARD = /-\d{5}-of-(\d{5})\.gguf$/i;
const infoCache = new Map<string, Promise<ModelInfo>>();

/** Header of a local model; a split model's metadata lives in its first part. */
export function modelInfo(name: string): Promise<ModelInfo> {
  const file = name.replace(SHARD, "-00001-of-$1.gguf");
  let info = infoCache.get(file);
  if (!info) {
    info = invoke<ModelInfo>("model_info", { name: file });
    info.catch(() => infoCache.delete(file));
    infoCache.set(file, info);
  }
  return info;
}

// --- settings -------------------------------------------------------------------

export type KvType = "f16" | "q8_0" | "q4_0";
export type FlashAttn = "auto" | "on" | "off";

/** Sampling for requests to a model; a missing field means the model's own recommendation (or llama-server's default). */
export type SamplingSettings = {
  temperature?: number;
  topK?: number;
  topP?: number;
  minP?: number;
  repeatPenalty?: number;
};

/** Per-model launch settings; a missing field means "auto" (the recommended value). */
export type LaunchSettings = { ctx?: number; ngl?: number; kv?: KvType; fa?: FlashAttn; threads?: number } & SamplingSettings;

const KEY = "dd:model-launch";

function load(): Record<string, LaunchSettings> {
  try {
    const parsed: unknown = JSON.parse(storage.getItem(KEY) ?? "{}");
    return parsed && typeof parsed === "object" ? (parsed as Record<string, LaunchSettings>) : {};
  } catch {
    return {};
  }
}

const [all, setAll] = createSignal(load());

function save(next: Record<string, LaunchSettings>) {
  setAll(next);
  if (Object.keys(next).length === 0) storage.removeItem(KEY);
  else storage.setItem(KEY, JSON.stringify(next));
}

export const launchSettings = (name: string): LaunchSettings => all()[modelKey(name)] ?? {};

/** Sets fields; `undefined` returns a field to auto. */
export function updateLaunchSettings(name: string, patch: LaunchSettings) {
  const key = modelKey(name);
  const next: LaunchSettings = { ...all()[key], ...patch };
  for (const field of Object.keys(next) as (keyof LaunchSettings)[]) {
    if (next[field] === undefined) delete next[field];
  }
  const { [key]: _, ...rest } = all();
  save(Object.keys(next).length > 0 ? { ...rest, [key]: next } : rest);
}

export function forgetLaunchSettings(name: string) {
  const { [modelKey(name)]: _, ...rest } = all();
  save(rest);
}

// --- estimate -------------------------------------------------------------------

const GIB = 1024 ** 3;
/** Video memory kept free for the desktop and other apps. */
const VRAM_RESERVE = 0.5 * GIB;
/** Share of RAM a model can take without starving the system. */
const RAM_SHARE = 0.75;
/** Tokens processed per step (llama-server's default physical batch). */
const UBATCH = 512;
/** Bytes per cached value. */
const KV_BYTES: Record<KvType, number> = { f16: 2, q8_0: 34 / 32, q4_0: 18 / 32 };

export const CONTEXT_STEPS = [2048, 4096, 8192, 16384, 32768, 65536, 131072, 262144, 524288, 1048576];
/** Beyond this a bigger context mostly costs memory and speed; it's not offered as the default. */
const GPU_CTX_CAP = 32768;
const CPU_CTX_CAP = 8192;
/** Below this, conversations get cut off quickly. */
export const SMALL_CTX = 4096;

/** Context sizes offered for a model: the standard steps up to what it was trained on. */
export function contextSteps(info: ModelInfo): number[] {
  const max = info.contextLength ?? 32768;
  const steps = CONTEXT_STEPS.filter((step) => step < max);
  return [...steps, max];
}

/** Where the model runs: a GPU (its memory) or the CPU. */
export type Target = { gpu: { memory: number } | null; ram: number };

export type Estimate = {
  layers: number;
  /** Layers on the GPU (llama.cpp counts the output layer too: layers + 1 = everything). */
  gpuLayers: number;
  /** Most layers that fit on the GPU with these settings. */
  maxGpuLayers: number;
  weights: number;
  kv: number;
  compute: number;
  /** What sits in video memory / RAM: weights, KV cache, buffers. */
  vramParts: [number, number, number];
  ramParts: [number, number, number];
  /** The weights and buffers fit on the GPU; only the KV cache pushes the model out. */
  kvOverflow: boolean;
  vramNeed: number;
  vram: number;
  ramNeed: number;
  ram: number;
  /** Everything on the GPU (or, for CPU, everything in RAM). */
  full: boolean;
  fits: boolean;
};

function layerCount(info: ModelInfo) {
  return info.blockCount ?? (info.headCountKv.length || 32);
}

/** Bytes of KV cache for `ctx` tokens. */
export function kvBytes(info: ModelInfo, ctx: number, kv: KvType): number {
  const layers = layerCount(info);
  const embd = info.embeddingLength ?? 4096;
  const heads = info.headCount ?? 32;
  const keyLength = info.keyLength ?? embd / heads;
  const valueLength = info.valueLength ?? embd / heads;
  // Without an explicit pattern: Gemma 2 alternates, Gemma 3 has 5 local layers per global one.
  const swaEvery = info.architecture?.startsWith("gemma3") ? 6 : info.architecture === "gemma2" ? 2 : 0;
  let values = 0;
  for (let layer = 0; layer < layers; layer++) {
    const perToken = info.kvLoraRank
      ? info.kvLoraRank + (info.ropeDimensionCount ?? 64)
      : (info.headCountKv[layer] ?? info.headCountKv[0] ?? heads) * (keyLength + valueLength);
    const swa =
      info.slidingWindow &&
      (info.slidingWindowPattern.length > 0
        ? info.slidingWindowPattern[layer]
        : swaEvery > 0 && (layer + 1) % swaEvery !== 0);
    const tokens = swa ? Math.min(ctx, (info.slidingWindow ?? ctx) + UBATCH) : ctx;
    values += perToken * tokens;
  }
  return values * KV_BYTES[kv];
}

/** Scratch buffers for one step: logits, activations and (without Flash Attention) attention scores. */
export function computeBytes(info: ModelInfo, ctx: number, flash: boolean): number {
  const vocab = info.vocabSize ?? 128000;
  const embd = info.embeddingLength ?? 4096;
  const heads = info.headCount ?? 32;
  const scores = flash ? 0 : heads * Math.min(ctx, 65536) * UBATCH * 4;
  return 0.15 * GIB + vocab * UBATCH * 4 + embd * UBATCH * 4 * 16 + scores;
}

export type Resolved = { ctx: number; kv: KvType; fa: FlashAttn; ngl: number | undefined };

export function estimate(info: ModelInfo, size: number, s: Resolved, target: Target): Estimate {
  const layers = layerCount(info);
  const flash = s.fa !== "off";
  const kv = kvBytes(info, s.ctx, s.kv);
  const compute = computeBytes(info, s.ctx, flash);
  const ram = target.ram * RAM_SHARE;
  const vram = target.gpu ? Math.max(0, target.gpu.memory - VRAM_RESERVE) : 0;

  const split = (gpuLayers: number) => {
    const share = Math.min(gpuLayers, layers + 1) / (layers + 1);
    const kvShare = Math.min(gpuLayers, layers) / layers;
    const vramParts: [number, number, number] = gpuLayers > 0 ? [size * share, kv * kvShare, compute] : [0, 0, 0];
    const ramParts: [number, number, number] = [
      size * (1 - share),
      kv * (1 - kvShare),
      gpuLayers > 0 ? 0.1 * GIB : compute,
    ];
    const sum = (parts: number[]) => parts.reduce((a, b) => a + b, 0);
    return { vramParts, ramParts, vramNeed: sum(vramParts), ramNeed: sum(ramParts) };
  };

  let maxGpuLayers = 0;
  if (target.gpu) {
    for (let n = layers + 1; n > 0; n--) {
      if (split(n).vramNeed <= vram) {
        maxGpuLayers = n;
        break;
      }
    }
  }
  const gpuLayers = target.gpu ? Math.min(s.ngl ?? maxGpuLayers, layers + 1) : 0;
  const { vramParts, ramParts, vramNeed, ramNeed } = split(gpuLayers);
  const full = target.gpu ? gpuLayers === layers + 1 && vramNeed <= vram : ramNeed <= ram;
  const all = split(layers + 1);
  return {
    vramParts,
    ramParts,
    kvOverflow: !!target.gpu && all.vramNeed > vram && all.vramNeed - all.vramParts[1] <= vram,
    layers,
    gpuLayers,
    maxGpuLayers,
    weights: size,
    kv,
    compute,
    vramNeed,
    vram,
    ramNeed,
    ram,
    full,
    fits: vramNeed <= vram && ramNeed <= ram,
  };
}

/** Largest context step that keeps the whole model on the device; undefined if even the smallest doesn't. */
export function maxFullContext(info: ModelInfo, size: number, s: Resolved, target: Target): number | undefined {
  let best: number | undefined;
  for (const ctx of contextSteps(info)) {
    if (estimate(info, size, { ...s, ctx, ngl: undefined }, target).full) best = ctx;
  }
  return best;
}

/** The default context: as much as fits on the device, but no more than is useful day to day. */
export function recommendedContext(info: ModelInfo, size: number, s: Resolved, target: Target): number {
  const steps = contextSteps(info);
  const cap = target.gpu ? GPU_CTX_CAP : CPU_CTX_CAP;
  const fit = maxFullContext(info, size, s, target);
  const below = steps.filter((step) => step <= cap);
  const capped = below[below.length - 1] ?? steps[0];
  return fit === undefined ? Math.min(capped, 8192) : Math.min(fit, capped);
}

// --- plan -----------------------------------------------------------------------

/** What the gateway's `llama_load` takes (mirrors `LaunchSpec` in gateway/llama.rs). */
export type LaunchSpec = {
  file: string;
  ctx: number;
  device: string | null;
  gpuLayers: number;
  kv: KvType;
  flashAttn: FlashAttn;
  threads: number | null;
};

export type LaunchPlan = {
  settings: LaunchSettings;
  device: ReturnType<typeof llamaDevices>[number] | undefined;
  target: Target;
  base: Resolved;
  recommended: number;
  resolved: Resolved;
  est: Estimate;
  spec: LaunchSpec;
};

/**
 * Everything about starting `name` with its saved settings on its chosen device: the resolved
 * values, the memory estimate and the spec to load it with. `reserved` is memory already taken
 * by other loaded models (subtracted from what this one may use).
 */
export function planLaunch(
  name: string,
  info: ModelInfo,
  reserved: { vram?: number; ram?: number } = {},
): LaunchPlan {
  const size = modelSetSize(name);
  const settings = launchSettings(name);
  const device = llamaDevices().find((item) => item.id === modelDevice(name));
  const target: Target = {
    gpu: device ? { memory: Math.max(0, device.memory - (reserved.vram ?? 0)) } : null,
    ram: Math.max(0, (hardware()?.ram ?? 0) - (reserved.ram ?? 0) / RAM_SHARE),
  };
  const steps = contextSteps(info);
  const base: Resolved = {
    ctx: 0,
    kv: settings.kv ?? "f16",
    fa: settings.fa ?? "auto",
    ngl: device ? settings.ngl : undefined,
  };
  const recommended = recommendedContext(info, size, base, target);
  const chosen = settings.ctx;
  const ctx = chosen && chosen <= (steps[steps.length - 1] ?? chosen) ? chosen : recommended;
  const resolved: Resolved = { ...base, ctx };
  const est = estimate(info, size, resolved, target);
  return {
    settings,
    device,
    target,
    base,
    recommended,
    resolved,
    est,
    spec: {
      file: name,
      ctx,
      device: device?.id ?? null,
      gpuLayers: device ? est.gpuLayers : 0,
      kv: resolved.kv,
      flashAttn: resolved.fa,
      threads: settings.threads ?? null,
    },
  };
}

/** Memory a running model takes, from the spec it was started with. */
export function runningUsage(info: ModelInfo, name: string, spec: LaunchSpec): { vram: number; ram: number } {
  const target: Target = { gpu: spec.device ? { memory: Number.MAX_SAFE_INTEGER } : null, ram: Number.MAX_SAFE_INTEGER };
  const est = estimate(
    info,
    modelSetSize(name),
    { ctx: spec.ctx, kv: spec.kv, fa: spec.flashAttn, ngl: spec.device ? spec.gpuLayers : undefined },
    target,
  );
  const sum = (parts: number[]) => parts.reduce((a, b) => a + b, 0);
  return { vram: spec.device ? sum(est.vramParts) : 0, ram: sum(est.ramParts) };
}

/** llama-server arguments for these settings (the model path and port are added at launch). */
export function launchArgs(settings: Resolved & { threads?: number }, device: string | null, gpuLayers: number) {
  const args = ["-c", String(settings.ctx)];
  if (device) args.push("--device", device, "-ngl", String(gpuLayers));
  else args.push("--device", "none", "-ngl", "0");
  if (settings.kv !== "f16") args.push("-ctk", settings.kv, "-ctv", settings.kv);
  args.push("-fa", settings.fa);
  if (settings.threads) args.push("-t", String(settings.threads));
  return args;
}
