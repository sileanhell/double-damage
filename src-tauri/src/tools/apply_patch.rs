//! OpenAI Codex `apply_patch` format, applied all-or-nothing inside the workspace.
//!
//! ```text
//! *** Begin Patch
//! *** Add File: path/new.txt
//! +line 1
//! +line 2
//! *** Update File: src/app.py
//! *** Move to: src/main.py          (optional rename)
//! @@ def greet():                   (optional context line(s) to seek to; several @@ lines nest)
//!  context line
//! -removed line
//! +added line
//! *** End of File                   (optional: the hunk must match at the end of the file)
//! *** Delete File: obsolete.txt
//! *** End Patch
//! ```
//!
//! Matching follows Codex: hunks are located in order, each at or after the previous one,
//! comparing whole lines exactly, then ignoring trailing whitespace, then ignoring surrounding
//! whitespace, then normalizing Unicode punctuation. Every section is validated against a
//! virtual view of the files (later sections see earlier ones) before anything is written;
//! if a write fails midway, already written files are restored.
//!
//! Result shape: `{ files: [{ path, movedFrom?, before?, after?, created, deleted }] }` with
//! LF-normalized, BOM-less `before`/`after` (absent for created/deleted files respectively).

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;

use super::{
    paths::{Workspace, to_unix},
    text::{self, Text},
};

#[derive(Debug, PartialEq)]
enum Section {
    Add {
        path: String,
        contents: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        chunks: Vec<Chunk>,
    },
}

#[derive(Debug, Default, PartialEq)]
struct Chunk {
    contexts: Vec<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    eof: bool,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub created: bool,
    pub deleted: bool,
}

#[derive(Debug, Serialize)]
pub struct PatchResult {
    pub files: Vec<FileChange>,
}

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const ADD: &str = "*** Add File:";
const DELETE: &str = "*** Delete File:";
const UPDATE: &str = "*** Update File:";
const MOVE: &str = "*** Move to:";
const EOF_MARK: &str = "*** End of File";

