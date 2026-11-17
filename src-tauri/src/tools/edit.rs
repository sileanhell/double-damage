//! Single-file edit methods: `str_replace` and Aider-style `search_replace`.
//! Both operate on LF-normalized text; the caller restores the file's BOM/CRLF style.

use serde::Deserialize;
use serde_json::Value;

use super::text::{normalize_newlines, split_lines};

/// Result of an edit applied to an LF body.
#[derive(Debug)]
pub struct Applied {
    pub after: String,
    pub replacements: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StrReplaceInput {
    #[serde(alias = "old_string", alias = "old")]
    old_string: String,
    #[serde(alias = "new_string", alias = "new")]
    new_string: String,
    #[serde(default, alias = "replace_all")]
    replace_all: bool,
}

/// `input: { oldString, newString, replaceAll? }`. `before` is `None` for a missing file.
pub fn str_replace(before: Option<&str>, input: &Value) -> Result<Applied, String> {
    let input: StrReplaceInput = serde_json::from_value(input.clone())
        .map_err(|err| format!("invalid str_replace input: {err}. Expected {{\"oldString\", \"newString\", \"replaceAll\"?}}"))?;
    let old = normalize_newlines(&input.old_string);
    let new = normalize_newlines(&input.new_string);
    if old == new {
        return Err("oldString and newString are identical; nothing to change".into());
    }
    let Some(body) = before else {
        if old.is_empty() {
            return Ok(Applied {
                after: new,
                replacements: 1,
            });
        }
        return Err(
            "the file does not exist. To create it, use an empty oldString (or the write tool)."
                .into(),
        );
    };
    if old.is_empty() {
        if body.is_empty() {
            return Ok(Applied {
                after: new,
                replacements: 1,
            });
        }
        return Err("oldString is empty, but the file already exists and is not empty. Quote the text to replace, or use write to replace the whole file.".into());
    }
    let count = body.matches(old.as_str()).count();
    match count {
        0 => Err(format!(
            "oldString was not found in the file. {}",
            not_found_hint(body, &old)
        )),
        1 => Ok(Applied {
            after: body.replacen(old.as_str(), &new, 1),
            replacements: 1,
        }),
        n if input.replace_all => Ok(Applied {
            after: body.replace(old.as_str(), &new),
            replacements: u32::try_from(n).unwrap_or(u32::MAX),
        }),
        n => {
            let lines = match_lines(body, &old);
            Err(format!(
                "oldString was found {n} times (at lines {lines}). Include more surrounding lines to make it unique, or set replaceAll to replace every occurrence."
            ))
        }
    }
}

fn match_lines(body: &str, needle: &str) -> String {
    let mut out: Vec<String> = body
        .match_indices(needle)
        .take(10)
        .map(|(i, _)| (body[..i].matches('\n').count() + 1).to_string())
        .collect();
    if body.matches(needle).count() > 10 {
        out.push("…".into());
    }
    out.join(", ")
}

/// Cheap hint for a failed match: where the first line of the search text appears loosely.
fn not_found_hint(body: &str, old: &str) -> String {
    let first = old
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if !first.is_empty() {
        let hits: Vec<String> = split_lines(body)
            .iter()
            .enumerate()
            .filter(|(_, l)| l.trim() == first || (first.len() >= 8 && l.contains(first)))
            .take(3)
            .map(|(i, l)| format!("line {}: {:?}", i + 1, super::text::cut_chars(l, 160).0))
            .collect();
        if !hits.is_empty() {
            return format!(
                "A similar line exists ({}); check whitespace, indentation and the following lines, then retry.",
                hits.join("; ")
            );
        }
    }
    "Re-read the file; its content may differ from what you expect.".into()
}

#[derive(Deserialize)]
struct SearchReplaceInput {
    blocks: String,
}

struct Block {
    search: Vec<String>,
    replace: Vec<String>,
}

fn marker(line: &str, ch: char, word: &str) -> bool {
    let t = line.trim();
    let run = t.chars().take_while(|c| *c == ch).count();
    (5..=9).contains(&run) && t[run..].trim() == word
}

fn parse_blocks(text: &str) -> Result<Vec<Block>, String> {
    let text = normalize_newlines(text);
    let mut blocks = Vec::new();
    let mut lines = text.split('\n').enumerate();
    while let Some((n, line)) = lines.next() {
        if !marker(line, '<', "SEARCH") {
            if marker(line, '=', "") || marker(line, '>', "REPLACE") {
                return Err(format!(
                    "line {}: '{}' without a preceding '<<<<<<< SEARCH'",
                    n + 1,
                    line.trim()
                ));
            }
            continue;
        }
        let mut search = Vec::new();
        let mut found_divider = false;
        for (_, l) in lines.by_ref() {
            if marker(l, '=', "") {
                found_divider = true;
                break;
            }
            search.push(l.to_owned());
        }
        if !found_divider {
            return Err(format!(
                "block {}: missing '=======' after SEARCH",
                blocks.len() + 1
            ));
        }
        let mut replace = Vec::new();
        let mut closed = false;
        for (_, l) in lines.by_ref() {
            if marker(l, '>', "REPLACE") {
                closed = true;
                break;
            }
            replace.push(l.to_owned());
        }
        if !closed {
            return Err(format!(
                "block {}: missing '>>>>>>> REPLACE'",
                blocks.len() + 1
            ));
        }
        blocks.push(Block { search, replace });
    }
    if blocks.is_empty() {
        return Err("no SEARCH/REPLACE blocks found. Use:\n<<<<<<< SEARCH\nexact existing lines\n=======\nnew lines\n>>>>>>> REPLACE".into());
    }
    Ok(blocks)
}

fn find_runs(lines: &[String], pattern: &[String], eq: impl Fn(&str, &str) -> bool) -> Vec<usize> {
    if pattern.len() > lines.len() {
        return Vec::new();
    }
    (0..=lines.len() - pattern.len())
        .filter(|&i| {
            pattern
                .iter()
                .enumerate()
                .all(|(k, p)| eq(&lines[i + k], p))
        })
        .collect()
}

/// `input: { blocks }`, applied in order; each SEARCH must match whole lines exactly once
/// (falling back to ignoring trailing whitespace). Empty SEARCH creates a new/empty file.
pub fn search_replace(before: Option<&str>, input: &Value) -> Result<Applied, String> {
    let input: SearchReplaceInput = serde_json::from_value(input.clone()).map_err(|err| {
        format!("invalid search_replace input: {err}. Expected {{\"blocks\": \"<<<<<<< SEARCH…\"}}")
    })?;
    let blocks = parse_blocks(&input.blocks)?;
    let body = before.unwrap_or("");
    let trailing_newline = body.ends_with('\n') || body.is_empty();
    let mut lines: Vec<String> = split_lines(body).into_iter().map(str::to_owned).collect();
    for (index, block) in blocks.iter().enumerate() {
        let n = index + 1;
        let is_empty_search = block.search.iter().all(|l| l.trim().is_empty());
        if is_empty_search {
            if lines.is_empty() {
                lines = block.replace.clone();
                continue;
            }
            return Err(format!(
                "block {n}: an empty SEARCH is only allowed to create a new or empty file; quote the lines to replace"
            ));
        }
        let mut hits = find_runs(&lines, &block.search, |a, b| a == b);
        if hits.is_empty() {
            hits = find_runs(&lines, &block.search, |a, b| a.trim_end() == b.trim_end());
        }
        match hits.as_slice() {
            [start] => {
                let start = *start;
                lines.splice(
                    start..start + block.search.len(),
                    block.replace.iter().cloned(),
                );
            }
            [] => {
                let joined = block.search.join("\n");
                return Err(format!(
                    "block {n}: SEARCH text not found{}. {}",
                    if index > 0 {
                        " (earlier blocks were applied first)"
                    } else {
                        ""
                    },
                    not_found_hint(&lines.join("\n"), &joined)
                ));
            }
            many => {
                let at: Vec<String> = many.iter().take(10).map(|i| (i + 1).to_string()).collect();
                return Err(format!(
                    "block {n}: SEARCH text matches {} places (lines {}); include more surrounding lines to make it unique",
                    many.len(),
                    at.join(", ")
                ));
            }
        }
    }
    let mut after = lines.join("\n");
    if trailing_newline && !lines.is_empty() {
        after.push('\n');
    }
    if before == Some(after.as_str()) {
        return Err("the blocks do not change the file".into());
    }
    Ok(Applied {
        after,
        replacements: u32::try_from(blocks.len()).unwrap_or(u32::MAX),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn str_replace_cases() {
        let body = "a\nfoo\nb\nfoo\n";
        let one = str_replace(
            Some("x\ny\n"),
            &json!({ "oldString": "y", "newString": "z" }),
        )
        .unwrap();
        assert_eq!(one.after, "x\nz\n");
        let err = str_replace(
            Some(body),
            &json!({ "oldString": "foo", "newString": "bar" }),
        )
        .unwrap_err();
        assert!(
            err.contains("2 times") && err.contains("lines 2, 4"),
            "{err}"
        );
        let all = str_replace(
            Some(body),
            &json!({ "oldString": "foo", "newString": "bar", "replaceAll": true }),
        )
        .unwrap();
        assert_eq!(
            (all.after.as_str(), all.replacements),
            ("a\nbar\nb\nbar\n", 2)
        );
        let err = str_replace(
            Some(body),
            &json!({ "oldString": "  b  \nnope", "newString": "x" }),
        )
        .unwrap_err();
        assert!(err.contains("not found") && err.contains("line 3"), "{err}");
        let err =
            str_replace(Some(body), &json!({ "oldString": "zzz", "newString": "x" })).unwrap_err();
        assert!(err.contains("Re-read"), "{err}");
        assert!(str_replace(Some(body), &json!({ "oldString": "a", "newString": "a" })).is_err());
        assert!(str_replace(Some(body), &json!({ "oldString": "", "newString": "a" })).is_err());
        assert_eq!(
            str_replace(None, &json!({ "oldString": "", "newString": "new" }))
                .unwrap()
                .after,
            "new"
        );
        assert!(str_replace(None, &json!({ "oldString": "x", "newString": "new" })).is_err());
        // CRLF in the model's strings matches LF-normalized text.
        let crlf = str_replace(
            Some("a\nb\n"),
            &json!({ "old_string": "a\r\nb", "new_string": "c\r\nd" }),
        )
        .unwrap();
        assert_eq!(crlf.after, "c\nd\n");
    }

    #[test]
    fn search_replace_cases() {
        let body = "fn a() {\n    1\n}\nfn b() {\n    1\n}\n";
        let blocks = "path.rs\n```\n<<<<<<< SEARCH\nfn a() {\n    1\n=======\nfn a() {\n    2\n>>>>>>> REPLACE\n```\n<<<<<<< SEARCH\nfn b() {   \n=======\nfn bee() {\n>>>>>>> REPLACE\n";
        let out = search_replace(Some(body), &json!({ "blocks": blocks })).unwrap();
        assert_eq!(out.after, "fn a() {\n    2\n}\nfn bee() {\n    1\n}\n");
        assert_eq!(out.replacements, 2);

        let ambiguous = "<<<<<<< SEARCH\n    1\n=======\n    3\n>>>>>>> REPLACE";
        assert!(
            search_replace(Some(body), &json!({ "blocks": ambiguous }))
                .unwrap_err()
                .contains("matches 2 places")
        );
        let missing = "<<<<<<< SEARCH\nnope\n=======\nx\n>>>>>>> REPLACE";
        assert!(
            search_replace(Some(body), &json!({ "blocks": missing }))
                .unwrap_err()
                .contains("not found")
        );
        let create = "<<<<<<< SEARCH\n=======\nnew file\n>>>>>>> REPLACE";
        assert_eq!(
            search_replace(None, &json!({ "blocks": create }))
                .unwrap()
                .after,
            "new file\n"
        );
        assert!(search_replace(Some(body), &json!({ "blocks": create })).is_err());
        assert!(
            search_replace(
                Some(body),
                &json!({ "blocks": "<<<<<<< SEARCH\nfn a() {\n" })
            )
            .is_err()
        );
        assert!(search_replace(Some(body), &json!({ "blocks": "nothing" })).is_err());
        // Sequential: the second block sees the first block's result.
        let seq = "<<<<<<< SEARCH\nfn a() {\n=======\nfn z() {\n>>>>>>> REPLACE\n<<<<<<< SEARCH\nfn z() {\n=======\nfn y() {\n>>>>>>> REPLACE";
        assert!(
            search_replace(Some(body), &json!({ "blocks": seq }))
                .unwrap()
                .after
                .starts_with("fn y() {")
        );
    }
}
