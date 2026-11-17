//! Text decoding/encoding helpers shared by the file tools.

use std::{
    fs,
    io::Write,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use super::paths::to_unix;

const BOM: &str = "\u{feff}";
/// Files larger than this are not loaded by read/edit.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Line-diff effort limit for files with mixed line endings; past it, changed regions just
/// get the dominant style.
const DIFF_MAX_EDITS: usize = 1000;

/// A text file split into its encoding style and LF-normalized body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text {
    pub bom: bool,
    /// The dominant line ending is CRLF (used for new and changed lines).
    pub crlf: bool,
    /// Only for files that mix CRLF and LF: for every `\n` of `body`, in order, whether it was
    /// `\r\n`. `None` when all endings are in the `crlf` style.
    pub mixed: Option<Vec<bool>>,
    /// Content without BOM, with every `\r\n` turned into `\n`.
    pub body: String,
}

impl Text {
    /// Re-applies the original BOM and line-ending style to a new LF body. In a file with
    /// mixed endings, lines kept from `self.body` keep their own ending (found by a line
    /// diff); new and changed lines get the dominant one.
    pub fn encode(&self, body: &str) -> Vec<u8> {
        let mut out = String::with_capacity(body.len() + body.len() / 16 + 16);
        if self.bom {
            out.push_str(BOM);
        }
        match &self.mixed {
            None if self.crlf => out.push_str(&body.replace('\n', "\r\n")),
            None => out.push_str(body),
            Some(endings) => {
                let old: Vec<&str> = self.body.split('\n').collect();
                let new: Vec<&str> = body.split('\n').collect();
                let kept = match_lines(&old, &new);
                for (j, line) in new.iter().enumerate() {
                    out.push_str(line);
                    if j + 1 < new.len() {
                        let crlf = kept[j]
                            .and_then(|i| endings.get(i).copied())
                            .unwrap_or(self.crlf);
                        out.push_str(if crlf { "\r\n" } else { "\n" });
                    }
                }
            }
        }
        out.into_bytes()
    }
}

/// For every line of `new`, the index of the equal line of `old` it was kept from (`None` for
/// added or changed lines), per a shortest line diff.
fn match_lines(old: &[&str], new: &[&str]) -> Vec<Option<usize>> {
    let mut kept = vec![None; new.len()];
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let room = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    for (j, slot) in kept.iter_mut().enumerate().take(prefix) {
        *slot = Some(j);
    }
    for s in 0..suffix {
        kept[new.len() - 1 - s] = Some(old.len() - 1 - s);
    }
    let a = &old[prefix..old.len() - suffix];
    let b = &new[prefix..new.len() - suffix];
    if let Some(pairs) = myers(a, b, DIFF_MAX_EDITS) {
        for (i, j) in pairs {
            kept[prefix + j] = Some(prefix + i);
        }
    }
    kept
}

/// Equal-line pairs `(index in a, index in b)` of a shortest edit script (Myers), or `None`
/// if it needs more than `max_edits` insertions/deletions. Memory is O(max_edits²).
fn myers(a: &[&str], b: &[&str], max_edits: usize) -> Option<Vec<(usize, usize)>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let limit = (n + m).min(max_edits as isize);
    let offset = limit + 1;
    let idx = |k: isize| (k + offset) as usize;
    let mut v = vec![0isize; 2 * offset as usize + 1];
    // trace[d] = furthest x on diagonals -d..=d after d edits.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    for d in 0..=limit {
        for k in (-d..=d).step_by(2) {
            let mut x = if k == -d || (k != d && v[idx(k - 1)] < v[idx(k + 1)]) {
                v[idx(k + 1)]
            } else {
                v[idx(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx(k)] = x;
            if x >= n && y >= m {
                trace.push(v[idx(-d)..=idx(d)].to_vec());
                return Some(backtrack(n, m, &trace));
            }
        }
        trace.push(v[idx(-d)..=idx(d)].to_vec());
    }
    None
}

