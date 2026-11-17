import { Button } from "@kobalte/core/button";
import { Meter } from "@kobalte/core/meter";
import { createResource, For, Show } from "solid-js";
import { type Account, fetchAccount } from "../lib/core";
import { locale, t } from "../lib/i18n";

const SECONDARY =
  "h-7 shrink-0 rounded-lg border border-white/10 bg-white/[0.03] px-2.5 text-[12px] text-white/80 transition-colors hover:border-white/20 hover:bg-white/[0.07] hover:text-white disabled:pointer-events-none disabled:opacity-50";

const date = (iso: string | null) =>
  iso ? new Date(iso).toLocaleString(locale(), { dateStyle: "medium", timeStyle: "short" }) : null;

const money = (value: number | null, currency: string | null) =>
  value === null ? "—" : value.toLocaleString(locale(), { style: "currency", currency: currency ?? "USD" });

/** Bar color by how much of the window is used. */
const tone = (percent: number) => (percent >= 90 ? "bg-red-400/80" : percent >= 70 ? "bg-amber-300/80" : "bg-emerald-400/70");

/**
 * A signed-in provider's plan, usage windows and extra-usage balance, as dd-core reports them
 * (`GET /dd/providers/{id}/account`).
 */
export function AccountUsage(props: { provider: "claude" | "devin" }) {
  let fresh = false;
  const [account, { refetch }] = createResource(
    () => props.provider,
    (provider): Promise<Account> => {
      const request = fetchAccount(provider, fresh);
      fresh = false;
      return request;
    },
  );
  const refresh = () => {
    fresh = true;
    void refetch();
  };

  return (
    <div>
      <div class="flex items-center justify-between gap-4">
        <div>
          <div class="text-sm font-medium text-white/85">{t("account.title")}</div>
          <p class="mt-1 text-xs text-white/40">{t("account.hint")}</p>
        </div>
        <Button onClick={refresh} disabled={account.loading} class={SECONDARY}>
          {account.loading ? t("account.loading") : t("account.refresh")}
        </Button>
      </div>

      <Show when={account.error}>
        <p class="mt-3 text-sm text-red-400">{t("account.failed", { reason: String(account.error) })}</p>
      </Show>

      <Show when={account.latest}>
        {(info) => (
          <div class="mt-3 space-y-3">
            <div class="flex flex-wrap gap-x-5 gap-y-1 text-[13px] text-white/70">
              <span>
                {t("account.plan")}: <span class="text-white/90">{info().plan.name ?? "—"}</span>
                <Show when={info().plan.tier}>{(tier) => <span class="text-white/40"> ({tier()})</span>}</Show>
              </span>
              <Show when={info().organization}>
                {(org) => (
                  <span>
                    {t("account.organization")}: <span class="text-white/90">{org()}</span>
                  </span>
                )}
              </Show>
              <Show when={date(info().plan.startedAt)}>
                {(since) => (
                  <span>
                    {t("account.since")}: <span class="text-white/90">{since()}</span>
                  </span>
                )}
              </Show>
              <Show when={date(info().plan.expiresAt)}>
                {(until) => (
                  <span>
                    {t("account.until")}: <span class="text-white/90">{until()}</span>
                  </span>
                )}
              </Show>
              <Show when={date(info().plan.trialEndsAt)}>
                {(trial) => (
                  <span>
                    {t("account.trialUntil")}: <span class="text-white/90">{trial()}</span>
                  </span>
                )}
              </Show>
            </div>

            <Show when={info().limits.length > 0} fallback={<p class="text-xs text-white/40">{t("account.noLimits")}</p>}>
              <div class="space-y-2.5">
                <For each={info().limits}>
                  {(limit) => {
                    const used = () => Math.max(0, limit.usedPercent ?? 0);
                    return (
                      <Meter
                        value={Math.min(100, used())}
                        minValue={0}
                        maxValue={100}
                        getValueLabel={() =>
                          limit.usedPercent === null ? "—" : t("account.used", { percent: Math.round(used()) })
                        }
                        class="block"
                      >
                        <div class="flex items-baseline justify-between gap-3 text-[12px]">
                          <Meter.Label class="text-white/75">{limit.label}</Meter.Label>
                          <span class="text-white/45">
                            <Meter.ValueLabel class="text-white/85" />
                            <Show when={date(limit.resetsAt)}>{(reset) => ` · ${t("account.resets", { when: reset() })}`}</Show>
                          </span>
                        </div>
                        <Meter.Track class="mt-1 h-1.5 overflow-hidden rounded-full bg-white/[0.07]">
                          <Meter.Fill class={`h-full rounded-full ${tone(used())} w-(--kb-meter-fill-width) transition-[width]`} />
                        </Meter.Track>
                      </Meter>
                    );
                  }}
                </For>
              </div>
            </Show>

            <Show when={info().balance}>
              {(balance) => (
                <p class="text-[12px] text-white/60">
                  {t("account.extraUsage")}:{" "}
                  <span class="text-white/85">
                    {balance().enabled
                      ? t("account.spent", {
                          used: money(balance().used, balance().currency),
                          limit: money(balance().limit, balance().currency),
                        })
                      : t("account.off")}
                  </span>
                </p>
              )}
            </Show>
          </div>
        )}
      </Show>
    </div>
  );
}
