import type Database from "@tauri-apps/plugin-sql";
import { createSignal } from "solid-js";
import { createStore, reconcile, type SetStoreFunction, unwrap } from "solid-js/store";
import type { Message } from "./chat-types";
import { messageText } from "./chat-types";
import { db, enqueue } from "./db";
import { t } from "./i18n";
import { storage } from "./storage";

/**
 * Chat sessions and their messages, kept in `sessions.db` (see `db.ts`).
 *
 * The in-memory state is authoritative: every change shows up at once and is written to the
 * database in the background, in order. Message lists are loaded per session on first use.
 */

/** `draft` marks a chat opened via "New chat" that has no messages yet (never stored); `pinned` lifts it into the pinned group. */
export type Session = {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  draft?: boolean;
  pinned?: boolean;
  /** Last gateway model id used in this chat. */
  model?: string;
  /** Folders the agent may work in (absolute Windows paths), attached in the prompt box. */
  workspace?: string[];
};

/** Title to show: drafts have none of their own and follow the UI language. */
export const sessionTitle = (session: Session) => (session.draft ? t("sidebar.newChat") : session.title);

const titleFrom = (text: string) => text.trim().split("\n")[0].slice(0, 80);

// --- session list ----------------------------------------------------------------

const [sessions, setSessionsSignal] = createSignal<Session[]>([]);
const [activeSessionId, setActiveSessionId] = createSignal<string | null>(null);

export { activeSessionId, sessions };

/** Drafts first, then the most recently used. */
const byRecent = (a: Session, b: Session) => Number(!!b.draft) - Number(!!a.draft) || b.updatedAt - a.updatedAt;

function setSessions(next: Session[]) {
  setSessionsSignal([...next].sort(byRecent));
}

const findSession = (id: string | null) => sessions().find((session) => session.id === id);

/** Replaces one session; returns the new value (undefined when it doesn't exist). */
function patchSession(id: string, patch: (session: Session) => Session): Session | undefined {
  let updated: Session | undefined;
  setSessions(sessions().map((session) => (session.id === id ? (updated = patch(session)) : session)));
  return updated;
}

// --- database rows -----------------------------------------------------------------

type SessionRow = {
  id: string;
  title: string;
  created_at: number;
  updated_at: number;
  pinned: number;
  model: string | null;
  workspace: string | null;
};

type MessageRow = {
  id: string;
  role: Message["role"];
  content: string;
  meta: string | null;
  created_at: number;
};

const fromSessionRow = (row: SessionRow): Session => ({
  id: row.id,
  title: row.title,
  createdAt: row.created_at,
  updatedAt: row.updated_at,
  ...(row.pinned ? { pinned: true } : {}),
  ...(row.model ? { model: row.model } : {}),
  ...(row.workspace ? { workspace: parseJson<string[]>(row.workspace, []) } : {}),
});

function parseJson<T>(text: string | null, fallback: T): T {
  if (text == null) return fallback;
  try {
    return JSON.parse(text) as T;
  } catch {
    return fallback;
  }
}

const fromMessageRow = (row: MessageRow): Message => ({
  ...parseJson<Partial<Message>>(row.meta, {}),
  id: row.id,
  role: row.role,
  content: parseJson(row.content, []),
  createdAt: row.created_at,
});

/** Optional fields, stored together as JSON (null when none is set). */
function metaOf(message: Message): string | null {
  const { model, usage, stats, stopReason, error, tools, compaction, attachments } = message;
  const meta = { model, usage, stats, stopReason, error, tools, compaction, attachments };
  return Object.values(meta).some((value) => value !== undefined) ? JSON.stringify(meta) : null;
}

const upsertSession = (connection: Database, session: Session) =>
  connection.execute(
    `INSERT INTO sessions (id, title, created_at, updated_at, pinned, model, workspace) VALUES ($1, $2, $3, $4, $5, $6, $7)
     ON CONFLICT(id) DO UPDATE SET title = excluded.title, updated_at = excluded.updated_at,
       pinned = excluded.pinned, model = excluded.model, workspace = excluded.workspace`,
    [
      session.id,
      session.title,
      session.createdAt,
      session.updatedAt,
      session.pinned ? 1 : 0,
      session.model ?? null,
      session.workspace?.length ? JSON.stringify(session.workspace) : null,
    ],
  );

/**
 * Statement inserting or updating a message (new ones go after the session's last message).
 * The values are taken right away: the in-memory message keeps changing while it streams.
 */
