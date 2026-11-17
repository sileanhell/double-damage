import { createSignal } from "solid-js";
import type { MessageKey } from "./i18n";
import { storage } from "./storage";
import type { ToolKind } from "./tools";

/**
 * How much the agent may do without asking:
 * - `ask`: every tool call waits for the user;
 * - `read`: reading and searching run at once, edits and bash ask;
 * - `edits`: reading and editing run at once, bash asks;
 * - `auto`: everything runs at once (not offered for local models).
 * Whatever the mode, tools only ever reach files inside the chat's workspace folders.
 */
export type ApprovalMode = "ask" | "read" | "edits" | "auto";

export type ApprovalInfo = { value: ApprovalMode; label: MessageKey; description: MessageKey };

export const APPROVAL_MODES: readonly ApprovalInfo[] = [
  { value: "ask", label: "approval.ask", description: "approval.askHint" },
  { value: "read", label: "approval.read", description: "approval.readHint" },
  { value: "edits", label: "approval.edits", description: "approval.editsHint" },
  { value: "auto", label: "approval.auto", description: "approval.autoHint" },
];

const APPROVAL_KEY = "dd:approval";
const DEFAULT_MODE: ApprovalMode = "read";
/** Earlier names of the modes. */
const LEGACY: Record<string, ApprovalMode> = { "read-only": "read", tools: "edits", off: "auto" };

const findMode = (value: string | null) => APPROVAL_MODES.find((item) => item.value === (LEGACY[value ?? ""] ?? value));

const [mode, setModeSignal] = createSignal<ApprovalMode>(findMode(storage.getItem(APPROVAL_KEY))?.value ?? DEFAULT_MODE);

/** Modes to offer: no `auto` for local models. */
export const approvalModes = (local: boolean) => APPROVAL_MODES.filter((item) => !local || item.value !== "auto");

/** The chosen mode, as it applies to a local or remote model. */
export function approvalMode(local: boolean): ApprovalMode {
  return local && mode() === "auto" ? "edits" : mode();
}

export const approvalInfo = (local: boolean) => findMode(approvalMode(local)) ?? findMode(DEFAULT_MODE)!;

export function setApprovalMode(value: ApprovalMode) {
  if (!findMode(value)) return;
  setModeSignal(value);
  storage.setItem(APPROVAL_KEY, value);
}

export function needsApproval(kind: ToolKind, current: ApprovalMode): boolean {
  if (current === "ask") return true;
  if (current === "read") return kind !== "read";
  if (current === "edits") return kind === "exec";
  return false;
}

// --- "allow always in this chat" ------------------------------------------------------------

const [alwaysAllowed, setAlwaysAllowed] = createSignal<Record<string, string[]>>({});

/** Tools the user allowed for the rest of a chat (kept until the app restarts). */
export const isAlwaysAllowed = (sessionId: string, key: string) => alwaysAllowed()[sessionId]?.includes(key) ?? false;

function allowAlways(sessionId: string, key: string) {
  if (isAlwaysAllowed(sessionId, key)) return;
  setAlwaysAllowed((all) => ({ ...all, [sessionId]: [...(all[sessionId] ?? []), key] }));
}

// --- pending questions ----------------------------------------------------------------------

export type Decision = { allow: true } | { allow: false; reason?: string; cancelled?: boolean };

/** `key` is what "always" allows (a tool name, or e.g. `edit:sensitive`); `note` explains the risk. */
type Pending = { sessionId: string; key: string; note?: string; resolve: (decision: Decision) => void };

const [pending, setPending] = createSignal<Record<string, Pending>>({});

/** Whether the tool call `toolUseId` is waiting for the user. */
export const awaitingApproval = (toolUseId: string) => toolUseId in pending();

/** Why the call needs extra care, shown on its card (undefined for an ordinary question). */
export const approvalNote = (toolUseId: string) => pending()[toolUseId]?.note;

/** Whether a chat has a question waiting for the user. */
export const sessionAwaitsApproval = (sessionId: string) =>
  Object.values(pending()).some((entry) => entry.sessionId === sessionId);

/** Asks the user about a tool call; the chat shows the question on the call's card. */
export function requestApproval(sessionId: string, toolUseId: string, key: string, note?: string): Promise<Decision> {
  return new Promise((resolve) => setPending((all) => ({ ...all, [toolUseId]: { sessionId, key, note, resolve } })));
}

/** The user's answer. `always` also allows this tool for the rest of the chat. */
export function answerApproval(toolUseId: string, decision: Decision, always = false) {
  const entry = pending()[toolUseId];
  if (!entry) return;
  setPending(({ [toolUseId]: _, ...rest }) => rest);
  if (decision.allow && always) allowAlways(entry.sessionId, entry.key);
  entry.resolve(decision);
}

/** Drops a chat's open questions (the turn was stopped). */
export function cancelApprovals(sessionId: string) {
  for (const [id, entry] of Object.entries(pending())) {
    if (entry.sessionId === sessionId) answerApproval(id, { allow: false, cancelled: true });
  }
}
