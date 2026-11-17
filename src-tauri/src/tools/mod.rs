//! File tools exposed to the LLM agent (invoked by the TS agent loop as Tauri commands).
//!
//! The model is untrusted: every path goes through [`paths::Workspace::resolve`], which keeps
//! all access inside the workspace folders (`roots`). Paths returned to the model use MSYS
//! style (`/c/Users/x/proj/src/a.ts`). Errors are written for the model to act on.
//!
//! `before`/`after` texts in results are BOM-less and LF-normalized (for UI diffs); files are
//! written back in their original BOM/CRLF style by the edit methods.

mod apply_patch;
mod edit;
mod hashline;
mod paths;
mod search;
mod sensitive;
mod text;

use std::{fs, io::ErrorKind, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use paths::{Workspace, to_unix};

const READ_DEFAULT_LIMIT: u32 = 2000;
const READ_LINE_CHARS: usize = 2000;
const WRITE_BEFORE_MAX: u64 = 1024 * 1024;
const INSTRUCTIONS_MAX: usize = 64 * 1024;

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(job)
        .await
        .map_err(|err| err.to_string())?
}

// ---------------------------------------------------------------- read

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResult {
    pub path: String,
    pub content: String,
    pub total_lines: u32,
    pub start_line: u32,
    pub end_line: u32,
    pub truncated: bool,
}

fn not_found(path: &Path) -> String {
    format!(
        "file not found: {}. Check the path with list_dir or glob.",
        to_unix(path)
    )
}

fn read(
    ws: &Workspace,
    path: &str,
    offset: Option<u32>,
    limit: Option<u32>,
    hashline: bool,
) -> Result<ReadResult, String> {
    read_at(&ws.resolve(path)?, offset, limit, hashline)
}

fn read_at(abs: &Path, offset: Option<u32>, limit: Option<u32>, hashline: bool) -> Result<ReadResult, String> {
    let abs = abs.to_path_buf();
    let display = to_unix(&abs);
    let meta = match fs::metadata(&abs) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => return Err(not_found(&abs)),
        Err(err) => return Err(format!("cannot access {display}: {err}")),
    };
    if meta.is_dir() {
        return Err(format!(
            "{display} is a directory, not a file; use list_dir to see its contents"
        ));
    }
    if meta.len() > text::MAX_FILE_BYTES {
        return Err(format!(
            "{display} is too large to read ({} bytes); use grep to search it",
            meta.len()
        ));
    }
    let bytes = fs::read(&abs).map_err(|err| format!("cannot read {display}: {err}"))?;
    if text::is_binary(&bytes) {
        return Err(format!(
            "{display} is a binary file and cannot be shown as text"
        ));
    }
    let decoded = text::decode(&bytes);
    let lines = text::split_lines(&decoded.body);
    let total = lines.len();
    let start = offset.unwrap_or(1).max(1) as usize;
    if total > 0 && start > total {
        return Err(format!(
            "offset {start} is past the end of {display}, which has {total} lines"
        ));
    }
    let limit = limit.unwrap_or(READ_DEFAULT_LIMIT).max(1) as usize;
    let end = total.min(start - 1 + limit);
    let mut content = String::new();
    let mut cut_any = false;
    for n in start..=end {
        let line = lines[n - 1];
        let (shown, cut) = text::cut_chars(line, READ_LINE_CHARS);
        if !content.is_empty() {
            content.push('\n');
        }
        if hashline {
            content.push_str(&format!("{n}#{}\t{shown}", hashline::line_hash(line)));
        } else {
            content.push_str(&format!("{n:>6}\t{shown}"));
        }
        if cut {
            content.push_str("… [line truncated]");
            cut_any = true;
        }
    }
    let to_u32 = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
    Ok(ReadResult {
        path: display,
        content,
        total_lines: to_u32(total),
        start_line: to_u32(start),
        end_line: to_u32(end),
        truncated: end < total || cut_any,
    })
}

