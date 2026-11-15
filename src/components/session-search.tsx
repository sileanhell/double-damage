import { Button } from "@kobalte/core/button";
import { Dialog } from "@kobalte/core/dialog";
import { TextField } from "@kobalte/core/text-field";
import { useNavigate } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { locale, t } from "../lib/i18n";
import { openSession, type Session, sessions, sessionTitle } from "../lib/sessions";
import { SearchIcon } from "./icons";

type Snippet = { text: string; at: number };
type Hit = { session: Session; snippet?: Snippet };

const flatten = (text: string) => text.replace(/\s+/g, " ").trim();

/** Matches by title or by any message text; content hits carry a window of text around the match. */
function search(list: Session[], query: string): Hit[] {
  const needle = flatten(query).toLowerCase();
  const hits: Hit[] = [];
  for (const session of list) {
    const texts = (session.messages ?? []).map((message) => flatten(message.text));
    if (!needle) {
      hits.push({ session, snippet: texts[0] ? { text: texts[0], at: 0 } : undefined });
      continue;
    }
    const titleHit = sessionTitle(session).toLowerCase().includes(needle);
    let snippet: Snippet | undefined;
    for (const text of texts) {
      const at = text.toLowerCase().indexOf(needle);
      if (at >= 0) {
        snippet = { text, at };
        break;
      }
    }
    if (titleHit || snippet) hits.push({ session, snippet });
  }
  return hits;
}

/** Renders `text` with the first case-insensitive occurrence of `needle` highlighted. */
function Highlight(props: { text: string; needle: string }) {
  const parts = () => {
    const needle = flatten(props.needle);
    const at = needle ? props.text.toLowerCase().indexOf(needle.toLowerCase()) : -1;
    if (at < 0) return { before: props.text, match: "", after: "" };
    return {
      before: props.text.slice(0, at),
      match: props.text.slice(at, at + needle.length),
      after: props.text.slice(at + needle.length),
    };
  };
  return (
    <>
      {parts().before}
      <Show when={parts().match}>
        <mark class="rounded-sm bg-accent/30 text-white">{parts().match}</mark>
      </Show>
      {parts().after}
    </>
  );
}

/** Trims long message text so the match stays visible near the start of a one-line preview. */
const excerpt = ({ text, at }: Snippet) => (at > 40 ? `…${text.slice(at - 30)}` : text);

const formatDate = (time: number) =>
  new Intl.DateTimeFormat(
    locale(),
    new Date(time).toDateString() === new Date().toDateString()
      ? { hour: "2-digit", minute: "2-digit" }
      : { day: "numeric", month: "short" },
  ).format(time);

/** Search icon button that opens a full-screen chat search (title + message content). */
export function SessionSearch() {
  const navigate = useNavigate();
  const [open, setOpen] = createSignal(false);
  const [query, setQuery] = createSignal("");
  const [active, setActive] = createSignal(0);
  const items: HTMLButtonElement[] = [];

  const hits = createMemo(() => search(sessions(), query()));

  createEffect(() => {
    query();
    setActive(0);
  });
  createEffect(() => items[active()]?.scrollIntoView({ block: "nearest" }));

  const onOpenChange = (next: boolean) => {
    if (next) setQuery("");
    setOpen(next);
  };

  const choose = (hit: Hit | undefined) => {
    if (!hit) return;
    openSession(hit.session.id);
    navigate("/");
    setOpen(false);
  };

  const onKeyDown = (event: KeyboardEvent) => {
    const count = hits().length;
    if (event.key === "ArrowDown" && count) {
      event.preventDefault();
      setActive((active() + 1) % count);
    } else if (event.key === "ArrowUp" && count) {
      event.preventDefault();
      setActive((active() - 1 + count) % count);
    } else if (event.key === "Enter") {
      event.preventDefault();
      choose(hits()[active()]);
    }
  };

  return (
    <Dialog open={open()} onOpenChange={onOpenChange}>
      <Dialog.Trigger
        title={t("search.open")}
        aria-label={t("search.open")}
        class="flex size-7 items-center justify-center rounded-lg text-white/45 transition-colors hover:bg-white/[0.06] hover:text-white"
      >
        <SearchIcon class="size-3.5" />
      </Dialog.Trigger>

      <Dialog.Portal>
        <Dialog.Overlay class="fixed inset-0 z-50 bg-black/60 backdrop-blur-md" />
        <div class="fixed inset-0 z-50 flex justify-center px-6 pt-[14vh] pb-10">
          <Dialog.Content class="flex max-h-full w-full max-w-2xl flex-col text-white outline-none">
            <Dialog.Title class="sr-only">{t("search.open")}</Dialog.Title>

            <TextField value={query()} onChange={setQuery} class="relative shrink-0">
              <SearchIcon class="pointer-events-none absolute top-1/2 left-5 size-5 -translate-y-1/2 text-white/50" />
              <TextField.Input
                type="search"
                aria-label={t("search.label")}
                placeholder={t("search.placeholder")}
                autocomplete="off"
                onKeyDown={onKeyDown}
                class="h-14 w-full rounded-2xl border border-white/10 bg-white/[0.06] pr-5 pl-13 text-base text-white shadow-2xl outline-none backdrop-blur-2xl placeholder:text-white/40 [&::-webkit-search-cancel-button]:hidden"
              />
            </TextField>

            <div class="thin-scroll mt-3 min-h-0 overflow-y-auto rounded-2xl border border-white/10 bg-white/[0.05] p-2 shadow-2xl backdrop-blur-2xl">
              <Show
                when={hits().length > 0}
                fallback={
                  <p class="px-3 py-6 text-center text-sm text-white/40">
                    {t(query().trim() ? "search.nothing" : "sidebar.empty")}
                  </p>
                }
              >
                <For each={hits()}>
                  {(hit, index) => (
                    <Button
                      ref={(el) => (items[index()] = el as HTMLButtonElement)}
                      onClick={() => choose(hit)}
                      onPointerMove={() => setActive(index())}
                      data-active={active() === index() ? "" : undefined}
                      class="block w-full rounded-lg px-2.5 py-2 text-left outline-none data-active:bg-white/[0.08] data-active:text-white"
                    >
                      <span class="flex items-baseline gap-3">
                        <span class="min-w-0 flex-1 truncate text-sm text-white/90">
                          <Highlight text={sessionTitle(hit.session)} needle={query()} />
                        </span>
                        <span class="shrink-0 text-xs text-white/35">{formatDate(hit.session.createdAt)}</span>
                      </span>
                      <Show when={hit.snippet}>
                        {(snippet) => (
                          <span class="mt-1 block truncate text-xs text-white/45">
                            <Highlight text={excerpt(snippet())} needle={query()} />
                          </span>
                        )}
                      </Show>
                    </Button>
                  )}
                </For>
              </Show>
            </div>
          </Dialog.Content>
        </div>
      </Dialog.Portal>
    </Dialog>
  );
}
