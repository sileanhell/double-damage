import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { createSignal } from "solid-js";
import { storage } from "./storage";

export type Background = { src: string; kind: "image" | "video" };

export type PresetId = "lines" | "glass" | "amber" | "spiral" | "glare" | "prism" | "bubbles" | "dunes";

/** `preview` is a 240p copy of the video for the settings thumbnails. */
export type BackgroundPreset = { id: PresetId; preview: string; background: Background };

const preset = (id: PresetId): BackgroundPreset => ({
  id,
  preview: `/backgrounds/preview/${id}.webm`,
  background: { src: `/backgrounds/${id}.webm`, kind: "video" },
});

/** Bundled backgrounds; the first one is the default. */
export const PRESETS: readonly BackgroundPreset[] = [
  preset("lines"),
  preset("glass"),
  preset("amber"),
  preset("spiral"),
  preset("glare"),
  preset("prism"),
  preset("bubbles"),
  preset("dunes"),
];

export const ACCEPTED_TYPES = "image/*,video/mp4,video/webm";

// The file itself is stored by the backend in `<data>/background/`; this flag only tells startup
// whether to wait for it instead of flashing the default.
const CUSTOM_FLAG = "dd:background-custom";
const PRESET_KEY = "dd:background-preset";
// Where older versions kept the file (inside the webview profile); read once to migrate.
const LEGACY_DB = "dd";
const LEGACY_STORE = "background";
const LEGACY_RECORD = "current";
const FALLBACK_ACCENT = { h: 0, s: 0, l: 1 };

const hasCustom = storage.getItem(CUSTOM_FLAG) === "1";
const storedPreset = PRESETS.find((p) => p.id === storage.getItem(PRESET_KEY)) ?? PRESETS[0];
const [background, setBackgroundSignal] = createSignal<Background | null>(hasCustom ? null : storedPreset.background);

export { background };

const [readySrc, setReadySrc] = createSignal<string | null>(null);

/** True once the current background is resolved and its media is ready to paint. */
export const backgroundReady = () => {
  const current = background();
  return current !== null && current.src === readySrc();
};

/** Called by <BackgroundMedia> when the element can render — also on error, so startup never stalls. */
export function markBackgroundReady(src: string) {
  setReadySrc(src);
}

/** Id of the active preset, or null while a user file is the background. */
export const activePresetId = () => PRESETS.find((p) => p.background === background())?.id ?? null;

// --- blur ------------------------------------------------------------------

const BLUR_KEY = "dd:background-blur";
export const BLUR_MAX = 64;
export const BLUR_DEFAULT = 0;

const clampBlur = (value: number) => Math.min(BLUR_MAX, Math.max(0, Math.round(value)));
const storedBlur = Number.parseFloat(storage.getItem(BLUR_KEY) ?? "");
const [backgroundBlur, setBlurSignal] = createSignal(Number.isFinite(storedBlur) ? clampBlur(storedBlur) : BLUR_DEFAULT);

/** Blur radius (px) applied to the app backdrop. */
export { backgroundBlur };

export function setBackgroundBlur(px: number) {
  const next = clampBlur(px);
  setBlurSignal(next);
  storage.setItem(BLUR_KEY, String(next));
}

function kindOf(type: string): Background["kind"] | null {
  if (type === "video/mp4" || type === "video/webm") return "video";
  if (type.startsWith("image/")) return "image";
  return null;
}

const VIDEO_EXTENSIONS = ["mp4", "webm"];
const kindOfPath = (path: string): Background["kind"] =>
  VIDEO_EXTENSIONS.includes(path.split(".").pop()?.toLowerCase() ?? "") ? "video" : "image";

// --- storage ---------------------------------------------------------------

/** Hands the file to the backend, which keeps it in the app data folder. */
async function saveFile(file: Blob) {
  const bytes = new Uint8Array(await file.arrayBuffer());
  await invoke("background_save", bytes, { headers: { "x-mime": file.type } });
}

/** The background a previous version kept in IndexedDB, if any; the store is dropped after. */
function takeLegacyFile(): Promise<Blob | undefined> {
  return new Promise((resolve) => {
    const open = indexedDB.open(LEGACY_DB);
    open.onerror = () => resolve(undefined);
    open.onsuccess = () => {
      const db = open.result;
      if (!db.objectStoreNames.contains(LEGACY_STORE)) {
        db.close();
        return resolve(undefined);
      }
      const request = db.transaction(LEGACY_STORE).objectStore(LEGACY_STORE).get(LEGACY_RECORD);
      request.onsuccess = () => {
        db.close();
        indexedDB.deleteDatabase(LEGACY_DB);
        resolve(request.result instanceof Blob ? request.result : undefined);
      };
      request.onerror = () => {
        db.close();
        resolve(undefined);
      };
    };
  });
}

// --- accent ----------------------------------------------------------------

type Hsl = { h: number; s: number; l: number };

function rgbToHsl(r: number, g: number, b: number): Hsl {
  r /= 255;
  g /= 255;
  b /= 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  const d = max - min;
  if (d === 0) return { h: 0, s: 0, l };
  const s = d / (1 - Math.abs(2 * l - 1));
  let h: number;
  if (max === r) h = ((g - b) / d) % 6;
  else if (max === g) h = (b - r) / d + 2;
  else h = (r - g) / d + 4;
  return { h: (h * 60 + 360) % 360, s, l };
}

