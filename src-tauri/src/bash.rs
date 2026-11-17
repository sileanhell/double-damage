//! Bash for the model's `bash` tool: Git for Windows' PortableGit, trimmed to the shell and its
//! command-line utilities (no git, vim, perl, ssh, gpg). Installed on request from Settings,
//! like llama-server. Every command runs in a fresh `bash` in its own Job Object, so it and
//! everything it started are gone once it finishes, times out or is cancelled.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, MutexGuard, atomic::Ordering},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, ipc::Channel};
use tokio::{io::AsyncReadExt, process::Command, sync::Notify};

use crate::tasks::{CANCELLED, Progress, Tasks, err_str, http_client, remove_dir_quiet, storage_root, stream_to_file};

const TASK_ID: &str = "bash";
const TASK_KIND: &str = "bash";
const DIR_NAME: &str = "bash";
const INSTALL_FILE: &str = "install.json";
/// Replaced or uninstalled installs are renamed to `<this><n>` first, then deleted best effort
/// (files of a program still running from them can't be deleted yet; the next install or
/// uninstall retries).
const OLD_PREFIX: &str = ".bash-old-";
const BASH_EXE: &str = "usr/bin/bash.exe";
const LATEST_URL: &str = "https://api.github.com/repos/git-for-windows/git/releases/latest";
const ASSET_PREFIX: &str = "PortableGit-";
const ASSET_SUFFIX: &str = "-64-bit.7z.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// --- install --------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BashStatus {
    installed: bool,
    version: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct InstallInfo {
    version: String,
}

#[derive(Deserialize)]
struct Release {
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    /// "sha256:<hex>", published by GitHub for every asset.
    #[serde(default)]
    digest: Option<String>,
}

impl Asset {
    /// "2.56.1" from "PortableGit-2.56.1-64-bit.7z.exe".
    fn version(&self) -> Option<&str> {
        self.name.strip_prefix(ASSET_PREFIX)?.strip_suffix(ASSET_SUFFIX)
    }
}

fn read_install(dir: &Path) -> Option<InstallInfo> {
    let info: InstallInfo = serde_json::from_slice(&std::fs::read(dir.join(INSTALL_FILE)).ok()?).ok()?;
    dir.join(BASH_EXE).is_file().then_some(info)
}

fn read_status(dir: &Path) -> BashStatus {
    match read_install(dir) {
        Some(info) => BashStatus { installed: true, version: Some(info.version) },
        None => BashStatus { installed: false, version: None },
    }
}

/// Root of the installed environment (the folder with `usr/`), if installed.
fn installed_root(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = storage_root(app)?.join(DIR_NAME);
    read_install(&dir).map(|_| dir).ok_or_else(|| "bash is not installed".into())
}

#[tauri::command]
pub async fn bash_status(app: AppHandle) -> Result<BashStatus, String> {
    let dir = storage_root(&app)?.join(DIR_NAME);
    tauri::async_runtime::spawn_blocking(move || read_status(&dir)).await.map_err(err_str)
}

async fn latest_asset() -> Result<Asset, String> {
    let resp = http_client()?
        .get(LATEST_URL)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(err_str)?
        .error_for_status()
        .map_err(|e| format!("failed to query Git for Windows releases: {e}"))?;
    let release: Release = serde_json::from_slice(&resp.bytes().await.map_err(err_str)?)
        .map_err(|e| format!("unexpected GitHub release response: {e}"))?;
    release
        .assets
        .into_iter()
        .find(|asset| asset.version().is_some())
        .ok_or_else(|| "the latest Git for Windows release has no 64-bit PortableGit".into())
}

/// The newest version available, for the "update" button.
#[tauri::command]
pub async fn bash_latest() -> Result<String, String> {
    let asset = latest_asset().await?;
    Ok(asset.version().unwrap_or_default().to_string())
}

/// A fresh name next to the install to move an old one aside to.
fn aside_dir(root: &Path) -> PathBuf {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    root.join(format!("{OLD_PREFIX}{stamp}"))
}

/// Deletes old installs moved aside earlier (best effort).
fn remove_old_installs(root: &Path) {
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().starts_with(OLD_PREFIX) {
            remove_dir_quiet(&entry.path());
        }
    }
}

const IN_USE: &str = "bash is in use (a program started from it may still be running); close it and try again";

