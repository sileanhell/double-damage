import { invoke } from "@tauri-apps/api/core";
import Database from "@tauri-apps/plugin-sql";

/**
 * The chat database: `sessions.db` (SQLite, via the Tauri SQL plugin) in the app data folder.
 *
 * Opened once per page load (switching the data folder recreates the window, so the page and
 * with it this connection start over). Outside Tauri (plain web dev server), or when opening
 * fails, `db` resolves to `null` and callers keep their data in memory only.
 *
 * Every access goes through {@link enqueue}, one task at a time, so writes land in the order
 * they were made and reads see every earlier write. (The plugin hands out pooled connections
 * per call, so multi-statement transactions can't be spread over several calls.)
 */

/**
 * Schema steps; `PRAGMA user_version` holds how many have been applied. Only ever append. Each
 * step must be safe to run again: the version is bumped in a separate call, so a crash in between
 * repeats the step on the next start.
 */
type Statement = string | ((db: Database) => Promise<void>);
const MIGRATIONS: Statement[][] = [
  [
    `CREATE TABLE IF NOT EXISTS sessions (
      id TEXT PRIMARY KEY,
      title TEXT NOT NULL,
      created_at INTEGER NOT NULL,
      updated_at INTEGER NOT NULL,
      pinned INTEGER NOT NULL DEFAULT 0,
      model TEXT
    )`,
    `CREATE TABLE IF NOT EXISTS messages (
      id TEXT PRIMARY KEY,
      session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
      seq INTEGER NOT NULL,
      role TEXT NOT NULL,
      content TEXT NOT NULL,
      text TEXT NOT NULL,
      meta TEXT,
      created_at INTEGER NOT NULL
    )`,
    "CREATE INDEX IF NOT EXISTS messages_session_seq ON messages(session_id, seq)",
  ],
  // 2: the folders the agent works in, per chat (JSON array of Windows paths).
  [(db) => addColumn(db, "sessions", "workspace", "TEXT")],
];

async function addColumn(db: Database, table: string, column: string, type: string) {
  const columns = await db.select<{ name: string }[]>(`PRAGMA table_info(${table})`);
  if (columns.some((info) => info.name === column)) return;
  await db.execute(`ALTER TABLE ${table} ADD COLUMN ${column} ${type}`);
}

/**
 * SQLite connection string for an absolute path. The plugin joins the part after `sqlite:` onto
 * its config folder (an absolute path replaces it), and sqlx percent-decodes the file name.
 */
const connectionString = (path: string) => `sqlite:${path.replace(/[%?#]/g, encodeURIComponent)}`;

async function open(): Promise<Database | null> {
  let path: string;
  try {
    path = await invoke<string>("sessions_db_path");
  } catch {
    return null; // not running in Tauri
  }
  try {
    const db = await Database.load(connectionString(path));
    // sqlx already turns foreign keys on for every pooled connection; this covers the first one
    // in case that default ever changes. The journal mode sticks to the file.
    await db.execute("PRAGMA foreign_keys = ON");
    await db.execute("PRAGMA journal_mode = WAL");
    const [{ user_version: version }] = await db.select<{ user_version: number }[]>("PRAGMA user_version");
    for (let step = version; step < MIGRATIONS.length; step++) {
      for (const statement of MIGRATIONS[step]) {
        if (typeof statement === "string") await db.execute(statement);
        else await statement(db);
      }
      await db.execute(`PRAGMA user_version = ${step + 1}`);
    }
    return db;
  } catch (error) {
    console.error("opening the chat database failed; chats are kept in memory only", error);
    return null;
  }
}

/** The open connection (null without one). Reassigned by `closeDb` / `reopenDb`. */
export let db: Promise<Database | null> = open();

let queue: Promise<unknown> = db;

/**
 * Runs `task` once every earlier task has finished. Resolves to its result, or to `undefined`
 * without a database or when it fails (the error is logged; later tasks still run).
 */
export function enqueue<T>(task: (db: Database) => Promise<T>): Promise<T | undefined> {
  const run = queue.then(async () => {
    const connection = await db;
    return connection ? task(connection) : undefined;
  });
  const settled = run.catch((error) => {
    console.error("chat database:", error);
    return undefined;
  });
  queue = settled;
  return settled;
}

/** Resolves once every task queued so far has finished, e.g. before the window closes. */
export const flushDb = (): Promise<void> => queue.then(() => undefined);

/**
 * Finishes pending writes and closes the file, so it can be moved (changing the data folder).
 * Until `reopenDb`, writes are dropped; the window is about to be replaced anyway.
 */
export async function closeDb() {
  await flushDb();
  const connection = await db;
  db = Promise.resolve(null);
  // Throws if the file stays open: moving it would then fail and leave the chats behind.
  await connection?.close();
}

/** Opens the file again, e.g. after moving the data folder failed. */
export function reopenDb() {
  db = open();
  queue = queue.then(() => db);
}
