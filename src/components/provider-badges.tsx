import { For, Show } from "solid-js";
import { t } from "../lib/i18n";
import type { Provider } from "../lib/providers";

/** Pills naming the providers that can run a model; a muted note when none can. */
export function ProviderBadges(props: { providers: readonly Provider[] }) {
  return (
    <Show
      when={props.providers.length > 0}
      fallback={<span class="text-xs text-white/35">{t("models.noProvider")}</span>}
    >
      <span class="flex flex-wrap gap-1.5">
        <For each={props.providers}>
          {(provider) => (
            <span class="rounded-md border border-accent/30 bg-accent/10 px-1.5 py-0.5 font-mono text-[11px] text-accent">
              {provider.name}
            </span>
          )}
        </For>
      </span>
    </Show>
  );
}