#[tauri::command]
pub async fn bash_uninstall(app: AppHandle, runs: State<'_, Runs>) -> Result<(), String> {
    let root = storage_root(&app)?;
    let dir = root.join(DIR_NAME);
    {
        // No command may run (or start) while the folder moves; renaming is all-or-nothing, so
        // a locked file leaves the install intact instead of half-deleted.
        let _maintenance = runs.maintenance()?;
        match std::fs::rename(&dir, aside_dir(&root)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{IN_USE} ({e})")),
        }
    }
    tauri::async_runtime::spawn_blocking(move || remove_old_installs(&root)).await.map_err(err_str)
}

/// Installs (or updates to) the latest PortableGit, trimmed.
#[tauri::command]
pub async fn bash_install(app: AppHandle) -> Result<BashStatus, String> {
    let guard = app.state::<Tasks>().start(TASK_ID)?;
    let root = storage_root(&app)?;
    let downloads = root.join(".bash-download");
    let tmp_dir = root.join(".bash-new");
    let mut progress = Progress::new(&app, &guard, TASK_KIND, "bash");

    let result = install_inner(&root, &downloads, &tmp_dir, &mut progress, &app.state::<Runs>()).await;
    progress.finish();
    remove_dir_quiet(&downloads);
    if result.is_err() {
        remove_dir_quiet(&tmp_dir);
    }
    drop(guard);
    result
}

async fn install_inner(
    root: &Path,
    downloads: &Path,
    tmp_dir: &Path,
    progress: &mut Progress,
    runs: &Runs,
) -> Result<BashStatus, String> {
    remove_dir_quiet(downloads);
    remove_dir_quiet(tmp_dir);
    remove_old_installs(root);
    tokio::fs::create_dir_all(downloads).await.map_err(err_str)?;

    let asset = latest_asset().await?;
    let version = asset.version().unwrap_or_default().to_string();
    // Never install an unverified download.
    let expected = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|hex| hex.len() == 64)
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "GitHub did not publish a SHA-256 checksum for {} (got {:?}), so the download can't be verified; not installing",
                asset.name, asset.digest
            )
        })?;
    progress.set_label(version.clone());
    progress.check_cancel()?;

    let archive = downloads.join(&asset.name);
    let resp = http_client()?
        .get(&asset.browser_download_url)
        .send()
        .await
        .map_err(err_str)?
        .error_for_status()
        .map_err(|e| format!("failed to download {}: {e}", asset.name))?;
    stream_to_file(resp, &archive, progress, 0, (asset.size > 0).then_some(asset.size)).await?;

    let path = archive.clone();
    let actual = tauri::async_runtime::spawn_blocking(move || sha256_file(&path)).await.map_err(err_str)??;
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(format!("{} is corrupted (checksum mismatch)", asset.name));
    }
    progress.check_cancel()?;

    unpack(&archive, tmp_dir, progress.cancel_flag()).await?;
    progress.check_cancel()?;

    let tmp = tmp_dir.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || trim(&tmp)).await.map_err(err_str)?;
    let json = serde_json::to_vec_pretty(&InstallInfo { version }).map_err(err_str)?;
    tokio::fs::write(tmp_dir.join(INSTALL_FILE), json).await.map_err(err_str)?;
    progress.check_cancel()?;

    let dest = root.join(DIR_NAME);
    {
        // Swap by renames (all-or-nothing) while no command runs or starts.
        let _maintenance = runs.maintenance()?;
        let aside = aside_dir(root);
        let had_old = match std::fs::rename(&dest, &aside) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(format!("{IN_USE} ({e})")),
        };
        if let Err(e) = std::fs::rename(tmp_dir, &dest) {
            if had_old {
                let _ = std::fs::rename(&aside, &dest);
            }
            return Err(format!("failed to put the new bash in place: {e}"));
        }
    }
    let cleanup = root.to_path_buf();
    let _ = tauri::async_runtime::spawn_blocking(move || remove_old_installs(&cleanup)).await;
    Ok(read_status(&dest))
}

/// The download is a 7-Zip self-extractor: a small program with the archive appended. Running it
/// would show its progress window, so the archive part is unpacked here instead.
async fn unpack(archive: &Path, dest: &Path, cancel: Arc<std::sync::atomic::AtomicBool>) -> Result<(), String> {
    let (archive, dest) = (archive.to_path_buf(), dest.to_path_buf());
    tauri::async_runtime::spawn_blocking(move || unpack_blocking(&archive, &dest, &cancel))
        .await
        .map_err(err_str)?
}

