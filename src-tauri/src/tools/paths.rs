//! Workspace boundary for model-supplied paths.
//!
//! The model is untrusted: every path it sends goes through [`Workspace::resolve`], which
//! accepts MSYS (`/c/x/proj`), Windows (`C:\x\proj`, `C:/x/proj`) and workspace-relative
//! paths and then:
//!
//! 1. normalizes `.`/`..` lexically and validates every component;
//! 2. **lexical gate**: the path must lie under a root *without any filesystem access*.
//!    Everything else (another drive, `\\server\share\…` of a server that is not a root's,
//!    `\\localhost\pipe\…`, `\\evil@80\s\…` WebDAV) is rejected here. This matters because
//!    merely opening a UNC path makes Windows connect to that server and offer the user's
//!    NTLM credentials. Device and verbatim forms (`\\?\`, `\\.\`, `\??\`) are rejected even
//!    earlier, by [`to_windows`];
//! 3. canonicalizes the longest existing prefix (symlinks, junctions, 8.3 names, on-disk
//!    case), never walking above the matched root;
//! 4. re-checks containment against the canonical roots, comparing names **exactly**.
//!
//! Why exact in step 4: a canonical path carries the on-disk spelling of every existing
//! component and the roots are canonical, so a path inside a root starts with exactly the
//! root's names. Any case folding that differs from NTFS's (which compares names through its
//! own upcase table) can be abused: Unicode `to_lowercase` maps U+212A KELVIN SIGN to `k`, so
//! `Des\u{212A}top` "equals" `Desktop` while NTFS sees two different folders, letting a
//! write create a tree outside the workspace. With exact comparison a not-yet-existing,
//! differently spelled folder simply does not match. Only the drive letter and the UNC
//! `server\share` are compared ASCII-case-insensitively (they are not NTFS names; host names
//! are ASCII-case-insensitive, and folding them any further, e.g. `ſ` → `S`, could name a
//! different host).
//!
//! The lexical gate in step 2 is only a filter, so it may be more lenient: it compares folder
//! names with `CompareStringOrdinal(ignoreCase)`, i.e. the OS upcase table NTFS also uses, so
//! a path typed in another case still gets to step 3, which settles it.
//!
//! Known limit: a symlink or junction *inside* the workspace that points to a UNC path is
//! followed in step 3 (the OS opens its target). Creating one needs the bash tool, which can
//! reach the network anyway.

use std::{
    ffi::{OsStr, OsString},
    fs,
    os::windows::ffi::OsStrExt,
    path::{Component, Path, PathBuf, Prefix},
};

use windows::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};

/// Canonicalized workspace roots (without the `\\?\` prefix).
#[derive(Debug, Clone)]
pub struct Workspace {
    /// Canonical roots that exist, in the order the UI sent them.
    roots: Vec<PathBuf>,
    /// The same roots as the UI spelled them (lexically normalized). A path written through a
    /// mapped drive, a symlinked folder or an 8.3 name passes the lexical gate via these.
    spelled: Vec<PathBuf>,
    /// Workspace folders that no longer exist (deleted, moved, unplugged), as spelled.
    missing: Vec<PathBuf>,
    /// The first folder (relative paths are resolved against it) is missing.
    primary_missing: bool,
}

impl Workspace {
    /// Validates and canonicalizes the workspace folders sent by the UI. Folders that no
    /// longer exist are skipped (and named in errors about paths inside them); it is an error
    /// only if none is usable.
    pub fn new(roots: &[String]) -> Result<Self, String> {
        if roots.is_empty() {
            return Err("no workspace folder is open, so file tools are unavailable".into());
        }
        let mut ws = Self {
            roots: Vec::with_capacity(roots.len()),
            spelled: Vec::with_capacity(roots.len()),
            missing: Vec::new(),
            primary_missing: false,
        };
        let mut problems = Vec::new();
        for (index, raw) in roots.iter().enumerate() {
            let path = PathBuf::from(raw.trim());
            if !path.is_absolute() {
                return Err(format!("workspace folder is not an absolute path: {raw}"));
            }
            let spelled = normalize(&path).unwrap_or_else(|_| path.clone());
            // Roots come from the user (folder picker), so touching them here is fine.
            let usable = match fs::metadata(&path) {
                Ok(meta) if meta.is_dir() => fs::canonicalize(&path)
                    .map_err(|err| format!("cannot resolve workspace folder {raw}: {err}")),
                Ok(_) => Err(format!("workspace folder is not a directory: {raw}")),
                Err(err) => Err(format!("workspace folder is not accessible: {raw} ({err})")),
            };
            match usable {
                Ok(canonical) => {
                    ws.roots.push(strip_verbatim(&canonical));
                    ws.spelled.push(spelled);
                }
                Err(problem) => {
                    ws.primary_missing |= index == 0;
                    ws.missing.push(spelled);
                    problems.push(problem);
                }
            }
        }
        if ws.roots.is_empty() {
            return Err(problems.join("; "));
        }
        Ok(ws)
    }

