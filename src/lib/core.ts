import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createSignal } from "solid-js";

// The app is a client of dd-core: the desktop shell starts it and tells the page where it
// listens; everything else (providers, sign-ins, local models) goes over its HTTP API.

/** Mirrors `CoreInfo` in src-tauri/src/core_process.rs. */
export type CoreInfo = {
  url: string | null;
  port: number;
  token: string;
  error: string | null;
  version: string | null;
  exe: string | null;
};

export const inTauri = "__TAURI_INTERNALS__" in window;

const [coreInfo, setCoreInfo] = createSignal<CoreInfo>();
/** Where the core listens and its token; undefined until the shell answers. */
export { coreInfo };

/** Resolves once the core is up (or immediately rejects with why it isn't). */
let waiters: { resolve: (info: CoreInfo) => void; reject: (error: string) => void }[] = [];

function update(info: CoreInfo) {
  const previous = coreInfo();
  setCoreInfo(info);
  if (info.url) {
    for (const waiter of waiters) waiter.resolve(info);
    waiters = [];
    connectEvents(info);
    // A (re)started core: whoever mirrors its state should ask again.
    if (previous?.url !== info.url) for (const handler of handlers.get("ready") ?? []) handler(info);
  } else if (info.error) {
    for (const waiter of waiters) waiter.reject(info.error);
    waiters = [];
  }
}

if (inTauri) {
  void listen<CoreInfo>("core-info", (event) => update(event.payload)).catch(console.error);
  void invoke<CoreInfo>("core_info").then(update).catch(console.error);
}

function ready(): Promise<CoreInfo> {
  const info = coreInfo();
  if (info?.url) return Promise.resolve(info);
  if (!inTauri) return Promise.reject("dd-core is only available in the desktop app");
  return new Promise((resolve, reject) => waiters.push({ resolve, reject }));
}

/** Restarts the core (it re-reads the port from the saved settings). */
export async function restartCore() {
  update(await invoke<CoreInfo>("core_restart"));
}

/**
 * Calls the core's API. Rejects with the error message as a string (like Tauri commands do),
 * so callers can show it as is.
 */
export async function api<T>(path: string, init: { method?: string; body?: unknown } = {}): Promise<T> {
  const info = await ready();
  let response: Response;
  try {
    response = await fetch(`${info.url}${path}`, {
      method: init.method ?? (init.body === undefined ? "GET" : "POST"),
      headers: {
        authorization: `Bearer ${info.token}`,
        ...(init.body === undefined ? {} : { "content-type": "application/json" }),
      },
      body: init.body === undefined ? undefined : JSON.stringify(init.body),
    });
  } catch (error) {
    throw `dd-core is unreachable: ${String(error)}`;
  }
  const text = await response.text();
  const data = text ? (JSON.parse(text) as unknown) : undefined;
  if (!response.ok) {
    const message = (data as { error?: { message?: string } } | undefined)?.error?.message;
    throw message ?? `HTTP ${response.status}`;
  }
  return data as T;
}

export const post = <T>(path: string, body: unknown = {}) => api<T>(path, { body });

// --- events ----------------------------------------------------------------------

type Handler = (data: unknown) => void;
const handlers = new Map<string, Set<Handler>>();

/**
 * Subscribes to an event of the core's `/dd/events` stream (`instances`, `provider`, `task`,
 * `token`), or to `ready`: the core (re)started and is reachable.
 */
export function onCoreEvent<T>(name: string, handler: (data: T) => void): () => void {
  let set = handlers.get(name);
  if (!set) handlers.set(name, (set = new Set()));
  set.add(handler as Handler);
  return () => set.delete(handler as Handler);
}

let stream: AbortController | undefined;
let streamUrl: string | undefined;

/** One stream per running core; reconnects while that core is up. */
function connectEvents(info: CoreInfo) {
  if (!info.url || (streamUrl === info.url && stream)) return;
  stream?.abort();
  const controller = new AbortController();
  stream = controller;
  streamUrl = info.url;
  void (async () => {
    while (!controller.signal.aborted) {
      try {
        const current = coreInfo();
        if (!current?.url) break;
        const response = await fetch(`${current.url}/dd/events`, {
          headers: { authorization: `Bearer ${current.token}` },
          signal: controller.signal,
        });
        if (!response.ok || !response.body) throw new Error(`HTTP ${response.status}`);
        const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
        let buffer = "";
        for (;;) {
          const { value, done } = await reader.read();
          if (done) break;
          buffer += value;
          let end: number;
          while ((end = buffer.indexOf("\n\n")) >= 0) {
            const block = buffer.slice(0, end);
            buffer = buffer.slice(end + 2);
            let name = "message";
            let data = "";
            for (const line of block.split("\n")) {
              if (line.startsWith("event:")) name = line.slice(6).trim();
              else if (line.startsWith("data:")) data += line.slice(5).trimStart();
            }
            if (!data) continue;
            let parsed: unknown;
            try {
              parsed = JSON.parse(data);
            } catch {
              continue;
            }
            for (const handler of handlers.get(name) ?? []) handler(parsed);
          }
        }
      } catch (error) {
        if (controller.signal.aborted) break;
        console.error("dd-core events:", error);
      }
      await new Promise((r) => setTimeout(r, 1000));
    }
    if (stream === controller) {
      stream = undefined;
      streamUrl = undefined;
    }
  })();
}

// --- accounts ----------------------------------------------------------------------

/** Mirrors `providers::Account` in core/src/providers/mod.rs. */
export type Account = {
  provider: string;
  email: string | null;
  organization: string | null;
  plan: {
    name: string | null;
    tier: string | null;
    billing: string | null;
    startedAt: string | null;
    expiresAt: string | null;
    trialEndsAt: string | null;
  };
  balance: {
    enabled: boolean;
    currency: string | null;
    used: number | null;
    limit: number | null;
    remaining: number | null;
  } | null;
  limits: { id: string; label: string; usedPercent: number | null; resetsAt: string | null; status: string | null }[];
  fetchedAt: number;
};

/** Plan, balance and usage limits of a provider's account. */
export const fetchAccount = (provider: string, fresh = false) =>
  api<Account>(`/dd/providers/${provider}/account${fresh ? "?refresh=1" : ""}`);