fn unpack_blocking(archive: &Path, dest: &Path, cancel: &std::sync::atomic::AtomicBool) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};

    /// The file seen from offset `.1` on (where the self-extractor's program ends).
    struct Offset(std::fs::File, u64);
    impl Read for Offset {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(buf)
        }
    }
    impl Seek for Offset {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            let pos = match pos {
                SeekFrom::Start(start) => SeekFrom::Start(start + self.1),
                other => other,
            };
            Ok(self.0.seek(pos)?.saturating_sub(self.1))
        }
    }

    const SIGNATURE: [u8; 6] = [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut file = std::fs::File::open(archive).map_err(err_str)?;
    let mut head = vec![0; 4 << 20];
    let read = file.read(&mut head).map_err(err_str)?;
    let start = head[..read]
        .windows(SIGNATURE.len())
        .position(|window| window == SIGNATURE)
        .ok_or_else(|| format!("{name} doesn't contain a 7-Zip archive"))? as u64;
    file.seek(SeekFrom::Start(start)).map_err(err_str)?;

    let result = sevenz_rust2::decompress_with_extract_fn(Offset(file, start), dest, |entry, reader, path| {
        if cancel.load(Ordering::SeqCst) {
            return Err(std::io::Error::other(CANCELLED).into());
        }
        sevenz_rust2::default_entry_extract_fn(entry, reader, path)
    });
    match result {
        Ok(()) if dest.join(BASH_EXE).is_file() => Ok(()),
        Ok(()) => Err(format!("{name} doesn't contain bash")),
        Err(_) if cancel.load(Ordering::SeqCst) => Err(CANCELLED.into()),
        Err(error) => Err(format!("unpacking {name} failed: {error}")),
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(err_str)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buf).map_err(err_str)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// What curl (the only tool kept from `ucrt64/`) needs next to it.
const CURL_FILES: [&str; 12] = [
    "curl.exe",
    "libbrotlicommon.dll",
    "libbrotlidec.dll",
    "libcurl-4.dll",
    "libiconv-2.dll",
    "libidn2-0.dll",
    "libintl-8.dll",
    "libpsl-5.dll",
    "libssh2-1.dll",
    "libunistring-5.dll",
    "libzstd.dll",
    "zlib1.dll",
];

/// Parts of `usr/share` that tools read at run time (`file`'s magic, awk's library, terminfo).
const SHARE_KEPT: [&str; 3] = ["misc", "awk", "terminfo"];

/// Programs in `usr/bin` that aren't wanted: git, editors, perl, ssh, gpg, the terminal.
fn dropped_program(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let stem = name.split('.').next().unwrap_or(&name);
    const EXACT: [&str; 12] =
        ["ex", "view", "vim", "vimdiff", "rview", "rvim", "vimtutor", "xxd", "scp", "sftp", "mintty", "tig"];
    const PREFIX: [&str; 9] = ["git", "perl", "cpan", "pod", "ssh", "gpg", "msys-perl", "start-ssh-agent", "nano"];
    EXACT.contains(&stem) || PREFIX.iter().any(|prefix| stem.starts_with(prefix)) || stem == "rnano"
}

/// Cuts the PortableGit tree down to bash and its utilities (~390 MB → ~75 MB). Best effort:
/// anything that can't be removed just stays.
fn trim(root: &Path) {
    let remove = |path: PathBuf| {
        let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
    };
    for name in ["cmd", "bin", "dev", "git-bash.exe", "git-cmd.exe", "README.portable"] {
        remove(root.join(name));
    }
    let entries = |dir: &Path| std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect::<Vec<_>>();
    let name_of = |path: &Path| path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

    let ucrt = root.join("ucrt64");
    for path in entries(&ucrt) {
        if name_of(&path) != "bin" {
            remove(path);
        }
    }
    for path in entries(&ucrt.join("bin")) {
        if !CURL_FILES.iter().any(|keep| keep.eq_ignore_ascii_case(&name_of(&path))) {
            remove(path);
        }
    }

    let usr = root.join("usr");
    for path in entries(&usr.join("share")) {
        if !SHARE_KEPT.contains(&name_of(&path).as_str()) {
            remove(path);
        }
    }
    for name in ["perl5", "ssh", "gnupg", "sasl2", "pkcs11"] {
        remove(usr.join("lib").join(name));
    }
    remove(usr.join("libexec").join("p11-kit"));
    for path in entries(&usr.join("bin")) {
        if path.is_file() && dropped_program(&name_of(&path)) {
            remove(path);
        }
    }
    for name in ["ssh", "vimrc", "gitconfig", "gitattributes", "tigrc", "nanorc", "minttyrc"] {
        remove(root.join("etc").join(name));
    }
}

// --- running commands -----------------------------------------------------------------

pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
pub const MAX_TIMEOUT_MS: u64 = 600_000;
/// What the model gets back: the start and the end of the output, this much of each.
const HEAD_BYTES: usize = 15 * 1024;
const TAIL_BYTES: usize = 15 * 1024;
/// Live output sent to the UI stops after this much (a runaway `yes` mustn't flood it).
const STREAM_LIMIT: usize = 256 * 1024;
const STREAM_INTERVAL: Duration = Duration::from_millis(50);

/// How long a cancel for a not (yet) running id is remembered.
const EARLY_CANCEL_TTL: Duration = Duration::from_secs(60);
const EARLY_CANCEL_MAX: usize = 256;
/// After bash exits, output still arriving (from background processes that escaped the job
/// or kept the pipes open) is read for at most this long.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(2);
/// After killing on timeout/cancel, how long to wait for bash to go before giving up on it.
const KILL_GRACE: Duration = Duration::from_secs(5);

/// The commands running (their cancel signals, by the id the page gave them), cancels that
/// arrived before their command, and whether the install is being replaced.
#[derive(Default)]
pub struct Runs(Mutex<RunsState>);

#[derive(Default)]
struct RunsState {
    running: HashMap<String, Arc<Notify>>,
    /// Ids cancelled before `bash_run` registered them (Stop right after starting); expire
    /// after [`EARLY_CANCEL_TTL`]. Ids are expected to be unique per run.
    early_cancels: HashMap<String, Instant>,
    /// Set while the install folder is moved; no command may start meanwhile.
    maintenance: bool,
}

impl RunsState {
    fn prune(&mut self) {
        self.early_cancels.retain(|_, at| at.elapsed() < EARLY_CANCEL_TTL);
    }
}

impl Runs {
    fn lock(&self) -> MutexGuard<'_, RunsState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Blocks new commands until dropped; fails while any command runs (its files are in use).
    fn maintenance(&self) -> Result<Maintenance<'_>, String> {
        let mut state = self.lock();
        if !state.running.is_empty() {
            return Err("a bash command is running; wait for it to finish (or stop it) and try again".into());
        }
        if state.maintenance {
            return Err("bash is already being installed or removed".into());
        }
        state.maintenance = true;
        Ok(Maintenance(self))
    }
}

