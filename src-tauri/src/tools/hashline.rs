//! Hashline editing: `tool_read` with `hashline: true` tags every line as `{n}#{hash}\t{text}`,
//! and edits address lines by those `n#hash` anchors. An anchor whose hash no longer matches
//! the current line is rejected, so the model can never edit lines it has not seen.
//!
//! Modeled on oh-my-pi's original hashline design (per-line `LINE#ID` anchors with
//! `replace`/`append`/`prepend` operations whose positions all refer to the file as it was
//! read, never shifted by earlier operations in the same call).
//!
//! # Input schema (`tool_edit` with `method: "hashline"`)
//!
//! ```json
//! {
//!   "edits": [
//!     { "op": "replace", "pos": "12#k3m", "end": "14#p2a", "lines": ["new 12-14 text"] },
//!     { "op": "replace", "pos": "20#abc", "lines": [] },
//!     { "op": "delete",  "pos": "30#x7q", "end": "31#zz2" },
//!     { "op": "append",  "pos": "40#qqq", "lines": ["inserted after line 40"] },
//!     { "op": "append",  "lines": ["appended at end of file"] },
//!     { "op": "prepend", "pos": "1#mmm", "lines": ["inserted before line 1"] },
//!     { "op": "prepend", "lines": ["inserted at start of file"] }
//!   ]
//! }
//! ```
//!
//! - `op`: `"replace"` (lines `pos`..=`end` become `lines`; `end` defaults to `pos`; empty
//!   `lines` deletes), `"delete"` (same as replace with no lines), `"append"` (insert after
//!   `pos`, or at end of file without `pos`), `"prepend"` (insert before `pos`, or at start of
//!   file without `pos`).
//! - `pos` / `end`: anchors `"N#HASH"` copied from the latest hashline read (anything after
//!   the hash, e.g. a pasted tab and line text, is ignored).
//! - `lines`: array of line strings without newlines (a single string is split on `\n`;
//!   `null` means no lines). If every line carries a copied `N#HASH\t` prefix it is stripped.
//! - All anchors refer to the file as last read. Replaced ranges must not overlap, and an
//!   insert may not target a line inside a replaced range (except the range's own first line
//!   for `prepend` and last line for `append`). Inserts at the same spot keep input order.

use serde::Deserialize;
use serde_json::Value;

/// Hash alphabet: lowercase letters and digits without look-alikes (no `l`, `o`, `0`, `1`).
const ALPHABET: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";

