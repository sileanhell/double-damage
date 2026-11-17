import { Select } from "@kobalte/core/select";
import { type ApprovalInfo, approvalInfo, approvalModes, setApprovalMode } from "../lib/approval";
import { t } from "../lib/i18n";
import { currentModel } from "../lib/models";
import { CheckIcon, ChevronDownIcon, ShieldIcon } from "./icons";

const TRIGGER =
  "flex h-8 items-center gap-1.5 rounded-lg border border-white/10 bg-white/[0.03] px-2.5 text-[13px] text-white/85 outline-none transition-colors hover:border-white/20 hover:bg-white/[0.07] hover:text-white focus-visible:border-white/25 data-expanded:border-white/20 data-expanded:bg-white/[0.07] data-expanded:text-white";

/** Approval mode selector shown next to the model picker. */
export function ApprovalPicker() {
  // Local models don't get "auto": they're too easy to talk into running something harmful.
  const local = () => currentModel()?.provider === "llama-server";
  return (
    <Select<ApprovalInfo>
      options={approvalModes(local())}
      optionValue="value"
      optionTextValue={(item) => t(item.label)}
      value={approvalInfo(local())}
      onChange={(item) => item && setApprovalMode(item.value)}
      disallowEmptySelection
      placement="top-start"
      gutter={8}
      itemComponent={(props) => (
        <Select.Item
          item={props.item}
          class="flex cursor-pointer items-start justify-between gap-3 rounded-lg px-2.5 py-1.5 text-white/80 outline-none transition-colors data-highlighted:bg-white/10 data-highlighted:text-white"
        >
          <div class="flex min-w-0 flex-col">
            <Select.ItemLabel class="text-sm">{t(props.item.rawValue.label)}</Select.ItemLabel>
            <Select.ItemDescription class="text-xs text-white/45">
              {t(props.item.rawValue.description)}
            </Select.ItemDescription>
          </div>
          <Select.ItemIndicator class="pt-0.5">
            <CheckIcon class="size-4 text-accent" />
          </Select.ItemIndicator>
        </Select.Item>
      )}
    >
      <Select.Trigger title={t("approval.trigger")} aria-label={t("approval.trigger")} class={TRIGGER}>
        <ShieldIcon class="size-3.5 text-white/60" />
        <Select.Value<ApprovalInfo> class="max-w-40 truncate">{(state) => t(state.selectedOption().label)}</Select.Value>
        <Select.Icon>
          <ChevronDownIcon class="size-3 transition-transform in-data-expanded:rotate-180" />
        </Select.Icon>
      </Select.Trigger>

      <Select.Portal>
        <Select.Content class="z-50 w-72 origin-bottom-left rounded-2xl border border-white/10 bg-white/[0.06] p-2 text-white shadow-2xl backdrop-blur-2xl outline-none transition-[opacity,transform] duration-150 data-closed:scale-[0.97] data-closed:opacity-0 data-expanded:scale-100 data-expanded:opacity-100">
          <div class="px-2.5 pt-1 pb-1 text-xs text-white/40">{t("approval.heading")}</div>
          <Select.Listbox class="outline-none" />
        </Select.Content>
      </Select.Portal>
    </Select>
  );
}
