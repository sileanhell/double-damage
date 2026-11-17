//! Paths where writing can lead to code execution, so the UI asks the user even when edits
//! are otherwise applied without asking.

use std::{
    ffi::OsStr,
    path::{Component, Path},
};

use super::paths::{Workspace, same_name, to_unix};

/// Locations where a write can make code run later (hooks, task/launch configs, tool
/// configs that name programs to run, CI). Matched against the components of the *resolved*
/// path (so `..`, symlinks/junctions and 8.3 names cannot hide them), at any depth, comparing
/// names case-insensitively the way NTFS does:
///
/// - `name/` (and `a/b/`): a folder; the folder itself and everything below it match;
/// - `name`: a file name; matches as the last component only.
///
/// Why each:
/// - `.git/` — hooks, `config` (`core.fsmonitor`, `core.sshCommand`, aliases, filters); also a
///   `.git` *file* (`gitdir:` redirection).
/// - `.vscode/`, `.idea/`, `.devcontainer/` — editor tasks, launch configs, `settings.json`
///   pointing at interpreters/formatters, dev container commands.
/// - `.husky/`, `.githooks/` — git hook folders used via `core.hooksPath`.
/// - `.github/workflows/` — CI runs with repository secrets.
/// - `.cargo/` — `config.toml` can set the linker, `runner`, `rustc-wrapper`.
/// - `.claude/` — agent settings can define hooks that run shell commands.
/// - `.envrc` — direnv runs it on `cd`.
/// - `.gitattributes` — selects filter/diff drivers (run as programs).
/// - `.gitmodules` — submodule URLs/commands fetched by `git submodule update`.
/// - `.npmrc` (`script-shell`, `node-options`), `.yarnrc` / `.yarnrc.yml` (`yarnPath`).
pub const SENSITIVE: &[&str] = &[
    ".git/",
    ".vscode/",
    ".idea/",
    ".devcontainer/",
    ".husky/",
    ".githooks/",
    ".github/workflows/",
    ".cargo/",
    ".claude/",
    ".envrc",
    ".gitattributes",
    ".gitmodules",
    ".npmrc",
    ".yarnrc",
    ".yarnrc.yml",
];

/// Whether a resolved absolute path points into a [`SENSITIVE`] location.
pub fn is_sensitive(path: &Path) -> bool {
    let names: Vec<&OsStr> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    SENSITIVE.iter().any(|pattern| {
        let folder = pattern.ends_with('/');
        let seq: Vec<&OsStr> = pattern
            .split('/')
            .filter(|s| !s.is_empty())
            .map(OsStr::new)
            .collect();
        if seq.is_empty() || seq.len() > names.len() {
            return false;
        }
        (0..=names.len() - seq.len()).any(|start| {
            (folder || start + seq.len() == names.len())
                && seq
                    .iter()
                    .zip(&names[start..])
                    .all(|(want, have)| same_name(want, have, true))
        })
    })
}

/// The unix spellings of those `paths` (resolved like the tools do) that are sensitive.
/// Paths that do not resolve (outside the workspace, invalid) are ignored: the tool call
/// will fail on them anyway.
pub fn filter(ws: &Workspace, paths: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in paths {
        let Ok(resolved) = ws.resolve(raw) else {
            continue;
        };
        if is_sensitive(&resolved) {
            let unix = to_unix(&resolved);
            if !out.contains(&unix) {
                out.push(unix);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        for p in [
            r"C:\r\.git",
            r"C:\r\.GIT\hooks\pre-commit",
            r"C:\r\a\.vscode\tasks.json",
            r"C:\r\.github\Workflows\x.yml",
            r"C:\r\.github\workflows",
            r"C:\r\sub\.envrc",
            r"C:\r\.Gitattributes",
            r"\\srv\share\r\.husky\pre-push",
            r"C:\Users\me\.vscode\extensions\x\main.js",
        ] {
            assert!(is_sensitive(Path::new(p)), "{p}");
        }
        for p in [
            r"C:\r\src\a.rs",
            r"C:\r\.github\ISSUE_TEMPLATE.md",
            r"C:\r\.github",
            r"C:\r\workflows\x.yml",
            r"C:\r\.envrc\x",
            r"C:\r\.envrc.example",
            r"C:\r\.gitignore",
            r"C:\r\my.git\x",
        ] {
            assert!(!is_sensitive(Path::new(p)), "{p}");
        }
    }
}
