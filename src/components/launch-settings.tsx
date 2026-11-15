import { Button } from "@kobalte/core/button";
import { SegmentedControl } from "@kobalte/core/segmented-control";
import { Slider } from "@kobalte/core/slider";
import { createResource, For, type JSX, Show } from "solid-js";
import { formatBytes } from "../lib/format";
import { hardware } from "../lib/hardware";
import { t } from "../lib/i18n";
import {
  contextSteps,
  type Estimate,
  estimate,
  type FlashAttn,
  type KvType,
  launchArgs,
  launchSettings,
  maxFullContext,
  type ModelInfo,
  modelInfo,
  recommendedContext,
  type Resolved,
  SMALL_CTX,
  type Target,
  updateLaunchSettings,
  forgetLaunchSettings,
} from "../lib/launch";
import { modelSetSize } from "../lib/local-models";
import { modelDevice } from "../lib/model-device";
import { llamaDevices } from "../lib/providers";
import { DeviceSelect } from "./device-select";

const LINK = "rounded text-xs text-accent/90 transition-colors hover:text-accent";

/** "131072" → "128K". */
const tokens = (n: number) => (n >= 1024 && n % 1024 === 0 ? `${n / 1024}K` : String(n));

/** Launch settings of a local model, with a memory estimate and hints. */
export function LaunchSettingsPanel(props: { model: string }) {
  const [info] = createResource(() => props.model, modelInfo);
  return (
    <Show
      when={info()}
      fallback={
        <p class="text-xs text-white/40">{info.error ? t("launch.noInfo") : t("launch.loading")}</p>
      }
    >
      {(loaded) => <Panel model={props.model} info={loaded()} />}
    </Show>
  );
}

type Tone = "ok" | "info" | "warn" | "bad";
const TONE: Record<Tone, string> = {
  ok: "text-emerald-300/90",
  info: "text-white/55",
  warn: "text-amber-300/90",
  bad: "text-red-300",
};
const DOT: Record<Tone, string> = { ok: "bg-emerald-400", info: "bg-white/30", warn: "bg-amber-400", bad: "bg-red-400" };

