import { invoke } from "@tauri-apps/api/core";
import { createResource, createRoot } from "solid-js";

export type Hardware = { ram: number; gpus: { name: string; vram: number }[] };

const GIB = 1024 ** 3;
/** Context (KV cache) and compute buffers on top of the weights, at a typical context size. */
const overhead = (size: number) => size * 0.15 + 1 * GIB;
/** Video memory kept free on each GPU for the desktop and other apps. */
const VRAM_RESERVE = 0.5 * GIB;
/** Share of RAM a model can take without starving the OS and other apps. */
const RAM_SHARE = 0.75;
/** Below this a GPU is an iGPU with a token carve-out: not worth offloading to. */
const MIN_VRAM = 2 * GIB;

const [hardware] = createRoot(() => createResource(() => invoke<Hardware>("hardware_info").catch(() => undefined)));
/** RAM and GPUs of this machine; undefined until known (or outside Tauri). */
export { hardware };

/** Shown only when the model fits neither all on GPU nor all on CPU... or splits between them. */
export type FitExtra = "partial" | "tight" | "none";

export type FitEstimate = {
  /** Fits entirely in video memory (all usable GPUs together). */
  gpu: boolean;
  /** Fits entirely in system RAM. */
  cpu: boolean;
  extra?: FitExtra;
  need: number;
  vram: number;
  ram: number;
  gpus: string[];
};

/** Rough estimate for a GGUF model of `size` bytes, with llama.cpp splitting layers across GPUs. */
export function estimateFit(size: number, hw: Hardware): FitEstimate {
  const gpus = hw.gpus.filter((gpu) => gpu.vram >= MIN_VRAM);
  const vram = gpus.reduce((sum, gpu) => sum + gpu.vram, 0);
  const usableVram = gpus.reduce((sum, gpu) => sum + Math.max(0, gpu.vram - VRAM_RESERVE), 0);
  const usableRam = hw.ram * RAM_SHARE;
  const need = size + overhead(size);

  const gpu = usableVram > 0 && need <= usableVram;
  const cpu = need <= usableRam;
  let extra: FitExtra | undefined;
  if (!gpu && usableVram > 0 && need <= usableVram + usableRam) extra = "partial";
  else if (!gpu && !cpu) extra = need <= hw.ram + vram ? "tight" : "none";
  return { gpu, cpu, extra, need, vram, ram: hw.ram, gpus: gpus.map((item) => item.name) };
}
