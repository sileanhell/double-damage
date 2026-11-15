import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

/**
 * Synchronous key/value settings, like localStorage, but kept in `settings.json` in the app data
 * folder. The backend injects the whole map as `window.__DD_STORE__` before the page loads;
 * writes are batched and sent back. Outside Tauri (plain web dev server) it falls back to
 * localStorage.
 */
declare global {
  interface Window {
    __DD_STORE__?: Record<string, string>;
  }
}

const native = window.__DD_STORE__ !== undefined;
const data = new Map(Object.entries(window.__DD_STORE__ ?? {}));
const pending = new Map<string, string | null>();
let timer: number | undefined;
let inflight: Promise<void> = Promise.resolve();

const MIGRATED = "dd:migrated-from-webview";
const FLUSH_DELAY = 250;

function schedule() {
  clearTimeout(timer);
  timer = window.setTimeout(() => void flushStorage(), FLUSH_DELAY);
}

/** Sends pending writes now; resolves once they are on disk. */
export function flushStorage(): Promise<void> {
  clearTimeout(timer);
  if (!native || pending.size === 0) return inflight;
  const changes = Object.fromEntries(pending);
  pending.clear();
  inflight = inflight.then(() => invoke<void>("store_set", { changes })).catch(console.error);
  return inflight;
}

export const storage = {
  getItem(key: string): string | null {
    return native ? (data.get(key) ?? null) : localStorage.getItem(key);
  },
  setItem(key: string, value: string) {
    if (!native) return localStorage.setItem(key, value);
    if (data.get(key) === value) return;
    data.set(key, value);
    pending.set(key, value);
    schedule();
  },
  removeItem(key: string) {
    if (!native) return localStorage.removeItem(key);
    if (!data.has(key)) return;
    data.delete(key);
    pending.set(key, null);
    schedule();
  },
};

if (native) {
  // Settings used to live in the webview's localStorage; carry them over once.
  if (!data.has(MIGRATED)) {
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key?.startsWith("dd:") && !data.has(key)) storage.setItem(key, localStorage.getItem(key) ?? "");
    }
    storage.setItem(MIGRATED, "1");
  }
  // Don't lose the last batch when the window closes.
  void getCurrentWindow()
    .onCloseRequested(() => flushStorage())
    .catch(console.error);
}