function upsertMessage(sessionId: string, message: Message) {
  const values = [
    message.id,
    sessionId,
    message.role,
    JSON.stringify(message.content),
    messageText(message),
    metaOf(message),
    message.createdAt,
  ];
  return (connection: Database) =>
    connection.execute(
      `INSERT INTO messages (id, session_id, seq, role, content, text, meta, created_at)
       VALUES ($1, $2, (SELECT COALESCE(MAX(seq), -1) + 1 FROM messages WHERE session_id = $2), $3, $4, $5, $6, $7)
       ON CONFLICT(id) DO UPDATE SET role = excluded.role, content = excluded.content, text = excluded.text,
         meta = excluded.meta`,
      values,
    );
}

/**
 * Writes the session row and, optionally, one of its messages. Drafts are skipped. `touch`
 * marks the chat as just used (it moves to the top); renaming or pinning doesn't.
 */
function persist(id: string, message?: Message, touch = true) {
  const session = patchSession(id, (session) =>
    session.draft || !touch ? session : { ...session, updatedAt: Date.now() },
  );
  if (!session || session.draft) return;
  const writeMessage = message && upsertMessage(id, message);
  void enqueue(async (connection) => {
    await upsertSession(connection, session);
    if (writeMessage) await writeMessage(connection);
  });
}

// --- messages ------------------------------------------------------------------------

type Thread = { list: Message[]; set: SetStoreFunction<Message[]> };
const threads = new Map<string, Thread>();

/** The session's message store, created (and filled from the database) on first use. */
function thread(sessionId: string): Thread {
  let found = threads.get(sessionId);
  if (found) return found;
  const [list, set] = createStore<Message[]>([]);
  const created: Thread = { list, set };
  threads.set(sessionId, created);
  if (!findSession(sessionId)?.draft) {
    void enqueue((connection) =>
      connection.select<MessageRow[]>(
        "SELECT id, role, content, meta, created_at FROM messages WHERE session_id = $1 ORDER BY seq",
        [sessionId],
      ),
    ).then((rows) => {
      if (!rows?.length || threads.get(sessionId) !== created) return;
      // Messages added while loading are newer than anything stored.
      const stored = rows.map(fromMessageRow);
      const ids = new Set(stored.map((message) => message.id));
      set((current) => [...stored, ...current.filter((message) => !ids.has(message.id))]);
    });
  }
  return created;
}

/** Reactive list of a session's messages; loads them from the DB on first use (returns [] until loaded). */
export function messages(sessionId: string): Message[] {
  return thread(sessionId).list;
}

/** Turns a draft into a real chat titled after `text`; no-op for real chats. */
function materialize(id: string, text: string) {
  patchSession(id, (session) => (session.draft ? { ...session, title: titleFrom(text), draft: false } : session));
}

/**
 * Adds a user message to the open chat. A draft becomes real and takes its title from the first
 * line; with no chat open a new one is created and opened. Persists.
 */
export function submitPrompt(prompt: string, attachments: string[] = []): { sessionId: string; message: Message } {
  const text = prompt.trim();
  const now = Date.now();
  const message: Message = {
    id: crypto.randomUUID(),
    role: "user",
    content: text ? [{ type: "text", text }] : [],
    createdAt: now,
    ...(attachments.length > 0 ? { attachments } : {}),
  };

  // A message of files only is titled after them.
  const title = text || attachments.map((path) => path.split(/[\\/]/).pop() ?? path).join(", ");
  let sessionId = activeSessionId();
  if (sessionId && findSession(sessionId)) {
    materialize(sessionId, title);
  } else {
    sessionId = crypto.randomUUID();
    setSessions([{ id: sessionId, title: titleFrom(title), createdAt: now, updatedAt: now }, ...sessions()]);
    setActiveSessionId(sessionId);
  }
  appendMessage(sessionId, message);
  return { sessionId, message };
}

/** Appends a message (e.g. an empty assistant reply that will stream in). Persists. */
export function appendMessage(sessionId: string, message: Message) {
  if (!findSession(sessionId)) return;
  materialize(sessionId, messageText(message));
  const { set } = thread(sessionId);
  // A copy: the store updates its messages in place, the caller's object stays as it was.
  set((list) => [...list, structuredClone(unwrap(message))]);
  persist(sessionId, message);
}

/**
 * Replaces a message in memory right away (fine-grained, so streaming only touches what changed).
 * `update` gets the current message and must return a new one without mutating it. Writes it to
 * the DB only when `persist` is true, i.e. once the reply is complete.
 */