function Panel(props: { model: string; info: ModelInfo }) {
  const size = () => modelSetSize(props.model);
  const settings = () => launchSettings(props.model);
  const device = () => llamaDevices().find((item) => item.id === modelDevice(props.model));
  const target = (): Target => ({
    gpu: device() ? { memory: device()?.memory ?? 0 } : null,
    ram: hardware()?.ram ?? 0,
  });
  const steps = () => contextSteps(props.info);
  const base = (): Resolved => ({
    ctx: 0,
    kv: settings().kv ?? "f16",
    fa: settings().fa ?? "auto",
    ngl: device() ? settings().ngl : undefined,
  });
  const recommended = () => recommendedContext(props.info, size(), base(), target());
  const ctx = () => {
    const chosen = settings().ctx;
    return chosen && chosen <= (steps()[steps().length - 1] ?? chosen) ? chosen : recommended();
  };
  const resolved = (): Resolved => ({ ...base(), ctx: ctx() });
  const est = () => estimate(props.info, size(), resolved(), target());
  const fullCtx = () => maxFullContext(props.info, size(), base(), target());
  const update = (patch: Parameters<typeof updateLaunchSettings>[1]) => updateLaunchSettings(props.model, patch);
  const customized = () => Object.keys(settings()).length > 0;
  const threadsMax = () => navigator.hardwareConcurrency || 8;

  const hints = () => hintsFor(props.info, est(), resolved(), settings().ngl, !!device(), fullCtx());
  const args = () => launchArgs({ ...resolved(), threads: settings().threads }, device()?.id ?? null, est().gpuLayers);

  return (
    <div class="space-y-5">
      <div class="flex items-center justify-between gap-4">
        <div>
          <div class="text-sm font-medium text-white/85">{t("models.runOn")}</div>
          <p class="mt-1 text-xs text-white/40">{t("launch.deviceHint")}</p>
        </div>
        <DeviceSelect model={props.model} />
      </div>

      <div class="space-y-3">
        <Show when={device()}>
          {(gpu) => (
            <MemoryBar
              title={t("launch.vramOf", { gpu: `${gpu().name} · ${gpu().backend}` })}
              parts={est().vramParts}
              capacity={est().vram}
            />
          )}
        </Show>
        <Show when={!device() || !est().full}>
          <MemoryBar title={t("launch.ram")} parts={est().ramParts} capacity={est().ram} />
        </Show>
        <ul class="space-y-1">
          <For each={hints()}>
            {(hint) => (
              <li class={`flex gap-2 text-xs leading-5 ${TONE[hint.tone]}`}>
                <span class={`mt-2 size-1.5 shrink-0 rounded-full ${DOT[hint.tone]}`} />
                {hint.text}
              </li>
            )}
          </For>
        </ul>
      </div>

      <StepSlider
        label={t("launch.context")}
        steps={steps()}
        value={ctx()}
        auto={settings().ctx === undefined}
        format={(value) => t("launch.tokens", { count: tokens(value) })}
        onChange={(value) => update({ ctx: value })}
        onReset={() => update({ ctx: undefined })}
        description={t("launch.contextHint", {
          trained: tokens(steps()[steps().length - 1] ?? 0),
          kv: formatBytes(est().kv),
          recommended: tokens(recommended()),
        })}
      />

      <Show when={device()}>
        <StepSlider
          label={t("launch.gpuLayers")}
          steps={Array.from({ length: est().layers + 2 }, (_, i) => i)}
          value={est().gpuLayers}
          auto={settings().ngl === undefined}
          format={(value) => t("launch.layersOf", { count: value, total: est().layers + 1 })}
          onChange={(value) => update({ ngl: value })}
          onReset={() => update({ ngl: undefined })}
          description={t("launch.gpuLayersHint", { max: est().maxGpuLayers, total: est().layers + 1 })}
        />
      </Show>

      <Field
        label={t("launch.kvType")}
        auto={settings().kv === undefined}
        onReset={() => update({ kv: undefined })}
        description={t("launch.kvTypeHint")}
      >
        <Segmented<KvType>
          label={t("launch.kvType")}
          value={resolved().kv}
          options={[
            { value: "f16", label: "f16" },
            { value: "q8_0", label: "q8_0" },
            { value: "q4_0", label: "q4_0" },
          ]}
          onChange={(kv) => update({ kv })}
        />
      </Field>

      <Field
        label={t("launch.flashAttn")}
        auto={settings().fa === undefined}
        onReset={() => update({ fa: undefined })}
        description={t("launch.flashAttnHint")}
      >
        <Segmented<FlashAttn>
          label={t("launch.flashAttn")}
          value={resolved().fa}
          options={[
            { value: "auto", label: t("launch.auto") },
            { value: "on", label: t("launch.on") },
            { value: "off", label: t("launch.off") },
          ]}
          onChange={(fa) => update({ fa })}
        />
      </Field>

      <StepSlider
        label={t("launch.threads")}
        steps={Array.from({ length: threadsMax() + 1 }, (_, i) => i)}
        value={settings().threads ?? 0}
        auto={settings().threads === undefined}
        format={(value) => (value === 0 ? t("launch.auto") : String(value))}
        onChange={(value) => update({ threads: value === 0 ? undefined : value })}
        onReset={() => update({ threads: undefined })}
        description={t(device() && est().full ? "launch.threadsGpuHint" : "launch.threadsHint")}
      />

      <div class="flex items-center justify-between gap-4 border-t border-white/[0.06] pt-4">
        <code class="min-w-0 truncate font-mono text-[11px] text-white/35" title={args().join(" ")}>
          llama-server {args().join(" ")}
        </code>
        <Show when={customized()}>
          <Button class={`${LINK} shrink-0`} onClick={() => forgetLaunchSettings(props.model)}>
            {t("launch.resetAll")}
          </Button>
        </Show>
      </div>
    </div>
  );
}

