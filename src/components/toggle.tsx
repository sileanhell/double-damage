import { Switch } from "@kobalte/core/switch";
import { Show } from "solid-js";

/** On/off row: label and optional description on the left, switch on the right. */
export function Toggle(props: {
  label: string;
  description?: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <Switch class="flex items-center justify-between gap-4" checked={props.checked} onChange={props.onChange}>
      <div>
        <Switch.Label class="block text-base font-medium">{props.label}</Switch.Label>
        <Show when={props.description}>
          <Switch.Description class="mt-1 text-sm text-white/45">{props.description}</Switch.Description>
        </Show>
      </div>
      <Switch.Input class="peer" />
      <Switch.Control class="inline-flex h-6 w-11 shrink-0 cursor-pointer items-center rounded-full border border-white/10 bg-white/10 px-0.5 transition-colors peer-focus-visible:ring-2 peer-focus-visible:ring-edge-active data-checked:border-transparent data-checked:bg-accent">
        <Switch.Thumb class="size-[18px] rounded-full bg-white shadow transition-transform data-checked:translate-x-5" />
      </Switch.Control>
    </Switch>
  );
}
