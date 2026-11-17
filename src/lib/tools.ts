import { invoke } from "@tauri-apps/api/core";
import { structuredPatch } from "diff";
import type { EditMethod } from "./agent-settings";
import { bashAvailable, DEFAULT_TIMEOUT_MS, MAX_TIMEOUT_MS, runBash } from "./bash";
import type { ToolMeta } from "./chat-types";

/**
 * The tools the agent can call. Definitions (name, description, JSON schema) go to the model;
 * `runTool` executes a call. File tools run in Rust (`src-tauri/src/tools`), which enforces the
 * workspace boundary; bash runs in its own shell per command (`bash.ts`).
 */

export type ToolName = "read_file" | "write" | "edit" | "list_dir" | "glob" | "grep" | "bash";

/** What a call may do, for the approval modes. */
export type ToolKind = "read" | "edit" | "exec";

export const TOOL_KIND: Record<ToolName, ToolKind> = {
  read_file: "read",
  list_dir: "read",
  glob: "read",
  grep: "read",
  write: "edit",
  edit: "edit",
  bash: "exec",
};

export type ToolDef = { name: ToolName; description: string; input_schema: Record<string, unknown> };

const object = (properties: Record<string, unknown>, required: string[]) => ({
  type: "object",
  properties,
  required,
  additionalProperties: false,
});
const str = (description: string) => ({ type: "string", description });
const int = (description: string) => ({ type: "integer", description });
const bool = (description: string) => ({ type: "boolean", description });

const PATH = str("Absolute Unix-style path inside the workspace, e.g. /c/Users/me/project/src/main.ts");

const EDIT_DEFS: Record<EditMethod, Pick<ToolDef, "description" | "input_schema">> = {
  str_replace: {
    description:
      "Replace text in a file. old_string must match the file exactly (including indentation) and occur exactly once, unless replace_all is true; include enough surrounding lines to make it unique. Read the file first. To create a file use write.",
    input_schema: object(
      {
        path: PATH,
        old_string: str("Exact text to replace"),
        new_string: str("Text to put in its place"),
        replace_all: bool("Replace every occurrence (default false)"),
      },
      ["path", "old_string", "new_string"],
    ),
  },
  search_replace: {
    description:
      "Edit a file with one or more SEARCH/REPLACE blocks, applied in order. Each SEARCH part must match the file exactly once.\n<<<<<<< SEARCH\nexact existing lines\n=======\nnew lines\n>>>>>>> REPLACE",
    input_schema: object(
      { path: PATH, blocks: str("One or more SEARCH/REPLACE blocks") },
      ["path", "blocks"],
    ),
  },
  apply_patch: {
    description:
      "Apply a patch in the apply_patch format; it can add, update, delete and move several files at once.\n*** Begin Patch\n*** Update File: /c/proj/a.ts\n@@ context line\n-old line\n+new line\n*** Add File: /c/proj/b.ts\n+content\n*** End Patch",
    input_schema: object({ patch: str("The whole patch, from *** Begin Patch to *** End Patch") }, ["patch"]),
  },
  hashline: {
    description:
      "Edit a file by line references from read_file, which shows each line as N#HASH. Address lines as \"N#HASH\"; an edit is rejected if the line changed since it was read (then read the file again).",
    input_schema: object(
      {
        path: PATH,
        edits: {
          type: "array",
          description: "Edits, applied together; every anchor refers to the file as last read",
          items: object(
            {
              op: {
                type: "string",
                enum: ["replace", "delete", "append", "prepend"],
                description: "replace/delete the lines pos..end; append after pos / prepend before pos (without pos: end / start of file)",
              },
              pos: str("Line anchor as N#HASH, e.g. 12#k3m"),
              end: str("Last line of the range as N#HASH (default: pos)"),
              lines: { type: "array", items: { type: "string" }, description: "New lines, without anchors" },
            },
            ["op"],
          ),
        },
      },
      ["path", "edits"],
    ),
  },
};

