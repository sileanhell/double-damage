import { Button } from "@kobalte/core/button";
import { Show } from "solid-js";
import { t } from "../lib/i18n";
import { widgetsEnabled } from "../lib/features";
import { setWidgetsPanelOpen, widgetsPanelOpen } from "../lib/widgets";
import { MoreIcon } from "./icons";

export function ChatHeader() {
  return (
    <header class="flex shrink-0 items-center justify-end px-4 pt-4">
      <Show when={widgetsEnabled() && !widgetsPanelOpen()}>
        <Button
          aria-label={t("widgets.title")}
          title={t("widgets.title")}
          onClick={() => setWidgetsPanelOpen(true)}
          class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-white/10 bg-white/[0.03] text-white/80 outline-none transition-colors hover:border-white/20 hover:bg-white/[0.07] hover:text-white focus-visible:border-white/25"
        >
          <MoreIcon class="size-4" />
        </Button>
      </Show>
    </header>
  );
}
