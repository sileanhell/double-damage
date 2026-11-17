import { Button } from "@kobalte/core/button";
import { Collapsible } from "@kobalte/core/collapsible";
import { TextField } from "@kobalte/core/text-field";
import { createSignal, Show } from "solid-js";
import { claudeCancelLogin, claudeLogin, claudeLogout, claudeRefreshModels, claudeStatus, claudeSubmitCode } from "../lib/claude";
import { t } from "../lib/i18n";
import { AccountUsage } from "./account-usage";
import { ProviderPromptSettings } from "./agent-settings";
import { ChevronDownIcon } from "./icons";

const PRIMARY =
  "h-8 shrink-0 rounded-lg bg-accent px-3 text-[13px] font-medium text-accent-fg transition hover:brightness-110 disabled:opacity-60";
const DANGER =
  "h-8 shrink-0 rounded-lg border border-red-400/30 px-3 text-[13px] font-medium text-red-300/90 transition-colors hover:bg-red-500/10 hover:text-red-300 disabled:opacity-60";
const SECONDARY =
  "h-8 shrink-0 rounded-lg border border-white/10 bg-white/[0.03] px-3 text-[13px] text-white/85 transition-colors hover:border-white/20 hover:bg-white/[0.07] hover:text-white disabled:pointer-events-none disabled:opacity-50";
const FIELD = "h-8 min-w-0 rounded-lg border border-white/10 bg-black/30 px-3 font-mono text-[13px] leading-8 text-white/80";

/** Claude provider card: sign in with a claude.ai account; its models join the catalog. */
export function ClaudeSettings() {
  const [error, setError] = createSignal("");
  const [code, setCode] = createSignal("");
  const [copied, setCopied] = createSignal(false);

  const signIn = async () => {
    setError("");
    try {
      await claudeLogin();
    } catch (reason) {
      setError(t("claude.failed", { reason: String(reason) }));
    }
  };

  const signOut = async () => {
    setError("");
    try {
      await claudeLogout();
    } catch (reason) {
      setError(t("claude.failed", { reason: String(reason) }));
    }
  };

  const submitCode = async () => {
    setError("");
    try {
      await claudeSubmitCode(code());
      setCode("");
    } catch (reason) {
      setError(t("claude.failed", { reason: String(reason) }));
    }
  };

  const copyLink = async () => {
    const url = claudeStatus()?.manualUrl;
    if (!url) return;
    await navigator.clipboard.writeText(url).catch(console.error);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  return (
    <Collapsible>
      <div class="flex items-center justify-between gap-4">
        <Collapsible.Trigger class="group min-w-0 flex-1 rounded-lg text-left outline-none focus-visible:ring-2 focus-visible:ring-edge-active">
          <div class="flex items-center gap-2">
            <ChevronDownIcon class="size-4 shrink-0 -rotate-90 text-white/40 transition-transform group-data-expanded:rotate-0" />
            <span class="text-base font-medium">Claude</span>
            <Show when={claudeStatus()?.signedIn}>
              <span class="rounded-md bg-emerald-400/10 px-1.5 py-0.5 text-[11px] text-emerald-300/90">
                {t("claude.signedIn")}
                <Show when={claudeStatus()?.email}>{(email) => ` · ${email()}`}</Show>
                <Show when={claudeStatus()?.subscription}>{(sub) => ` · ${sub()}`}</Show>
                <Show when={claudeStatus()?.models}>{(count) => ` · ${t("claude.models", { count: count() })}`}</Show>
              </span>
            </Show>
          </div>
          <p class="mt-1 text-sm text-white/45">{t("claude.hint")}</p>
        </Collapsible.Trigger>

        <Show
          when={claudeStatus()?.signingIn}
          fallback={
            <Show
              when={claudeStatus()?.signedIn}
              fallback={
                <Button onClick={signIn} disabled={!claudeStatus()} class={PRIMARY}>
                  {t("claude.signIn")}
                </Button>
              }
            >
              <Button onClick={signOut} class={DANGER}>
                {t("claude.signOut")}
              </Button>
            </Show>
          }
        >
          <div class="flex shrink-0 items-center gap-3">
            <span class="text-sm text-white/45">{t("claude.signingIn")}</span>
            <Button onClick={claudeCancelLogin} class={DANGER}>
              {t("common.cancel")}
            </Button>
          </div>
        </Show>
      </div>

      <Show when={claudeStatus()?.signingIn}>
        <div class="mt-4 rounded-xl border border-white/[0.06] bg-black/20 p-4">
          <div class="flex items-center gap-2">
            <TextField value={code()} onChange={setCode} class="min-w-0 flex-1">
              <TextField.Input
                aria-label={t("claude.pasteLabel")}
                placeholder={t("claude.pastePlaceholder")}
                autocomplete="off"
                onKeyDown={(event) => event.key === "Enter" && code().includes("#") && void submitCode()}
                class={`${FIELD} w-full outline-none placeholder:text-white/30 focus:border-white/25`}
              />
            </TextField>
            <Button onClick={() => void submitCode()} disabled={!code().includes("#")} class={PRIMARY}>
              {t("claude.pasteSubmit")}
            </Button>
            <Show when={claudeStatus()?.manualUrl}>
              <Button onClick={() => void copyLink()} class={SECONDARY}>
                {copied() ? t("claude.copied") : t("claude.copyLink")}
              </Button>
            </Show>
          </div>
          <p class="mt-2 text-xs text-white/35">{t("claude.pasteHint")}</p>
        </div>
      </Show>

      <Show when={error()}>
        <p class="mt-3 text-sm text-red-400">{error()}</p>
      </Show>

      <Collapsible.Content class="mt-5 border-t border-white/[0.06] pt-4">
        <Show when={claudeStatus()?.signedIn}>
          <AccountUsage provider="claude" />
          <div class="mt-5 border-t border-white/[0.06] pt-5" />
          <ClaudeModels />
          <div class="mt-5 border-t border-white/[0.06] pt-5" />
        </Show>
        <ProviderPromptSettings provider="claude" prefix={claudeStatus()?.systemPrefix} />
      </Collapsible.Content>
    </Collapsible>
  );
}

/** The account's remote catalog, with a manual refresh for when it changes upstream. */
function ClaudeModels() {
  const [refreshing, setRefreshing] = createSignal(false);
  const refresh = async () => {
    setRefreshing(true);
    try {
      await claudeRefreshModels();
    } finally {
      setRefreshing(false);
    }
  };
  return (
    <div class="flex items-center justify-between gap-4">
      <div>
        <div class="text-sm font-medium text-white/85">{t("claude.catalog")}</div>
        <p class="mt-1 text-xs text-white/40">{t("claude.catalogHint")}</p>
      </div>
      <Button onClick={refresh} disabled={refreshing()} class={PRIMARY}>
        {refreshing() ? t("claude.refreshing") : t("claude.refresh")}
      </Button>
    </div>
  );
}