/// Reads a text file as numbered lines (`     1\tline`, or `1#abc\tline` with `hashline`).
/// `files` are files the user attached to messages: those can be read wherever they are (read
/// only, exactly those files), even with no workspace folder.
#[tauri::command]
pub async fn tool_read(
    roots: Vec<String>,
    path: String,
    offset: Option<u32>,
    limit: Option<u32>,
    hashline: Option<bool>,
    files: Option<Vec<String>>,
) -> Result<ReadResult, String> {
    blocking(move || {
        let files = files.unwrap_or_default();
        let hashline = hashline.unwrap_or(false);
        if let Some(file) = paths::match_attached(&path, &files) {
            return read_at(&file, offset, limit, hashline);
        }
        if roots.is_empty() && !files.is_empty() {
            let list: Vec<String> = files.iter().map(|file| to_unix(Path::new(file))).collect();
            return Err(format!(
                "no workspace folder is open; only the attached files can be read: {}",
                list.join(", ")
            ));
        }
        read(&Workspace::new(&roots)?, &path, offset, limit, hashline)
    })
    .await
}

// ---------------------------------------------------------------- write

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteResult {
    pub path: String,
    pub created: bool,
    pub before: Option<String>,
    pub after: String,
}

fn write(ws: &Workspace, path: &str, content: &str) -> Result<WriteResult, String> {
    let abs = ws.resolve(path)?;
    let display = to_unix(&abs);
    let (created, before) = match fs::metadata(&abs) {
        Ok(meta) if meta.is_dir() => {
            return Err(format!("{display} is a directory; choose a file path"));
        }
        Ok(meta) => {
            let before = (meta.len() <= WRITE_BEFORE_MAX)
                .then(|| fs::read(&abs).ok())
                .flatten()
                .filter(|bytes| !text::is_binary(bytes))
                .map(|bytes| text::decode(&bytes).body);
            (false, before)
        }
        Err(err) if err.kind() == ErrorKind::NotFound => (true, None),
        Err(err) => return Err(format!("cannot access {display}: {err}")),
    };
    text::atomic_write(&abs, content.as_bytes())?;
    let after = text::normalize_newlines(content.strip_prefix('\u{feff}').unwrap_or(content));
    Ok(WriteResult {
        path: display,
        created,
        before,
        after,
    })
}

/// Writes `content` exactly (atomically), creating parent directories.
#[tauri::command]
pub async fn tool_write(
    roots: Vec<String>,
    path: String,
    content: String,
) -> Result<WriteResult, String> {
    blocking(move || write(&Workspace::new(&roots)?, &path, &content)).await
}

// ---------------------------------------------------------------- edit

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditResult {
    pub path: String,
    pub before: Option<String>,
    pub after: String,
    pub replacements: u32,
}

#[derive(Deserialize)]
struct PatchInput {
    #[serde(alias = "input")]
    patch: String,
}

fn edit(ws: &Workspace, path: Option<&str>, method: &str, input: &Value) -> Result<Value, String> {
    if method == "apply_patch" {
        let input: PatchInput = serde_json::from_value(input.clone()).map_err(|err| {
            format!(
                "invalid apply_patch input: {err}. Expected {{\"patch\": \"*** Begin Patch…\"}}"
            )
        })?;
        let result = apply_patch::apply(ws, &input.patch)?;
        return serde_json::to_value(result).map_err(|err| err.to_string());
    }
    let path = path
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| format!("{method} needs a file path"))?;
    let abs = ws.resolve(path)?;
    let display = to_unix(&abs);
    let existing = text::read_for_edit(&abs)?;
    let before = existing.as_ref().map(|t| t.body.as_str());
    let applied = match method {
        "str_replace" => edit::str_replace(before, input)?,
        "search_replace" => edit::search_replace(before, input)?,
        "hashline" => {
            let Some(body) = before else {
                return Err(format!(
                    "{}; hashline edits need an existing file (use write to create one)",
                    not_found(&abs)
                ));
            };
            let (after, replacements) = hashline::apply(body, input)?;
            edit::Applied {
                after,
                replacements,
            }
        }
        other => {
            return Err(format!(
                "unknown edit method \"{other}\"; use str_replace, apply_patch, search_replace or hashline"
            ));
        }
    };
    if before == Some(applied.after.as_str()) {
        return Err(format!("the edit does not change {display}"));
    }
    let style = existing.clone().unwrap_or_default();
    text::atomic_write(&abs, &style.encode(&applied.after))?;
    let result = EditResult {
        path: display,
        before: existing.map(|t| t.body),
        after: applied.after,
        replacements: applied.replacements,
    };
    serde_json::to_value(result).map_err(|err| err.to_string())
}

