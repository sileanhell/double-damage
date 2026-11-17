import { Channel, invoke } from "@tauri-apps/api/core";
import { createResource, createRoot, createSignal } from "solid-js";
import { runTask } from "./tasks";

/**
 * Bash for the model's `bash` tool: a trimmed PortableGit installed from Settings. Once it's
 * installed the tool is available; every command runs in a fresh shell.
 */

export type BashStatus = { installed: boolean; version: string | null };

export const BASH_TASK = "bash";
const inTauri = "__TAURI_INTERNALS__" in window;

const [bashStatus, setBashStatus] = createSignal<BashStatus | undefined>();
/** Install state; undefined until the backend has answered. */
export { bashStatus };

/** The `bash` tool can be offered to the model. */
export const bashAvailable = () => !!bashStatus()?.installed;

export async function refreshBash() {
  if (!inTauri) return setBashStatus({ installed: false, version: null });
  try {
    setBashStatus(await invoke<BashStatus>("bash_status"));
  } catch (error) {
    console.error(error);
    setBashStatus({ installed: false, version: null });
  }
}

/** Installs the latest version, or updates to it. */
export async function installBash() {
  const status = await runTask({ id: BASH_TASK, kind: "bash", label: "bash" }, () => invoke<BashStatus>("bash_install"));
  setBashStatus(status);
  refetchLatest();
}

export async function uninstallBash() {
  await invoke("bash_uninstall");
  await refreshBash();
}

const [latestBash, { refetch: refetchLatest }] = createRoot(() =>
  createResource(
    () => (bashStatus()?.installed ? bashStatus()?.version : false),
    () => invoke<string>("bash_latest").catch((): null => null),
  ),
);
/** The newest version available (checked when installed), for the update button. */
export { latestBash };

void refreshBash();

// --- running ---------------------------------------------------------------------------

export type BashResult = {
  exitCode: number | null;
  timedOut: boolean;
  cancelled: boolean;
  /** stdout and stderr interleaved; the middle is cut when it's long. */
  output: string;
  omitted: number;
  durationMs: number;
};

export const DEFAULT_TIMEOUT_MS = 120_000;
export const MAX_TIMEOUT_MS = 600_000;

/**
 * Runs `command` in a fresh bash in `cwd` (a Windows path). `onOutput` gets the output live;
 * aborting `signal` kills the command and everything it started.
 */
export async function runBash(
  options: { command: string; cwd: string; timeoutMs?: number },
  onOutput?: (text: string) => void,
  signal?: AbortSignal,
): Promise<BashResult> {
  const id = crypto.randomUUID();
  const channel = new Channel<string>();
  if (onOutput) channel.onmessage = onOutput;
  const cancel = () => void invoke("bash_cancel", { id }).catch(console.error);
  signal?.addEventListener("abort", cancel, { once: true });
  try {
    return await invoke<BashResult>("bash_run", {
      request: { id, command: options.command, cwd: options.cwd, timeoutMs: options.timeoutMs ?? null },
      onOutput: channel,
    });
  } finally {
    signal?.removeEventListener("abort", cancel);
  }
}
