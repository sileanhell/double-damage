import { Select } from "@kobalte/core/select";
import type { JSX } from "solid-js";
import { CheckIcon, ChevronDownIcon } from "./icons";

type Option<T extends string> = { value: T; label: string };

const TRIGGER = {
  ghost:
    "h-8 rounded-lg px-2.5 text-white/75 transition-colors hover:bg-white/10 hover:text-white focus-visible:bg-white/10 data-expanded:bg-white/10 data-expanded:text-white",
  outline:
    "h-8 rounded-lg border border-white/10 bg-white/[0.03] px-2.5 text-white/85 transition-colors hover:border-white/20 hover:bg-white/[0.07] hover:text-white focus-visible:border-white/25 data-expanded:border-white/20 data-expanded:bg-white/[0.07] data-expanded:text-white",
};

export function Dropdown<T extends string>(props: {
  value: T;
  options: readonly Option<T>[];
  onChange: (value: T) => void;
  icon?: JSX.Element;
  title: string;
  variant?: keyof typeof TRIGGER;
  placement?: "top-start" | "bottom-start";
}) {
  return (
    <Select<Option<T>>
      value={props.options.find((option) => option.value === props.value)}
      onChange={(option) => option && props.onChange(option.value)}
      options={[...props.options]}
      optionValue="value"
      optionTextValue="label"
      disallowEmptySelection
      placement={props.placement ?? "top-start"}
      gutter={8}
      itemComponent={(itemProps) => (
        <Select.Item
          item={itemProps.item}
          class="flex cursor-pointer items-center justify-between gap-4 rounded-lg px-2.5 py-1.5 text-sm text-white/80 outline-none transition-colors data-highlighted:bg-white/10 data-highlighted:text-white"
        >
          <Select.ItemLabel class="whitespace-nowrap">{itemProps.item.rawValue.label}</Select.ItemLabel>
          <Select.ItemIndicator>
            <CheckIcon class="size-4 text-accent" />
          </Select.ItemIndicator>
        </Select.Item>
      )}
    >
      <Select.Trigger
        aria-label={props.title}
        title={props.title}
        class={`flex items-center gap-1.5 text-[13px] outline-none ${TRIGGER[props.variant ?? "ghost"]}`}
      >
        {props.icon}
        <Select.Value<Option<T>> class="max-w-40 truncate">{(state) => state.selectedOption()?.label}</Select.Value>
        <Select.Icon class="transition-transform data-expanded:rotate-180">
          <ChevronDownIcon class="size-3" />
        </Select.Icon>
      </Select.Trigger>
      <Select.Portal>
        <Select.Content class="z-50 min-w-48 origin-top-left rounded-xl border border-white/10 bg-white/[0.06] p-1.5 text-white shadow-2xl backdrop-blur-2xl transition-[opacity,transform] duration-150 data-closed:scale-[0.97] data-closed:opacity-0 data-expanded:scale-100 data-expanded:opacity-100">
          <div class="px-2.5 pt-1.5 pb-1 text-xs text-white/40">{props.title}</div>
          <Select.Listbox class="outline-none" />
        </Select.Content>
      </Select.Portal>
    </Select>
  );
}