    /// The canonical roots that exist.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Resolves a model-supplied path to an absolute path inside the workspace.
    pub fn resolve(&self, raw: &str) -> Result<PathBuf, String> {
        let text = unquote(raw.trim());
        if text.is_empty() {
            return Err("path is empty".into());
        }
        if text.chars().any(|c| c == '\0' || c.is_control()) {
            return Err(format!("path contains control characters: {text:?}"));
        }
        let windows = to_windows(text)?;
        let joined = if windows.is_absolute() {
            windows
        } else {
            if self.primary_missing {
                return Err(format!(
                    "relative path '{text}' cannot be resolved: the first workspace folder {} no longer exists. Use an absolute path inside {}.",
                    to_unix(&self.missing[0]),
                    self.allowed()
                ));
            }
            self.roots[0].join(windows)
        };
        let normalized = normalize(&joined)?;
        // Lexical gate: nothing outside the roots is ever opened.
        let Some(floor) = self.lexical_floor(&normalized) else {
            return Err(self.outside_error(&normalized));
        };
        let resolved = resolve_existing(&normalized, floor)?;
        if self.contains(&resolved) {
            Ok(resolved)
        } else {
            Err(self.outside_error(&resolved))
        }
    }

    /// Pure lexical check (no filesystem access): if `path` (normalized, absolute) is under a
    /// root as canonicalized or as spelled, the component count of that root.
    fn lexical_floor(&self, path: &Path) -> Option<usize> {
        let key = Key::of(path);
        self.roots
            .iter()
            .chain(&self.spelled)
            .filter_map(|root| {
                let root = Key::of(root);
                key.starts_with(&root, true).then(|| root.depth())
            })
            .min()
    }

    /// Whether a canonicalized absolute path is inside one of the (canonical) roots; folder
    /// names are compared exactly (see the module docs).
    pub fn contains(&self, path: &Path) -> bool {
        let key = Key::of(path);
        self.roots
            .iter()
            .any(|root| key.starts_with(&Key::of(root), false))
    }

    fn allowed(&self) -> String {
        let allowed: Vec<String> = self.roots.iter().map(|root| to_unix(root)).collect();
        allowed.join(", ")
    }

    fn outside_error(&self, path: &Path) -> String {
        let key = Key::of(path);
        if let Some(gone) = self
            .missing
            .iter()
            .find(|gone| key.starts_with(&Key::of(gone), true))
        {
            return format!(
                "{} is in the workspace folder {}, which no longer exists (it was deleted, moved or is unavailable). Only paths inside {} can be accessed.",
                to_unix(path),
                to_unix(gone),
                self.allowed()
            );
        }
        format!(
            "path is outside the workspace: {}. Only paths inside {} can be accessed.",
            to_unix(path),
            self.allowed()
        )
    }
}

/// An absolute path split for comparison: prefix (drive / UNC share, ASCII-lowercased) and
/// the normal components.
struct Key<'a> {
    prefix: Option<OsString>,
    parts: Vec<&'a OsStr>,
}

impl<'a> Key<'a> {
    fn of(path: &'a Path) -> Self {
        let mut prefix = None;
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(p) => prefix = Some(prefix_key(p.kind(), p.as_os_str())),
                Component::Normal(name) => parts.push(name),
                Component::RootDir | Component::CurDir | Component::ParentDir => {}
            }
        }
        Self { prefix, parts }
    }

    /// `Path::components().count()` of the path this key came from (prefix + root + parts).
    fn depth(&self) -> usize {
        self.parts.len() + 2
    }

    fn starts_with(&self, root: &Key<'_>, ignore_case: bool) -> bool {
        self.prefix.is_some()
            && self.prefix == root.prefix
            && self.parts.len() >= root.parts.len()
            && self
                .parts
                .iter()
                .zip(&root.parts)
                .all(|(a, b)| same_name(a, b, ignore_case))
    }
}

