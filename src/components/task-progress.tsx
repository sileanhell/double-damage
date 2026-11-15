import { Button } from "@kobalte/core/button";
import { Progress } from "@kobalte/core/progress";
import { Show } from "solid-js";
import { formatBytes, formatSpeed } from "../lib/format";
import { t } from "../lib/i18n";
import { cancelTask, type Task, taskProgress } from "../lib/tasks";
import { CloseIcon } from "./icons";

/** Progress bar for a running backend task, with sizes and a cancel button. */
export function TaskProgress(props: { task: Task; label?: string }) {
  const fraction = () => taskProgress(props.task);
  return (
    <Progress
      value={(fraction() ?? 0) * 100}
      indeterminate={fraction() === undefined}
      getValueLabel={({ value }) => `${Math.round(value)}%`}
      class="w-full"
    >
      <div class="flex items-center gap-3">
        <Progress.Label class="min-w-0 flex-1 truncate text-sm text-white/80">{props.label ?? props.task.label}</Progress.Label>
        <Show when={props.task.speed !== undefined && props.task.received > 0}>
          <span class="shrink-0 text-xs text-white/60 tabular-nums">{formatSpeed(props.task.speed ?? 0)}</span>
        </Show>
        <span class="shrink-0 text-xs text-white/45 tabular-nums">
          {formatBytes(props.task.received)}
          <Show when={props.task.total}>{(total) => ` / ${formatBytes(total())}`}</Show>
        </span>
        <Show when={fraction() !== undefined}>
          <Progress.ValueLabel class="w-9 shrink-0 text-right text-xs text-white/60 tabular-nums" />
        </Show>
        <Button
          title={t("common.cancel")}
          aria-label={t("common.cancel")}
          onClick={() => cancelTask(props.task.id)}
          class="flex size-6 shrink-0 items-center justify-center rounded-md text-white/45 transition-colors hover:bg-white/[0.08] hover:text-white"
        >
          <CloseIcon class="size-3.5" />
        </Button>
      </div>
      <Progress.Track class="mt-2 h-1.5 w-full overflow-hidden rounded-full bg-white/10">
        {/* Kobalte only exposes the fill as a CSS variable; the width has to be applied here. */}
        <Progress.Fill
          class="h-full rounded-full bg-accent transition-[width] duration-200"
          classList={{
            "w-(--kb-progress-fill-width)": fraction() !== undefined,
            "w-[30%] animate-load-slide": fraction() === undefined,
          }}
        />
      </Progress.Track>
    </Progress>
  );
}
