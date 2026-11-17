//! Directory listing, glob and grep over the workspace (gitignore-aware).

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    io,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::SystemTime,
};

use globset::{GlobBuilder, GlobMatcher};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::{WalkBuilder, WalkState};
use serde::Serialize;

use super::{
    paths::{Workspace, to_unix},
    text::cut_chars,
};

const LIST_MAX_ENTRIES: usize = 1000;
const LIST_WALK_CAP: usize = 50_000;
const GLOB_MAX_RESULTS: usize = 500;
const GREP_LINE_CHARS: usize = 300;

fn walker(base: &Path, hidden: bool) -> WalkBuilder {
    let mut builder = WalkBuilder::new(base);
    builder
        .hidden(!hidden)
        .require_git(false)
        .git_global(false)
        .filter_entry(|entry| entry.file_name() != ".git");
    builder
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().min(12))
}

/// Resolves an optional directory/file argument to the list of search bases.
fn bases(ws: &Workspace, path: Option<&str>) -> Result<Vec<PathBuf>, String> {
    match path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => {
            let abs = ws.resolve(p)?;
            if !abs.exists() {
                return Err(format!("{} does not exist", to_unix(&abs)));
            }
            Ok(vec![abs])
        }
        None => Ok(ws.roots().to_vec()),
    }
}

fn rel_slash(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

// ---------------------------------------------------------------- list

struct ListEntry {
    name: String,
    is_dir: bool,
    size: u64,
}

/// Text listing of one directory tree, dirs first, indented by depth.
pub fn list(ws: &Workspace, path: Option<&str>, depth: Option<u32>) -> Result<String, String> {
    let depth = depth.unwrap_or(1).clamp(1, 5) as usize;
    let mut out = String::new();
    for base in bases(ws, path)? {
        if !base.is_dir() {
            return Err(format!(
                "{} is a file, not a directory; use read to view it",
                to_unix(&base)
            ));
        }
        if !out.is_empty() {
            out.push('\n');
        }
        list_one(&base, depth, &mut out);
    }
    Ok(out)
}

fn list_one(base: &Path, depth: usize, out: &mut String) {
    let mut children: BTreeMap<PathBuf, Vec<ListEntry>> = BTreeMap::new();
    let mut seen = 0usize;
    let mut capped = false;
    for entry in walker(base, true).max_depth(Some(depth)).build().flatten() {
        if entry.depth() == 0 {
            continue;
        }
        seen += 1;
        if seen > LIST_WALK_CAP {
            capped = true;
            break;
        }
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        let size = if is_dir {
            0
        } else {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        };
        let parent = entry.path().parent().unwrap_or(base).to_path_buf();
        children.entry(parent).or_default().push(ListEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir,
            size,
        });
    }
    for entries in children.values_mut() {
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
    }
    let _ = writeln!(out, "{}/", to_unix(base).trim_end_matches('/'));
    let total: usize = children.values().map(Vec::len).sum();
    if total == 0 {
        out.push_str("  (empty)\n");
        return;
    }
    let mut printed = 0usize;
    print_tree(base, 1, &children, &mut printed, out);
    if printed < total || capped {
        let more = total - printed;
        let _ = writeln!(
            out,
            "… {more}{} more entries; list a subdirectory or use a smaller depth",
            if capped { "+" } else { "" }
        );
    }
}

fn print_tree(
    dir: &Path,
    level: usize,
    children: &BTreeMap<PathBuf, Vec<ListEntry>>,
    printed: &mut usize,
    out: &mut String,
) {
    let Some(entries) = children.get(dir) else {
        return;
    };
    for entry in entries {
        if *printed >= LIST_MAX_ENTRIES {
            return;
        }
        *printed += 1;
        let indent = "  ".repeat(level);
        if entry.is_dir {
            let _ = writeln!(out, "{indent}{}/", entry.name);
            print_tree(&dir.join(&entry.name), level + 1, children, printed, out);
        } else {
            let _ = writeln!(out, "{indent}{}  ({})", entry.name, human_size(entry.size));
        }
    }
}

// ---------------------------------------------------------------- glob

/// A glob over `/`-separated relative paths; patterns without `/` match file names at any depth.
pub struct PathGlob {
    matcher: GlobMatcher,
    name_only: bool,
}

