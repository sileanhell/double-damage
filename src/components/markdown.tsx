import { Button } from "@kobalte/core/button";
import { Dialog } from "@kobalte/core/dialog";
import { ToggleButton } from "@kobalte/core/toggle-button";
import { type Accessor, createEffect, createMemo, createSignal, Index, type JSX, Match, on, onCleanup, Show, Switch } from "solid-js";
import { t } from "../lib/i18n";
import { highlight } from "../lib/markdown/highlight";
import "../lib/markdown/markdown.css";
import { type MermaidResult, renderMermaid } from "../lib/markdown/mermaid";
import { canOpen, openExternal, urlParts } from "../lib/markdown/open-link";
import { type Block, blockParser, escapeHtml, safeUrl } from "../lib/markdown/render";
import { CheckIcon } from "./icons";

type CodeBlockData = Extract<Block, { kind: "code" }>;
type MermaidBlockData = Extract<Block, { kind: "mermaid" }>;

/**
 * Renders an assistant reply (GFM). Built for streaming: `text` may grow every frame; only the
 * last top-level block is re-parsed and re-rendered, finished blocks keep their DOM.
 * Raw HTML shows as text, images aren't loaded, links open a confirmation dialog.
 */
export function Markdown(props: { text: string; streaming?: boolean; class?: string }): JSX.Element {
  const parse = blockParser();
  const blocks = createMemo(() => parse(props.text));
  const [link, setLink] = createSignal<string>();

  // Links are plain elements with `data-href` (see render.ts); nothing here may navigate the webview.
  const linkAt = (event: Event): string | undefined | null => {
    const target = event.target as Element | null;
    const marked = target?.closest?.("[data-href]");
    if (marked) return marked.getAttribute("data-href");
    // Anchors that slipped in some other way (e.g. inside a mermaid SVG).
    const anchor = target?.closest?.("a");
    if (anchor) return safeUrl(anchor.getAttribute("href") ?? anchor.getAttribute("xlink:href")) ?? null;
    return undefined;
  };
  const onClick = (event: MouseEvent) => {
    const url = linkAt(event);
    if (url === undefined) return;
    event.preventDefault();
    if (url) setLink(url);
  };
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    const url = linkAt(event);
    if (!url) return;
    event.preventDefault();
    setLink(url);
  };

  return (
    <div class={`md ${props.class ?? ""}`} onClick={onClick} onAuxClick={onClick} onKeyDown={onKeyDown}>
      <Index each={blocks()}>
        {(block, index) => <BlockView block={block} live={!!props.streaming && index === blocks().length - 1} />}
      </Index>
      <Show when={link()}>{(url) => <LinkDialog url={url()} onClose={() => setLink(undefined)} />}</Show>
    </div>
  );
}

function BlockView(props: { block: Accessor<Block>; live: boolean }) {
  // Switch on the kind only, so a growing block updates in place instead of being rebuilt.
  const kind = createMemo(() => props.block().kind);
  return (
    <Switch>
      <Match when={kind() === "html"}>
        <div class="md-block" innerHTML={(props.block() as Extract<Block, { kind: "html" }>).html} />
      </Match>
      <Match when={kind() === "code"}>
        <CodeBlock
          lang={(props.block() as CodeBlockData).lang}
          code={(props.block() as CodeBlockData).code}
          complete={(props.block() as CodeBlockData).closed || !props.live}
        />
      </Match>
      <Match when={kind() === "mermaid"}>
        <MermaidBlock
          code={(props.block() as MermaidBlockData).code}
          complete={(props.block() as MermaidBlockData).closed || !props.live}
        />
      </Match>
    </Switch>
  );
}

const HEADER_BUTTON =
  "inline-flex h-6 items-center gap-1 rounded-md px-2 text-xs text-white/50 outline-none transition-colors hover:bg-white/[0.07] hover:text-white/90 focus-visible:bg-white/[0.07] data-[pressed]:text-white/90";

function CopyButton(props: { text: string; label?: string }) {
  const [copied, setCopied] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(timer));
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(props.text);
    } catch {
      return;
    }
    setCopied(true);
    clearTimeout(timer);
    timer = setTimeout(() => setCopied(false), 1500);
  };
  return (
    <Button class={HEADER_BUTTON} onClick={copy}>
      <Show when={copied()} fallback={props.label ?? t("markdown.copy")}>
        <CheckIcon class="size-3.5" />
        {t("markdown.copied")}
      </Show>
    </Button>
  );
}

/** Code block chrome: a header with the label, extra controls and Copy; the body below. */
function Frame(props: { label: string; copy: string; controls?: JSX.Element; children: JSX.Element }) {
  return (
    <div class="md-frame overflow-hidden rounded-xl border border-white/10 bg-black/30">
      <div class="flex h-8 items-center gap-2 border-b border-white/[0.06] bg-white/[0.03] pr-1 pl-3">
        <span class="min-w-0 flex-1 truncate font-mono text-xs text-white/45">{props.label}</span>
        {props.controls}
        <CopyButton text={props.copy} />
      </div>
      {props.children}
    </div>
  );
}

