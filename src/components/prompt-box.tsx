import { Button } from "@kobalte/core/button";
import { DropdownMenu } from "@kobalte/core/dropdown-menu";
import { FileField } from "@kobalte/core/file-field";
import { TextField } from "@kobalte/core/text-field";
import { invoke } from "@tauri-apps/api/core";
import { createSignal, For, Show } from "solid-js";
import { t } from "../lib/i18n";
import { submitPrompt } from "../lib/sessions";
import { ApprovalPicker } from "./approval-picker";
import { ArrowUpIcon, CloseIcon, FolderIcon, PaperclipIcon, PlusIcon } from "./icons";
import { ModelPicker } from "./model-picker";

const SQUARE_BUTTON =
  "flex size-8 shrink-0 items-center justify-center rounded-lg border border-white/10 bg-white/[0.03] text-white/80 outline-none transition-colors hover:border-white/20 hover:bg-white/[0.07] hover:text-white focus-visible:border-white/25 data-expanded:border-white/20 data-expanded:bg-white/[0.07] data-expanded:text-white";

const MENU_ITEM =
  "flex cursor-pointer items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-sm text-white/80 outline-none transition-colors data-highlighted:bg-white/10 data-highlighted:text-white";

type Attachment = { kind: "file"; file: File } | { kind: "folder"; path: string };

const attachmentName = (item: Attachment) =>
  item.kind === "file" ? item.file.name : (item.path.split(/[\\/]/).filter(Boolean).pop() ?? item.path);

export function PromptBox() {
  const [prompt, setPrompt] = createSignal("");
  const [attachments, setAttachments] = createSignal<Attachment[]>([]);
  let fileInput!: HTMLInputElement;

  const canSend = () => prompt().trim().length > 0;

  const send = (event: SubmitEvent) => {
    event.preventDefault();
    if (!canSend()) return;
    submitPrompt(prompt());
    setPrompt("");
    setAttachments([]);
  };

  const addFolder = async () => {
    const path = await invoke<string | null>("attachment_pick_folder", { title: t("prompt.addFolder") }).catch(
      (reason) => {
        console.error(reason);
        return null;
      },
    );
    if (!path) return;
    if (attachments().some((item) => item.kind === "folder" && item.path === path)) return;
    setAttachments([...attachments(), { kind: "folder", path }]);
  };

  return (
    <form
      onSubmit={send}
      class="relative w-full rounded-2xl border border-white/10 bg-white/[0.03] shadow-[0_20px_60px_-20px_rgb(0_0_0/0.8)] backdrop-blur-2xl"
    >
      <TextField value={prompt()} onChange={setPrompt} class="px-5 pt-4">
        <TextField.TextArea
          autoResize
          submitOnEnter
          rows={2}
          aria-label={t("prompt.label")}
          placeholder={t("prompt.placeholder")}
          class="thin-scroll max-h-60 min-h-12 w-full resize-none bg-transparent text-base leading-6 text-white outline-none placeholder:text-white/45"
        />
      </TextField>

      <Show when={attachments().length > 0}>
        <div class="flex flex-wrap gap-2 px-6 pt-3">
          <For each={attachments()}>
            {(item) => (
              <span
                title={item.kind === "folder" ? item.path : undefined}
                class="flex max-w-56 items-center gap-1.5 rounded-lg border border-white/10 bg-white/5 py-1 pr-1.5 pl-2.5 text-xs text-white/80"
              >
                <Show when={item.kind === "folder"} fallback={<PaperclipIcon class="size-3 shrink-0 text-white/50" />}>
                  <FolderIcon class="size-3 shrink-0 text-white/50" />
                </Show>
                <span class="truncate">{attachmentName(item)}</span>
                <Button
                  title={t("prompt.remove")}
                  aria-label={t("prompt.removeFile", { name: attachmentName(item) })}
                  class="rounded p-0.5 text-white/50"
                  onClick={() => setAttachments(attachments().filter((other) => other !== item))}
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
          {/* FileField keeps its own uncontrolled list, so it only serves as the picker here (opened
              from the menu); the attachments shown above live in our signal and can be cleared on send. */}
          <FileField
            multiple
            maxFiles={Number.POSITIVE_INFINITY}
            class="contents"
            onFileAccept={(files) => {
              setAttachments([...attachments(), ...files.map((file) => ({ kind: "file" as const, file }))]);
              fileInput.value = ""; // allow picking the same file again
            }}
          >
            <FileField.HiddenInput ref={fileInput} />
          </FileField>

          <DropdownMenu placement="top-start" gutter={8}>
            <DropdownMenu.Trigger title={t("prompt.add")} aria-label={t("prompt.add")} class={SQUARE_BUTTON}>
              <PlusIcon class="size-4" />
            </DropdownMenu.Trigger>
            <DropdownMenu.Portal>
              <DropdownMenu.Content class="z-50 min-w-48 origin-bottom-left rounded-2xl border border-white/10 bg-white/[0.06] p-2 text-white shadow-2xl backdrop-blur-2xl outline-none transition-[opacity,transform] duration-150 data-closed:scale-[0.97] data-closed:opacity-0 data-expanded:scale-100 data-expanded:opacity-100">
                <DropdownMenu.Item class={MENU_ITEM} onSelect={() => fileInput.click()}>
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

        <Button
          type="submit"
          title={t("prompt.send")}
          aria-label={t("prompt.send")}
          disabled={!canSend()}
          class="flex size-8 items-center justify-center rounded-lg bg-edge-active text-black disabled:bg-white/[0.06] disabled:text-white/35"
        >
          <ArrowUpIcon class="size-4" />
        </Button>
      </div>
    </form>
  );
}
