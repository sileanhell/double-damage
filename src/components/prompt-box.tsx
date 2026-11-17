import { Button } from "@kobalte/core/button";
import { DropdownMenu } from "@kobalte/core/dropdown-menu";
import { TextField } from "@kobalte/core/text-field";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { createSignal, For, onCleanup, Show } from "solid-js";
import { t } from "../lib/i18n";
import { compact, send as sendPrompt, stop, turnIn } from "../lib/chat";
import { currentModel } from "../lib/models";
import { activeSessionId, messages, sessionWorkspace, setSessionWorkspace } from "../lib/sessions";
import { ApprovalPicker } from "./approval-picker";
import { ArrowUpIcon, CloseIcon, FolderIcon, PaperclipIcon, PlusIcon, StopIcon } from "./icons";
import { ModelPicker } from "./model-picker";

const SQUARE_BUTTON =
  "flex size-8 shrink-0 items-center justify-center rounded-lg border border-white/10 bg-white/[0.03] text-white/80 outline-none transition-colors hover:border-white/20 hover:bg-white/[0.07] hover:text-white focus-visible:border-white/25 data-expanded:border-white/20 data-expanded:bg-white/[0.07] data-expanded:text-white";

const MENU_ITEM =
  "flex cursor-pointer items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-sm text-white/80 outline-none transition-colors data-highlighted:bg-white/10 data-highlighted:text-white";

const fileName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path;
const sameList = (list: string[], path: string) => list.some((other) => other.toLowerCase() === path.toLowerCase());