fn parse(patch: &str) -> Result<Vec<Section>, String> {
    let patch = text::normalize_newlines(patch);
    let mut lines: Vec<&str> = patch.lines().collect();
    // Tolerate a wrapping heredoc or Markdown fence.
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.len() >= 2 {
        let first = lines[0].trim();
        let last = lines[lines.len() - 1].trim();
        let fenced = first.starts_with("```") && last == "```";
        let heredoc = first.contains("<<") && (last == "EOF" || last == "'EOF'");
        if fenced || heredoc {
            lines = lines[1..lines.len() - 1].to_vec();
        }
    }
    if lines.first().map(|l| l.trim()) != Some(BEGIN) {
        return Err(format!("the patch must start with a '{BEGIN}' line"));
    }
    if lines.last().map(|l| l.trim()) != Some(END) {
        return Err(format!("the patch must end with a '{END}' line"));
    }
    let body = &lines[1..lines.len() - 1];
    let mut sections = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let line = body[i].trim();
        let number = i + 2;
        if line.is_empty() {
            i += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix(ADD) {
            let path = header_path(path, number)?;
            i += 1;
            let mut contents = String::new();
            while i < body.len() && !is_section_header(body[i]) {
                if body[i].trim().is_empty() {
                    contents.push('\n');
                    i += 1;
                    continue;
                }
                let Some(text) = body[i].strip_prefix('+') else {
                    return Err(format!(
                        "line {}: every line of an added file must start with '+', got {:?}",
                        i + 2,
                        body[i]
                    ));
                };
                contents.push_str(text);
                contents.push('\n');
                i += 1;
            }
            sections.push(Section::Add { path, contents });
        } else if let Some(path) = line.strip_prefix(DELETE) {
            sections.push(Section::Delete {
                path: header_path(path, number)?,
            });
            i += 1;
        } else if let Some(path) = line.strip_prefix(UPDATE) {
            let path = header_path(path, number)?;
            i += 1;
            let mut move_to = None;
            if i < body.len()
                && let Some(dest) = body[i].trim().strip_prefix(MOVE)
            {
                move_to = Some(header_path(dest, i + 2)?);
                i += 1;
            }
            let mut chunks: Vec<Chunk> = Vec::new();
            let mut current: Option<Chunk> = None;
            while i < body.len() && !is_section_header(body[i]) {
                let raw = body[i];
                let trimmed = raw.trim();
                if trimmed == "@@" || trimmed.starts_with("@@ ") {
                    let context = trimmed.strip_prefix("@@").unwrap_or("").trim();
                    // Consecutive @@ lines accumulate into one chunk's context list.
                    let chunk = match current.take() {
                        Some(c) if c.old_lines.is_empty() && c.new_lines.is_empty() => c,
                        Some(c) => {
                            chunks.push(c);
                            Chunk::default()
                        }
                        None => Chunk::default(),
                    };
                    let mut chunk = chunk;
                    if !context.is_empty() {
                        chunk.contexts.push(context.to_owned());
                    }
                    current = Some(chunk);
                } else if trimmed == EOF_MARK {
                    let mut chunk = current
                        .take()
                        .ok_or_else(|| format!("line {}: '{EOF_MARK}' without a hunk", i + 2))?;
                    chunk.eof = true;
                    chunks.push(chunk);
                } else {
                    let chunk = current.get_or_insert_with(Chunk::default);
                    if raw.is_empty() {
                        chunk.old_lines.push(String::new());
                        chunk.new_lines.push(String::new());
                    } else if let Some(text) = raw.strip_prefix(' ') {
                        chunk.old_lines.push(text.to_owned());
                        chunk.new_lines.push(text.to_owned());
                    } else if let Some(text) = raw.strip_prefix('-') {
                        chunk.old_lines.push(text.to_owned());
                    } else if let Some(text) = raw.strip_prefix('+') {
                        chunk.new_lines.push(text.to_owned());
                    } else {
                        return Err(format!(
                            "line {}: unexpected line in update hunk for {path}: {raw:?}. Hunk lines must start with ' ', '-' or '+', and hunks with '@@'.",
                            i + 2
                        ));
                    }
                }
                i += 1;
            }
            if let Some(chunk) = current {
                chunks.push(chunk);
            }
            chunks.retain(|c| {
                !(c.old_lines.is_empty() && c.new_lines.is_empty() && c.contexts.is_empty())
            });
            if chunks
                .iter()
                .any(|c| c.old_lines.is_empty() && c.new_lines.is_empty())
            {
                return Err(format!("update of {path} has an empty hunk"));
            }
            if chunks.is_empty() && move_to.is_none() {
                return Err(format!("update of {path} has no hunks"));
            }
            sections.push(Section::Update {
                path,
                move_to,
                chunks,
            });
        } else {
            return Err(format!(
                "line {number}: expected '{ADD}', '{DELETE}' or '{UPDATE}', got {:?}",
                body[i]
            ));
        }
    }
    if sections.is_empty() {
        return Err("the patch contains no file sections".into());
    }
    Ok(sections)
}

/// Every path a patch names in `Add File` / `Update File` / `Delete File` / `Move to` lines,
/// as written (trimmed), in order, without duplicates. No filesystem access, no validation:
/// it scans header lines the same way [`parse`] recognizes them (and `Move to` anywhere), so
/// it lists at least every path [`apply`] would touch, even for a patch that fails to parse.
pub fn touched_paths(patch: &str) -> Vec<String> {
    let patch = text::normalize_newlines(patch);
    let mut out: Vec<String> = Vec::new();
    for line in patch.lines() {
        let line = line.trim();
        let path = [ADD, DELETE, UPDATE, MOVE]
            .iter()
            .find_map(|marker| line.strip_prefix(marker))
            .map(str::trim);
        if let Some(path) = path
            && !path.is_empty()
            && !out.iter().any(|p| p == path)
        {
            out.push(path.to_owned());
        }
    }
    out
}

