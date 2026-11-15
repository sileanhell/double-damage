import { type RouteSectionProps, useLocation } from "@solidjs/router";
import { createEffect, lazy, on, Show } from "solid-js";
import { BackgroundMedia } from "../components/background-media";
import { backgroundBlur } from "../lib/background";
import {
  SIDEBAR_DEFAULT,
  setSidebarWidth,
  setWidgetsWidth,
  sidebarWidth,
  WIDGETS_DEFAULT,
  widgetsWidth,
} from "../lib/layout";
import { PanelResizer } from "../components/panel-resizer";
import { Sidebar } from "../components/sidebar";
import { discardDraftsExcept } from "../lib/sessions";
import { widgetsEnabled } from "../lib/features";
import { setWidgetsPanelOpen, widgetsPanelOpen } from "../lib/widgets";

// Loaded on demand so a disabled widgets feature never fetches its code.
const WidgetsPanel = lazy(() => import("../components/widgets-panel").then((m) => ({ default: m.WidgetsPanel })));

/** Shell shared by every page: backdrop, sidebar and the main panel the route renders into. */
export function AppLayout(props: RouteSectionProps) {
  const location = useLocation();
  // Leaving the chat page (e.g. to settings) abandons an empty "New chat" draft.
  createEffect(
    on(
      () => location.pathname,
      (path) => {
        if (path !== "/") discardDraftsExcept(null);
      },
    ),
  );

  return (
    <div class="relative isolate flex h-full overflow-hidden bg-(--surface)">
      {/* The user background only shows through as a faint, heavily blurred texture. */}
      <BackgroundMedia
        class="absolute inset-0 -z-10 size-full scale-110 opacity-35"
        style={{ filter: backgroundBlur() > 0 ? `blur(${backgroundBlur()}px)` : undefined }}
      />
      <div class="absolute inset-0 -z-10 bg-[linear-gradient(180deg,var(--shade-top),var(--shade-bottom))]" />

      <Sidebar />
      <PanelResizer side="left" width={sidebarWidth} setWidth={setSidebarWidth} defaultWidth={SIDEBAR_DEFAULT} />

      <section class="relative min-w-0 flex-1">
        <div class="relative h-full">{props.children}</div>
      </section>

      <Show when={widgetsEnabled() && widgetsPanelOpen()}>
        <PanelResizer side="right" width={widgetsWidth} setWidth={setWidgetsWidth} defaultWidth={WIDGETS_DEFAULT} />
        <WidgetsPanel onClose={() => setWidgetsPanelOpen(false)} />
      </Show>
    </div>
  );
}
