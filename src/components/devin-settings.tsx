import { Button } from "@kobalte/core/button";
import { Collapsible } from "@kobalte/core/collapsible";
import { createSignal, Show } from "solid-js";
import { devinCancelLogin, devinLogin, devinLogout, devinRefreshModels, devinStatus } from "../lib/devin";
import { t } from "../lib/i18n";
import { AccountUsage } from "./account-usage";
import { ProviderPromptSettings } from "./agent-settings";
import { ChevronDownIcon } from "./icons";

const PRIMARY =
  "h-8 shrink-0 rounded-lg bg-accent px-3 text-[13px] font-medium text-accent-fg transition hover:brightness-110 disabled:opacity-60";
const DANGER =
  "h-8 shrink-0 rounded-lg border border-red-400/30 px-3 text-[13px] font-medium text-red-300/90 transition-colors hover:bg-red-500/10 hover:text-red-300 disabled:opacity-60";

/** Devin provider card: sign in once and its remote models join the catalog. */
export function DevinSettings() {
  const [error, setError] = createSignal("");

  const signIn = async () => {
    setError("");
    try {
      await devinLogin();
    } catch (reason) {
      setError(t("devin.failed", { reason: String(reason) }));
    }
  };

  const signOut = async () => {
    setError("");
    try {
      await devinLogout();
    } catch (reason) {
      setError(t("devin.failed", { reason: String(reason) }));
    }
  };

  return (
    <Collapsible>
      <div class="flex items-center justify-between gap-4">
        <Collapsible.Trigger class="group min-w-0 flex-1 rounded-lg text-left outline-none focus-visible:ring-2 focus-visible:ring-edge-active">
          <div class="flex items-center gap-2">
            <ChevronDownIcon class="size-4 shrink-0 -rotate-90 text-white/40 transition-transform group-data-expanded:rotate-0" />
            <span class="text-base font-medium">Devin</span>
            <Show when={devinStatus()?.signedIn}>
              <span class="rounded-md bg-emerald-400/10 px-1.5 py-0.5 text-[11px] text-emerald-300/90">
                {t("devin.signedIn")}
                <Show when={devinStatus()?.email}>{(email) => ` · ${email()}`}</Show>
                <Show when={devinStatus()?.models}>{(count) => ` · ${t("devin.models", { count: count() })}`}</Show>
              </span>
            </Show>
          </div>
          <p class="mt-1 text-sm text-white/45">{t("devin.hint")}</p>
        </Collapsible.Trigger>

        <Show
          when={devinStatus()?.signingIn}
          fallback={
            <Show
              when={devinStatus()?.signedIn}
              fallback={
                <Button onClick={signIn} disabled={!devinStatus()} class={PRIMARY}>
                  {t("devin.signIn")}
                </Button>
              }
            >
              <Button onClick={signOut} class={DANGER}>
                {t("devin.signOut")}
              </Button>
            </Show>
          }
        >
          <div class="flex shrink-0 items-center gap-3">
            <span class="text-sm text-white/45">{t("devin.signingIn")}</span>
            <Button onClick={devinCancelLogin} class={DANGER}>
              {t("common.cancel")}
            </Button>
          </div>
        </Show>
      </div>

      <Show when={error()}>
        <p class="mt-3 text-sm text-red-400">{error()}</p>
      </Show>

      <Collapsible.Content class="mt-5 border-t border-white/[0.06] pt-4">
        <Show when={devinStatus()?.signedIn}>
          <AccountUsage provider="devin" />
          <div class="mt-5 border-t border-white/[0.06] pt-5" />
          <DevinModels />
          <div class="mt-5 border-t border-white/[0.06] pt-5" />
        </Show>
        <ProviderPromptSettings provider="devin" />
      </Collapsible.Content>
    </Collapsible>
  );
}

/** The account's remote catalog, with a manual refresh for when it changes upstream. */
function DevinModels() {
  const [refreshing, setRefreshing] = createSignal(false);
  const refresh = async () => {
    setRefreshing(true);
    try {
      await devinRefreshModels();
    } finally {
      setRefreshing(false);
    }
  };
  return (
    <div class="flex items-center justify-between gap-4">
      <div>
        <div class="text-sm font-medium text-white/85">{t("devin.catalog")}</div>
        <p class="mt-1 text-xs text-white/40">{t("devin.catalogHint")}</p>
      </div>
      <Button onClick={refresh} disabled={refreshing()} class={PRIMARY}>
        {refreshing() ? t("devin.refreshing") : t("devin.refresh")}
      </Button>
    </div>
  );
}