struct Maintenance<'a>(&'a Runs);

impl Drop for Maintenance<'_> {
    fn drop(&mut self) {
        self.0.lock().maintenance = false;
    }
}

/// Unregisters a run when dropped.
struct Registered<'a> {
    runs: &'a Runs,
    id: String,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.runs.lock().running.remove(&self.id);
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRequest {
    id: String,
    command: String,
    /// Windows path of the folder to run in.
    cwd: String,
    timeout_ms: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunResult {
    exit_code: Option<i32>,
    timed_out: bool,
    cancelled: bool,
    /// stdout and stderr interleaved; the middle is cut when longer than the limit.
    output: String,
    /// Bytes left out of `output`.
    omitted: u64,
    duration_ms: u64,
}

impl RunResult {
    /// Stop was pressed before the command got going: it never starts.
    fn cancelled_before_start() -> Self {
        Self { exit_code: None, timed_out: false, cancelled: true, output: String::new(), omitted: 0, duration_ms: 0 }
    }
}

/// Start and end of a stream, plus how much was in between.
#[derive(Default)]
struct Capture {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    total: u64,
}

impl Capture {
    fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len() as u64;
        let room = HEAD_BYTES.saturating_sub(self.head.len());
        let (head, rest) = bytes.split_at(room.min(bytes.len()));
        self.head.extend_from_slice(head);
        self.tail.extend(rest);
        let excess = self.tail.len().saturating_sub(TAIL_BYTES);
        self.tail.drain(..excess);
    }

    fn finish(self) -> (String, u64) {
        let omitted = self.total - self.head.len() as u64 - self.tail.len() as u64;
        let mut text = String::from_utf8_lossy(&self.head).into_owned();
        if omitted > 0 {
            text.push_str(&format!("\n\n[… {omitted} bytes of output omitted …]\n\n"));
        }
        let tail: Vec<u8> = self.tail.into_iter().collect();
        text.push_str(&String::from_utf8_lossy(&tail));
        (text, omitted)
    }
}