fn backtrack(n: isize, m: isize, trace: &[Vec<isize>]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..trace.len()).rev() {
        let d = d as isize;
        let prev = &trace[(d - 1) as usize];
        let at = |k: isize| prev[(k + d - 1) as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = at(prev_k);
        let prev_y = prev_x - prev_k;
        // The edit moves from (prev_x, prev_y) to `mid`; then a diagonal of equal lines.
        let mid_x = if prev_k == k + 1 { prev_x } else { prev_x + 1 };
        while x > mid_x && x > 0 && y > 0 {
            x -= 1;
            y -= 1;
            pairs.push((x as usize, y as usize));
        }
        x = prev_x;
        y = prev_y;
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        pairs.push((x as usize, y as usize));
    }
    pairs
}

/// `true` when the first 8 KB contain a NUL byte.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8192)].contains(&0)
}

/// Decodes bytes as (lossy) UTF-8, stripping the BOM and normalizing CRLF.
pub fn decode(bytes: &[u8]) -> Text {
    let text = String::from_utf8_lossy(bytes);
    let (bom, text) = match text.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, text.as_ref()),
    };
    let bytes = text.as_bytes();
    let endings: Vec<bool> = bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .map(|(i, _)| i > 0 && bytes[i - 1] == b'\r')
        .collect();
    let crlf_count = endings.iter().filter(|e| **e).count();
    let lf_count = endings.len();
    let crlf = crlf_count > 0 && crlf_count * 2 >= lf_count;
    let mixed = (crlf_count > 0 && crlf_count < lf_count).then_some(endings);
    Text {
        bom,
        crlf,
        mixed,
        body: normalize_newlines(text),
    }
}

/// Turns `\r\n` into `\n`.
pub fn normalize_newlines(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n")
    } else {
        text.to_owned()
    }
}

