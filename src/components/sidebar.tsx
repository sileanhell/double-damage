import { Button } from "@kobalte/core/button";
import { useLocation, useNavigate } from "@solidjs/router";
import { createMemo, For, Show } from "solid-js";
import { t } from "../lib/i18n";
import { sidebarWidth } from "../lib/layout";
import { sessions, startNewChat } from "../lib/sessions";
import { EditIcon, SettingsIcon } from "./icons";
import { SessionItem } from "./session-item";
import { SessionSearch } from "./session-search";

export function Sidebar() {
  const navigate = useNavigate();
  const location = useLocation();

  const onHome = () => location.pathname === "/";
  const current = (active: boolean) => (active ? "page" : undefined);

  const pinned = createMemo(() => sessions().filter((session) => session.pinned));
  const unpinned = createMemo(() => sessions().filter((session) => !session.pinned));

  return (
    <aside
      class="relative z-10 flex h-full min-w-0 shrink-0 flex-col border-r border-white/[0.07] bg-white/[0.02] p-4"
      style={{ width: `${sidebarWidth()}vw` }}
    >
      <Button
        onClick={() => {
          startNewChat();
          navigate("/");
        }}
        class="flex h-10 shrink-0 items-center gap-3 rounded-xl px-3 text-sm text-white/85 transition-colors hover:bg-white/[0.06] hover:text-white aria-[current=page]:bg-white/[0.08] aria-[current=page]:text-white"
      >
        <EditIcon class="size-4 shrink-0" />
        <span class="truncate">{t("sidebar.newChat")}</span>
      </Button>
      <Button
        onClick={() => navigate("/settings")}
        aria-current={current(location.pathname === "/settings")}
        class="mt-0.5 flex h-10 shrink-0 items-center gap-3 rounded-xl px-3 text-sm text-white/85 transition-colors hover:bg-white/[0.06] hover:text-white aria-[current=page]:bg-white/[0.08] aria-[current=page]:text-white"
      >
        <SettingsIcon class="size-4 shrink-0" />
        <span class="truncate">{t("sidebar.settings")}</span>
      </Button>

      <Show when={pinned().length > 0}>
        <h2 class="mt-5 mb-2 pr-1.5 pl-3 text-xs font-medium tracking-wider text-white/40 uppercase">
          {t("sidebar.pinned")}
        </h2>
        <For each={pinned()}>{(session) => <SessionItem session={session} onHome={onHome} />}</For>
      </Show>

      <div class="mt-5 mb-2 flex items-center justify-between pr-1.5 pl-3">
        <h2 id="sessions-label" class="text-xs font-medium tracking-wider text-white/40 uppercase">
          {t("sidebar.sessions")}
        </h2>
        <SessionSearch />
      </div>
      <nav aria-labelledby="sessions-label" class="thin-scroll -mr-2 min-h-0 flex-1 overflow-y-auto pr-2">
        <Show
          when={unpinned().length > 0}
          fallback={
            <Show when={pinned().length === 0}>
              <p class="px-3 py-1 text-sm text-white/30">{t("sidebar.empty")}</p>
            </Show>
          }
        >
          <For each={unpinned()}>{(session) => <SessionItem session={session} onHome={onHome} />}</For>
        </Show>
      </nav>
    </aside>
  );
}
