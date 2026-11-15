import { Show } from "solid-js";
import { formatBytes } from "../lib/format";
import { estimateFit, type FitEstimate, type FitExtra, hardware } from "../lib/hardware";
import { t } from "../lib/i18n";

const BADGE = "shrink-0 cursor-default rounded-md px-1.5 py-0.5 text-[11px] whitespace-nowrap";
const YES = "bg-emerald-400/10 text-emerald-300";
const NO = "bg-white/[0.05] text-white/40";
const EXTRA: Record<FitExtra, string> = {
  partial: "bg-sky-400/10 text-sky-300",
  tight: "bg-orange-400/10 text-orange-300",
  none: "bg-red-400/10 text-red-300",
};
const EXTRA_LABEL = { partial: "fit.partial", tight: "fit.tight", none: "fit.none" } as const;

function details(fit: FitEstimate) {
  return [
    t("fit.need", { size: formatBytes(fit.need) }),
    fit.gpus.length > 0
      ? t("fit.vram", { size: formatBytes(fit.vram), gpu: fit.gpus.join(" + ") })
      : t("fit.noGpu"),
    t("fit.ram", { size: formatBytes(fit.ram) }),
  ].join("\n");
}

/**
 * Whether a model of `size` bytes runs on this machine: always a GPU and a CPU badge, plus
 * "partly fits" / "tight fit" / "not enough resources" when relevant.
 */
export function FitBadge(props: { size: number }) {
  const estimate = () => {
    const hw = hardware();
    return hw && props.size > 0 ? estimateFit(props.size, hw) : undefined;
  };

  return (
    <Show when={estimate()}>
      {(fit) => (
        <span class="flex flex-wrap items-center gap-1" title={details(fit())}>
          <span class={`${BADGE} ${fit().gpu ? YES : NO}`}>{t(fit().gpu ? "fit.gpuYes" : "fit.gpuNo")}</span>
          <span class={`${BADGE} ${fit().cpu ? YES : NO}`}>{t(fit().cpu ? "fit.cpuYes" : "fit.cpuNo")}</span>
          <Show when={fit().extra}>
            {(extra) => <span class={`${BADGE} ${EXTRA[extra()]}`}>{t(EXTRA_LABEL[extra()])}</span>}
          </Show>
        </span>
      )}
    </Show>
  );
}