/** The tools offered this turn: none without a workspace; bash only once installed. */
export function toolDefs(editMethod: EditMethod): ToolDef[] {
  const defs: ToolDef[] = [
    {
      name: "read_file",
      description: `Read a text file. Returns lines as "${editMethod === "hashline" ? "N#HASH" : "N"}<tab>text", at most 2000 lines per call; use offset/limit for longer files.`,
      input_schema: object(
        { path: PATH, offset: int("1-based line to start at"), limit: int("Number of lines (default 2000)") },
        ["path"],
      ),
    },
    {
      name: "write",
      description: "Create a file or overwrite it completely with content. Prefer edit for changes to existing files.",
      input_schema: object({ path: PATH, content: str("The full file content") }, ["path", "content"]),
    },
    { name: "edit", ...EDIT_DEFS[editMethod] },
    {
      name: "list_dir",
      description: "List a directory (folders end with /), respecting .gitignore.",
      input_schema: object(
        { path: { ...PATH, description: "Directory; default: the first workspace folder" }, depth: int("Levels to show, 1–5 (default 1)") },
        [],
      ),
    },
    {
      name: "glob",
      description: "Find files by glob pattern (e.g. **/*.ts), newest first, respecting .gitignore.",
      input_schema: object({ pattern: str("Glob pattern"), path: { ...PATH, description: "Folder to search in; default: all workspace folders" } }, ["pattern"]),
    },
    {
      name: "grep",
      description:
        "Search file contents with a regular expression (Rust regex syntax), respecting .gitignore. mode: files (default, matching paths), content (matching lines as path:line:text), count.",
      input_schema: object(
        {
          pattern: str("Regular expression"),
          path: { ...PATH, description: "File or folder to search; default: all workspace folders" },
          glob: str("Only files matching this glob, e.g. *.rs"),
          ignore_case: bool("Case-insensitive"),
          mode: { type: "string", enum: ["files", "content", "count"] },
          context: int("Lines of context around matches in content mode (max 10)"),
          max_results: int("Maximum results (default 200)"),
        },
        ["pattern"],
      ),
    },
  ];
  if (bashAvailable()) {
    defs.push({
      name: "bash",
      description: `Run a bash command (Git Bash on Windows) in the first workspace folder. Each call is a fresh non-interactive shell: cd and variables don't persist, background jobs end with the command. Default timeout ${DEFAULT_TIMEOUT_MS / 60_000} min, at most ${MAX_TIMEOUT_MS / 60_000}. Long output is cut to its start and end. Use the dedicated tools to read, search and edit files.`,
      input_schema: object(
        {
          command: str("The command to run"),
          description: str("A few words on what it does, shown to the user"),
          timeout_ms: int(`Timeout in ms (default ${DEFAULT_TIMEOUT_MS}, max ${MAX_TIMEOUT_MS})`),
        },
        ["command"],
      ),
    });
  }
  return defs;
}

// --- running -------------------------------------------------------------------------------

/**
 * Paths an edit call would write where git or an editor runs files as code (`.git/`, `.vscode/`,
 * hooks…): such edits ask like bash does. When that can't be worked out, every path counts.
 */
export async function sensitivePaths(call: { name: string; input: unknown }, roots: string[], method: EditMethod): Promise<string[]> {
  const input = (call.input && typeof call.input === "object" ? call.input : {}) as Record<string, unknown>;
  let paths: string[] = [];
  try {
    if (call.name === "edit" && method === "apply_patch") {
      paths = await invoke<string[]>("tool_patch_paths", { patch: typeof input.patch === "string" ? input.patch : "" });
    } else if (typeof input.path === "string") {
      paths = [input.path];
    }
    if (paths.length === 0) return [];
    return await invoke<string[]>("tool_sensitive", { roots, paths });
  } catch (error) {
    console.error(error);
    return paths.length > 0 ? paths : ["?"];
  }
}

export type ToolCall = { id: string; name: string; input: unknown };

export type ToolOutcome = {
  /** What the model gets back. */
  content: string;
  isError: boolean;
  meta: ToolMeta;
};

type Input = Record<string, unknown>;
const text = (value: unknown) => (typeof value === "string" ? value : undefined);
const num = (value: unknown) => (typeof value === "number" && Number.isFinite(value) ? Math.floor(value) : undefined);

const DIFF_TIMEOUT_MS = 50;
/** Diffs are kept with the chat and drawn line by line: past this they're cut. */
const DIFF_MAX_LINES = 1500;
const DIFF_MAX_CHARS = 150_000;

/** Unified diff for the UI; a whole-file summary when it'd take too long to compute. */
export function diffOf(path: string, before: string | null | undefined, after: string): string {
  const patch = structuredPatch(path, path, before ?? "", after, "", "", { context: 3, timeout: DIFF_TIMEOUT_MS });
  const lines = (s: string) => (s ? s.split("\n").length : 0);
  if (!patch) return `@@ rewritten: ${lines(before ?? "")} → ${lines(after)} lines @@\n`;
  const out = [`--- ${path}`, `+++ ${path}`];
  for (const hunk of patch.hunks) {
    out.push(`@@ -${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines} @@`, ...hunk.lines);
  }
  let chars = 0;
  const kept = out.findIndex((line, index) => (chars += line.length + 1) > DIFF_MAX_CHARS || index >= DIFF_MAX_LINES);
  if (kept < 0) return out.join("\n");
  return [...out.slice(0, kept), `@@ … ${out.length - kept} more diff lines not shown @@`].join("\n");
}

type FileChange = { path: string; before?: string | null; after: string | null; created?: boolean; deleted?: boolean; movedFrom?: string };

function changeOutcome(changes: FileChange[], summary: string): ToolOutcome {
  return {
    content: summary,
    isError: false,
    meta: {
      files: changes.map((change) => change.path),
      diff: changes.map((change) => diffOf(change.path, change.before, change.deleted ? "" : (change.after ?? ""))).join("\n"),
    },
  };
}

/**
 * Executes a call in `roots` (the workspace, Windows paths). Errors are returned as outcomes
 * for the model, not thrown; only aborting (`signal`) throws.
 */