export function PromptBox() {
  const [prompt, setPrompt] = createSignal("");
  /** Files for the next message only (absolute paths); folders below belong to the chat. */
  const [attachments, setAttachments] = createSignal<string[]>([]);
  const [dragging, setDragging] = createSignal(false);
  /**
   * Workspace folders: the open chat's own (kept with it), or, before a chat exists, the ones
   * picked for the next prompt.
   */
  const [pendingFolders, setPendingFolders] = createSignal<string[]>([]);
  const folders = () => {
    const id = activeSessionId();
    return id ? sessionWorkspace(id) : pendingFolders();
  };
  const setFolders = (next: string[]) => {
    const id = activeSessionId();
    if (id) setSessionWorkspace(id, next);
    else setPendingFolders(next);
  };

  const addFiles = (paths: string[]) => setAttachments((list) => [...list, ...paths.filter((path) => !sameList(list, path))]);
  const addFolders = (paths: string[]) => setFolders([...folders(), ...paths.filter((path) => !sameList(folders(), path))]);

  /** Dropped or pasted paths: folders join the chat's workspace, files go with the next message. */
  const addPaths = async (paths: string[]) => {
    if (paths.length === 0) return;
    const kinds = await invoke<{ path: string; dir: boolean }[]>("attachment_kinds", { paths }).catch((reason) => {
      console.error(reason);
      return [];
    });
    addFolders(kinds.filter((kind) => kind.dir).map((kind) => kind.path));
    addFiles(kinds.filter((kind) => !kind.dir).map((kind) => kind.path));
  };

  // Files dragged onto the window from Explorer.
  if ("__TAURI_INTERNALS__" in window) {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "enter" || event.payload.type === "over") setDragging(true);
      else if (event.payload.type === "leave") setDragging(false);
      else {
        setDragging(false);
        void addPaths(event.payload.paths);
      }
    });
    onCleanup(() => void unlisten.then((stop) => stop()));
  }

  /** Ctrl+V of files copied in Explorer; text and images paste as usual. */
  const paste = (event: ClipboardEvent) => {
    if (!event.clipboardData?.types.includes("Files")) return;
    void invoke<string[]>("clipboard_file_paths").then((paths) => {
      if (paths.length > 0) void addPaths(paths);
    });
    if (!event.clipboardData.types.includes("text/plain")) event.preventDefault();
  };

  const pickFiles = async () => {
    const paths = await invoke<string[]>("attachment_pick_files", { title: t("prompt.addFile") }).catch((reason) => {
      console.error(reason);
      return [];
    });
    addFiles(paths);
  };

  /** The open chat is answering (or waiting its turn): the button stops it instead. */
  const busy = () => !!turnIn(activeSessionId());
  const canSend = () => (prompt().trim().length > 0 || attachments().length > 0) && !!currentModel() && !busy();

  /** `/compact [what to keep]`: compacts the open chat instead of sending a message. */
  const command = () => /^\/compact(?:\s+([\s\S]*))?$/.exec(prompt().trim());
  const typingCommand = () => prompt().trimStart().startsWith("/");

  const send = (event: SubmitEvent) => {
    event.preventDefault();
    const model = currentModel();
    if (!canSend() || !model) return;
    const compactCommand = command();
    if (compactCommand) {
      const id = activeSessionId();
      if (!id || messages(id).length === 0) return;
      compact(id, model, compactCommand[1]?.trim());
      setPrompt("");
      return;
    }
    sendPrompt(prompt(), model, activeSessionId() ? undefined : pendingFolders(), attachments());
    setPrompt("");
    setAttachments([]);
    setPendingFolders([]);
  };

  const addFolder = async () => {
    const path = await invoke<string | null>("attachment_pick_folder", { title: t("prompt.addFolder") }).catch(
      (reason) => {
        console.error(reason);
        return null;
      },
    );
    if (path) addFolders([path]);
  };

  return (
    <form
      onSubmit={send}
      class="relative w-full rounded-2xl border bg-white/[0.03] shadow-[0_20px_60px_-20px_rgb(0_0_0/0.8)] backdrop-blur-2xl transition-colors"
      classList={{ "border-accent/60": dragging(), "border-white/10": !dragging() }}
    >
      <Show when={dragging()}>
        <div class="pointer-events-none absolute inset-0 z-10 flex items-center justify-center rounded-2xl bg-black/50 text-sm text-white/85">
          {t("prompt.dropHere")}
        </div>
      </Show>
      <TextField value={prompt()} onChange={setPrompt} class="px-5 pt-4">
        <TextField.TextArea
          autoResize
          submitOnEnter
          rows={2}
          aria-label={t("prompt.label")}
          placeholder={t("prompt.placeholder")}
          onPaste={paste}
          class="thin-scroll max-h-60 min-h-12 w-full resize-none bg-transparent text-base leading-6 text-white outline-none placeholder:text-white/45"
        />
      </TextField>

      <Show when={attachments().length > 0 || folders().length > 0}>
        <div class="flex flex-wrap gap-2 px-6 pt-3">
          <For each={folders()}>
            {(path) => (
              <span
                title={t("prompt.workspace", { path })}
                class="flex max-w-56 items-center gap-1.5 rounded-lg border border-accent/25 bg-accent/10 py-1 pr-1.5 pl-2.5 text-xs text-white/85"
              >
                <FolderIcon class="size-3 shrink-0 text-accent" />
                <span class="truncate">{fileName(path)}</span>
                <Button
                  title={t("prompt.remove")}
                  aria-label={t("prompt.removeFile", { name: fileName(path) })}
                  class="rounded p-0.5 text-white/50"
                  onClick={() => setFolders(folders().filter((other) => other !== path))}
                >
                  <CloseIcon class="size-3" />
                </Button>
              </span>
            )}
          </For>
          <For each={attachments()}>
            {(path) => (
              <span
                title={path}
                class="flex max-w-56 items-center gap-1.5 rounded-lg border border-white/10 bg-white/5 py-1 pr-1.5 pl-2.5 text-xs text-white/80"
              >
                <PaperclipIcon class="size-3 shrink-0 text-white/50" />
                <span class="truncate">{fileName(path)}</span>
                <Button
                  title={t("prompt.remove")}
                  aria-label={t("prompt.removeFile", { name: fileName(path) })}
                  class="rounded p-0.5 text-white/50"
                  onClick={() => setAttachments(attachments().filter((other) => other !== path))}
                >
                  <CloseIcon class="size-3" />
                </Button>
              </span>
            )}
          </For>
        </div>
      </Show>

      <div class="flex items-center justify-between gap-3 p-3 pt-2">
        <div class="flex items-center gap-2">
          <DropdownMenu placement="top-start" gutter={8}>
            <DropdownMenu.Trigger title={t("prompt.add")} aria-label={t("prompt.add")} class={SQUARE_BUTTON}>
              <PlusIcon class="size-4" />
            </DropdownMenu.Trigger>
            <DropdownMenu.Portal>
              <DropdownMenu.Content class="z-50 min-w-48 origin-bottom-left rounded-2xl border border-white/10 bg-white/[0.06] p-2 text-white shadow-2xl backdrop-blur-2xl outline-none transition-[opacity,transform] duration-150 data-closed:scale-[0.97] data-closed:opacity-0 data-expanded:scale-100 data-expanded:opacity-100">
                <DropdownMenu.Item class={MENU_ITEM} onSelect={() => void pickFiles()}>
                  <PaperclipIcon class="size-4 text-white/60" />
                  <DropdownMenu.ItemLabel>{t("prompt.addFile")}</DropdownMenu.ItemLabel>
                </DropdownMenu.Item>
                <DropdownMenu.Item class={MENU_ITEM} onSelect={() => void addFolder()}>
                  <FolderIcon class="size-4 text-white/60" />
                  <DropdownMenu.ItemLabel>{t("prompt.addFolder")}</DropdownMenu.ItemLabel>
                </DropdownMenu.Item>
              </DropdownMenu.Content>
            </DropdownMenu.Portal>
          </DropdownMenu>

          <ModelPicker />
          <ApprovalPicker />
        </div>

        <Show
          when={busy()}
          fallback={
            <Button
              type="submit"
              title={currentModel() ? t("prompt.send") : t("model.none")}
              aria-label={t("prompt.send")}
              disabled={!canSend()}
              class="flex size-8 items-center justify-center rounded-lg bg-edge-active text-black disabled:bg-white/[0.06] disabled:text-white/35"
            >
              <ArrowUpIcon class="size-4" />
            </Button>
          }
        >
          <Button
            type="button"
            title={t("prompt.stop")}
            aria-label={t("prompt.stop")}
            onClick={() => {
              const id = activeSessionId();
              if (id) stop(id);
            }}
            class="flex size-8 items-center justify-center rounded-lg bg-white/85 text-black transition-colors hover:bg-white"
          >
            <StopIcon class="size-4" />
          </Button>
        </Show>
      </div>
      <Show when={typingCommand()}>
        <p class="px-6 pb-3 -mt-1 text-xs" classList={{ "text-accent": !!command(), "text-white/40": !command() }}>
          {t("prompt.compactHint")}
        </p>
      </Show>
    </form>
  );
}