impl PathGlob {
    pub fn new(pattern: &str) -> Result<Self, String> {
        let pattern = pattern.trim().replace('\\', "/");
        let pattern = pattern.trim_start_matches("./");
        if pattern.is_empty() {
            return Err("glob pattern is empty".into());
        }
        let matcher = GlobBuilder::new(pattern)
            .case_insensitive(true)
            .literal_separator(true)
            .build()
            .map_err(|err| format!("invalid glob pattern: {err}"))?
            .compile_matcher();
        Ok(Self {
            matcher,
            name_only: !pattern.contains('/'),
        })
    }

    pub fn is_match(&self, rel: &str) -> bool {
        if self.name_only {
            self.matcher.is_match(rel.rsplit('/').next().unwrap_or(rel))
        } else {
            self.matcher.is_match(rel)
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobResult {
    pub paths: Vec<String>,
    pub truncated: bool,
}

/// Splits an absolute glob into its literal directory prefix and the pattern remainder.
fn split_absolute(pattern: &str) -> Option<(String, String)> {
    let p = pattern.trim().replace('\\', "/");
    let absolute = p.starts_with('/') || (p.len() >= 2 && p.as_bytes()[1] == b':');
    if !absolute {
        return None;
    }
    let parts: Vec<&str> = p.split('/').collect();
    let first_glob = parts
        .iter()
        .position(|c| c.contains(['*', '?', '[', '{']))?;
    let prefix = parts[..first_glob].join("/");
    let rest = parts[first_glob..].join("/");
    Some((
        if prefix.is_empty() {
            "/".into()
        } else {
            prefix
        },
        rest,
    ))
}

pub fn glob(ws: &Workspace, pattern: &str, path: Option<&str>) -> Result<GlobResult, String> {
    let (bases, pattern) = match split_absolute(pattern) {
        Some((prefix, rest)) => (bases(ws, Some(&prefix))?, rest),
        None => (bases(ws, path)?, pattern.to_owned()),
    };
    let glob = PathGlob::new(&pattern)?;
    let hidden = pattern.starts_with('.') || pattern.contains("/.");
    let found: Mutex<Vec<(SystemTime, String)>> = Mutex::new(Vec::new());
    for base in &bases {
        if !base.is_dir() {
            return Err(format!("{} is not a directory", to_unix(base)));
        }
        walker(base, hidden)
            .threads(threads())
            .build_parallel()
            .run(|| {
                let glob = &glob;
                let found = &found;
                Box::new(move |entry| {
                    let Ok(entry) = entry else {
                        return WalkState::Continue;
                    };
                    if !entry.file_type().is_some_and(|t| t.is_file()) {
                        return WalkState::Continue;
                    }
                    if glob.is_match(&rel_slash(entry.path(), base)) {
                        let modified = entry
                            .metadata()
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .unwrap_or(SystemTime::UNIX_EPOCH);
                        found
                            .lock()
                            .expect("lock")
                            .push((modified, to_unix(entry.path())));
                    }
                    WalkState::Continue
                })
            });
    }
    let mut found = found.into_inner().expect("lock");
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let truncated = found.len() > GLOB_MAX_RESULTS;
    found.truncate(GLOB_MAX_RESULTS);
    Ok(GlobResult {
        paths: found.into_iter().map(|(_, p)| p).collect(),
        truncated,
    })
}

// ---------------------------------------------------------------- grep

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Content,
    Files,
    Count,
}

impl Mode {
    pub fn parse(mode: Option<&str>) -> Result<Self, String> {
        match mode.unwrap_or("files") {
            "content" => Ok(Self::Content),
            "files" | "files_with_matches" => Ok(Self::Files),
            "count" => Ok(Self::Count),
            other => Err(format!(
                "unknown grep mode \"{other}\"; use content, files or count"
            )),
        }
    }
}

pub struct GrepOptions<'a> {
    pub pattern: &'a str,
    pub path: Option<&'a str>,
    pub glob: Option<&'a str>,
    pub ignore_case: bool,
    pub mode: Mode,
    pub context: u32,
    pub max_results: u32,
}

enum Item {
    Line {
        number: u64,
        matched: bool,
        text: String,
    },
    Break,
}

struct FileHits {
    path: PathBuf,
    items: Vec<Item>,
    matches: usize,
}

struct Collect<'a> {
    mode: Mode,
    budget: &'a AtomicUsize,
    items: Vec<Item>,
    matches: usize,
    binary: bool,
}

fn line_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end_matches(['\n', '\r']);
    let (cut, was_cut) = cut_chars(text, GREP_LINE_CHARS);
    if was_cut {
        format!("{cut}…")
    } else {
        cut.to_owned()
    }
}