/// Splits off the longest valid UTF-8 prefix, keeping a cut-off character for the next chunk.
fn take_utf8(pending: &mut Vec<u8>) -> String {
    let valid = match std::str::from_utf8(pending) {
        Ok(_) => pending.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => pending.len(), // really invalid: lossy below
    };
    let rest = pending.split_off(valid);
    let text = String::from_utf8_lossy(pending).into_owned();
    *pending = rest;
    text
}

/// Runs `command` in a fresh bash, streaming its output to `on_output`.
#[tauri::command]
pub async fn bash_run(
    app: AppHandle,
    runs: State<'_, Runs>,
    request: RunRequest,
    on_output: Channel<String>,
) -> Result<RunResult, String> {
    if request.command.trim().is_empty() {
        return Err("empty command".into());
    }
    let timeout = Duration::from_millis(request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).clamp(1_000, MAX_TIMEOUT_MS));

    // Register first: from here on an install/uninstall can't move the folder away.
    let cancel = Arc::new(Notify::new());
    let _registered = {
        let mut state = runs.lock();
        state.prune();
        if state.maintenance {
            return Err("bash is being installed or removed; try again when that has finished".into());
        }
        if state.running.contains_key(&request.id) {
            return Err("a command with this id is already running".into());
        }
        if state.early_cancels.remove(&request.id).is_some() {
            return Ok(RunResult::cancelled_before_start());
        }
        state.running.insert(request.id.clone(), cancel.clone());
        Registered { runs: &runs, id: request.id.clone() }
    };
    let root = installed_root(&app)?;
    let cwd = PathBuf::from(&request.cwd);
    if !cwd.is_absolute() || !cwd.is_dir() {
        return Err(format!("working folder not found: {}", request.cwd));
    }
    run(&root, &cwd, &request.command, timeout, &cancel, &on_output).await
}

/// Stops a command. A cancel for an id that isn't running yet is remembered for a while, so a
/// Stop pressed right after starting isn't lost.
#[tauri::command]
pub fn bash_cancel(runs: State<'_, Runs>, id: String) {
    let mut state = runs.lock();
    if let Some(cancel) = state.running.get(&id) {
        // `notify_one` stores a permit if `run` isn't waiting yet.
        cancel.notify_one();
        return;
    }
    state.prune();
    if state.early_cancels.len() >= EARLY_CANCEL_MAX
        && let Some(oldest) = state.early_cancels.iter().min_by_key(|(_, at)| **at).map(|(id, _)| id.clone())
    {
        state.early_cancels.remove(&oldest);
    }
    state.early_cancels.insert(id, Instant::now());
}

/// Variables of the app's environment that would change how a non-interactive bash starts or
/// parses: `BASH_ENV`/`ENV` are sourced even with `--norc`; `SHELLOPTS`/`BASHOPTS` are
/// imported as options; `CDPATH`, `GLOBIGNORE` and `PROMPT_COMMAND` change `cd`, globbing and
/// run code. Exported functions (`BASH_FUNC_*`) are removed too.
const REMOVED_ENV: [&str; 7] = ["BASH_ENV", "ENV", "SHELLOPTS", "BASHOPTS", "CDPATH", "GLOBIGNORE", "PROMPT_COMMAND"];

fn scrub_environment(cmd: &mut Command) {
    for name in REMOVED_ENV {
        cmd.env_remove(name);
    }
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().to_ascii_uppercase().starts_with("BASH_FUNC_") {
            cmd.env_remove(name);
        }
    }
}

fn environment(root: &Path) -> Vec<(&'static str, String)> {
    let mut path = format!("{};{}", root.join("usr").join("bin").display(), root.join("ucrt64").join("bin").display());
    if let Ok(system) = std::env::var("PATH") {
        path.push(';');
        path.push_str(&system);
    }
    let mut env = vec![
        ("PATH", path),
        ("LANG", "C.UTF-8".into()),
        ("TERM", "dumb".into()),
        // Nothing may wait for a key press: there's no one at the keyboard.
        ("PAGER", "cat".into()),
        ("GIT_PAGER", "cat".into()),
        ("GIT_TERMINAL_PROMPT", "0".into()),
    ];
    if let Ok(home) = std::env::var("USERPROFILE") {
        env.push(("HOME", home));
    }
    env
}

const CREATE_SUSPENDED: u32 = 0x0000_0004;

