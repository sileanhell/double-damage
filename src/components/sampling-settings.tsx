import { Button } from "@kobalte/core/button";
import { NumberField } from "@kobalte/core/number-field";
import { createSignal, For, Show } from "solid-js";
import { catalogModels } from "../lib/gateway";
import { locale, type MessageKey, t } from "../lib/i18n";
import { launchSettings, type SamplingSettings, updateLaunchSettings } from "../lib/launch";

type Field = {
  key: keyof SamplingSettings;
  label: MessageKey;
  hint: MessageKey;
  min: number;
  max: number;
  step: number;
  /** llama-server's own default, shown when the model recommends nothing. */
  fallback: number;
};

const FIELDS: Field[] = [
  { key: "temperature", label: "sampling.temperature", hint: "sampling.temperatureHint", min: 0, max: 2, step: 0.05, fallback: 0.8 },
  { key: "topK", label: "sampling.topK", hint: "sampling.topKHint", min: 0, max: 200, step: 1, fallback: 40 },
  { key: "topP", label: "sampling.topP", hint: "sampling.topPHint", min: 0, max: 1, step: 0.01, fallback: 0.95 },
  { key: "minP", label: "sampling.minP", hint: "sampling.minPHint", min: 0, max: 1, step: 0.01, fallback: 0.05 },
  { key: "repeatPenalty", label: "sampling.repeatPenalty", hint: "sampling.repeatPenaltyHint", min: 0.5, max: 2, step: 0.01, fallback: 1 },
];

const LINK = "rounded text-xs text-accent/90 transition-colors hover:text-accent";

/** Per-model sampling; empty fields use the model's recommendation, else llama-server's default. */
export function SamplingSettingsPanel(props: { model: string }) {
  const recommended = () => catalogModels().find((model) => model.file === props.model)?.sampling;
  const settings = () => launchSettings(props.model);

  return (
    <div class="space-y-3">
      <div>
        <div class="text-sm font-medium text-white/85">{t("sampling.title")}</div>
        <p class="mt-1 text-xs text-white/40">{t("sampling.hint")}</p>
      </div>
      <div class="grid grid-cols-[repeat(auto-fill,minmax(10rem,1fr))] gap-3">
        <For each={FIELDS}>
          {(field) => {
            const auto = () => settings()[field.key] === undefined;
            const suggested = () => recommended()?.[field.key] ?? undefined;
            const shown = () => settings()[field.key] ?? suggested() ?? field.fallback;
            // Kobalte takes `rawValue` as a starting value only, so the field is rebuilt (keyed
            // on this) when the value changes from outside: a reset, or the model's recommendation
            // arriving with the catalog. Not on the user's own edits, which would steal focus.
            const [generation, setGeneration] = createSignal(0);
            const outside = () => `${generation()}:${suggested() ?? "-"}`;
            return (
              <Show when={outside()} keyed>
              <NumberField
                rawValue={shown()}
                onRawValueChange={(value) => {
                  // It also reports the starting value on mount: only real edits are saved.
                  if (Number.isNaN(value) || Math.abs(value - shown()) < 1e-9) return;
                  updateLaunchSettings(props.model, { [field.key]: value });
                }}
                minValue={field.min}
                maxValue={field.max}
                step={field.step}
                formatOptions={{ maximumFractionDigits: 2 }}
                class="rounded-xl border border-white/10 bg-white/[0.02] p-2.5"
                title={t(field.hint)}
              >
                <div class="flex items-center justify-between gap-2">
                  <NumberField.Label class="text-xs text-white/60">{t(field.label)}</NumberField.Label>
                  <Show
                    when={auto()}
                    fallback={
                      <Button
                        class={LINK}
                        onClick={() => {
                          updateLaunchSettings(props.model, { [field.key]: undefined });
                          setGeneration(generation() + 1);
                        }}
                      >
                        {t("launch.resetAuto")}
                      </Button>
                    }
                  >
                    <span class="rounded bg-white/[0.06] px-1.5 py-px text-[10px] text-white/45">
                      {suggested() !== undefined ? t("sampling.fromModel") : t("launch.auto")}
                    </span>
                  </Show>
                </div>
                <NumberField.Input
                  lang={locale()}
                  class="mt-1.5 h-7 w-full rounded-md border border-white/10 bg-black/30 px-2 font-mono text-[13px] text-white/85 outline-none focus:border-white/25"
                  classList={{ "text-white/50": auto() }}
                />
              </NumberField>
              </Show>
            );
          }}
        </For>
      </div>
    </div>
  );
}