/// Edits a file with `method` = `str_replace` | `search_replace` | `hashline` | `apply_patch`.
/// Returns [`EditResult`], or for `apply_patch` `{ files: [...] }` (see `apply_patch.rs`).
#[tauri::command]
pub async fn tool_edit(
    roots: Vec<String>,
    path: Option<String>,
    method: String,
    input: Value,
) -> Result<Value, String> {
    blocking(move || edit(&Workspace::new(&roots)?, path.as_deref(), &method, &input)).await
}

// ---------------------------------------------------------------- list / glob / grep

/// Directory tree as text (dirs first, file sizes), gitignore-aware.
#[tauri::command]
pub async fn tool_list(
    roots: Vec<String>,
    path: Option<String>,
    depth: Option<u32>,
) -> Result<String, String> {
    blocking(move || search::list(&Workspace::new(&roots)?, path.as_deref(), depth)).await
}

/// Files matching a glob, newest first (max 500).
#[tauri::command]
pub async fn tool_glob(
    roots: Vec<String>,
    pattern: String,
    path: Option<String>,
) -> Result<search::GlobResult, String> {
    blocking(move || search::glob(&Workspace::new(&roots)?, &pattern, path.as_deref())).await
}

/// Regex search; `mode` = `files` (default) | `content` | `count`.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn tool_grep(
    roots: Vec<String>,
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    ignore_case: Option<bool>,
    mode: Option<String>,
    context: Option<u32>,
    max_results: Option<u32>,
) -> Result<String, String> {
    blocking(move || {
        let ws = Workspace::new(&roots)?;
        let opts = search::GrepOptions {
            pattern: &pattern,
            path: path.as_deref(),
            glob: glob.as_deref(),
            ignore_case: ignore_case.unwrap_or(false),
            mode: search::Mode::parse(mode.as_deref())?,
            context: context.unwrap_or(0),
            max_results: max_results.unwrap_or(200),
        };
        search::grep(&ws, &opts)
    })
    .await
}

// ---------------------------------------------------------------- instructions

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Instructions {
    pub root: String,
    pub file: String,
    pub content: String,
}

fn instructions(ws: &Workspace) -> Vec<Instructions> {
    let mut out = Vec::new();
    for root in ws.roots() {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        let mut found: Vec<(usize, std::path::PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                let rank = ["agents.md", "claude.md"].iter().position(|n| *n == name)?;
                Some((rank, entry.path()))
            })
            .collect();
        found.sort();
        for (_, path) in found {
            // Symlinked instruction files must not leak content from outside the workspace.
            let Ok(resolved) = ws.resolve(&path.to_string_lossy()) else {
                continue;
            };
            if !resolved.is_file() {
                continue;
            }
            let Ok(bytes) = fs::read(&resolved) else {
                continue;
            };
            let mut content = text::decode(&bytes).body;
            if content.len() > INSTRUCTIONS_MAX {
                let mut cut = INSTRUCTIONS_MAX;
                while !content.is_char_boundary(cut) {
                    cut -= 1;
                }
                content.truncate(cut);
                content.push_str("\n… [truncated at 64 KB]");
            }
            out.push(Instructions {
                root: to_unix(root),
                file: to_unix(&path),
                content,
            });
        }
    }
    out
}

/// `AGENTS.md` / `CLAUDE.md` at each root (case-insensitive), for the system prompt.
#[tauri::command]
pub async fn workspace_instructions(roots: Vec<String>) -> Result<Vec<Instructions>, String> {
    blocking(move || Ok(instructions(&Workspace::new(&roots)?))).await
}