function luminance({ h, s, l }: Hsl) {
  const channel = (n: number) => {
    const k = (n + h / 30) % 12;
    const c = l - s * Math.min(l, 1 - l) * Math.max(-1, Math.min(k - 3, 9 - k, 1));
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(0) + 0.7152 * channel(8) + 0.0722 * channel(4);
}

async function loadFrame({ src, kind }: Background): Promise<CanvasImageSource> {
  if (kind === "image") {
    const image = new Image();
    image.crossOrigin = "anonymous";
    image.src = src;
    await image.decode();
    return image;
  }

  const video = document.createElement("video");
  video.crossOrigin = "anonymous";
  video.muted = true;
  video.preload = "auto";
  const once = (event: string) =>
    new Promise<void>((resolve, reject) => {
      video.addEventListener(event, () => resolve(), { once: true });
      video.addEventListener("error", () => reject(video.error), { once: true });
    });
  video.src = src;
  await once("loadeddata");
  // Sample a quarter in: the very first frame is often a fade-in.
  if (Number.isFinite(video.duration) && video.duration > 0) {
    video.currentTime = video.duration * 0.25;
    await once("seeked");
  }
  return video;
}

/** Picks the dominant vivid hue of the media and normalizes it to a UI-friendly lightness. */
async function extractAccent(target: Background): Promise<Hsl> {
  const frame = await loadFrame(target);

  const size = 64;
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = size;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx) return FALLBACK_ACCENT;
  ctx.drawImage(frame, 0, 0, size, size);
  const { data } = ctx.getImageData(0, 0, size, size);

  const BINS = 24;
  const bins = Array.from({ length: BINS }, () => ({ weight: 0, x: 0, y: 0, s: 0 }));
  for (let i = 0; i < data.length; i += 4) {
    const { h, s, l } = rgbToHsl(data[i], data[i + 1], data[i + 2]);
    if (s < 0.15 || l < 0.08 || l > 0.95) continue;
    // Chroma: favors pixels that are both saturated and neither too dark nor too bright.
    const weight = s * (1 - Math.abs(2 * l - 1));
    const bin = bins[Math.floor(h / (360 / BINS)) % BINS];
    const rad = (h * Math.PI) / 180;
    bin.weight += weight;
    bin.x += Math.cos(rad) * weight;
    bin.y += Math.sin(rad) * weight;
    bin.s += s * weight;
  }

  const best = bins.reduce((a, b) => (b.weight > a.weight ? b : a));
  if (best.weight === 0) return FALLBACK_ACCENT; // grayscale media
  const h = ((Math.atan2(best.y, best.x) * 180) / Math.PI + 360) % 360;
  const s = Math.min(0.9, Math.max(0.5, best.s / best.weight));
  return { h, s, l: 0.62 };
}

let accentRequest = 0;

async function refreshAccent(target: Background) {
  const request = ++accentRequest;
  const color = await extractAccent(target).catch(() => FALLBACK_ACCENT);
  if (request !== accentRequest) return;
  const css = `hsl(${color.h.toFixed(0)} ${(color.s * 100).toFixed(0)}% ${(color.l * 100).toFixed(0)}%)`;
  const root = document.documentElement.style;
  root.setProperty("--accent", css);
  root.setProperty("--accent-fg", luminance(color) > 0.4 ? "#000" : "#fff");
}

// --- public API ------------------------------------------------------------

function apply(next: Background) {
  const previous = background();
  setBackgroundSignal(next);
  if (previous?.src.startsWith("blob:")) URL.revokeObjectURL(previous.src);
  void refreshAccent(next);
}

/**
 * Sets a user image or mp4/webm video as the background.
 * Throws on unsupported types; returns false when the file was applied but could not be persisted.
 */
export async function setBackgroundFromFile(file: File): Promise<boolean> {
  const kind = kindOf(file.type);
  if (!kind) throw new Error(`Unsupported background type: ${file.type}`);

  apply({ src: URL.createObjectURL(file), kind });
  try {
    await saveFile(file);
    storage.setItem(CUSTOM_FLAG, "1");
    return true;
  } catch (error) {
    console.error(error);
    return false;
  }
}

/** Switches to a bundled preset, dropping any stored user file. */
export function setBackgroundPreset(id: string) {
  const next = PRESETS.find((p) => p.id === id);
  if (!next || next.background === background()) return;
  if (storage.getItem(CUSTOM_FLAG) === "1") {
    storage.removeItem(CUSTOM_FLAG);
    invoke("background_clear").catch(console.error);
  }
  if (next === PRESETS[0]) storage.removeItem(PRESET_KEY);
  else storage.setItem(PRESET_KEY, next.id);
  apply(next.background);
}

async function restore() {
  const path = await invoke<string | null>("background_path").catch(() => null);
  // The query only defeats the webview cache when the file was replaced; the backend ignores it.
  if (path) return apply({ src: `${convertFileSrc(path)}?v=${Date.now()}`, kind: kindOfPath(path) });

  const legacy = await takeLegacyFile();
  const kind = legacy && kindOf(legacy.type);
  if (legacy && kind) {
    apply({ src: URL.createObjectURL(legacy), kind });
    await saveFile(legacy).catch(console.error);
    return;
  }
  storage.removeItem(CUSTOM_FLAG);
  apply(storedPreset.background);
}

if (hasCustom) void restore();
else void refreshAccent(storedPreset.background);
