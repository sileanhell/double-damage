import { createSignal } from "solid-js";
import { api, inTauri, onCoreEvent, post } from "./core";

// --- types (mirror core/src/providers/claude.rs) -----------------------------------

export type ClaudeStatus = {
  signedIn: boolean;
  email: string | null;
  subscription: string | null;
  /** ms epoch the access token dies at. */
  expiresAt: number | null;
  /** Models the account's catalog currently lists. */
  models: number;
  /** A sign-in is waiting on the browser or a pasted code. */
  signingIn: boolean;
  /** The sign-in page of the flow in progress. */
  loginUrl: string | null;
  /** The sign-in URL whose callback shows the code for pasting. */
  manualUrl: string | null;
  /** The system block every request starts with (fixed, Anthropic requires it). */
  systemPrefix: string;
};

const BASE = "/dd/providers/claude";

const [claudeStatus, setClaudeStatus] = createSignal<ClaudeStatus>();
/** Sign-in state for the Claude provider; undefined until the core answers. */
export { claudeStatus };

export async function refreshClaude() {
  if (!inTauri) return;
  setClaudeStatus(await api<ClaudeStatus>(BASE));
}

/** Opens the Claude sign-in page; resolves when the browser flow lands or fails. */
export async function claudeLogin() {
  setClaudeStatus(await post<ClaudeStatus>(`${BASE}/login`, { open: true }));
}

/** Delivers a `code#state` the user pasted from the hosted callback page. */
export async function claudeSubmitCode(code: string) {
  setClaudeStatus(await post<ClaudeStatus>(`${BASE}/code`, { code }));
}

export async function claudeCancelLogin() {
  setClaudeStatus(await post<ClaudeStatus>(`${BASE}/login/cancel`));
}

export async function claudeLogout() {
  setClaudeStatus(await post<ClaudeStatus>(`${BASE}/logout`));
}

/** Re-fetches the account's model list. */
export async function claudeRefreshModels() {
  await post<{ count: number }>(`${BASE}/models/refresh`);
}

if (inTauri) {
  const refresh = () => void refreshClaude().catch(console.error);
  onCoreEvent("ready", refresh);
  // Sign-in state changes in the core.
  onCoreEvent<{ id: string }>("provider", (data) => data.id === "claude" && refresh());
}