function hintsFor(
  info: ModelInfo,
  est: Estimate,
  s: Resolved,
  manualNgl: number | undefined,
  gpu: boolean,
  fullCtx: number | undefined,
): { tone: Tone; text: string }[] {
  const out: { tone: Tone; text: string }[] = [];
  const total = est.layers + 1;
  if (!est.fits) {
    out.push({
      tone: "bad",
      text: t("launch.hint.noMemory", {
        need: formatBytes(est.vramNeed + est.ramNeed),
        have: formatBytes(est.vram + est.ram),
      }),
    });
  } else if (gpu && est.full) {
    out.push({ tone: "ok", text: t("launch.hint.allGpu") });
  } else if (gpu && est.gpuLayers > 0) {
    out.push({ tone: "warn", text: t("launch.hint.partial", { count: est.gpuLayers, total }) });
  } else if (gpu) {
    out.push({ tone: "warn", text: t("launch.hint.noGpuLayers") });
  } else {
    out.push({ tone: "info", text: t("launch.hint.cpu") });
  }

  if (gpu && !est.full && est.kvOverflow) {
    out.push({
      tone: "warn",
      text: t(s.kv === "f16" ? "launch.hint.kvOverflow" : "launch.hint.kvOverflowQuant", { kv: formatBytes(est.kv) }),
    });
  }
  if (gpu && !est.full && fullCtx !== undefined && fullCtx < s.ctx) {
    out.push({ tone: "info", text: t("launch.hint.shrinkCtx", { ctx: tokens(fullCtx) }) });
  }
  if (gpu && manualNgl !== undefined && manualNgl > est.maxGpuLayers) {
    out.push({ tone: "bad", text: t("launch.hint.tooManyLayers", { max: est.maxGpuLayers }) });
  } else if (gpu && manualNgl !== undefined && manualNgl < est.maxGpuLayers) {
    out.push({ tone: "info", text: t("launch.hint.moreLayers", { max: est.maxGpuLayers }) });
  }
  if (s.kv !== "f16" && s.fa === "off") out.push({ tone: "bad", text: t("launch.hint.quantNeedsFa") });
  if (s.kv === "q4_0") out.push({ tone: "warn", text: t("launch.hint.q4") });
  if (s.fa === "off" && est.compute > 1024 ** 3) {
    out.push({ tone: "info", text: t("launch.hint.faOff", { size: formatBytes(est.compute) }) });
  }
  if (s.ctx < SMALL_CTX) out.push({ tone: "warn", text: t("launch.hint.smallCtx") });
  if (info.slidingWindow) out.push({ tone: "info", text: t("launch.hint.swa", { window: info.slidingWindow }) });
  return out;
}

const SEGMENTS = ["bg-sky-400/80", "bg-violet-400/80", "bg-white/35"];

/** Stacked bar: model weights, KV cache and buffers against the memory that's available. */
function MemoryBar(props: { title: string; parts: [number, number, number]; capacity: number }) {
  const need = () => props.parts.reduce((a, b) => a + b, 0);
  const scale = () => Math.max(need(), props.capacity) || 1;
  const over = () => need() > props.capacity;
  const labels = () => [t("launch.weights"), t("launch.kv"), t("launch.buffers")];
  return (
    <div>
      <div class="flex items-center justify-between gap-4 text-xs">
        <span class="truncate text-white/55">{props.title}</span>
        <span class={`shrink-0 tabular-nums ${over() ? "text-red-300" : "text-white/45"}`}>
          {formatBytes(need())} / {formatBytes(props.capacity)}
        </span>
      </div>
      <div class="relative mt-1.5 flex h-2 overflow-hidden rounded-full bg-white/[0.07]">
        <For each={props.parts}>
          {(part, i) => <div class={SEGMENTS[i()]} style={{ width: `${(part / scale()) * 100}%` }} />}
        </For>
        <Show when={over()}>
          <div class="absolute inset-y-0 w-px bg-red-300" style={{ left: `${(props.capacity / scale()) * 100}%` }} />
        </Show>
      </div>
      <div class="mt-1.5 flex flex-wrap gap-x-4 gap-y-1 text-[11px] text-white/40 tabular-nums">
        <For each={props.parts}>
          {(part, i) => (
            <Show when={part > 0}>
              <span class="flex items-center gap-1.5">
                <span class={`size-2 rounded-sm ${SEGMENTS[i()]}`} />
                {labels()[i()]} {formatBytes(part)}
              </span>
            </Show>
          )}
        </For>
      </div>
    </div>
  );
}