function Code(props: { lang: string; code: string; complete: boolean }) {
  // `?? ""`: for a moment while a block changes kind, the old view may still read the new block.
  const html = createMemo(() => highlight(props.code ?? "", props.lang ?? "", props.complete) ?? escapeHtml(props.code ?? ""));
  return (
    <pre class="md-code thin-scroll overflow-x-auto px-3.5 py-2.5 font-mono text-[13px] leading-[1.6] whitespace-pre text-white/85">
      <code class="hljs" innerHTML={html()} />
    </pre>
  );
}

function CodeBlock(props: { lang: string; code: string; complete: boolean }) {
  return (
    <Frame label={props.lang || t("markdown.code")} copy={props.code}>
      <Code lang={props.lang} code={props.code} complete={props.complete} />
    </Frame>
  );
}

/** A ```mermaid block: source while it streams, the diagram once it's complete. */
function MermaidBlock(props: { code: string; complete: boolean }) {
  const [result, setResult] = createSignal<MermaidResult>();
  const [showSource, setShowSource] = createSignal(false);
  createEffect(
    on(
      () => (props.complete ? props.code : undefined),
      (source) => {
        setResult(undefined);
        if (!source?.trim()) return;
        let alive = true;
        onCleanup(() => (alive = false));
        void renderMermaid(source).then((next) => alive && setResult(next));
      },
    ),
  );
  const svg = () => {
    const r = result();
    return r && "svg" in r ? r.svg : undefined;
  };
  const error = () => {
    const r = result();
    return r && "error" in r ? r.error : undefined;
  };

  return (
    <Show when={svg()} fallback={
      <div class="flex flex-col gap-1.5">
        <CodeBlock lang="mermaid" code={props.code} complete={props.complete} />
        <Show when={error() !== undefined}>
          <p class="text-xs leading-5 text-red-300/80">{t("markdown.diagramError", { error: error() || "?" })}</p>
        </Show>
      </div>
    }>
      {(diagram) => (
        <Frame
          label={t("markdown.diagram")}
          copy={props.code}
          controls={
            <ToggleButton class={HEADER_BUTTON} pressed={showSource()} onChange={setShowSource}>
              {showSource() ? t("markdown.hideSource") : t("markdown.showSource")}
            </ToggleButton>
          }
        >
          <Show when={!showSource()} fallback={<Code lang="mermaid" code={props.code} complete />}>
            <div class="md-mermaid thin-scroll flex justify-center overflow-x-auto p-4" innerHTML={diagram()} />
          </Show>
        </Frame>
      )}
    </Show>
  );
}

const DIALOG_BUTTON = "h-8 rounded-lg px-3 text-[13px] outline-none transition-colors";

/** Asks before a link from the (untrusted) reply leaves the app; shows where it really goes. */
function LinkDialog(props: { url: string; onClose: () => void }) {
  const parts = () => urlParts(props.url);
  const mail = () => props.url.startsWith("mailto:");
  const open = () => {
    void openExternal(props.url).catch((error) => console.error("open link", error));
    props.onClose();
  };
  const copy = () => {
    void navigator.clipboard.writeText(mail() ? parts().host : props.url).catch(() => {});
    props.onClose();
  };

  return (
    <Dialog open onOpenChange={(open) => !open && props.onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay class="fixed inset-0 z-50 bg-black/60 backdrop-blur-md" />
        <div class="fixed inset-0 z-50 flex justify-center px-6 pt-[18vh]">
          <Dialog.Content class="h-fit w-full max-w-lg rounded-2xl border border-white/10 bg-white/[0.06] p-5 text-white shadow-2xl outline-none backdrop-blur-2xl">
            <Dialog.Title class="text-sm font-medium text-white/90">
              {mail() ? t("markdown.link.mailTitle") : t("markdown.link.title")}
            </Dialog.Title>
            <Dialog.Description class="mt-2 text-sm text-white/55">{t("markdown.link.hint")}</Dialog.Description>
            <p class="thin-scroll mt-3 max-h-40 overflow-y-auto rounded-lg border border-white/10 bg-black/30 px-3 py-2 font-mono text-[13px] leading-5 break-all text-white/45 select-text">
              {parts().before}
              <span class="font-semibold text-white">{parts().host}</span>
              {parts().after}
            </p>
            <div class="mt-4 flex flex-wrap justify-end gap-2">
              <Button onClick={props.onClose} class={`${DIALOG_BUTTON} text-white/60 hover:bg-white/[0.06] hover:text-white`}>
                {t("common.cancel")}
              </Button>
              <Button onClick={copy} class={`${DIALOG_BUTTON} border border-white/10 text-white/80 hover:bg-white/[0.07]`}>
                {mail() ? t("markdown.link.copyAddress") : t("markdown.link.copy")}
              </Button>
              <Show when={canOpen(props.url)}>
                <Button onClick={open} class={`${DIALOG_BUTTON} bg-accent font-medium text-accent-fg hover:brightness-110`}>
                  {t("markdown.link.open")}
                </Button>
              </Show>
            </div>
          </Dialog.Content>
        </div>
      </Dialog.Portal>
    </Dialog>
  );
}
