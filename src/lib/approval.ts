import { createSignal } from "solid-js";
import type { MessageKey } from "./i18n";
import { storage } from "./storage";

/**
 * How much the agent may do without asking the user first.
 * - `auto`: each action is checked by the provider's safety gateway, which decides whether it is safe.
 * - `tools`: after a one-time permission for the workspace folder, tools and MCP may do anything inside it.
 * - `read-only`: only reads the workspace folder; access to any other folder asks the user.
 * - `off`: no approvals at all.
 */
export type ApprovalMode = "auto" | "tools" | "read-only" | "off";

export type ApprovalInfo = { value: ApprovalMode; label: MessageKey; description: MessageKey };

export const APPROVAL_MODES: readonly ApprovalInfo[] = [
  { value: "auto", label: "approval.auto", description: "approval.autoHint" },
  { value: "tools", label: "approval.tools", description: "approval.toolsHint" },
  { value: "read-only", label: "approval.readOnly", description: "approval.readOnlyHint" },
  { value: "off", label: "approval.off", description: "approval.offHint" },
];

const APPROVAL_KEY = "dd:approval";
const DEFAULT_MODE: ApprovalMode = "read-only";

const findMode = (value: string | null) => APPROVAL_MODES.find((item) => item.value === value);

const [mode, setModeSignal] = createSignal<ApprovalMode>(findMode(storage.getItem(APPROVAL_KEY))?.value ?? DEFAULT_MODE);

export const approvalInfo = () => findMode(mode()) ?? findMode(DEFAULT_MODE)!;

export function setApprovalMode(value: ApprovalMode) {
  if (!findMode(value)) return;
  setModeSignal(value);
  storage.setItem(APPROVAL_KEY, value);
}