impl Sink for Collect<'_> {
    type Error = io::Error;

    fn matched(&mut self, _: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, io::Error> {
        if self.mode != Mode::Count {
            // Take one unit of the global result budget (files count once).
            let take = self.mode == Mode::Content || self.matches == 0;
            if take
                && self
                    .budget
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |b| b.checked_sub(1))
                    .is_err()
            {
                return Ok(false);
            }
        }
        self.matches += 1;
        match self.mode {
            Mode::Content => {
                self.items.push(Item::Line {
                    number: mat.line_number().unwrap_or(0),
                    matched: true,
                    text: line_text(mat.bytes()),
                });
                Ok(true)
            }
            Mode::Files => Ok(false),
            Mode::Count => Ok(true),
        }
    }

    fn context(&mut self, _: &Searcher, ctx: &SinkContext<'_>) -> Result<bool, io::Error> {
        if self.mode == Mode::Content {
            self.items.push(Item::Line {
                number: ctx.line_number().unwrap_or(0),
                matched: false,
                text: line_text(ctx.bytes()),
            });
        }
        Ok(true)
    }

    fn context_break(&mut self, _: &Searcher) -> Result<bool, io::Error> {
        if self.mode == Mode::Content && !self.items.is_empty() {
            self.items.push(Item::Break);
        }
        Ok(true)
    }

    fn binary_data(&mut self, _: &Searcher, _: u64) -> Result<bool, io::Error> {
        self.binary = true;
        Ok(false)
    }
}

pub fn grep(ws: &Workspace, opts: &GrepOptions<'_>) -> Result<String, String> {
    let bases = bases(ws, opts.path)?;
    let context = opts.context.min(10) as usize;
    let max_results = opts.max_results.clamp(1, 5000) as usize;
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(opts.ignore_case)
        .multi_line(true)
        .crlf(true)
        .line_terminator(Some(b'\n'))
        .build(opts.pattern)
        .map_err(|err| format!("invalid regex: {err}"))?;
    let glob = opts.glob.map(PathGlob::new).transpose()?;
    let context = if opts.mode == Mode::Content {
        context
    } else {
        0
    };
    let searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .line_number(true)
        .before_context(context)
        .after_context(context)
        .build();
    let budget = AtomicUsize::new(max_results);
    let hits: Mutex<Vec<FileHits>> = Mutex::new(Vec::new());

    let search_file = |searcher: &mut Searcher, path: &Path| {
        if budget.load(Ordering::Relaxed) == 0 {
            return false;
        }
        let mut sink = Collect {
            mode: opts.mode,
            budget: &budget,
            items: Vec::new(),
            matches: 0,
            binary: false,
        };
        if searcher.search_path(&matcher, path, &mut sink).is_ok()
            && sink.matches > 0
            && !sink.binary
        {
            hits.lock().expect("lock").push(FileHits {
                path: path.to_path_buf(),
                items: sink.items,
                matches: sink.matches,
            });
        }
        true
    };

    for base in &bases {
        if base.is_file() {
            search_file(&mut searcher.clone(), base);
            continue;
        }
        walker(base, false)
            .threads(threads())
            .build_parallel()
            .run(|| {
                let mut searcher = searcher.clone();
                let glob = glob.as_ref();
                let search_file = &search_file;
                Box::new(move |entry| {
                    let Ok(entry) = entry else {
                        return WalkState::Continue;
                    };
                    if !entry.file_type().is_some_and(|t| t.is_file()) {
                        return WalkState::Continue;
                    }
                    if let Some(glob) = glob
                        && !glob.is_match(&rel_slash(entry.path(), base))
                    {
                        return WalkState::Continue;
                    }
                    if search_file(&mut searcher, entry.path()) {
                        WalkState::Continue
                    } else {
                        WalkState::Quit
                    }
                })
            });
    }

    let mut hits = hits.into_inner().expect("lock");
    hits.sort_by(|a, b| a.path.cmp(&b.path));
    let truncated = budget.load(Ordering::SeqCst) == 0;
    Ok(format_hits(
        &hits,
        opts.mode,
        context > 0,
        truncated,
        max_results,
    ))
}