fn header_path(raw: &str, line: usize) -> Result<String, String> {
    let path = raw.trim();
    if path.is_empty() {
        Err(format!("line {line}: missing file path"))
    } else {
        Ok(path.to_owned())
    }
}

fn is_section_header(line: &str) -> bool {
    let t = line.trim();
    t.starts_with(ADD) || t.starts_with(DELETE) || t.starts_with(UPDATE)
}

/// Normalizes Unicode punctuation and spaces to ASCII for the last fuzzy pass.
fn normalize_punct(s: &str) -> String {
    s.trim()
        .chars()
        .map(|c| match c {
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

fn seek(lines: &[&str], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let last = lines.len() - pattern.len();
    let from = if eof { last } else { start };
    if from > last {
        return None;
    }
    type LineEq = fn(&str, &str) -> bool;
    let passes: [LineEq; 4] = [
        |a, b| a == b,
        |a, b| a.trim_end() == b.trim_end(),
        |a, b| a.trim() == b.trim(),
        |a, b| normalize_punct(a) == normalize_punct(b),
    ];
    for eq in passes {
        for i in from..=last {
            if pattern.iter().enumerate().all(|(k, p)| eq(lines[i + k], p)) {
                return Some(i);
            }
        }
    }
    None
}

/// Applies update chunks to an LF body; the result always ends with a newline.
fn apply_chunks(body: &str, chunks: &[Chunk], display: &str) -> Result<String, String> {
    let mut lines: Vec<&str> = body.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut index = 0;
    for chunk in chunks {
        for context in &chunk.contexts {
            let found = seek(&lines, std::slice::from_ref(context), index, false)
                .ok_or_else(|| format!("failed to find context '@@ {context}' in {display}"))?;
            index = found + 1;
        }
        if chunk.old_lines.is_empty() {
            let at = if chunk.contexts.is_empty() || chunk.eof {
                lines.len()
            } else {
                index
            };
            replacements.push((at, 0, chunk.new_lines.clone()));
            continue;
        }
        let mut old = chunk.old_lines.as_slice();
        let mut new = chunk.new_lines.as_slice();
        let mut found = seek(&lines, old, index, chunk.eof);
        if found.is_none() && old.last().is_some_and(String::is_empty) {
            old = &old[..old.len() - 1];
            if new.last().is_some_and(String::is_empty) {
                new = &new[..new.len() - 1];
            }
            found = seek(&lines, old, index, chunk.eof);
        }
        let Some(start) = found else {
            return Err(format!(
                "failed to find the expected lines in {display}:\n{}\nRe-read the file and make sure the context and '-' lines match it exactly.",
                chunk.old_lines.join("\n")
            ));
        };
        replacements.push((start, old.len(), new.to_vec()));
        index = start + old.len();
    }
    replacements.sort_by_key(|r| r.0);
    for pair in replacements.windows(2) {
        if pair[0].0 + pair[0].1 > pair[1].0 {
            return Err(format!("hunks overlap in {display}"));
        }
    }
    let mut out: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
    for (start, len, new) in replacements.into_iter().rev() {
        out.splice(start..start + len, new);
    }
    let mut result = out.join("\n");
    result.push('\n');
    Ok(result)
}

/// Virtual file state while validating.
#[derive(Clone)]
struct Entry {
    /// Original state on disk (None = did not exist).
    original: Option<Vec<u8>>,
    /// Planned state (None = deleted / absent).
    planned: Option<String>,
    style: Text,
}

fn key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

struct Plan {
    entries: HashMap<String, (PathBuf, Entry)>,
    order: Vec<String>,
}

impl Plan {
    fn load(&mut self, path: &Path) -> Result<&mut Entry, String> {
        let k = key(path);
        if !self.entries.contains_key(&k) {
            let display = to_unix(path);
            let entry = match fs::metadata(path) {
                Ok(meta) if meta.is_dir() => {
                    return Err(format!("{display} is a directory, not a file"));
                }
                Ok(_) => {
                    let bytes =
                        fs::read(path).map_err(|err| format!("cannot read {display}: {err}"))?;
                    let decoded = text::decode(&bytes);
                    let text = (!text::is_binary(&bytes) && std::str::from_utf8(&bytes).is_ok())
                        .then(|| decoded.body.clone());
                    Entry {
                        original: Some(bytes),
                        planned: text.or(Some(String::from("\0binary"))),
                        style: decoded,
                    }
                }
                Err(_) => Entry {
                    original: None,
                    planned: None,
                    style: Text::default(),
                },
            };
            self.entries.insert(k.clone(), (path.to_path_buf(), entry));
            self.order.push(k.clone());
        }
        Ok(&mut self.entries.get_mut(&k).expect("inserted").1)
    }
}

/// Parses, validates and applies a patch. Returns per-file changes.
pub fn apply(ws: &Workspace, patch: &str) -> Result<PatchResult, String> {
    let sections = parse(patch)?;
    let mut plan = Plan {
        entries: HashMap::new(),
        order: Vec::new(),
    };
    let mut changes: Vec<FileChange> = Vec::new();
    let is_binary = |s: &Option<String>| s.as_deref() == Some("\0binary");

    for section in &sections {
        match section {
            Section::Add { path, contents } => {
                let abs = ws.resolve(path)?;
                let display = to_unix(&abs);
                let entry = plan.load(&abs)?;
                if entry.planned.is_some() {
                    return Err(format!(
                        "cannot add {display}: it already exists. Use '*** Update File:' instead."
                    ));
                }
                entry.planned = Some(contents.clone());
                changes.push(FileChange {
                    path: display,
                    moved_from: None,
                    before: None,
                    after: Some(contents.clone()),
                    created: true,
                    deleted: false,
                });
            }
            Section::Delete { path } => {
                let abs = ws.resolve(path)?;
                let display = to_unix(&abs);
                let entry = plan.load(&abs)?;
                let Some(before) = entry.planned.take() else {
                    return Err(format!("cannot delete {display}: the file does not exist"));
                };
                changes.push(FileChange {
                    path: display,
                    moved_from: None,
                    before: (before != "\0binary").then_some(before),
                    after: None,
                    created: false,
                    deleted: true,
                });
            }
            Section::Update {
                path,
                move_to,
                chunks,
            } => {
                let abs = ws.resolve(path)?;
                let display = to_unix(&abs);
                let entry = plan.load(&abs)?;
                let Some(before) = entry.planned.clone() else {
                    return Err(format!(
                        "cannot update {display}: the file does not exist. Use '*** Add File:' to create it."
                    ));
                };
                if is_binary(&entry.planned) {
                    return Err(format!(
                        "cannot update {display}: it is a binary or non-UTF-8 file"
                    ));
                }
                let style = entry.style.clone();
                let after = if chunks.is_empty() {
                    before.clone()
                } else {
                    apply_chunks(&before, chunks, &display)?
                };
                match move_to {
                    Some(dest) => {
                        let dest_abs = ws.resolve(dest)?;
                        let dest_display = to_unix(&dest_abs);
                        if key(&dest_abs) != key(&abs) {
                            let dest_entry = plan.load(&dest_abs)?;
                            if dest_entry.planned.is_some() {
                                return Err(format!(
                                    "cannot move {display} to {dest_display}: the destination already exists"
                                ));
                            }
                            dest_entry.planned = Some(after.clone());
                            // The moved file keeps the source's style, including per-line
                            // endings of a mixed file (encode diffs against `style.body`).
                            dest_entry.style = style;
                            plan.load(&abs)?.planned = None;
                        } else {
                            plan.load(&abs)?.planned = Some(after.clone());
                        }
                        changes.push(FileChange {
                            path: dest_display,
                            moved_from: Some(display),
                            before: Some(before),
                            after: Some(after),
                            created: false,
                            deleted: false,
                        });
                    }
                    None => {
                        if after == before {
                            return Err(format!(
                                "the update of {display} does not change anything"
                            ));
                        }
                        entry_set(&mut plan, &abs, after.clone())?;
                        changes.push(FileChange {
                            path: display,
                            moved_from: None,
                            before: Some(before),
                            after: Some(after),
                            created: false,
                            deleted: false,
                        });
                    }
                }
            }
        }
    }

    commit(&plan)?;
    Ok(PatchResult { files: changes })
}

fn entry_set(plan: &mut Plan, path: &Path, body: String) -> Result<(), String> {
    plan.load(path)?.planned = Some(body);
    Ok(())
}

/// Writes every changed file, then removes deleted ones; restores on failure.
fn commit(plan: &Plan) -> Result<(), String> {
    let mut done: Vec<(&PathBuf, &Entry)> = Vec::new();
    let mut result = Ok(());
    // Writes first, deletes last, so a failed write leaves sources of moves intact.
    let mut ops: Vec<&(PathBuf, Entry)> = plan.order.iter().map(|k| &plan.entries[k]).collect();
    ops.sort_by_key(|(_, e)| e.planned.is_none());
    for (path, entry) in ops {
        let original_text = entry.original.as_ref().map(|_| entry.style.body.as_str());
        let outcome = match (&entry.planned, &entry.original) {
            (Some(body), _) if body == "\0binary" => continue,
            (Some(body), _) if original_text == Some(body.as_str()) => continue,
            (Some(body), _) => text::atomic_write(path, &entry.style.encode(body)),
            (None, Some(_)) => fs::remove_file(path)
                .map_err(|err| format!("cannot delete {}: {err}", to_unix(path))),
            (None, None) => continue,
        };
        if let Err(err) = outcome {
            result = Err(err);
            break;
        }
        done.push((path, entry));
    }
    if let Err(err) = result {
        let mut restore_failures = Vec::new();
        for (path, entry) in done.into_iter().rev() {
            let restored = match &entry.original {
                Some(bytes) => text::atomic_write(path, bytes),
                None => fs::remove_file(path).map_err(|e| e.to_string()),
            };
            if restored.is_err() {
                restore_failures.push(to_unix(path));
            }
        }
        return Err(if restore_failures.is_empty() {
            format!("{err}; no files were changed")
        } else {
            format!("{err}; could not restore: {}", restore_failures.join(", "))
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> (tempfile::TempDir, PathBuf, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let root =
            super::super::paths::strip_verbatim(&fs::canonicalize(dir.path()).unwrap()).join("p");
        fs::create_dir_all(&root).unwrap();
        let ws = Workspace::new(&[root.to_string_lossy().into_owned()]).unwrap();
        (dir, root, ws)
    }

    #[test]
    fn parses_all_sections() {
        let patch = "*** Begin Patch\n*** Add File: a.txt\n+hi\n*** Update File: b.txt\n*** Move to: c.txt\n@@ fn x\n ctx\n-old\n+new\n*** End of File\n*** Delete File: d.txt\n*** End Patch\n";
        let sections = parse(patch).unwrap();
        assert_eq!(sections.len(), 3);
        assert_eq!(
            sections[0],
            Section::Add {
                path: "a.txt".into(),
                contents: "hi\n".into()
            }
        );
        let Section::Update {
            move_to, chunks, ..
        } = &sections[1]
        else {
            panic!()
        };
        assert_eq!(move_to.as_deref(), Some("c.txt"));
        assert_eq!(chunks[0].contexts, vec!["fn x"]);
        assert!(chunks[0].eof);
        assert!(parse("*** Update File: a\n").is_err());
        assert!(parse("*** Begin Patch\n*** End Patch").is_err());
        assert!(parse("*** Begin Patch\n*** Update File: a\n*** End Patch").is_err());
        assert!(parse("*** Begin Patch\n*** Frobnicate: a\n*** End Patch").is_err());
        assert!(parse("```\n*** Begin Patch\n*** Delete File: a\n*** End Patch\n```").is_ok());
    }

    #[test]
    fn update_multiple_chunks_preserves_crlf_and_bom() {
        let (_d, root, ws) = ws();
        fs::write(
            root.join("a.txt"),
            "\u{feff}one\r\ntwo\r\nthree\r\nfour\r\nfive\r\n",
        )
        .unwrap();
        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n one\n-two\n+TWO\n@@\n four\n-five\n+FIVE\n*** End Patch";
        let result = apply(&ws, patch).unwrap();
        assert_eq!(
            result.files[0].after.as_deref(),
            Some("one\nTWO\nthree\nfour\nFIVE\n")
        );
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "\u{feff}one\r\nTWO\r\nthree\r\nfour\r\nFIVE\r\n"
        );
    }

    #[test]
    fn fuzzy_whitespace_and_context_header() {
        let (_d, root, ws) = ws();
        fs::write(root.join("a.py"), "class A:\n    def f():\n        return 1  \nclass B:\n    def f():\n        return 1\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: a.py\n@@ class B:\n     def f():\n-        return 1\n+        return 2\n*** End Patch";
        apply(&ws, patch).unwrap();
        let got = fs::read_to_string(root.join("a.py")).unwrap();
        assert!(
            got.ends_with("class B:\n    def f():\n        return 2\n"),
            "{got}"
        );
        assert!(got.contains("return 1  \n"));
    }

    #[test]
    fn add_delete_move() {
        let (_d, root, ws) = ws();
        fs::write(root.join("old.txt"), "a\nb\n").unwrap();
        fs::write(root.join("gone.txt"), "x\n").unwrap();
        let patch = "*** Begin Patch\n*** Add File: dir/new.txt\n+hello\n*** Update File: old.txt\n*** Move to: moved/new_name.txt\n@@\n-b\n+B\n*** Delete File: gone.txt\n*** End Patch";
        let result = apply(&ws, patch).unwrap();
        assert_eq!(result.files.len(), 3);
        assert!(result.files[0].created);
        assert_eq!(
            result.files[1]
                .moved_from
                .as_deref()
                .map(|p| p.ends_with("/old.txt")),
            Some(true)
        );
        assert!(result.files[2].deleted);
        assert_eq!(
            fs::read_to_string(root.join("dir/new.txt")).unwrap(),
            "hello\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("moved/new_name.txt")).unwrap(),
            "a\nB\n"
        );
        assert!(!root.join("old.txt").exists() && !root.join("gone.txt").exists());
    }

    #[test]
    fn all_or_nothing() {
        let (_d, root, ws) = ws();
        fs::write(root.join("a.txt"), "a\n").unwrap();
        fs::write(root.join("b.txt"), "b\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n-a\n+A\n*** Add File: c.txt\n+c\n*** Update File: b.txt\n@@\n-missing\n+x\n*** End Patch";
        let err = apply(&ws, patch).unwrap_err();
        assert!(err.contains("failed to find"), "{err}");
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "a\n");
        assert!(!root.join("c.txt").exists());
    }

    #[test]
    fn workspace_escape_rejected_before_writing() {
        let (_d, root, ws) = ws();
        fs::write(root.join("a.txt"), "a\n").unwrap();
        for bad in ["../evil.txt", "/c/Windows/evil.txt", "C:\\evil.txt"] {
            let patch = format!(
                "*** Begin Patch\n*** Update File: a.txt\n@@\n-a\n+A\n*** Add File: {bad}\n+x\n*** End Patch"
            );
            let err = apply(&ws, &patch).unwrap_err();
            assert!(err.contains("outside the workspace"), "{bad}: {err}");
        }
        let patch =
            "*** Begin Patch\n*** Update File: a.txt\n*** Move to: ../out.txt\n*** End Patch";
        assert!(
            apply(&ws, patch)
                .unwrap_err()
                .contains("outside the workspace")
        );
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "a\n");
    }

    #[test]
    fn rejects_bad_targets() {
        let (_d, root, ws) = ws();
        fs::write(root.join("a.txt"), "a\n").unwrap();
        fs::create_dir(root.join("d")).unwrap();
        let cases = [
            ("*** Add File: a.txt\n+x", "already exists"),
            ("*** Delete File: nope.txt", "does not exist"),
            ("*** Update File: nope.txt\n@@\n-a\n+b", "does not exist"),
            ("*** Delete File: d", "directory"),
            (
                "*** Update File: a.txt\n*** Move to: a2.txt\n@@\n-zzz\n+b",
                "failed to find",
            ),
        ];
        for (body, needle) in cases {
            let err = apply(&ws, &format!("*** Begin Patch\n{body}\n*** End Patch")).unwrap_err();
            assert!(err.contains(needle), "{body}: {err}");
        }
        fs::write(root.join("b.txt"), "b\n").unwrap();
        let err = apply(
            &ws,
            "*** Begin Patch\n*** Update File: a.txt\n*** Move to: b.txt\n*** End Patch",
        )
        .unwrap_err();
        assert!(err.contains("destination already exists"));
    }

    #[test]
    fn pure_addition_appends_and_eof() {
        let (_d, root, ws) = ws();
        fs::write(root.join("a.txt"), "a\nb").unwrap();
        apply(
            &ws,
            "*** Begin Patch\n*** Update File: a.txt\n@@\n+c\n*** End Patch",
        )
        .unwrap();
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "a\nb\nc\n");
        fs::write(root.join("e.txt"), "x\ny\nx\n").unwrap();
        apply(
            &ws,
            "*** Begin Patch\n*** Update File: e.txt\n@@\n-x\n+X\n*** End of File\n*** End Patch",
        )
        .unwrap();
        assert_eq!(fs::read_to_string(root.join("e.txt")).unwrap(), "x\ny\nX\n");
    }

    #[test]
    fn touched_paths_lists_headers() {
        let patch = "*** Begin Patch\n*** Add File: a.txt\n+*** Delete File: not-a-header\n*** Update File:  b/c.txt \n*** Move to: //srv/s/d.txt\n@@\n-x\n+y\n*** Delete File: a.txt\n*** Delete File: e.txt\n*** End Patch";
        assert_eq!(
            touched_paths(patch),
            vec!["a.txt", "b/c.txt", "//srv/s/d.txt", "e.txt"]
        );
        // Works on patches that do not parse, too.
        assert_eq!(touched_paths("*** Update File: x\r\ngarbage"), vec!["x"]);
        assert!(touched_paths("").is_empty());
    }

    #[test]
    fn unc_paths_rejected_without_fs_access() {
        let (_d, root, ws) = ws();
        fs::write(root.join("a.txt"), "a\n").unwrap();
        for section in [
            "*** Add File: //nonexistent.invalid/share/x.txt\n+x",
            "*** Delete File: //nonexistent.invalid/share/x.txt",
            "*** Update File: //nonexistent.invalid/share/x.txt\n@@\n-a\n+b",
            "*** Update File: a.txt\n*** Move to: //nonexistent.invalid/share/x.txt\n@@\n-a\n+b",
        ] {
            let started = std::time::Instant::now();
            let err =
                apply(&ws, &format!("*** Begin Patch\n{section}\n*** End Patch")).unwrap_err();
            assert!(err.contains("outside the workspace"), "{section}: {err}");
            assert!(started.elapsed() < std::time::Duration::from_millis(500));
        }
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "a\n");
    }

    #[test]
    fn mixed_line_endings_preserved() {
        let (_d, root, ws) = ws();
        fs::write(root.join("m.txt"), "one\r\ntwo\nthree\r\nfour\r\n").unwrap();
        apply(
            &ws,
            "*** Begin Patch\n*** Update File: m.txt\n*** Move to: n.txt\n@@\n three\n-four\n+FOUR\n+five\n*** End Patch",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("n.txt")).unwrap(),
            "one\r\ntwo\nthree\r\nFOUR\r\nfive\r\n"
        );
    }

    #[test]
    fn sequential_sections_see_earlier_ones() {
        let (_d, root, ws) = ws();
        let patch = "*** Begin Patch\n*** Add File: n.txt\n+one\n*** Update File: n.txt\n@@\n-one\n+two\n*** End Patch";
        apply(&ws, patch).unwrap();
        assert_eq!(fs::read_to_string(root.join("n.txt")).unwrap(), "two\n");
    }
}