/// Three-character tag of a line's content (trailing whitespace ignored).
pub fn line_hash(line: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in line.trim_end().bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    // Fold the high bits in so short lines still spread over the alphabet.
    hash ^= hash >> 15;
    (0..3)
        .map(|i| ALPHABET[((hash >> (i * 5)) & 31) as usize] as char)
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Input {
    edits: Vec<Op>,
}

#[derive(Debug, Deserialize)]
struct Op {
    op: String,
    #[serde(default, alias = "start", alias = "line")]
    pos: Option<String>,
    #[serde(default)]
    end: Option<String>,
    #[serde(default, alias = "content", alias = "text")]
    lines: Option<Value>,
}

struct Range {
    start: usize,
    end: usize,
    lines: Vec<String>,
    index: usize,
}

/// Applies hashline operations to an LF body. Returns the new body and the operation count.
pub fn apply(body: &str, input: &Value) -> Result<(String, u32), String> {
    let input: Input = serde_json::from_value(input.clone())
        .map_err(|err| format!("invalid hashline input: {err}. Expected {{\"edits\": [{{\"op\", \"pos\", \"end\", \"lines\"}}]}}"))?;
    if input.edits.is_empty() {
        return Err("hashline input has no edits".into());
    }
    let lines = super::text::split_lines(body);
    let count = lines.len();
    let mut ranges: Vec<Range> = Vec::new();
    // (line index 0..=count, inserted lines): before[i] goes before original line i (0-based);
    // before[count] is the end of the file.
    let mut before: Vec<Vec<String>> = vec![Vec::new(); count + 1];
    let mut after: Vec<Vec<String>> = vec![Vec::new(); count];
    let mut head: Vec<String> = Vec::new();
    let mut inserts: Vec<(usize, bool, usize)> = Vec::new(); // (line, is_append, edit index)

    for (index, op) in input.edits.iter().enumerate() {
        let n = index + 1;
        let new_lines = parse_lines(op.lines.as_ref()).map_err(|e| format!("edit #{n}: {e}"))?;
        match op.op.as_str() {
            "replace" | "delete" => {
                let pos = op
                    .pos
                    .as_deref()
                    .ok_or_else(|| format!("edit #{n} ({}) needs \"pos\"", op.op))?;
                let start = resolve(pos, &lines).map_err(|e| format!("edit #{n}: {e}"))?;
                let end = match op.end.as_deref() {
                    Some(end) => resolve(end, &lines).map_err(|e| format!("edit #{n}: {e}"))?,
                    None => start,
                };
                if end < start {
                    return Err(format!(
                        "edit #{n}: end line {} is before pos line {}",
                        end + 1,
                        start + 1
                    ));
                }
                if op.op == "delete" && !new_lines.is_empty() {
                    return Err(format!("edit #{n}: delete takes no lines; use replace"));
                }
                ranges.push(Range {
                    start,
                    end,
                    lines: new_lines,
                    index: n,
                });
            }
            "append" | "prepend" => {
                if new_lines.is_empty() {
                    return Err(format!("edit #{n}: {} needs at least one line", op.op));
                }
                if op.end.is_some() {
                    return Err(format!(
                        "edit #{n}: {} takes only \"pos\", not \"end\"",
                        op.op
                    ));
                }
                let append = op.op == "append";
                match op.pos.as_deref() {
                    Some(pos) => {
                        let line = resolve(pos, &lines).map_err(|e| format!("edit #{n}: {e}"))?;
                        inserts.push((line, append, n));
                        if append {
                            after[line].extend(new_lines);
                        } else {
                            before[line].extend(new_lines);
                        }
                    }
                    None if append => before[count].extend(new_lines),
                    None => head.extend(new_lines),
                }
            }
            other => {
                return Err(format!(
                    "edit #{n}: unknown op \"{other}\"; use replace, delete, append or prepend"
                ));
            }
        }
    }

    // Overlap checks.
    let mut owner: Vec<Option<usize>> = vec![None; count];
    for (slot, range) in ranges.iter().enumerate() {
        for (line, slot_owner) in owner
            .iter_mut()
            .enumerate()
            .take(range.end + 1)
            .skip(range.start)
        {
            if let Some(other) = *slot_owner {
                return Err(format!(
                    "edits #{} and #{} both change line {}; merge them into one replace",
                    ranges[other].index,
                    range.index,
                    line + 1
                ));
            }
            *slot_owner = Some(slot);
        }
    }
    for &(line, append, index) in &inserts {
        if let Some(slot) = owner[line] {
            let range = &ranges[slot];
            let boundary = if append {
                range.end == line
            } else {
                range.start == line
            };
            if !boundary {
                return Err(format!(
                    "edit #{index} inserts relative to line {} which edit #{} replaces; put those lines into that replace instead",
                    line + 1,
                    range.index
                ));
            }
        }
    }

    let mut out: Vec<&str> = Vec::with_capacity(count + 16);
    out.extend(head.iter().map(String::as_str));
    for (line, text) in lines.iter().enumerate() {
        out.extend(before[line].iter().map(String::as_str));
        match owner[line] {
            Some(slot) if ranges[slot].start == line => {
                out.extend(ranges[slot].lines.iter().map(String::as_str))
            }
            Some(_) => {}
            None => out.push(text),
        }
        out.extend(after[line].iter().map(String::as_str));
    }
    out.extend(before[count].iter().map(String::as_str));

    let mut result = out.join("\n");
    let trailing = body.ends_with('\n') || body.is_empty();
    if trailing && !out.is_empty() {
        result.push('\n');
    }
    Ok((result, u32::try_from(input.edits.len()).unwrap_or(u32::MAX)))
}

/// Parses `"N#hash"` and checks it against the current lines; returns the 0-based index.
fn resolve(anchor: &str, lines: &[&str]) -> Result<usize, String> {
    let text = anchor.trim();
    let (number, rest) = text.split_once('#').ok_or_else(|| {
        format!("anchor \"{anchor}\" has no hash; use the N#hash tag from a hashline read (e.g. \"12#k3m\")")
    })?;
    let number: usize = number
        .trim()
        .parse()
        .map_err(|_| format!("anchor \"{anchor}\" does not start with a line number"))?;
    let hash: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    if hash.is_empty() {
        return Err(format!("anchor \"{anchor}\" has an empty hash"));
    }
    if number == 0 || number > lines.len() {
        return Err(format!(
            "line {number} does not exist (the file has {} lines). Re-read the file with hashline and retry.",
            lines.len()
        ));
    }
    let actual = line_hash(lines[number - 1]);
    if !actual.eq_ignore_ascii_case(&hash) {
        let (shown, _) = super::text::cut_chars(lines[number - 1], 200);
        return Err(format!(
            "line {number} has changed since it was read (expected #{hash}, now {number}#{actual}: {shown:?}). The file was modified; re-read it with hashline and retry with fresh anchors."
        ));
    }
    Ok(number - 1)
}

fn parse_lines(value: Option<&Value>) -> Result<Vec<String>, String> {
    let mut lines: Vec<String> = match value {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(text)) => {
            let text = super::text::normalize_newlines(text);
            let text = text.strip_suffix('\n').unwrap_or(&text);
            text.split('\n').map(str::to_owned).collect()
        }
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(s) => Ok(super::text::normalize_newlines(s)),
                other => Err(format!("\"lines\" must contain strings, got {other}")),
            })
            .collect::<Result<_, _>>()?,
        Some(other) => {
            return Err(format!(
                "\"lines\" must be an array of strings, got {other}"
            ));
        }
    };
    // Lines containing newlines are split so every element is exactly one line.
    if lines.iter().any(|line| line.contains('\n')) {
        lines = lines
            .iter()
            .flat_map(|line| line.split('\n').map(str::to_owned))
            .collect::<Vec<_>>();
    }
    if !lines.is_empty() && lines.iter().all(|line| copied_prefix(line).is_some()) {
        lines = lines
            .iter()
            .map(|line| line[copied_prefix(line).unwrap_or(0)..].to_owned())
            .collect();
    }
    Ok(lines)
}