fn format_hits(
    hits: &[FileHits],
    mode: Mode,
    with_context: bool,
    truncated: bool,
    max: usize,
) -> String {
    if hits.is_empty() {
        return "No matches found.".into();
    }
    let mut out = String::new();
    for (index, file) in hits.iter().enumerate() {
        let path = to_unix(&file.path);
        match mode {
            Mode::Files => {
                let _ = writeln!(out, "{path}");
            }
            Mode::Count => {
                let _ = writeln!(out, "{path}:{}", file.matches);
            }
            Mode::Content => {
                if with_context && index > 0 {
                    out.push_str("--\n");
                }
                for item in &file.items {
                    match item {
                        Item::Line {
                            number,
                            matched: true,
                            text,
                        } => {
                            let _ = writeln!(out, "{path}:{number}:{text}");
                        }
                        Item::Line {
                            number,
                            matched: false,
                            text,
                        } => {
                            let _ = writeln!(out, "{path}-{number}-{text}");
                        }
                        Item::Break => out.push_str("--\n"),
                    }
                }
            }
        }
    }
    if mode == Mode::Count {
        let total: usize = hits.iter().map(|f| f.matches).sum();
        let _ = writeln!(out, "{total} matches in {} files", hits.len());
    }
    if truncated {
        let unit = if mode == Mode::Content {
            "matching lines"
        } else {
            "files"
        };
        let _ = writeln!(
            out,
            "… stopped after {max} {unit}; narrow the pattern, path or glob to see more"
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let root =
            super::super::paths::strip_verbatim(&fs::canonicalize(dir.path()).unwrap()).join("p");
        fs::create_dir_all(root.join("src/deep")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        fs::write(root.join(".git/config"), "needle\n").unwrap();
        fs::write(root.join("target/out.rs"), "needle\n").unwrap();
        fs::write(
            root.join("src/a.rs"),
            "fn a() {}\n// needle one\nlet x = 1;\n",
        )
        .unwrap();
        fs::write(root.join("src/deep/b.rs"), "one\r\ntwo NEEDLE\r\nthree\r\n").unwrap();
        fs::write(root.join("src/bin.dat"), b"needle\0\0binary").unwrap();
        fs::write(root.join("readme.md"), "# needle\n").unwrap();
        let ws = Workspace::new(&[root.to_string_lossy().into_owned()]).unwrap();
        (dir, root, ws)
    }

    fn opts<'a>(pattern: &'a str, mode: Mode) -> GrepOptions<'a> {
        GrepOptions {
            pattern,
            path: None,
            glob: None,
            ignore_case: false,
            mode,
            context: 0,
            max_results: 200,
        }
    }

    #[test]
    fn list_respects_gitignore() {
        let (_d, _root, ws) = fixture();
        let out = list(&ws, None, Some(3)).unwrap();
        assert!(out.contains("  src/\n    deep/\n      b.rs"), "{out}");
        assert!(!out.contains("target") && !out.contains(".git/"), "{out}");
        assert!(out.contains(".gitignore"), "{out}");
        let shallow = list(&ws, Some("src"), None).unwrap();
        assert!(
            shallow.contains("deep/") && !shallow.contains("b.rs"),
            "{shallow}"
        );
        assert!(
            list(&ws, Some("src/a.rs"), None)
                .unwrap_err()
                .contains("use read")
        );
        assert!(
            list(&ws, Some("../"), None)
                .unwrap_err()
                .contains("outside")
        );
    }

    #[test]
    fn glob_matches() {
        let (_d, root, ws) = fixture();
        let all = glob(&ws, "**/*.rs", None).unwrap();
        assert_eq!(all.paths.len(), 2, "{:?}", all.paths);
        let names = glob(&ws, "*.RS", None).unwrap();
        assert_eq!(names.paths.len(), 2);
        let top = glob(&ws, "src/*.rs", None).unwrap();
        assert_eq!(top.paths.len(), 1);
        let sub = glob(&ws, "*.rs", Some("src/deep")).unwrap();
        assert_eq!(sub.paths.len(), 1);
        let abs = glob(&ws, &format!("{}/src/**/*.rs", to_unix(&root)), None).unwrap();
        assert_eq!(abs.paths.len(), 2);
        assert!(
            glob(&ws, "/c/Windows/*.exe", None)
                .unwrap_err()
                .contains("outside")
        );
        let hidden = glob(&ws, ".gitignore", None).unwrap();
        assert_eq!(hidden.paths.len(), 1);
    }

    #[test]
    fn grep_modes() {
        let (_d, _root, ws) = fixture();
        let files = grep(&ws, &opts("needle", Mode::Files)).unwrap();
        assert_eq!(files.lines().count(), 2, "{files}");
        assert!(files.contains("/src/a.rs") && files.contains("readme.md"));
        let mut o = opts("needle", Mode::Content);
        o.ignore_case = true;
        let content = grep(&ws, &o).unwrap();
        assert!(
            content.contains("/src/deep/b.rs:2:two NEEDLE\n"),
            "{content}"
        );
        assert!(content.contains("/src/a.rs:2:// needle one"), "{content}");
        o.context = 1;
        o.glob = Some("*.rs");
        let ctx = grep(&ws, &o).unwrap();
        assert!(
            ctx.contains("/src/a.rs-1-fn a() {}") && ctx.contains("/src/a.rs-3-let x = 1;"),
            "{ctx}"
        );
        assert!(!ctx.contains("readme"));
        let count = grep(&ws, &opts("e", Mode::Count)).unwrap();
        assert!(count.contains("matches in"), "{count}");
        let mut limited = opts("needle|one", Mode::Content);
        limited.max_results = 1;
        assert!(grep(&ws, &limited).unwrap().contains("stopped after 1"));
        assert_eq!(
            grep(&ws, &opts("zzzz", Mode::Files)).unwrap(),
            "No matches found."
        );
        assert!(
            grep(&ws, &opts("(", Mode::Files))
                .unwrap_err()
                .contains("invalid regex")
        );
        let mut single = opts("two", Mode::Content);
        single.path = Some("src/deep/b.rs");
        assert!(grep(&ws, &single).unwrap().contains(":2:two NEEDLE"));
        let mut eol = opts("two NEEDLE$", Mode::Files);
        eol.path = Some("src");
        assert!(grep(&ws, &eol).unwrap().contains("b.rs"));
    }

    /// Plain `regex` line search, only used to compare against grep-searcher.
    fn grep_plain(base: &Path, pattern: &str, mode: Mode) -> usize {
        let re = regex::bytes::RegexBuilder::new(pattern)
            .multi_line(true)
            .build()
            .unwrap();
        let count = AtomicUsize::new(0);
        walker(base, false)
            .threads(threads())
            .build_parallel()
            .run(|| {
                let re = &re;
                let count = &count;
                Box::new(move |entry| {
                    let Ok(entry) = entry else {
                        return WalkState::Continue;
                    };
                    if !entry.file_type().is_some_and(|t| t.is_file()) {
                        return WalkState::Continue;
                    }
                    let Ok(bytes) = fs::read(entry.path()) else {
                        return WalkState::Continue;
                    };
                    if super::super::text::is_binary(&bytes) {
                        return WalkState::Continue;
                    }
                    match mode {
                        Mode::Files => {
                            if re.is_match(&bytes) {
                                count.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        _ => {
                            for (n, line) in bytes.split(|b| *b == b'\n').enumerate() {
                                if re.is_match(line) {
                                    let _ = (n, line_text(line));
                                    count.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                    WalkState::Continue
                })
            });
        count.into_inner()
    }

    /// `cargo test --release bench_grep -- --ignored --nocapture`
    #[test]
    #[ignore = "benchmark"]
    fn bench_grep() {
        let here = fs::canonicalize(concat!(env!("CARGO_MANIFEST_DIR"), "/..")).unwrap();
        let here = super::super::paths::strip_verbatim(&here);
        let big = PathBuf::from(r"C:\Users\FOSI\Desktop\cli_godbless");
        for base in [here, big] {
            if !base.exists() {
                continue;
            }
            let ws = Workspace::new(&[base.to_string_lossy().into_owned()]).unwrap();
            for (pattern, mode) in [
                ("fn [a-z_]+_test", Mode::Files),
                ("TODO|FIXME", Mode::Content),
                ("(?i)workspace", Mode::Count),
            ] {
                for round in 0..2 {
                    let mut o = opts(pattern, mode);
                    o.max_results = u32::MAX;
                    let start = std::time::Instant::now();
                    let out = grep(&ws, &o).unwrap();
                    let searcher_time = start.elapsed();
                    let start = std::time::Instant::now();
                    let plain = grep_plain(&base, pattern, mode);
                    let plain_time = start.elapsed();
                    println!(
                        "{} {pattern:?} {mode:?} round {round}: grep-searcher {:?} ({} lines out), plain regex {:?} ({plain})",
                        to_unix(&base),
                        searcher_time,
                        out.lines().count(),
                        plain_time
                    );
                }
            }
        }
    }
}
