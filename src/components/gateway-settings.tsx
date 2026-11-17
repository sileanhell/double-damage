import { Button } from "@kobalte/core/button";
import { TextField } from "@kobalte/core/text-field";
import { createSignal, Show } from "solid-js";
import {
  DEFAULT_GATEWAY_PORT,
  GATEWAY_PORT_KEY,
  gatewayInfo,
  regenerateGatewayToken,
  restartGateway,
} from "../lib/gateway";
import { t } from "../lib/i18n";
import { flushStorage, storage } from "../lib/storage";

const SECONDARY =
  "h-8 shrink-0 rounded-lg border border-white/10 bg-white/[0.03] px-3 text-[13px] text-white/85 transition-colors hover:border-white/20 hover:bg-white/[0.07] hover:text-white disabled:pointer-events-none disabled:opacity-50";
const FIELD =
  "h-8 min-w-0 rounded-lg border border-white/10 bg-black/30 px-3 font-mono text-[13px] leading-8 text-white/80";

/** The local OpenAI/Anthropic-compatible endpoint: address, port and access token for other apps. */
export function GatewaySettings() {
  const [port, setPort] = createSignal(storage.getItem(GATEWAY_PORT_KEY) ?? String(DEFAULT_GATEWAY_PORT));
  const [showToken, setShowToken] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [copied, setCopied] = createSignal<"url" | "token" | null>(null);
  const [error, setError] = createSignal("");

  const validPort = () => {
    const value = Number(port());
    return Number.isInteger(value) && value >= 1024 && value <= 65535;
  };
  const portChanged = () => Number(port()) !== gatewayInfo()?.port;

  const run = async (action: () => Promise<void>) => {
    setError("");
    setBusy(true);
    try {
      await action();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const applyPort = () =>
    run(async () => {
      storage.setItem(GATEWAY_PORT_KEY, String(Number(port())));
      await flushStorage(); // the backend reads the port from the saved settings
      await restartGateway();
    });

  const copy = async (what: "url" | "token", text: string | null | undefined) => {
    if (!text) return;
    await navigator.clipboard.writeText(text).catch(console.error);
    setCopied(what);
    setTimeout(() => setCopied(null), 1500);
  };

  const masked = () => {
    const token = gatewayInfo()?.token ?? "";
    return showToken() ? token : `${token.slice(0, 6)}${"•".repeat(Math.max(0, Math.min(24, token.length - 6)))}`;
  };

  return (
    <div>
      <div class="text-base font-medium">{t("gateway.title")}</div>
      <p class="mt-1 text-sm text-white/45">{t("gateway.hint")}</p>

      <div class="mt-4 space-y-3">
        <div class="flex items-center gap-2">
          <span class="w-20 shrink-0 text-sm text-white/55">{t("gateway.address")}</span>
          <div class={`${FIELD} flex-1 truncate select-text`}>
            <Show when={gatewayInfo()?.url} fallback={<span class="text-white/35">{t("gateway.stopped")}</span>}>
              {(url) => `${url()}/v1`}
            </Show>
          </div>
          <Button class={SECONDARY} disabled={!gatewayInfo()?.url} onClick={() => copy("url", `${gatewayInfo()?.url}/v1`)}>
            {copied() === "url" ? t("gateway.copied") : t("gateway.copy")}
          </Button>
        </div>

        <TextField
          value={port()}
          onChange={(value) => setPort(value.replace(/\D/g, "").slice(0, 5))}
          validationState={validPort() ? "valid" : "invalid"}
          class="flex items-center gap-2"
        >
          <TextField.Label class="w-20 shrink-0 text-sm text-white/55">{t("gateway.port")}</TextField.Label>
          <TextField.Input
            inputMode="numeric"
            class={`${FIELD} w-28 outline-none focus:border-white/25 data-invalid:border-red-400/50`}
            onKeyDown={(event) => event.key === "Enter" && validPort() && portChanged() && void applyPort()}
          />
          <Button class={SECONDARY} disabled={busy() || !validPort() || !portChanged()} onClick={() => void applyPort()}>
            {t("gateway.apply")}
          </Button>
        </TextField>

        <div class="flex items-center gap-2">
          <span class="w-20 shrink-0 text-sm text-white/55">{t("gateway.token")}</span>
          <div class={`${FIELD} flex-1 truncate select-text`}>{masked()}</div>
          <Button class={SECONDARY} onClick={() => setShowToken(!showToken())}>
            {showToken() ? t("gateway.hide") : t("gateway.show")}
          </Button>
          <Button class={SECONDARY} onClick={() => copy("token", gatewayInfo()?.token)}>
            {copied() === "token" ? t("gateway.copied") : t("gateway.copy")}
          </Button>
          <Button class={SECONDARY} disabled={busy()} onClick={() => void run(regenerateGatewayToken)}>
            {t("gateway.regenerate")}
          </Button>
        </div>
        <p class="text-xs text-white/35">{t("gateway.tokenHint")}</p>
      </div>

      <Show when={gatewayInfo()?.error ?? error()}>
        {(message) => <p class="mt-3 text-sm text-red-400">{message()}</p>}
      </Show>
    </div>
  );
}