export function updateMessage(
  sessionId: string,
  id: string,
  update: (message: Message) => Message,
  persistNow = false,
) {
  const found = threads.get(sessionId);
  if (!found) return;
  const index = found.list.findIndex((message) => message.id === id);
  if (index < 0) return;
  found.set(index, reconcile(update(unwrap(found.list[index]))));
  if (persistNow) persist(sessionId, found.list[index]);
}

/** Sets the folders the agent may work in (persisted; a draft keeps them until it becomes real). */
export function setSessionWorkspace(sessionId: string, folders: string[]) {
  patchSession(sessionId, (session) => ({ ...session, workspace: folders }));
  persist(sessionId, undefined, false);
}

/** The session's workspace folders (absolute Windows paths). */
export const sessionWorkspace = (sessionId: string | null) => findSession(sessionId)?.workspace ?? [];

/** Sets the session's last-used model id (persisted). */
export function setSessionModel(sessionId: string, model: string) {
  if (findSession(sessionId)?.model === model) return;
  patchSession(sessionId, (session) => ({ ...session, model }));
  persist(sessionId, undefined, false);
}

// --- chat list actions -------------------------------------------------------------------

const isDraft = (id: string | null) => !!findSession(id)?.draft;

/** Opens an empty chat that shows up in the list right away; reuses the current one if it is still empty. */
export function startNewChat() {
  if (isDraft(activeSessionId())) return;
  const now = Date.now();
  const draft: Session = { id: crypto.randomUUID(), title: "", createdAt: now, updatedAt: now, draft: true };
  setSessions([draft, ...sessions()]);
  setActiveSessionId(draft.id);
}

/** Removes every empty draft except `keepId`; called whenever the user leaves a chat. */
export function discardDraftsExcept(keepId: string | null) {
  const dropped = sessions().filter((session) => session.draft && session.id !== keepId);
  if (dropped.length === 0) return;
  for (const session of dropped) threads.delete(session.id);
  setSessions(sessions().filter((session) => !dropped.includes(session)));
  if (!findSession(activeSessionId())) setActiveSessionId(null);
}

/** Switches to an existing chat (and starts loading its messages); an empty draft being left behind is deleted. */
export function openSession(id: string) {
  setActiveSessionId(id);
  discardDraftsExcept(id);
  thread(id);
}

/** Deletes a chat outright; removing the open one clears the active id. */
export function deleteSession(id: string) {
  const session = findSession(id);
  if (!session) return;
  setSessions(sessions().filter((other) => other.id !== id));
  threads.delete(id);
  if (activeSessionId() === id) setActiveSessionId(null);
  if (session.draft) return;
  void enqueue(async (connection) => {
    // Foreign keys cascade this already; deleting explicitly keeps it right even if they're off.
    await connection.execute("DELETE FROM messages WHERE session_id = $1", [id]);
    await connection.execute("DELETE FROM sessions WHERE id = $1", [id]);
  });
}

/** Renames a chat; a blank name is ignored. */
export function renameSession(id: string, title: string) {
  const next = title.trim();
  if (!next) return;
  patchSession(id, (session) => ({ ...session, title: next }));
  persist(id, undefined, false);
}

/** Moves a chat between the pinned group and the regular list. */
export function togglePinned(id: string) {
  patchSession(id, (session) => ({ ...session, pinned: !session.pinned }));
  persist(id, undefined, false);
}

// --- search ------------------------------------------------------------------------------

export type Snippet = { text: string; at: number };
export type SearchHit = { session: Session; snippet?: Snippet };

const flatten = (text: string) => text.replace(/\s+/g, " ").trim();
/** How much text after the match a snippet keeps (the preview is one truncated line). */
const SNIPPET_TAIL = 200;

function snippetOf(text: string, needle: string): Snippet | undefined {
  const flat = flatten(text);
  if (!needle) return flat ? { text: flat.slice(0, SNIPPET_TAIL), at: 0 } : undefined;
  const at = flat.toLowerCase().indexOf(needle);
  // SQLite's LIKE and JS lowercasing can disagree outside ASCII; keep the hit, unhighlighted.
  return { text: flat.slice(0, Math.max(at, 0) + needle.length + SNIPPET_TAIL), at: Math.max(at, 0) };
}