fn prefix_key(prefix: Prefix<'_>, raw: &OsStr) -> OsString {
    match prefix {
        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
            OsString::from(format!("{}:", (d as char).to_ascii_lowercase()))
        }
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            let mut key = OsString::from(r"\\");
            key.push(server.to_ascii_lowercase());
            key.push(r"\");
            key.push(share.to_ascii_lowercase());
            key
        }
        // Other device forms never match a normal root; compare them verbatim.
        _ => raw.to_owned(),
    }
}

/// Compares two file names exactly, or case-insensitively the way NTFS does
/// (`CompareStringOrdinal` with `bIgnoreCase`, which uses the OS upcase table).
pub fn same_name(a: &OsStr, b: &OsStr, ignore_case: bool) -> bool {
    if a == b {
        return true;
    }
    if !ignore_case {
        return false;
    }
    let a: Vec<u16> = a.encode_wide().collect();
    let b: Vec<u16> = b.encode_wide().collect();
    // SAFETY: plain call on two valid slices.
    unsafe { CompareStringOrdinal(&a, &b, true) == CSTR_EQUAL }
}

/// The attached file (from `files`, absolute Windows paths the user picked) that a model path
/// names, compared without touching the filesystem. The returned path is the user's, never the
/// model's spelling, so a match can only ever open one of those files.
pub fn match_attached(raw: &str, files: &[String]) -> Option<PathBuf> {
    let text = unquote(raw.trim());
    if text.is_empty() || text.chars().any(|c| c == '\0' || c.is_control()) {
        return None;
    }
    let wanted = normalize(&to_windows(text).ok()?).ok()?;
    if !wanted.is_absolute() {
        return None;
    }
    let key = Key::of(&wanted);
    files.iter().find_map(|file| {
        let file = normalize(Path::new(file)).ok()?;
        let other = Key::of(&file);
        (key.depth() == other.depth() && key.starts_with(&other, true)).then(|| file.clone())
    })
}

/// Convenience wrapper: validates `roots`, then resolves `raw` inside them.
#[allow(dead_code)]
pub fn from_model(roots: &[String], raw: &str) -> Result<PathBuf, String> {
    Workspace::new(roots)?.resolve(raw)
}

/// Formats a Windows path the way MSYS bash shows it: `C:\x\y` → `/c/x/y`,
/// `\\server\share\x` → `//server/share/x`.
pub fn to_unix(path: &Path) -> String {
    let text = strip_verbatim(path).to_string_lossy().into_owned();
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = text[2..].replace('\\', "/");
        let rest = rest.trim_end_matches('/');
        return format!("/{drive}{}", if rest.is_empty() { "/" } else { rest });
    }
    text.replace('\\', "/")
}

/// Removes the `\\?\` / `\\?\UNC\` prefix that `fs::canonicalize` adds.
pub fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            return PathBuf::from(rest);
        }
    }
    path.to_path_buf()
}

fn unquote(text: &str) -> &str {
    for quote in ['"', '\'', '`'] {
        if text.len() >= 2
            && let Some(inner) = text.strip_prefix(quote).and_then(|t| t.strip_suffix(quote))
        {
            return inner.trim();
        }
    }
    text
}