function FieldHeader(props: { label: JSX.Element; value?: JSX.Element; auto: boolean; onReset: () => void }) {
  return (
    <div class="flex items-center justify-between gap-4">
      <div class="flex items-center gap-2 text-sm font-medium text-white/85">
        {props.label}
        <Show
          when={props.auto}
          fallback={
            <Button class={LINK} onClick={props.onReset}>
              {t("launch.resetAuto")}
            </Button>
          }
        >
          <span class="rounded bg-white/[0.06] px-1.5 py-px text-[10px] font-normal text-white/45">
            {t("launch.auto")}
          </span>
        </Show>
      </div>
      {props.value}
    </div>
  );
}

function Field(props: { label: string; auto: boolean; onReset: () => void; description: string; children: JSX.Element }) {
  return (
    <div>
      <FieldHeader label={props.label} auto={props.auto} onReset={props.onReset} value={props.children} />
      <p class="mt-1.5 text-xs text-white/40">{props.description}</p>
    </div>
  );
}

/** Slider over a fixed list of values (context sizes are powers of two, not a linear range). */
function StepSlider(props: {
  label: string;
  steps: number[];
  value: number;
  auto: boolean;
  format: (value: number) => string;
  onChange: (value: number) => void;
  onReset: () => void;
  description: string;
}) {
  const index = () => {
    const i = props.steps.findIndex((step) => step >= props.value);
    return i === -1 ? props.steps.length - 1 : i;
  };
  return (
    <Slider
      class="w-full select-none"
      minValue={0}
      maxValue={Math.max(0, props.steps.length - 1)}
      step={1}
      value={[index()]}
      onChange={([i]) => props.onChange(props.steps[i] ?? props.value)}
      getValueLabel={({ values }) => props.format(props.steps[values[0]] ?? props.value)}
    >
      <FieldHeader
        label={<Slider.Label>{props.label}</Slider.Label>}
        value={<Slider.ValueLabel class="text-sm text-white/55 tabular-nums" />}
        auto={props.auto}
        onReset={props.onReset}
      />
      <Slider.Track class="relative mt-3 h-1.5 w-full rounded-full bg-white/10">
        <Slider.Fill class="absolute h-full rounded-full bg-accent" />
        <Slider.Thumb class="-top-[5px] block size-4 rounded-full border border-white/20 bg-white shadow outline-none transition-shadow focus-visible:ring-4 focus-visible:ring-edge">
          <Slider.Input />
        </Slider.Thumb>
      </Slider.Track>
      <p class="mt-2 text-xs text-white/40">{props.description}</p>
    </Slider>
  );
}

function Segmented<T extends string>(props: {
  label: string;
  value: T;
  options: { value: T; label: string }[];
  onChange: (value: T) => void;
}) {
  return (
    <SegmentedControl
      aria-label={props.label}
      value={props.value}
      onChange={(value) => props.onChange(value as T)}
      class="relative flex shrink-0 rounded-lg bg-white/[0.05] p-0.5"
    >
      <SegmentedControl.Indicator class="absolute rounded-md bg-white/12 transition-all duration-150" />
      <For each={props.options}>
        {(option) => (
          <SegmentedControl.Item value={option.value} class="relative z-10">
            <SegmentedControl.ItemInput class="peer" />
            <SegmentedControl.ItemLabel class="flex h-7 cursor-pointer items-center rounded-md px-3 text-[13px] text-white/55 transition-colors hover:text-white/85 data-checked:text-white peer-focus-visible:ring-2 peer-focus-visible:ring-edge-active">
              {option.label}
            </SegmentedControl.ItemLabel>
          </SegmentedControl.Item>
        )}
      </For>
    </SegmentedControl>
  );
}
