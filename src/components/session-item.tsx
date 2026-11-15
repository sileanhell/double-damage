import { Button } from "@kobalte/core/button";
import { ContextMenu } from "@kobalte/core/context-menu";
import { Dialog } from "@kobalte/core/dialog";
import { TextField } from "@kobalte/core/text-field";
import { useNavigate } from "@solidjs/router";
import { createSignal } from "solid-js";
import { t } from "../lib/i18n";
import {
  activeSessionId,
  deleteSession,
  openSession,
  renameSession,
  type Session,
  sessionTitle,
  togglePinned,
} from "../lib/sessions";
import { EditIcon, PinIcon, TrashIcon } from "./icons";

const MENU_ITEM =
  "flex w-full cursor-pointer items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-sm text-white/80 outline-none transition-colors data-highlighted:bg-white/[0.08] data-highlighted:text-white";

/** One session row; right-click opens actions (pin, rename, delete). */
export function SessionItem(props: { session: Session; onHome: () => boolean }) {
  const navigate = useNavigate();
  const [renaming, setRenaming] = createSignal(false);
  const [name, setName] = createSignal("");

  const startRename = () => {
    setName(sessionTitle(props.session));
    setRenaming(true);
  };
  const saveRename = () => {
    renameSession(props.session.id, name());
    setRenaming(false);
  };

  return (
    <>
      <ContextMenu>
        <ContextMenu.Trigger
          as={Button}
          title={sessionTitle(props.session)}
          onClick={() => {
            openSession(props.session.id);
            navigate("/");
          }}
          aria-current={props.onHome() && activeSessionId() === props.session.id ? "page" : undefined}
          class="block h-9 w-full truncate rounded-xl px-3 text-left text-[13px] text-white transition-colors hover:bg-white/[0.05] aria-[current=page]:bg-white/[0.08] data-expanded:bg-white/[0.05]"
        >
          {sessionTitle(props.session)}
        </ContextMenu.Trigger>
        <ContextMenu.Portal>
          <ContextMenu.Content class="z-50 min-w-44 rounded-xl border border-white/10 bg-white/[0.06] p-1.5 shadow-2xl outline-none backdrop-blur-2xl">
            <ContextMenu.Item onSelect={() => togglePinned(props.session.id)} class={MENU_ITEM}>
              <PinIcon class="size-3.5 shrink-0 text-white/50" />
              <span class="min-w-0 flex-1">{t(props.session.pinned ? "session.unpin" : "session.pin")}</span>
            </ContextMenu.Item>
            <ContextMenu.Item onSelect={startRename} class={MENU_ITEM}>
              <EditIcon class="size-3.5 shrink-0 text-white/50" />
              <span class="min-w-0 flex-1">{t("session.rename")}</span>
            </ContextMenu.Item>
            <ContextMenu.Separator class="-mx-1.5 my-1.5 h-px border-0 bg-white/10" />
            <ContextMenu.Item
              onSelect={() => deleteSession(props.session.id)}
              class="flex w-full cursor-pointer items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-sm text-red-300/80 outline-none transition-colors data-highlighted:bg-red-500/10 data-highlighted:text-red-300"
            >
              <TrashIcon class="size-3.5 shrink-0" />
              <span class="min-w-0 flex-1">{t("common.delete")}</span>
            </ContextMenu.Item>
          </ContextMenu.Content>
        </ContextMenu.Portal>
      </ContextMenu>

      <Dialog open={renaming()} onOpenChange={setRenaming}>
        <Dialog.Portal>
          <Dialog.Overlay class="fixed inset-0 z-50 bg-black/60 backdrop-blur-md" />
          <div class="fixed inset-0 z-50 flex justify-center px-6 pt-[20vh]">
            <Dialog.Content class="h-fit w-full max-w-sm rounded-2xl border border-white/10 bg-white/[0.06] p-4 text-white shadow-2xl outline-none backdrop-blur-2xl">
              <Dialog.Title class="mb-3 text-sm font-medium text-white/90">{t("session.renameTitle")}</Dialog.Title>
              <TextField value={name()} onChange={setName}>
                <TextField.Input
                  aria-label={t("session.nameLabel")}
                  autocomplete="off"
                  onKeyDown={(event) => event.key === "Enter" && saveRename()}
                  class="h-10 w-full rounded-xl border border-white/10 bg-black/40 px-3 text-sm text-white outline-none placeholder:text-white/40 focus:border-white/25"
                />
              </TextField>
              <div class="mt-4 flex justify-end gap-2">
                <Button
                  onClick={() => setRenaming(false)}
                  class="h-8 rounded-lg px-3 text-[13px] text-white/60 transition-colors hover:bg-white/[0.06] hover:text-white"
                >
                  {t("common.cancel")}
                </Button>
                <Button
                  onClick={saveRename}
                  class="h-8 rounded-lg bg-accent px-3 text-[13px] text-accent-fg transition-opacity hover:opacity-85"
                >
                  {t("common.save")}
                </Button>
              </div>
            </Dialog.Content>
          </div>
        </Dialog.Portal>
      </Dialog>
    </>
  );
}