/// Splits an LF body into lines; a trailing newline does not start an extra line.
pub fn split_lines(body: &str) -> Vec<&str> {
    if body.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = body.split('\n').collect();
    if body.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Truncates to at most `max` chars (on a char boundary).
pub fn cut_chars(text: &str, max: usize) -> (&str, bool) {
    match text.char_indices().nth(max) {
        Some((index, _)) => (&text[..index], true),
        None => (text, false),
    }
}

/// Reads a file for editing. `Ok(None)` if it does not exist.
pub fn read_for_edit(path: &Path) -> Result<Option<Text>, String> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot access {}: {err}", to_unix(path))),
    };
    if meta.is_dir() {
        return Err(format!("{} is a directory, not a file", to_unix(path)));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} is too large to edit ({} bytes); use write to replace it",
            to_unix(path),
            meta.len()
        ));
    }
    let bytes = fs::read(path).map_err(|err| format!("cannot read {}: {err}", to_unix(path)))?;
    if is_binary(&bytes) {
        return Err(format!(
            "{} is a binary file and cannot be edited as text",
            to_unix(path)
        ));
    }
    if std::str::from_utf8(&bytes).is_err() {
        return Err(format!(
            "{} is not valid UTF-8; editing it would corrupt it. Use write to replace the whole file if that is intended.",
            to_unix(path)
        ));
    }
    Ok(Some(decode(&bytes)))
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `data` via a temp file in the same directory and a rename, creating parent dirs.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let display = to_unix(path);
    if path.is_dir() {
        return Err(format!("{display} is a directory, not a file"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("{display} has no parent directory"))?;
    fs::create_dir_all(parent)
        .map_err(|err| format!("cannot create directory {}: {err}", to_unix(parent)))?;
    if let Ok(meta) = fs::metadata(path)
        && meta.permissions().readonly()
    {
        return Err(format!("{display} is read-only"));
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let nonce = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{name}.{}-{nonce}.dd-tmp", std::process::id()));
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(data)?;
            file.sync_all()
        });
    if let Err(err) = written {
        let _ = fs::remove_file(&tmp);
        return Err(format!("cannot write {display}: {err}"));
    }
    // Editors, indexers or antivirus can briefly hold the target open; retry, then fall back.
    let mut last = None;
    for attempt in 0..8 {
        match fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(err) => last = Some(err),
        }
        std::thread::sleep(Duration::from_millis(25 * (attempt + 1)));
    }
    let _ = fs::remove_file(&tmp);
    fs::write(path, data).map_err(|err| {
        format!(
            "cannot write {display}: {err} (rename failed: {})",
            last.map(|e| e.to_string()).unwrap_or_default()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_and_encode_roundtrip() {
        let raw = "\u{feff}a\r\nb\r\n";
        let text = decode(raw.as_bytes());
        assert!(text.bom && text.crlf);
        assert_eq!(text.body, "a\nb\n");
        assert_eq!(text.encode(&text.body), raw.as_bytes());
        let lf = decode(b"a\nb");
        assert!(!lf.bom && !lf.crlf);
        assert_eq!(lf.encode("x\ny"), b"x\ny");
    }

    #[test]
    fn mixed_endings_roundtrip_unchanged() {
        for raw in [
            "a\r\nb\nc\r\n",
            "a\nb\r\nc",
            "\u{feff}x\r\n\ny\n\r\n",
            "a\r\r\nb\n",
        ] {
            let text = decode(raw.as_bytes());
            assert!(text.mixed.is_some(), "{raw:?}");
            assert_eq!(text.encode(&text.body), raw.as_bytes(), "{raw:?}");
        }
        assert!(decode(b"a\r\nb\r\n").mixed.is_none());
        assert!(decode(b"a\nb\n").mixed.is_none());
    }

    #[test]
    fn mixed_endings_preserved_for_untouched_lines() {
        // Mostly CRLF; line "two" and "five" are LF.
        let raw = "one\r\ntwo\nthree\r\nfour\r\nfive\nsix\r\n";
        let text = decode(raw.as_bytes());
        assert!(text.crlf);
        // Change "three", insert a line after "four", delete "six".
        let after = "one\ntwo\nTHREE\nfour\nnew\nfive\n";
        assert_eq!(
            String::from_utf8(text.encode(after)).unwrap(),
            "one\r\ntwo\nTHREE\r\nfour\r\nnew\r\nfive\n"
        );
        // Mostly LF; the CRLF line keeps CRLF after edits on both sides of it.
        let raw = "a\nb\nc\r\nd\ne\n";
        let text = decode(raw.as_bytes());
        assert!(!text.crlf);
        assert_eq!(
            String::from_utf8(text.encode("A\nb\nc\nd\nE\nf\n")).unwrap(),
            "A\nb\nc\r\nd\nE\nf\n"
        );
        // Duplicate lines: the diff keeps the right one.
        let raw = "x\r\nx\nx\r\n";
        let text = decode(raw.as_bytes());
        assert_eq!(
            String::from_utf8(text.encode("x\nx\ny\nx\n")).unwrap(),
            "x\r\nx\ny\r\nx\r\n"
        );
        // Last line without a newline gaining one gets the dominant style.
        let text = decode(b"a\r\nb\nc");
        assert_eq!(text.encode("a\nb\nc\nd"), b"a\r\nb\nc\r\nd");
    }

    #[test]
    fn myers_matches_lcs() {
        let a = ["a", "b", "c", "a", "b", "b", "a"];
        let b = ["c", "b", "a", "b", "a", "c"];
        let pairs = myers(&a, &b, 100).unwrap();
        // LCS length of the classic example is 4.
        assert_eq!(pairs.len(), 4);
        for (i, j) in &pairs {
            assert_eq!(a[*i], b[*j]);
        }
        let mut sorted = pairs.clone();
        sorted.sort();
        assert!(sorted.windows(2).all(|w| w[0].1 < w[1].1));
        assert_eq!(myers(&[], &[], 10).unwrap(), vec![]);
        assert_eq!(myers(&["x"], &[], 10).unwrap(), vec![]);
        assert!(myers(&["a", "b", "c"], &["d", "e", "f"], 2).is_none());
        // Over the limit: changed region falls back to the dominant style, rest kept.
        let old: Vec<String> = (0..3000).map(|i| format!("l{i}")).collect();
        let new: Vec<String> = (0..3000).map(|i| format!("n{i}")).collect();
        let old: Vec<&str> = old.iter().map(String::as_str).collect();
        let new: Vec<&str> = new.iter().map(String::as_str).collect();
        assert!(match_lines(&old, &new).iter().all(Option::is_none));
    }

    #[test]
    fn lines() {
        assert_eq!(split_lines(""), Vec::<&str>::new());
        assert_eq!(split_lines("a"), vec!["a"]);
        assert_eq!(split_lines("a\n"), vec!["a"]);
        assert_eq!(split_lines("a\n\n"), vec!["a", ""]);
    }

    #[test]
    fn atomic_write_creates_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b.txt");
        atomic_write(&path, b"one").unwrap();
        atomic_write(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }
}
