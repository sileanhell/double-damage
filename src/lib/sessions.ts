import { storage } from "./storage";
import { createSignal } from "solid-js";
import { t } from "./i18n";

export type Message = { role: "user" | "assistant"; text: string; at: number };

/** `draft` marks a chat opened via "New chat" that has no messages yet; `pinned` lifts it into the pinned group. */
export type Session = { id: string; title: string; createdAt: number; messages?: Message[]; draft?: boolean; pinned?: boolean };

/** Title to show: drafts have none of their own and follow the UI language. */
export const sessionTitle = (session: Session) => (session.draft ? t("sidebar.newChat") : session.title);

const KEY = "dd:sessions";

function load(): Session[] {
  try {
    const parsed = JSON.parse(storage.getItem(KEY) ?? "[]");
    // Drafts never survive a restart: they would be empty chats nobody left on purpose.
    return Array.isArray(parsed) ? parsed.filter((session: Session) => !session.draft) : [];
  } catch {
    return [];
  }
}

const [sessions, setSessionsSignal] = createSignal<Session[]>(load());
const [activeSessionId, setActiveSessionId] = createSignal<string | null>(null);

export { activeSessionId, sessions };

function save(next: Session[]) {
  setSessionsSignal(next);
  storage.setItem(KEY, JSON.stringify(next.filter((session) => !session.draft)));
}

const isDraft = (id: string | null) => sessions().some((session) => session.id === id && session.draft);

/** Opens an empty chat that shows up in the list right away; reuses the current one if it is still empty. */
export function startNewChat() {
  if (isDraft(activeSessionId())) return;
  const draft: Session = { id: crypto.randomUUID(), title: "", createdAt: Date.now(), draft: true };
  save([draft, ...sessions()]);
  setActiveSessionId(draft.id);
}

/**
 * Stores a user message in the open chat. A draft gets its title from the first message;
 * with no chat open a new one is created.
 */
export function submitPrompt(prompt: string) {
  const text = prompt.trim();
  const message: Message = { role: "user", text, at: Date.now() };
  const title = text.split("\n")[0].slice(0, 80);
  const id = activeSessionId();

  if (id && sessions().some((session) => session.id === id)) {
    save(
      sessions().map((session) =>
        session.id === id
          ? {
              ...session,
              ...(session.draft ? { title, draft: false } : {}),
              messages: [...(session.messages ?? []), message],
            }
          : session,
      ),
    );
    return;
  }
  const session: Session = { id: crypto.randomUUID(), title, createdAt: Date.now(), messages: [message] };
  save([session, ...sessions()]);
  setActiveSessionId(session.id);
}

/** Removes every empty draft except `keepId`; called whenever the user leaves a chat. */
export function discardDraftsExcept(keepId: string | null) {
  const next = sessions().filter((session) => !session.draft || session.id === keepId);
  if (next.length === sessions().length) return;
  save(next);
  if (!next.some((session) => session.id === activeSessionId())) setActiveSessionId(null);
}

/** Switches to an existing chat; an empty draft being left behind is deleted. */
export function openSession(id: string) {
  setActiveSessionId(id);
  discardDraftsExcept(id);
}

/** Deletes a chat outright; removing the open one clears the active id. */
export function deleteSession(id: string) {
  save(sessions().filter((session) => session.id !== id));
  if (activeSessionId() === id) setActiveSessionId(null);
}

/** Renames a chat; a blank name is ignored. */
export function renameSession(id: string, title: string) {
  const next = title.trim();
  if (!next) return;
  save(sessions().map((session) => (session.id === id ? { ...session, title: next } : session)));
}

/** Moves a chat between the pinned group and the regular list. */
export function togglePinned(id: string) {
  save(sessions().map((session) => (session.id === id ? { ...session, pinned: !session.pinned } : session)));
}