/// Converts MSYS/Unix-style spellings to a Windows path (absolute or relative). Purely
/// lexical.
fn to_windows(text: &str) -> Result<PathBuf, String> {
    let win = text.replace('/', "\\");
    for device in [r"\\?\", r"\\.\", r"\??\"] {
        if win.starts_with(device) {
            return Err(format!(
                "device and verbatim paths ({device}…) are not allowed: {text}. Use a normal path such as /c/Users/…"
            ));
        }
    }
    if win.starts_with(r"\\") {
        // UNC path; allowed only if its server\share is a root's (lexical gate).
        return Ok(PathBuf::from(win));
    }
    if let Some(rest) = win.strip_prefix('\\') {
        // MSYS drive path: \c, \c\..., \cygdrive\c\..., \mnt\c\...
        let rest = rest
            .strip_prefix(r"cygdrive\")
            .or_else(|| rest.strip_prefix(r"mnt\"))
            .unwrap_or(rest);
        let bytes = rest.as_bytes();
        if !bytes.is_empty()
            && bytes[0].is_ascii_alphabetic()
            && (bytes.len() == 1 || bytes[1] == b'\\')
        {
            let drive = (bytes[0] as char).to_ascii_uppercase();
            return Ok(PathBuf::from(format!(
                r"{drive}:\{}",
                rest.get(2..).unwrap_or("")
            )));
        }
        return Err(format!(
            "'{text}' is not a path on this Windows machine. Use /c/… style absolute paths or a path relative to the workspace."
        ));
    }
    let bytes = win.as_bytes();
    if bytes.len() >= 2
        && bytes[1] == b':'
        && bytes[0].is_ascii_alphabetic()
        && bytes.get(2) != Some(&b'\\')
    {
        return Err(format!(
            "drive-relative path '{text}' is ambiguous. Use an absolute path like /c/… or a path relative to the workspace."
        ));
    }
    Ok(PathBuf::from(win))
}

/// Lexically resolves `.` and `..` and validates every component.
fn normalize(path: &Path) -> Result<PathBuf, String> {
    let mut prefix: Option<OsString> = None;
    let mut has_root = false;
    let mut parts: Vec<OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(p) => {
                match p.kind() {
                    Prefix::Disk(_) | Prefix::UNC(..) => {}
                    _ => return Err(format!("unsupported path form: {}", path.display())),
                }
                prefix = Some(p.as_os_str().to_owned());
            }
            Component::RootDir => has_root = true,
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(name) => {
                validate_component(&name.to_string_lossy())?;
                parts.push(name.to_owned());
            }
        }
    }
    let Some(prefix) = prefix.filter(|_| has_root) else {
        return Err(format!("not an absolute path: {}", path.display()));
    };
    let mut out = PathBuf::from(prefix);
    out.push("\\");
    for part in parts {
        out.push(part);
    }
    Ok(out)
}

const RESERVED: [&str; 26] = [
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    "CONIN$", "CONOUT$",
];

fn validate_component(name: &str) -> Result<(), String> {
    if name.contains(':') {
        return Err(format!(
            "'{name}' contains ':' (alternate data streams and drive letters inside a path are not allowed)"
        ));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return Err(format!(
            "'{name}' contains the character '{bad}', which is not allowed in Windows file names"
        ));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(format!(
            "'{name}' ends with a dot or space, which Windows silently strips; choose a different name"
        ));
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    let superscript = ["COM¹", "COM²", "COM³", "LPT¹", "LPT²", "LPT³"];
    if RESERVED.contains(&stem.as_str()) || superscript.iter().any(|s| stem == s.to_uppercase()) {
        return Err(format!(
            "'{name}' is a reserved Windows device name and cannot be used as a file name"
        ));
    }
    Ok(())
}

/// Canonicalizes the longest existing prefix of `path` and re-appends the rest. Never looks
/// at anything with fewer than `floor` components (above the root the path was matched to).
fn resolve_existing(path: &Path, floor: usize) -> Result<PathBuf, String> {
    let mut existing = path.to_path_buf();
    let mut rest: Vec<OsString> = Vec::new();
    while fs::symlink_metadata(&existing).is_err() {
        if existing.components().count() <= floor {
            return Err(format!(
                "the workspace folder {} is not accessible (was it deleted or moved?)",
                to_unix(&existing)
            ));
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_owned());
                existing = parent.to_path_buf();
            }
            _ => return Err(format!("cannot resolve {}", to_unix(path))),
        }
    }
    let canonical = fs::canonicalize(&existing).map_err(|err| {
        format!(
            "cannot resolve {} ({err}); it may be a broken symlink or junction",
            to_unix(&existing)
        )
    })?;
    let mut out = strip_verbatim(&canonical);
    for name in rest.iter().rev() {
        out.push(name);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        ws: Workspace,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = strip_verbatim(&fs::canonicalize(dir.path()).unwrap()).join("proj");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("a.ts"), "x").unwrap();
        let ws = Workspace::new(&[root.to_string_lossy().into_owned()]).unwrap();
        Fixture {
            _dir: dir,
            root,
            ws,
        }
    }

    fn unix(p: &Path) -> String {
        to_unix(p)
    }

    #[test]
    fn attached_files_match_only_themselves() {
        let files = vec![r"C:\Users\Me\Docs\report.pdf".to_string()];
        let hit = PathBuf::from(r"C:\Users\Me\Docs\report.pdf");
        for raw in [
            "/c/Users/Me/Docs/report.pdf",
            "/c/users/me/docs/REPORT.pdf",
            r"C:\Users\Me\Docs\report.pdf",
            "C:/Users/Me/Docs/../Docs/report.pdf",
            "'/c/Users/Me/Docs/report.pdf'",
        ] {
            assert_eq!(match_attached(raw, &files), Some(hit.clone()), "{raw}");
        }
        for raw in [
            "/c/Users/Me/Docs",
            "/c/Users/Me/Docs/report.pdf/x",
            "/c/Users/Me/Docs/other.pdf",
            "report.pdf",
            "//evil.example/share/report.pdf",
            "/c/Users/Me/Docs/report.pdf:stream",
            "",
        ] {
            assert_eq!(match_attached(raw, &files), None, "{raw}");
        }
        assert_eq!(match_attached("/c/Users/Me/Docs/report.pdf", &[]), None);
    }

    #[test]
    fn to_unix_formats() {
        assert_eq!(to_unix(Path::new(r"C:\Users\x\proj")), "/c/Users/x/proj");
        assert_eq!(to_unix(Path::new(r"C:\")), "/c/");
        assert_eq!(to_unix(Path::new(r"\\?\D:\a\b")), "/d/a/b");
        assert_eq!(to_unix(Path::new(r"\\?\UNC\srv\share\a")), "//srv/share/a");
        assert_eq!(to_unix(Path::new(r"\\srv\share\a")), "//srv/share/a");
        assert_eq!(to_unix(Path::new(r"src\a.ts")), "src/a.ts");
    }

    #[test]
    fn accepts_all_spellings() {
        let f = fixture();
        let expected = f.root.join("src").join("a.ts");
        let u = unix(&expected);
        let win = expected.to_string_lossy().into_owned();
        for input in [
            u.clone(),
            win.clone(),
            win.replace('\\', "/"),
            "src/a.ts".into(),
            r"src\a.ts".into(),
            "./src/./a.ts".into(),
            "src/../src/a.ts".into(),
            "src//a.ts".into(),
            format!("\"{u}\""),
            format!("  {u}  "),
            u.to_uppercase().replacen("/C/", "/c/", 1),
            win.to_lowercase(),
            u.replacen("/c/", "/cygdrive/c/", 1),
            win.replace('\\', "/").replacen('/', "\\", 2),
        ] {
            let got =
                f.ws.resolve(&input)
                    .unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(unix(&got).to_lowercase(), u.to_lowercase(), "input {input}");
        }
    }

    #[test]
    fn existing_paths_get_canonical_case() {
        let f = fixture();
        let got = f.ws.resolve("SRC/A.TS").unwrap();
        assert_eq!(got, f.root.join("src").join("a.ts"));
    }

    #[test]
    fn trailing_slash_and_root_itself() {
        let f = fixture();
        assert_eq!(f.ws.resolve("src/").unwrap(), f.root.join("src"));
        assert_eq!(f.ws.resolve(".").unwrap(), f.root);
        assert_eq!(
            f.ws.resolve(&format!("{}/", unix(&f.root))).unwrap(),
            f.root
        );
    }

    #[test]
    fn nonexistent_paths_inside_ok() {
        let f = fixture();
        assert_eq!(
            f.ws.resolve("new/dir/b.ts").unwrap(),
            f.root.join("new").join("dir").join("b.ts")
        );
        assert_eq!(f.ws.resolve("new/../c.ts").unwrap(), f.root.join("c.ts"));
    }

    #[test]
    fn rejects_traversal() {
        let f = fixture();
        for input in [
            "..",
            "../x",
            "src/../../x",
            "src/../../../../../../Windows/System32",
            r"..\proj2\a",
            "/c/Windows/System32/drivers/etc/hosts",
            r"C:\Windows",
            "C:/Windows",
            "/",
            "/tmp/x",
            "/usr/bin/env",
            "/c",
        ] {
            let err = f.ws.resolve(input).unwrap_err();
            assert!(
                err.contains("outside the workspace") || err.contains("not a path on this Windows"),
                "{input}: {err}"
            );
        }
    }

    #[test]
    fn sibling_with_common_prefix_is_outside() {
        let f = fixture();
        let sibling = f.root.with_file_name("proj2");
        fs::create_dir_all(&sibling).unwrap();
        let err = f.ws.resolve(&unix(&sibling.join("a"))).unwrap_err();
        assert!(err.contains("outside the workspace"), "{err}");
        let err = f.ws.resolve("../proj2/a").unwrap_err();
        assert!(err.contains("outside the workspace"), "{err}");
    }

    #[test]
    fn rejects_special_forms() {
        let f = fixture();
        for (input, needle) in [
            (r"\\?\C:\x", "verbatim"),
            (r"\\.\PhysicalDrive0", "verbatim"),
            ("//?/C:/x", "verbatim"),
            ("C:foo", "drive-relative"),
            ("c:", "drive-relative"),
            ("src/a.ts:secret", "alternate data streams"),
            ("src/a.ts::$DATA", "alternate data streams"),
            ("CON", "reserved"),
            ("src/nul.txt", "reserved"),
            ("src/Com1", "reserved"),
            ("lpt9.log", "reserved"),
            ("foo.", "dot or space"),
            ("foo /bar", "dot or space"),
            ("a*b", "not allowed"),
            ("a?b", "not allowed"),
            ("", "empty"),
            ("a\0b", "control"),
        ] {
            let err = f.ws.resolve(input).unwrap_err();
            assert!(err.contains(needle), "{input}: {err}");
        }
        // UNC outside the roots.
        let err = f.ws.resolve(r"\\localhost\c$\Windows").unwrap_err();
        assert!(
            err.contains("outside")
                || err.contains("cannot resolve")
                || err.contains("not allowed"),
            "{err}"
        );
        // Not reserved: names merely starting with a device name.
        assert!(f.ws.resolve("console.log").is_ok());
        assert!(f.ws.resolve("nullable/x").is_ok());
    }

    #[test]
    fn multiple_roots() {
        let f = fixture();
        let other = f.root.with_file_name("other");
        fs::create_dir_all(&other).unwrap();
        let ws = Workspace::new(&[
            f.root.to_string_lossy().into_owned(),
            other.to_string_lossy().into_owned(),
        ])
        .unwrap();
        assert_eq!(
            ws.resolve(&unix(&other.join("x.txt"))).unwrap(),
            other.join("x.txt")
        );
        // Relative paths resolve against the first root.
        assert_eq!(ws.resolve("x.txt").unwrap(), f.root.join("x.txt"));
        assert!(ws.resolve("../other/x.txt").is_ok());
        assert!(ws.resolve("../third/x.txt").is_err());
    }

    #[test]
    fn invalid_roots() {
        assert!(Workspace::new(&[]).is_err());
        assert!(Workspace::new(&["relative".into()]).is_err());
        assert!(Workspace::new(&[r"C:\definitely\missing\dd-root".into()]).is_err());
        let f = fixture();
        let file = f.root.join("src").join("a.ts");
        assert!(
            Workspace::new(&[file.to_string_lossy().into_owned()])
                .unwrap_err()
                .contains("not a directory")
        );
    }

    #[test]
    fn junction_escape_rejected() {
        let f = fixture();
        let outside = f.root.with_file_name("secret");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("key.txt"), "k").unwrap();
        let link = f.root.join("link");
        if std::os::windows::fs::symlink_dir(&outside, &link).is_err() {
            eprintln!("skipping: cannot create directory symlinks without privilege");
            return;
        }
        for input in [
            "link/key.txt",
            "link/new.txt",
            "link",
            "link/sub/deeper.txt",
        ] {
            let err = f.ws.resolve(input).unwrap_err();
            assert!(err.contains("outside the workspace"), "{input}: {err}");
        }
        // A link that points back inside the workspace is fine.
        let inner = f.root.join("inner");
        std::os::windows::fs::symlink_dir(f.root.join("src"), &inner).unwrap();
        assert_eq!(
            f.ws.resolve("inner/a.ts").unwrap(),
            f.root.join("src").join("a.ts")
        );
    }

    #[test]
    fn real_junction_escape_rejected() {
        // Junctions need no privilege; create one with mklink /J.
        let f = fixture();
        let outside = f.root.with_file_name("jsecret");
        fs::create_dir_all(&outside).unwrap();
        let link = f.root.join("junction");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output();
        if !status.is_ok_and(|o| o.status.success()) {
            eprintln!("skipping: mklink /J failed");
            return;
        }
        for input in [
            "junction",
            "junction/x.txt",
            "junction/a/b/c.txt",
            "JUNCTION/../junction/x",
        ] {
            let err = f.ws.resolve(input).unwrap_err();
            assert!(err.contains("outside the workspace"), "{input}: {err}");
        }
    }

    #[test]
    fn dangling_symlink_rejected() {
        let f = fixture();
        let target = f.root.with_file_name("gone");
        let link = f.root.join("dangling");
        if std::os::windows::fs::symlink_file(&target, &link).is_err() {
            eprintln!("skipping: cannot create symlinks without privilege");
            return;
        }
        assert!(f.ws.resolve("dangling").is_err());
    }

    #[test]
    fn from_model_wrapper() {
        let f = fixture();
        let roots = [f.root.to_string_lossy().into_owned()];
        assert_eq!(
            from_model(&roots, "src/a.ts").unwrap(),
            f.root.join("src").join("a.ts")
        );
        assert!(from_model(&roots, "../x").is_err());
    }

    /// Asserts `input` is rejected as outside the workspace, fast enough that no network
    /// connection can have been attempted (a UNC lookup of an unknown host takes seconds).
    fn rejected_without_fs(ws: &Workspace, input: &str) {
        let started = std::time::Instant::now();
        let err = ws.resolve(input).unwrap_err();
        let took = started.elapsed();
        assert!(
            err.contains("outside the workspace")
                || err.contains("not allowed")
                || err.contains("not an absolute path"),
            "{input}: {err}"
        );
        assert!(
            took < std::time::Duration::from_millis(200),
            "{input} took {took:?}"
        );
    }

    #[test]
    fn unc_and_device_paths_rejected_before_any_fs_access() {
        let f = fixture();
        for input in [
            "//nonexistent.invalid/share/x",
            r"\\nonexistent.invalid\share\x",
            "//nonexistent.invalid/share",
            "//evil.example/s/x",
            "//evil@80/s/x",
            "//evil@SSL@443/DavWWWRoot/x",
            r"\\localhost\pipe\x",
            r"\\localhost\c$\Windows",
            r"\\127.0.0.1\c$\x",
            "//?/UNC/nonexistent.invalid/s/x",
            r"\\?\UNC\nonexistent.invalid\s\x",
            r"\\.\pipe\x",
            r"\??\C:\x",
            // `..` cannot climb out of the share into another server.
            "//nonexistent.invalid/share/../../other/x",
        ] {
            rejected_without_fs(&f.ws, input);
        }
        // The lexical gate itself is pure: a foreign share never gets a floor.
        let gate = |p: &str| {
            f.ws.lexical_floor(&normalize(&to_windows(p).unwrap()).unwrap())
        };
        assert!(gate("//nonexistent.invalid/share/x").is_none());
        assert!(gate(&format!("{}/x", unix(&f.root))).is_some());
    }

    #[test]
    fn tools_reject_unc_without_fs_access() {
        let f = fixture();
        let roots = [f.root.to_string_lossy().into_owned()];
        let started = std::time::Instant::now();
        assert!(from_model(&roots, "//nonexistent.invalid/share/x").is_err());
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
    }

    #[test]
    fn unc_server_must_match_root_share() {
        // A (fake) UNC root: the gate passes paths under it only, with ASCII-only folding of
        // server and share; `ſ` (long s) upper-cases to `S` but must not match `s`.
        let ws = Workspace {
            roots: vec![PathBuf::from(r"\\server\share\proj")],
            spelled: vec![],
            missing: vec![],
            primary_missing: false,
        };
        let gate = |p: &str| ws.lexical_floor(&normalize(&to_windows(p).unwrap()).unwrap());
        assert!(gate("//SERVER/Share/proj/x").is_some());
        assert!(gate("//server/share/proj").is_some());
        assert!(gate("//server/share/other/x").is_none());
        assert!(gate("//\u{17F}erver/share/proj/x").is_none());
        assert!(gate("//server/\u{17F}hare/proj/x").is_none());
        assert!(gate("//other/share/proj/x").is_none());
        assert!(gate("//server/share2/proj/x").is_none());
        assert!(gate("/c/proj/x").is_none());
    }

    #[test]
    fn unicode_case_folding_does_not_escape() {
        // Root .../Desktop/proj; writes through look-alike spellings of "Desktop" must not
        // land in a different (new) folder next to it.
        let dir = tempfile::tempdir().unwrap();
        let base = strip_verbatim(&fs::canonicalize(dir.path()).unwrap());
        let mut variants = vec![
            ("Desktop", "Des\u{212A}top"), // KELVIN SIGN; to_lowercase() == "k"
            ("Stra\u{df}e", "STRASSE"),    // ß upper-cases to "SS"
            ("Stra\u{df}e", "strasse"),
            (
                "\u{3bb}\u{3cc}\u{3b3}\u{3bf}\u{3c2}",
                "\u{3bb}\u{3cc}\u{3b3}\u{3bf}\u{3c3}",
            ), // final ς vs σ
            (
                "\u{3bb}\u{3cc}\u{3b3}\u{3bf}\u{3c2}",
                "\u{39b}\u{38c}\u{393}\u{39f}\u{3a3}",
            ), // ΛΌΓΟΣ
        ];
        variants.push(("i\u{307}x", "\u{130}x")); // combining dot vs İ
        for (index, (name, look_alike)) in variants.into_iter().enumerate() {
            let parent = base.join(format!("case-{index}"));
            let root = parent.join(name).join("proj");
            fs::create_dir_all(&root).unwrap();
            let ws = Workspace::new(&[root.to_string_lossy().into_owned()]).unwrap();
            let canonical_root = ws.roots()[0].clone();
            let input = format!("{}/{look_alike}/proj/x/y.txt", unix(&parent));
            match ws.resolve(&input) {
                // NTFS treats the spelling as the same folder: then it resolved to the real one.
                Ok(path) => {
                    assert!(
                        path.starts_with(&canonical_root),
                        "{input} -> {}",
                        path.display()
                    );
                    assert!(fs::symlink_metadata(parent.join(look_alike)).is_ok());
                }
                Err(err) => {
                    assert!(err.contains("outside the workspace"), "{input}: {err}");
                }
            }
            // The Kelvin sign never names the same NTFS folder as "k".
            if look_alike.contains('\u{212A}') {
                assert!(ws.resolve(&input).is_err(), "{input}");
            }
            // Same spelling in other ASCII case works.
            let upper = format!("{}/{}/PROJ/x.txt", unix(&parent), name);
            assert_eq!(ws.resolve(&upper).unwrap(), canonical_root.join("x.txt"));
        }
    }

    #[test]
    fn contains_compares_canonical_names_exactly() {
        let f = fixture();
        assert!(f.ws.contains(&f.root.join("x")));
        let kelvin = PathBuf::from(f.root.to_string_lossy().replace("proj", "pro\u{212A}j"));
        assert!(!f.ws.contains(&kelvin));
        let upper = PathBuf::from(f.root.to_string_lossy().replace("proj", "PROJ"));
        assert!(!f.ws.contains(&upper));
        // Drive letters are not NTFS names; their case does not matter.
        let text = f.root.to_string_lossy().into_owned();
        let other_drive_case = if text.as_bytes()[0].is_ascii_uppercase() {
            text.to_ascii_lowercase()[..1].to_string() + &text[1..]
        } else {
            text.to_ascii_uppercase()[..1].to_string() + &text[1..]
        };
        assert!(f.ws.contains(Path::new(&other_drive_case)));
    }

    #[test]
    fn missing_roots_are_skipped_and_named() {
        let f = fixture();
        let gone = f.root.with_file_name("gone-root");
        let gone_text = gone.to_string_lossy().into_owned();
        let ws =
            Workspace::new(&[f.root.to_string_lossy().into_owned(), gone_text.clone()]).unwrap();
        assert_eq!(ws.roots().len(), 1);
        assert_eq!(
            ws.resolve("src/a.ts").unwrap(),
            f.root.join("src").join("a.ts")
        );
        let err = ws.resolve(&format!("{}/x.txt", unix(&gone))).unwrap_err();
        assert!(
            err.contains("no longer exists") && err.contains("gone-root"),
            "{err}"
        );

        // Missing first root: absolute paths into the other root still work, relative ones
        // explain why they cannot be resolved.
        let ws =
            Workspace::new(&[gone_text.clone(), f.root.to_string_lossy().into_owned()]).unwrap();
        assert!(ws.resolve(&unix(&f.root.join("src/a.ts"))).is_ok());
        let err = ws.resolve("src/a.ts").unwrap_err();
        assert!(err.contains("no longer exists"), "{err}");

        // None left: an error naming the folder.
        let err = Workspace::new(&[gone_text]).unwrap_err();
        assert!(err.contains("gone-root"), "{err}");
    }

    #[test]
    fn root_deleted_after_validation() {
        let f = fixture();
        let other = f.root.with_file_name("short-lived");
        fs::create_dir_all(&other).unwrap();
        let ws = Workspace::new(&[
            f.root.to_string_lossy().into_owned(),
            other.to_string_lossy().into_owned(),
        ])
        .unwrap();
        fs::remove_dir_all(&other).unwrap();
        // Resolution never walks above the root it matched.
        let err = ws.resolve(&unix(&other.join("a/b.txt"))).unwrap_err();
        assert!(err.contains("not accessible"), "{err}");
    }
}