/// Length of a copied `   12#abc\t` read prefix, if present.
fn copied_prefix(line: &str) -> Option<usize> {
    let trimmed = line.trim_start_matches(' ');
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = &trimmed[digits..];
    let hash = rest.strip_prefix('#')?;
    if hash.len() < 4
        || !hash.as_bytes()[..3].iter().all(|b| ALPHABET.contains(b))
        || hash.as_bytes()[3] != b'\t'
    {
        return None;
    }
    Some(line.len() - hash.len() + 4)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn anchor(body: &str, n: usize) -> String {
        format!(
            "{n}#{}",
            line_hash(super::super::text::split_lines(body)[n - 1])
        )
    }

    const BODY: &str = "one\ntwo\nthree\nfour\n";

    #[test]
    fn hash_is_stable_and_short() {
        assert_eq!(line_hash("abc"), line_hash("abc  "));
        assert_eq!(line_hash("abc").len(), 3);
        assert_ne!(line_hash("abc"), line_hash("abd"));
    }

    #[test]
    fn replace_range_and_single() {
        let input = json!({ "edits": [
            { "op": "replace", "pos": anchor(BODY, 2), "end": anchor(BODY, 3), "lines": ["TWO", "THREE", "extra"] },
            { "op": "replace", "pos": anchor(BODY, 4), "lines": "FOUR" },
        ]});
        let (out, n) = apply(BODY, &input).unwrap();
        assert_eq!(out, "one\nTWO\nTHREE\nextra\nFOUR\n");
        assert_eq!(n, 2);
    }

    #[test]
    fn insert_and_delete() {
        let input = json!({ "edits": [
            { "op": "prepend", "lines": ["head"] },
            { "op": "append", "pos": anchor(BODY, 1), "lines": ["after one"] },
            { "op": "delete", "pos": anchor(BODY, 3) },
            { "op": "prepend", "pos": anchor(BODY, 4), "lines": ["before four"] },
            { "op": "append", "lines": ["tail"] },
        ]});
        let (out, _) = apply(BODY, &input).unwrap();
        assert_eq!(out, "head\none\nafter one\ntwo\nbefore four\nfour\ntail\n");
    }

    #[test]
    fn stale_hash_rejected() {
        let input = json!({ "edits": [{ "op": "replace", "pos": "2#zzz", "lines": ["x"] }] });
        let err = apply(BODY, &input).unwrap_err();
        assert!(
            err.contains("changed since it was read") && err.contains("re-read"),
            "{err}"
        );
        let stale = anchor("one\nTWO-old\n", 2);
        let input = json!({ "edits": [{ "op": "replace", "pos": stale, "lines": ["x"] }] });
        assert!(apply(BODY, &input).is_err());
    }

    #[test]
    fn bad_anchors() {
        for pos in ["2", "99#abc", "0#abc", "x#abc", "2#"] {
            let input = json!({ "edits": [{ "op": "replace", "pos": pos, "lines": ["x"] }] });
            assert!(apply(BODY, &input).is_err(), "{pos}");
        }
    }

    #[test]
    fn overlap_rejected() {
        let input = json!({ "edits": [
            { "op": "replace", "pos": anchor(BODY, 1), "end": anchor(BODY, 3), "lines": ["x"] },
            { "op": "delete", "pos": anchor(BODY, 3) },
        ]});
        assert!(
            apply(BODY, &input)
                .unwrap_err()
                .contains("both change line 3")
        );
        let input = json!({ "edits": [
            { "op": "replace", "pos": anchor(BODY, 1), "end": anchor(BODY, 3), "lines": ["x"] },
            { "op": "append", "pos": anchor(BODY, 2), "lines": ["y"] },
        ]});
        assert!(apply(BODY, &input).is_err());
        let input = json!({ "edits": [
            { "op": "replace", "pos": anchor(BODY, 1), "end": anchor(BODY, 3), "lines": ["x"] },
            { "op": "append", "pos": anchor(BODY, 3), "lines": ["y"] },
        ]});
        assert_eq!(apply(BODY, &input).unwrap().0, "x\ny\nfour\n");
    }

    #[test]
    fn copied_prefixes_and_pasted_anchor_text() {
        let pos = format!("{}\ttwo", anchor(BODY, 2));
        let input = json!({ "edits": [{ "op": "replace", "pos": pos, "lines": [format!("{}\tTWO", anchor(BODY, 2))] }] });
        assert_eq!(apply(BODY, &input).unwrap().0, "one\nTWO\nthree\nfour\n");
    }

    #[test]
    fn empty_file_and_no_trailing_newline() {
        let input = json!({ "edits": [{ "op": "append", "lines": ["a", "b"] }] });
        assert_eq!(apply("", &input).unwrap().0, "a\nb\n");
        let input =
            json!({ "edits": [{ "op": "replace", "pos": anchor("x\ny", 2), "lines": ["z"] }] });
        assert_eq!(apply("x\ny", &input).unwrap().0, "x\nz");
    }
}