async fn run(
    root: &Path,
    cwd: &Path,
    command: &str,
    timeout: Duration,
    cancel: &Notify,
    on_output: &Channel<String>,
) -> Result<RunResult, String> {
    let started = Instant::now();
    let mut cmd = Command::new(root.join(BASH_EXE));
    // The command travels in an environment variable: on the command line the MSYS runtime
    // would expand globs and treat a leading `@` as a response file.
    cmd.args(["--noprofile", "--norc", "-c", "__dd_command=$DD_COMMAND; unset DD_COMMAND; eval \"$__dd_command\""]);
    scrub_environment(&mut cmd);
    cmd.env("DD_COMMAND", command)
        .envs(environment(root))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Suspended until it is in its job, so nothing it starts can be outside the job.
        .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED)
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("failed to start bash: {e}"))?;
    // Everything the command starts joins this job; dropping it ends them all. Should the job
    // fail (very unlikely: nested jobs work since Windows 8) the command still runs; then
    // kills reach bash only, and the bounded drain below keeps leftovers from hanging us.
    let tree = crate::gateway::job::Tree::adopt(&child);
    if let Err(e) = crate::gateway::job::resume(&child) {
        let _ = child.start_kill();
        return Err(format!("failed to start bash: {e}"));
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let mut readers = Vec::new();
    for mut pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Unpin + Send>), child.stderr.take().map(|p| Box::new(p) as _)].into_iter().flatten() {
        let tx = tx.clone();
        readers.push(tokio::spawn(async move {
            let mut buf = vec![0; 16 * 1024];
            while let Ok(n) = pipe.read(&mut buf).await {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        }));
    }
    drop(tx);

    let mut capture = Capture::default();
    let mut pending = Vec::new();
    let mut streamed = 0usize;
    let mut last_send = Instant::now();
    let mut send = |pending: &mut Vec<u8>, streamed: &mut usize, force: bool| {
        if pending.is_empty() || *streamed >= STREAM_LIMIT || (!force && last_send.elapsed() < STREAM_INTERVAL) {
            return;
        }
        last_send = Instant::now();
        let text = take_utf8(pending);
        *streamed += text.len();
        let _ = on_output.send(text);
        if *streamed >= STREAM_LIMIT {
            let _ = on_output.send("\n[… live output stopped; the rest is in the result …]\n".into());
        }
    };

    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut status = None;
    let (mut timed_out, mut cancelled) = (false, false);
    let mut pipes_open = true;
    // Once set, we stop waiting at this point (bash exited and the drain is over, or it was
    // killed and didn't go). Only ever moves earlier.
    let mut give_up: Option<tokio::time::Instant> = None;
    let mut gave_up = false;
    let stop_within = |give_up: &mut Option<tokio::time::Instant>, within: Duration| {
        let at = tokio::time::Instant::now() + within;
        *give_up = Some(give_up.map_or(at, |current| current.min(at)));
    };
    while status.is_none() || pipes_open {
        tokio::select! {
            chunk = rx.recv(), if pipes_open => match chunk {
                Some(chunk) => {
                    capture.push(&chunk);
                    if streamed < STREAM_LIMIT {
                        pending.extend_from_slice(&chunk);
                    }
                    send(&mut pending, &mut streamed, false);
                }
                None => pipes_open = false,
            },
            exit = child.wait(), if status.is_none() => {
                status = Some(exit.map_err(err_str)?);
                // A fresh shell per command: what it left running in the background goes too
                // (its open pipes would otherwise keep us waiting).
                if let Some(tree) = &tree { tree.kill(); }
                stop_within(&mut give_up, DRAIN_AFTER_EXIT);
            }
            // Fires once (a completed `Sleep` stays ready).
            _ = &mut deadline, if !timed_out && status.is_none() => {
                timed_out = true;
                kill(&mut child, tree.as_ref());
                stop_within(&mut give_up, KILL_GRACE);
            }
            // Active the whole time, also while draining after bash exited.
            _ = cancel.notified(), if !cancelled => {
                cancelled = true;
                kill(&mut child, tree.as_ref());
                stop_within(&mut give_up, if status.is_some() { Duration::ZERO } else { KILL_GRACE });
            }
            _ = tokio::time::sleep_until(give_up.unwrap_or_else(tokio::time::Instant::now)), if give_up.is_some() => {
                gave_up = true;
                break;
            }
        }
    }
    for reader in &readers {
        reader.abort();
    }
    if gave_up && !cancelled {
        let note: &[u8] = if status.is_some() {
            b"\n[stopped reading output: a background process kept it open after the command finished]\n"
        } else {
            b"\n[gave up waiting: the command did not stop after being killed]\n"
        };
        capture.push(note);
    }
    send(&mut pending, &mut streamed, true);
    let (output, omitted) = capture.finish();
    let exit_code = if timed_out || cancelled { None } else { status.and_then(|s| s.code()) };
    Ok(RunResult { exit_code, timed_out, cancelled, output, omitted, duration_ms: started.elapsed().as_millis() as u64 })
}