export async function runTool(
  call: ToolCall,
  roots: string[],
  editMethod: EditMethod,
  /** Files attached to messages: read_file may open these wherever they are. */
  files: string[],
  onOutput?: (text: string) => void,
  signal?: AbortSignal,
): Promise<ToolOutcome> {
  const input = (call.input && typeof call.input === "object" ? call.input : {}) as Input;
  try {
    return await execute(call.name, input, roots, editMethod, files, onOutput, signal);
  } catch (error) {
    if (signal?.aborted) throw error;
    return { content: `Error: ${error instanceof Error ? error.message : String(error)}`, isError: true, meta: {} };
  }
}

async function execute(
  name: string,
  input: Input,
  roots: string[],
  editMethod: EditMethod,
  files: string[],
  onOutput?: (text: string) => void,
  signal?: AbortSignal,
): Promise<ToolOutcome> {
  const ok = (content: string, meta: ToolMeta = {}): ToolOutcome => ({ content, isError: false, meta });
  switch (name) {
    case "read_file": {
      const result = await invoke<{ path: string; content: string; totalLines: number; startLine: number; endLine: number; truncated: boolean }>(
        "tool_read",
        { roots, path: text(input.path) ?? "", offset: num(input.offset), limit: num(input.limit), hashline: editMethod === "hashline", files },
      );
      let content = result.content || "(empty file)";
      if (result.truncated || result.endLine < result.totalLines) {
        content += `\n\n[Lines ${result.startLine}–${result.endLine} of ${result.totalLines}. Use offset to read more.]`;
      }
      return ok(content);
    }
    case "write": {
      const result = await invoke<{ path: string; created: boolean; before: string | null; after: string }>("tool_write", {
        roots,
        path: text(input.path) ?? "",
        content: text(input.content) ?? "",
      });
      return changeOutcome([result], `${result.created ? "Created" : "Wrote"} ${result.path}`);
    }
    case "edit":
      return edit(input, roots, editMethod);
    case "list_dir":
      return ok(await invoke<string>("tool_list", { roots, path: text(input.path), depth: num(input.depth) }));
    case "glob": {
      const result = await invoke<{ paths: string[]; truncated: boolean }>("tool_glob", {
        roots,
        pattern: text(input.pattern) ?? "",
        path: text(input.path),
      });
      if (result.paths.length === 0) return ok("No files found.");
      return ok(result.paths.join("\n") + (result.truncated ? "\n[More files match; narrow the pattern.]" : ""));
    }
    case "grep": {
      const mode = text(input.mode);
      return ok(
        await invoke<string>("tool_grep", {
          roots,
          pattern: text(input.pattern) ?? "",
          path: text(input.path),
          glob: text(input.glob),
          ignoreCase: input.ignore_case === true,
          mode: mode === "content" || mode === "count" ? mode : "files",
          context: num(input.context),
          maxResults: num(input.max_results),
        }),
      );
    }
    case "bash": {
      if (!bashAvailable()) return { content: "Error: bash is not installed.", isError: true, meta: {} };
      const command = text(input.command) ?? "";
      const result = await runBash({ command, cwd: roots[0], timeoutMs: num(input.timeout_ms) }, onOutput, signal);
      const parts = [result.output.trimEnd() || "(no output)"];
      if (result.timedOut) parts.push(`[Timed out after ${result.durationMs} ms; the command and its processes were killed.]`);
      else if (result.cancelled) parts.push("[Stopped by the user.]");
      else if (result.exitCode !== 0) parts.push(`[Exit code ${result.exitCode}]`);
      return {
        content: parts.join("\n\n"),
        isError: result.timedOut || result.exitCode !== 0,
        meta: { exitCode: result.exitCode, timedOut: result.timedOut, durationMs: result.durationMs, cancelled: result.cancelled },
      };
    }
    default:
      return { content: `Error: unknown tool "${name}".`, isError: true, meta: {} };
  }
}

async function edit(input: Input, roots: string[], method: EditMethod): Promise<ToolOutcome> {
  const path = text(input.path) ?? "";
  if (method === "apply_patch") {
    const result = await invoke<{ files: FileChange[] }>("tool_edit", {
      roots,
      path,
      method,
      input: { patch: text(input.patch) ?? "" },
    });
    const verb = (change: FileChange) =>
      change.deleted ? "Deleted" : change.created ? "Created" : change.movedFrom ? `Moved ${change.movedFrom} →` : "Updated";
    return changeOutcome(result.files, result.files.map((change) => `${verb(change)} ${change.path}`).join("\n"));
  }
  const args =
    method === "str_replace"
      ? { oldString: text(input.old_string) ?? "", newString: text(input.new_string) ?? "", replaceAll: input.replace_all === true }
      : method === "search_replace"
        ? { blocks: text(input.blocks) ?? "" }
        : { edits: Array.isArray(input.edits) ? input.edits : [] };
  const result = await invoke<{ path: string; before: string | null; after: string; replacements: number }>("tool_edit", {
    roots,
    path,
    method,
    input: args,
  });
  const count = result.replacements > 1 ? ` (${result.replacements} replacements)` : "";
  return changeOutcome([result], `Edited ${result.path}${count}`);
}
