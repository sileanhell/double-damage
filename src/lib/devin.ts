import { createSignal } from "solid-js";
import { api, inTauri, onCoreEvent, post } from "./core";

// --- types (mirror core/src/providers/devin.rs) ------------------------------------

export type DevinStatus = {
  signedIn: boolean;
  email: string | null;
  models: number;
  /** A sign-in is waiting on the browser. */
  signingIn: boolean;
  /** The sign-in page of the flow in progress. */
  loginUrl: string | null;
};

const BASE = "/dd/providers/devin";

const [devinStatus, setDevinStatus] = createSignal<DevinStatus>();
/** Sign-in state for the Devin provider; undefined until the core answers. */
export { devinStatus };

export async function refreshDevin() {
  if (!inTauri) return;
  setDevinStatus(await api<DevinStatus>(BASE));
}

/** Opens the Devin sign-in page; resolves when the browser flow lands or fails. */
export async function devinLogin() {
  setDevinStatus(await post<DevinStatus>(`${BASE}/login`, { open: true }));
}

export async function devinCancelLogin() {
  setDevinStatus(await post<DevinStatus>(`${BASE}/login/cancel`));
}

export async function devinLogout() {
  setDevinStatus(await post<DevinStatus>(`${BASE}/logout`));
}

/** Re-fetches the account's model catalog. */
export async function devinRefreshModels() {
  await post<{ count: number }>(`${BASE}/models/refresh`);
}

if (inTauri) {
  const refresh = () => void refreshDevin().catch(console.error);
  onCoreEvent("ready", refresh);
  // Sign-in and the model list change in the core; the catalog re-reads from here.
  onCoreEvent<{ id: string }>("provider", (data) => data.id === "devin" && refresh());
}