fn kill(child: &mut tokio::process::Child, tree: Option<&crate::gateway::job::Tree>) {
    match tree {
        Some(tree) => tree.kill(),
        None => {
            // Don't wait for the exit here: the main loop does, with a bound.
            let _ = child.start_kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_keeps_head_and_tail() {
        let mut capture = Capture::default();
        capture.push(&vec![b'a'; HEAD_BYTES + 10]);
        capture.push(&vec![b'b'; TAIL_BYTES + 5]);
        let (text, omitted) = capture.finish();
        assert_eq!(omitted, 15);
        assert!(text.starts_with('a') && text.ends_with('b'));
        assert!(text.contains("15 bytes of output omitted"));

        let mut small = Capture::default();
        small.push(b"hello");
        assert_eq!(small.finish(), ("hello".to_string(), 0));
    }

    #[test]
    fn utf8_is_split_on_character_boundaries() {
        let mut pending = "при".as_bytes().to_vec();
        pending.pop(); // cut the last character in half
        assert_eq!(take_utf8(&mut pending), "пр");
        assert_eq!(pending.len(), 1);
        pending.push("и".as_bytes()[1]);
        assert_eq!(take_utf8(&mut pending), "и");
        assert!(pending.is_empty());
    }

    /// Needs a (trimmed) PortableGit: `DD_TEST_BASH_ROOT` = the folder with `usr/`.
    #[tokio::test]
    #[ignore]
    async fn runs_commands() {
        let root = PathBuf::from(std::env::var("DD_TEST_BASH_ROOT").expect("DD_TEST_BASH_ROOT"));
        let cwd = std::env::temp_dir();
        let streamed = Arc::new(Mutex::new(String::new()));
        let sink = streamed.clone();
        let channel: Channel<String> = Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(json) = body {
                sink.lock().unwrap().push_str(&serde_json::from_str::<String>(&json).unwrap_or_default());
            }
            Ok(())
        });
        let never = Notify::new();
        let go = |command: &'static str, timeout: u64| {
            let (root, cwd, channel, never) = (&root, &cwd, &channel, &never);
            async move { run(root, cwd, command, Duration::from_millis(timeout), never, channel).await.unwrap() }
        };

        let r = go("echo привет; echo err >&2; pwd; exit 3", 10_000).await;
        println!("basic: {:?} {:?} {}ms", r.exit_code, r.output, r.duration_ms);
        assert_eq!(r.exit_code, Some(3));
        assert!(r.output.contains("привет") && r.output.contains("err"));
        assert!(streamed.lock().unwrap().contains("привет"));

        // Background jobs don't keep the command alive; they're ended with it.
        let t = Instant::now();
        let r = go("sleep 30 & echo started", 10_000).await;
        println!("background: {:?} {}ms", r.output.trim(), t.elapsed().as_millis());
        assert!(t.elapsed() < Duration::from_secs(5));

        let t = Instant::now();
        let r = go("sleep 30; echo never", 1_500).await;
        println!("timeout: timed_out={} {}ms", r.timed_out, t.elapsed().as_millis());
        assert!(r.timed_out && r.exit_code.is_none() && !r.output.contains("never"));
        assert!(t.elapsed() < Duration::from_secs(5));

        let r = go("seq 1 200000", 10_000).await;
        println!("big: omitted={} len={}", r.omitted, r.output.len());
        assert!(r.omitted > 0 && r.output.starts_with("1\n") && r.output.trim_end().ends_with("200000"));

        let cancel = Arc::new(Notify::new());
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            trigger.notify_one();
        });
        let t = Instant::now();
        let r = run(&root, &cwd, "sleep 30", Duration::from_secs(20), &cancel, &channel).await.unwrap();
        println!("cancel: cancelled={} {}ms", r.cancelled, t.elapsed().as_millis());
        assert!(r.cancelled && t.elapsed() < Duration::from_secs(5));

        // A Stop that arrived before the command waited for it still stops it.
        let early = Notify::new();
        early.notify_one();
        let t = Instant::now();
        let r = run(&root, &cwd, "sleep 30", Duration::from_secs(20), &early, &channel).await.unwrap();
        println!("early cancel: cancelled={} {}ms", r.cancelled, t.elapsed().as_millis());
        assert!(r.cancelled && t.elapsed() < Duration::from_secs(5));

        // The timeout fires once; the run ends promptly instead of spinning on the deadline.
        let t = Instant::now();
        let r = go("trap '' TERM; sleep 30", 1_000).await;
        println!("timeout (trap): timed_out={} {}ms", r.timed_out, t.elapsed().as_millis());
        assert!(r.timed_out && t.elapsed() < Duration::from_secs(8));

        // The started process is in its job from its first instruction: a grandchild that
        // detaches (setsid + nohup + disown) still ends with the command.
        let t = Instant::now();
        let r = go("setsid nohup sleep 30 >/dev/null 2>&1 & disown; echo detached", 10_000).await;
        println!("detached: {:?} {}ms", r.output.trim(), t.elapsed().as_millis());
        assert!(r.output.contains("detached") && t.elapsed() < Duration::from_secs(5));

        // The app's environment can't inject startup code or options into bash.
        let script = std::env::temp_dir().join("dd-bash-env-test.sh");
        std::fs::write(&script, "echo PWNED-BASH-ENV\n").unwrap();
        // SAFETY: test-only; nothing else in this test process reads these concurrently.
        unsafe {
            std::env::set_var("BASH_ENV", &script);
            std::env::set_var("ENV", &script);
            std::env::set_var("SHELLOPTS", "xtrace");
            std::env::set_var("BASH_FUNC_echo%%", "() { builtin echo PWNED-FUNC; }");
        }
        let r = go("echo clean; shopt -po xtrace", 10_000).await;
        unsafe {
            for name in ["BASH_ENV", "ENV", "SHELLOPTS", "BASH_FUNC_echo%%"] {
                std::env::remove_var(name);
            }
        }
        let _ = std::fs::remove_file(&script);
        println!("env: {:?}", r.output);
        assert!(r.output.contains("clean") && !r.output.contains("PWNED") && r.output.contains("set +o xtrace"));

        let r = go("command -v curl grep sed awk; echo $HOME; command -v vim || echo no-vim", 10_000).await;
        println!("tools: {}", r.output);
    }

    /// `DD_TEST_PORTABLEGIT_EXE` = a downloaded PortableGit-…-64-bit.7z.exe, `DD_TEST_SHA256` its
    /// published digest. Unpacks into a folder with a space and Cyrillic in its name.
    #[tokio::test]
    #[ignore]
    async fn verifies_and_unpacks() {
        let exe = PathBuf::from(std::env::var("DD_TEST_PORTABLEGIT_EXE").expect("DD_TEST_PORTABLEGIT_EXE"));
        if let Ok(expected) = std::env::var("DD_TEST_SHA256") {
            assert_eq!(sha256_file(&exe).unwrap(), expected);
        }
        let dest = std::env::temp_dir().join("dd bash тест");
        let _ = std::fs::remove_dir_all(&dest);
        let t = Instant::now();
        unpack(&exe, &dest, Arc::new(std::sync::atomic::AtomicBool::new(false))).await.unwrap();
        trim(&dest);
        println!("unpacked + trimmed in {}ms", t.elapsed().as_millis());
        assert!(dest.join(BASH_EXE).is_file());
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// Trims a copy of an unpacked PortableGit in place: `DD_TEST_TRIM_DIR`.
    #[test]
    #[ignore]
    fn trims_a_real_tree() {
        let dir = PathBuf::from(std::env::var("DD_TEST_TRIM_DIR").expect("DD_TEST_TRIM_DIR"));
        trim(&dir);
        assert!(dir.join(BASH_EXE).is_file());
        assert!(dir.join("ucrt64/bin/curl.exe").is_file());
        assert!(!dir.join("ucrt64/bin/git.exe").exists());
        assert!(!dir.join("usr/bin/vim.exe").exists());
    }

    #[test]
    fn trims_unwanted_programs() {
        for name in ["vim.exe", "git.exe", "git-upload-pack.exe", "perl5.42.exe", "ssh-keygen.exe", "gpg.exe", "msys-perl5_42.dll"] {
            assert!(dropped_program(name), "{name}");
        }
        for name in ["bash.exe", "grep.exe", "sed.exe", "gawk.exe", "find.exe", "patch.exe", "less.exe", "msys-2.0.dll"] {
            assert!(!dropped_program(name), "{name}");
        }
    }
}
