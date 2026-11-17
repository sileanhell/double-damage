import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createSignal } from "solid-js";

/** Long-running backend job (install, download, import) as reported by `task-progress` events. */
export type Task = {
  id: string;
  kind: "llama-server" | "bash" | "download" | "import";
  label: string;
  received: number;
  total: number | null;
  /** Smoothed transfer rate in bytes per second; undefined until it can be measured. */
  speed?: number;
  /** When `received` was last updated (performance.now()), for measuring `speed`. */
  at?: number;
};

const [tasks, setTasks] = createSignal<Record<string, Task>>({});

export const task = (id: string): Task | undefined => tasks()[id];
export const tasksOfKind = (...kinds: Task["kind"][]) => Object.values(tasks()).filter((item) => kinds.includes(item.kind));

/** Fraction done in 0..1, or undefined while the size is unknown. */
export const taskProgress = (item: Task) => (item.total ? Math.min(1, item.received / item.total) : undefined);

const put = (item: Task) => setTasks((all) => ({ ...all, [item.id]: item }));
const drop = (id: string) =>
  setTasks((all) => {
    const { [id]: _, ...rest } = all;
    return rest;
  });

/** Weight of the newest sample in the speed average; lower is steadier. */
const SPEED_SMOOTHING = 0.25;

void listen<Task>("task-progress", (event) => {
  const previous = tasks()[event.payload.id];
  // Ignore late events from a task that has already settled.
  if (!previous) return;
  const now = performance.now();
  let speed = previous.speed;
  if (previous.at !== undefined && now > previous.at && event.payload.received >= previous.received) {
    const sample = ((event.payload.received - previous.received) * 1000) / (now - previous.at);
    speed = speed === undefined ? sample : speed + SPEED_SMOOTHING * (sample - speed);
  }
  // A download keeps the name it was started with (a split model is named after the whole set).
  const label = previous.kind === "download" && previous.label ? previous.label : event.payload.label;
  put({ ...event.payload, label, speed, at: now });
}).catch(() => {
  // Not running inside Tauri (plain web dev server).
});

/** The backend's way of saying a task stopped because the user cancelled it. */
export const isCancelled = (error: unknown) => error === "cancelled";

/** Tracks `id` as a running task until `run` settles; progress events fill in the details. */
export async function runTask<T>(seed: Pick<Task, "id" | "kind" | "label">, run: () => Promise<T>): Promise<T> {
  put({ ...seed, received: 0, total: null });
  try {
    return await run();
  } finally {
    drop(seed.id);
  }
}

export function cancelTask(id: string) {
  void invoke("task_cancel", { id }).catch(console.error);
}
