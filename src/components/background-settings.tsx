import { FileField } from "@kobalte/core/file-field";
import { RadioGroup } from "@kobalte/core/radio-group";
import { Slider } from "@kobalte/core/slider";
import { createSignal, For, Show } from "solid-js";
import {
  ACCEPTED_TYPES,
  activePresetId,
  BLUR_MAX,
  backgroundBlur,
  PRESETS,
  setBackgroundBlur,
  setBackgroundFromFile,
  setBackgroundPreset,
  type BackgroundPreset,
} from "../lib/background";
import { t } from "../lib/i18n";

/** Background picker: upload, presets, blur and the derived accent color. */
export function BackgroundSettings() {
  const [error, setError] = createSignal("");
  let fileInput!: HTMLInputElement;

  const pick = async (file: File | undefined) => {
    fileInput.value = ""; // allow picking the same file again
    if (!file) return;
    setError("");
    try {
      const persisted = await setBackgroundFromFile(file);
      if (!persisted) setError(t("background.notSaved"));
    } catch {
      setError(t("background.unsupported"));
    }
  };

  return (
    <>
      <FileField
        accept={ACCEPTED_TYPES}
        onFileAccept={(files) => pick(files[0])}
        onFileReject={() => setError(t("background.unsupported"))}
      >
        <FileField.Label class="block text-base font-medium">{t("background.title")}</FileField.Label>
        <FileField.Description class="mt-1 text-sm text-white/45">{t("background.hint")}</FileField.Description>
        <div class="mt-4 flex items-center gap-2">
          <FileField.HiddenInput ref={fileInput} />
          <FileField.Trigger class="h-8 rounded-lg bg-accent px-3 text-[13px] font-medium text-accent-fg transition hover:brightness-110">
            {t("background.pick")}
          </FileField.Trigger>
          <div class="ml-auto flex items-center gap-2 text-sm text-white/45" title={t("background.accentHint")}>
            {t("background.accent")}
            <span class="size-5 rounded-full border border-white/20 bg-accent" />
          </div>
        </div>
      </FileField>

      <Show when={error()}>
        <p class="mt-3 text-sm text-red-400">{error()}</p>
      </Show>

      <RadioGroup
        class="mt-6"
        value={activePresetId() ?? ""}
        onChange={(id) => {
          setError("");
          setBackgroundPreset(id);
        }}
      >
        <RadioGroup.Label class="text-sm font-medium">{t("background.presets")}</RadioGroup.Label>
        <div class="mt-3 grid grid-cols-4 gap-2">
          <For each={PRESETS}>{(preset) => <PresetThumb preset={preset} />}</For>
        </div>
      </RadioGroup>

      <Slider
        class="mt-6 w-full select-none"
        minValue={0}
        maxValue={BLUR_MAX}
        step={1}
        value={[backgroundBlur()]}
        onChange={([px]) => setBackgroundBlur(px)}
        getValueLabel={({ values }) => `${values[0]} px`}
      >
        <div class="flex items-center justify-between">
          <Slider.Label class="text-sm font-medium">{t("background.blur")}</Slider.Label>
          <Slider.ValueLabel class="text-sm text-white/45 tabular-nums" />
        </div>
        <Slider.Track class="relative mt-3 h-1.5 w-full rounded-full bg-white/10">
          <Slider.Fill class="absolute h-full rounded-full bg-accent" />
          <Slider.Thumb class="-top-[5px] block size-4 rounded-full border border-white/20 bg-white shadow outline-none transition-shadow focus-visible:ring-4 focus-visible:ring-edge">
            <Slider.Input />
          </Slider.Thumb>
        </Slider.Track>
      </Slider>
    </>
  );
}

/** Preset tile: shows a loader until the preview video has its first frame (or failed). */
function PresetThumb(props: { preset: BackgroundPreset }) {
  const [ready, setReady] = createSignal(false);
  return (
    <RadioGroup.Item
      value={props.preset.id}
      class="group relative overflow-hidden rounded-lg border border-white/10 transition-colors hover:border-white/25 has-[:focus-visible]:ring-2 has-[:focus-visible]:ring-edge-active data-checked:border-edge-active"
    >
      <RadioGroup.ItemInput />
      <RadioGroup.ItemLabel class="block cursor-pointer">
        <video
          src={props.preset.preview}
          ref={(el) => el.readyState >= 2 && setReady(true)}
          on:loadeddata={() => setReady(true)}
          on:error={() => setReady(true)}
          autoplay
          loop
          muted
          playsinline
          preload="auto"
          disablepictureinpicture
          class="pointer-events-none aspect-4/3 w-full object-cover opacity-80 transition-opacity group-hover:opacity-100 group-data-checked:opacity-100"
        />
        <Show when={!ready()}>
          <div aria-hidden="true" class="pointer-events-none absolute inset-0 overflow-hidden bg-white/5">
            <div class="absolute inset-0 animate-skeleton-sweep bg-linear-to-r from-transparent via-white/10 to-transparent" />
          </div>
        </Show>
      </RadioGroup.ItemLabel>
    </RadioGroup.Item>
  );
}