// ---------------------------------------------------------------- permission helpers

/// Of `paths` (resolved like the file tools do; paths that do not resolve are ignored), the
/// unix spellings of those pointing into locations where a write can lead to code execution
/// (see [`sensitive::SENSITIVE`]). The UI asks the user before such writes even when edits are
/// otherwise applied without asking.
#[tauri::command]
pub async fn tool_sensitive(roots: Vec<String>, paths: Vec<String>) -> Result<Vec<String>, String> {
    blocking(move || Ok(sensitive::filter(&Workspace::new(&roots)?, &paths))).await
}

/// The paths an `apply_patch` patch names (`Add File`, `Update File`, `Delete File`,
/// `Move to`), raw as written, without filesystem access. Feed them to [`tool_sensitive`].
#[tauri::command]
pub fn tool_patch_paths(patch: String) -> Vec<String> {
    apply_patch::touched_paths(&patch)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let root = paths::strip_verbatim(&fs::canonicalize(dir.path()).unwrap()).join("p");
        fs::create_dir_all(&root).unwrap();
        let ws = Workspace::new(&[root.to_string_lossy().into_owned()]).unwrap();
        (dir, root, ws)
    }

    #[test]
    fn read_formats_lines() {
        let (_d, root, ws) = fixture();
        let long = "x".repeat(2100);
        fs::write(
            root.join("a.txt"),
            format!("\u{feff}one\r\ntwo\r\n{long}\r\nfour\r\n"),
        )
        .unwrap();
        let r = read(&ws, "a.txt", None, None, false).unwrap();
        assert!(
            r.content
                .starts_with("     1\tone\n     2\ttwo\n     3\txxx"),
            "{}",
            r.content
        );
        assert!(r.content.contains("… [line truncated]\n     4\tfour"));
        assert_eq!(
            (r.total_lines, r.start_line, r.end_line, r.truncated),
            (4, 1, 4, true)
        );
        let r = read(&ws, "a.txt", Some(2), Some(1), false).unwrap();
        assert_eq!(
            (r.content.as_str(), r.end_line, r.truncated),
            ("     2\ttwo", 2, true)
        );
        let r = read(&ws, "a.txt", Some(4), None, true).unwrap();
        assert_eq!(
            r.content,
            format!("4#{}\tfour", hashline::line_hash("four"))
        );
        assert!(
            read(&ws, "a.txt", Some(9), None, false)
                .unwrap_err()
                .contains("past the end")
        );
        assert!(
            read(&ws, ".", None, None, false)
                .unwrap_err()
                .contains("list_dir")
        );
        assert!(
            read(&ws, "nope.txt", None, None, false)
                .unwrap_err()
                .contains("not found")
        );
        fs::write(root.join("b.bin"), b"ab\0cd").unwrap();
        assert!(
            read(&ws, "b.bin", None, None, false)
                .unwrap_err()
                .contains("binary")
        );
        fs::write(root.join("empty.txt"), "").unwrap();
        let r = read(&ws, "empty.txt", None, None, false).unwrap();
        assert_eq!((r.content.as_str(), r.total_lines), ("", 0));
        assert!(
            read(&ws, "../x", None, None, false)
                .unwrap_err()
                .contains("outside the workspace")
        );
    }

    #[test]
    fn write_creates_and_reports_before() {
        let (_d, root, ws) = fixture();
        let w = write(&ws, "new/dir/a.txt", "hello\r\n").unwrap();
        assert!(w.created && w.before.is_none());
        assert_eq!(w.after, "hello\n");
        assert_eq!(fs::read(root.join("new/dir/a.txt")).unwrap(), b"hello\r\n");
        let w = write(&ws, "new/dir/a.txt", "bye").unwrap();
        assert_eq!((w.created, w.before.as_deref()), (false, Some("hello\n")));
        assert!(write(&ws, "new", "x").unwrap_err().contains("directory"));
        assert!(
            write(&ws, "../evil.txt", "x")
                .unwrap_err()
                .contains("outside")
        );
    }

    #[test]
    fn edit_preserves_crlf_and_bom() {
        let (_d, root, ws) = fixture();
        fs::write(root.join("a.txt"), "\u{feff}alpha\r\nbeta\r\n").unwrap();
        let out = edit(
            &ws,
            Some("a.txt"),
            "str_replace",
            &json!({ "oldString": "beta", "newString": "gamma\ndelta" }),
        )
        .unwrap();
        assert_eq!(out["after"], "alpha\ngamma\ndelta\n");
        assert_eq!(out["before"], "alpha\nbeta\n");
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "\u{feff}alpha\r\ngamma\r\ndelta\r\n"
        );

        let blocks = "<<<<<<< SEARCH\nalpha\n=======\nALPHA\n>>>>>>> REPLACE";
        edit(
            &ws,
            Some("a.txt"),
            "search_replace",
            &json!({ "blocks": blocks }),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "\u{feff}ALPHA\r\ngamma\r\ndelta\r\n"
        );

        let anchor = format!("2#{}", hashline::line_hash("gamma"));
        edit(
            &ws,
            Some("a.txt"),
            "hashline",
            &json!({ "edits": [{ "op": "replace", "pos": anchor, "lines": ["G"] }] }),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "\u{feff}ALPHA\r\nG\r\ndelta\r\n"
        );
        // The same anchor is now stale.
        let err = edit(
            &ws,
            Some("a.txt"),
            "hashline",
            &json!({ "edits": [{ "op": "delete", "pos": anchor }] }),
        )
        .unwrap_err();
        assert!(err.contains("re-read"), "{err}");
    }

    #[test]
    fn edit_create_and_errors() {
        let (_d, root, ws) = fixture();
        edit(
            &ws,
            Some("n/x.txt"),
            "str_replace",
            &json!({ "oldString": "", "newString": "new\n" }),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(root.join("n/x.txt")).unwrap(), "new\n");
        let blocks = "<<<<<<< SEARCH\n=======\nmade\n>>>>>>> REPLACE";
        let out = edit(
            &ws,
            Some("y.txt"),
            "search_replace",
            &json!({ "blocks": blocks }),
        )
        .unwrap();
        assert!(out["before"].is_null());
        assert!(
            edit(&ws, Some("z.txt"), "hashline", &json!({ "edits": [] }))
                .unwrap_err()
                .contains("not found")
        );
        assert!(
            edit(&ws, Some("y.txt"), "frob", &json!({}))
                .unwrap_err()
                .contains("unknown edit method")
        );
        assert!(
            edit(&ws, None, "str_replace", &json!({}))
                .unwrap_err()
                .contains("needs a file path")
        );
        assert!(
            edit(
                &ws,
                Some("../q.txt"),
                "str_replace",
                &json!({ "oldString": "", "newString": "x" })
            )
            .unwrap_err()
            .contains("outside")
        );
        fs::write(root.join("latin1.txt"), b"caf\xe9\n").unwrap();
        let err = edit(
            &ws,
            Some("latin1.txt"),
            "str_replace",
            &json!({ "oldString": "caf", "newString": "x" }),
        )
        .unwrap_err();
        assert!(err.contains("not valid UTF-8"), "{err}");
        let out = edit(
            &ws,
            None,
            "apply_patch",
            &json!({ "patch": "*** Begin Patch\n*** Add File: p.txt\n+p\n*** End Patch" }),
        )
        .unwrap();
        assert_eq!(out["files"][0]["created"], true);
        assert_eq!(fs::read_to_string(root.join("p.txt")).unwrap(), "p\n");
    }

    #[test]
    fn edit_preserves_mixed_line_endings() {
        let (_d, root, ws) = fixture();
        fs::write(root.join("m.txt"), "a\r\nb\nc\r\nd\r\n").unwrap();
        edit(
            &ws,
            Some("m.txt"),
            "str_replace",
            &json!({ "oldString": "d", "newString": "D\nE" }),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("m.txt")).unwrap(),
            "a\r\nb\nc\r\nD\r\nE\r\n"
        );
    }

    #[test]
    fn no_tool_touches_unc_paths() {
        let (_d, root, ws) = fixture();
        fs::write(root.join("a.txt"), "a\n").unwrap();
        let unc = "//nonexistent.invalid/share/x";
        let started = std::time::Instant::now();
        let errors = [
            read(&ws, unc, None, None, false).unwrap_err(),
            write(&ws, unc, "x").unwrap_err(),
            edit(
                &ws,
                Some(unc),
                "str_replace",
                &json!({ "oldString": "", "newString": "x" }),
            )
            .unwrap_err(),
            search::list(&ws, Some(unc), None).unwrap_err(),
            search::glob(&ws, "*", Some(unc)).unwrap_err(),
            search::glob(&ws, "//nonexistent.invalid/share/*", None).unwrap_err(),
            search::glob(&ws, "//evil@80/s/**/*.txt", None).unwrap_err(),
            search::grep(
                &ws,
                &search::GrepOptions {
                    pattern: "a",
                    path: Some(unc),
                    glob: None,
                    ignore_case: false,
                    mode: search::Mode::Files,
                    context: 0,
                    max_results: 10,
                },
            )
            .unwrap_err(),
        ];
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        for err in errors {
            assert!(err.contains("outside the workspace"), "{err}");
        }
        assert!(sensitive::filter(&ws, &[unc.to_owned()]).is_empty());
    }

    #[test]
    fn sensitive_paths() {
        let (_d, root, ws) = fixture();
        fs::create_dir_all(root.join(".git/hooks")).unwrap();
        let inputs: Vec<String> = [
            ".git/hooks/pre-commit",
            ".GIT/config",
            "sub/.Git",
            "src/a.ts",
            ".vscode/settings.json",
            ".github/workflows/ci.yml",
            ".github/ISSUE_TEMPLATE.md",
            ".envrc",
            "sub/.gitattributes",
            "x/../.gitmodules",
            "notes/.envrc.md",
            ".idea/workspace.xml",
            ".devcontainer/devcontainer.json",
            ".husky/pre-push",
            ".githooks/x",
            "../outside/.git/config",
            "//nonexistent.invalid/s/.git/x",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let flagged = sensitive::filter(&ws, &inputs);
        let base = to_unix(&root);
        let expect = |rel: &str| format!("{base}/{rel}");
        assert_eq!(
            flagged,
            vec![
                expect(".git/hooks/pre-commit"),
                expect(".git/config"),
                expect("sub/.Git"),
                expect(".vscode/settings.json"),
                expect(".github/workflows/ci.yml"),
                expect(".envrc"),
                expect("sub/.gitattributes"),
                expect(".gitmodules"),
                expect(".idea/workspace.xml"),
                expect(".devcontainer/devcontainer.json"),
                expect(".husky/pre-push"),
                expect(".githooks/x"),
            ]
        );
    }

    #[test]
    fn sensitive_through_junction() {
        let (_d, root, ws) = fixture();
        fs::create_dir_all(root.join(".git/hooks")).unwrap();
        let link = root.join("innocent");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(root.join(".git"))
            .output();
        if !made.is_ok_and(|o| o.status.success()) {
            eprintln!("skipping: mklink /J failed");
            return;
        }
        let flagged = sensitive::filter(&ws, &["innocent/hooks/pre-commit".to_owned()]);
        assert_eq!(
            flagged,
            vec![format!("{}/.git/hooks/pre-commit", to_unix(&root))]
        );
    }

    #[test]
    fn instructions_found_case_insensitively() {
        let (_d, root, ws) = fixture();
        assert!(instructions(&ws).is_empty());
        fs::write(root.join("Agents.md"), "\u{feff}a\r\nb").unwrap();
        fs::write(root.join("CLAUDE.MD"), "x".repeat(70_000)).unwrap();
        let found = instructions(&ws);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].content, "a\nb");
        assert!(found[0].file.ends_with("/Agents.md"));
        assert!(found[1].content.ends_with("[truncated at 64 KB]"));
        assert!(found[1].content.len() < 66_000);
    }
}
