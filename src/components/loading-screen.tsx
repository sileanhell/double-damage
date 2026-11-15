import { Progress } from "@kobalte/core/progress";
import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { backgroundReady } from "../lib/background";
import { t } from "../lib/i18n";

const MIN_VISIBLE_MS = 500;
const MAX_WAIT_MS = 15_000;
const FADE_MS = 700;

/**
 * Covers the app until the background media is ready to paint, then fades out.
 * A short minimum visibility keeps a cached background from flashing the screen
 * for a single frame; a failsafe timeout opens the app even if media stalls.
 */
export function LoadingScreen() {
  const [fading, setFading] = createSignal(false);
  const [gone, setGone] = createSignal(false);
  const [stalled, setStalled] = createSignal(false);
  const mountedAt = Date.now();

  onMount(() => {
    const failsafe = setTimeout(() => setStalled(true), MAX_WAIT_MS);
    onCleanup(() => clearTimeout(failsafe));
  });

  createEffect(() => {
    if (!backgroundReady() && !stalled()) return;
    const delay = Math.max(0, MIN_VISIBLE_MS - (Date.now() - mountedAt));
    const fade = setTimeout(() => setFading(true), delay);
    const remove = setTimeout(() => setGone(true), delay + FADE_MS + 100);
    onCleanup(() => {
      clearTimeout(fade);
      clearTimeout(remove);
    });
  });

  return (
    <Show when={!gone()}>
      <div
        class={`fixed inset-0 z-[100] flex items-center justify-center bg-black transition-opacity duration-700 ease-in-out ${
          fading() ? "pointer-events-none opacity-0" : "opacity-100"
        }`}
      >
        <Progress indeterminate aria-label={t("common.loading")} class="w-44">
          <Progress.Track class="h-1 w-full overflow-hidden rounded-full bg-white/10">
            <Progress.Fill class="h-full w-[30%] animate-load-slide rounded-full bg-white" />
          </Progress.Track>
        </Progress>
      </div>
    </Show>
  );
}