/** First message (or first one containing `needle`) of each stored session, from the database. */
async function storedTexts(needle: string): Promise<Map<string, string>> {
  const escaped = needle.replace(/[\\%_]/g, (char) => `\\${char}`);
  const rows = await enqueue((connection) =>
    connection.select<{ session_id: string; text: string }[]>(
      needle
        ? "SELECT session_id, text, MIN(seq) FROM messages WHERE text LIKE $1 ESCAPE '\\' GROUP BY session_id"
        : "SELECT session_id, text, MIN(seq) FROM messages WHERE text <> '' GROUP BY session_id",
      needle ? [`%${escaped}%`] : [],
    ),
  );
  return new Map((rows ?? []).map((row) => [row.session_id, row.text]));
}

/**
 * Searches titles and message text (case-insensitive, ASCII-wise like SQL LIKE). With an empty
 * query every chat is returned, newest first, with its first message as snippet.
 */
export async function searchSessions(query: string): Promise<SearchHit[]> {
  const needle = flatten(query).toLowerCase();
  const stored = await storedTexts(needle);
  const hits: SearchHit[] = [];
  for (const session of sessions()) {
    // Loaded chats are searched in memory: they may hold messages not written yet.
    const loaded = threads.get(session.id)?.list;
    const text = loaded?.length
      ? loaded.map(messageText).find((text) => (needle ? flatten(text).toLowerCase().includes(needle) : flatten(text)))
      : stored.get(session.id);
    const snippet = text === undefined ? undefined : snippetOf(text, needle);
    if (!needle || snippet || sessionTitle(session).toLowerCase().includes(needle)) hits.push({ session, snippet });
  }
  return hits;
}

// --- startup -------------------------------------------------------------------------------

const LEGACY_KEY = "dd:sessions";
type LegacySession = {
  id: string;
  title: string;
  createdAt: number;
  messages?: { role: Message["role"]; text: string; at: number }[];
  draft?: boolean;
  pinned?: boolean;
};

/** Sessions saved by the settings-store version, if any are left (drafts were never meant to survive). */
function legacySessions(): { session: Session; messages: Message[] }[] {
  const raw = storage.getItem(LEGACY_KEY);
  if (raw == null) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];
  return (parsed as LegacySession[])
    .filter((old) => old && typeof old.id === "string" && !old.draft)
    .map((old) => {
      // Stable ids: a migration interrupted halfway can run again without duplicates or gaps.
      const list: Message[] = (old.messages ?? []).map((message, index) => ({
        id: `${old.id}:${index}`,
        role: message.role,
        content: [{ type: "text", text: message.text ?? "" }],
        createdAt: message.at ?? old.createdAt,
      }));
      const createdAt = old.createdAt ?? Date.now();
      return {
        session: {
          id: old.id,
          title: old.title ?? "",
          createdAt,
          updatedAt: Math.max(createdAt, ...list.map((message) => message.createdAt)),
          ...(old.pinned ? { pinned: true } : {}),
        },
        messages: list,
      };
    });
}

/** Copies the old sessions into the database once, then drops them from the settings. */
async function migrateLegacy(connection: Database) {
  const legacy = legacySessions();
  for (const { session, messages: list } of legacy) {
    await connection.execute(
      `INSERT OR IGNORE INTO sessions (id, title, created_at, updated_at, pinned, model)
       VALUES ($1, $2, $3, $4, $5, NULL)`,
      [session.id, session.title, session.createdAt, session.updatedAt, session.pinned ? 1 : 0],
    );
    // Upserts by stable id: after an interrupted run the missing messages are added, none twice.
    for (const message of list) await upsertMessage(session.id, message)(connection);
  }
  if (storage.getItem(LEGACY_KEY) != null) storage.removeItem(LEGACY_KEY);
}

/** Adds sessions that aren't in memory yet (whatever was created meanwhile stays as it is). */
function mergeLoaded(loaded: Session[]) {
  const known = new Set(sessions().map((session) => session.id));
  setSessions([...sessions(), ...loaded.filter((session) => !known.has(session.id))]);
}

async function loadSessions() {
  if (!(await db)) {
    // No database (plain web dev server): show the old sessions for this page load only.
    const legacy = legacySessions();
    for (const { session, messages: list } of legacy) {
      const [store, set] = createStore<Message[]>(list);
      threads.set(session.id, { list: store, set });
    }
    mergeLoaded(legacy.map(({ session }) => session));
    return;
  }
  await enqueue(migrateLegacy);
  const rows = await enqueue((connection) =>
    connection.select<SessionRow[]>(
      "SELECT id, title, created_at, updated_at, pinned, model, workspace FROM sessions ORDER BY updated_at DESC",
    ),
  );
  mergeLoaded((rows ?? []).map(fromSessionRow));
}

void loadSessions();
