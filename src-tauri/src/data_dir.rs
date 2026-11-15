//! The app data folder: settings, the custom background, models, llama-server and the webview
//! profile. It can be changed from settings without restarting.
//!
//! - Default: the folder `dd.exe` runs from, wherever the exe is moved.
//!   In dev that is `src-tauri/target/debug`.
//! - Custom: the `DD_DATA_DIR` environment variable. Picking a folder in settings saves it as a
//!   user variable; a value set for the process takes its place, as environment variables do.
//!
//! WebView2 keeps a profile locked for as long as the process runs, so the profile can't be
//! moved: everything worth keeping lives in plain files instead (see `store.rs`), the new window
//! starts a fresh profile, and the old one is deleted on the next launch.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::Serialize;
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, State};
use tauri_plugin_dialog::DialogExt;

use crate::tasks::err_str;

/// Environment variable holding the custom folder.
const VAR: &str = "DD_DATA_DIR";
/// Profiles left behind by switches (kept in the new data folder), deleted on the next launch.
const STALE_FILE: &str = "stale-webviews.json";

pub const WEBVIEW_DIR: &str = "webview";
/// What a switch carries over; the exe and unrelated files next to it are never touched.
const ENTRIES: [&str; 4] = [crate::store::FILE, crate::background::DIR, "models", "llama-server"];

/// Current data folder (managed state). Changes at runtime when the user picks another one.
pub struct DataDir(RwLock<PathBuf>);

impl DataDir {
    pub fn new(path: PathBuf) -> Self {
        Self(RwLock::new(path))
    }

    pub fn get(&self) -> PathBuf {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set(&self, path: PathBuf) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = path;
    }
}

/// True while the main window is being recreated for a new data folder; the app must not
/// exit when that window closes.
pub static SWITCHING: AtomicBool = AtomicBool::new(false);

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn default_dir() -> PathBuf {
    exe_dir()
}

// --- custom folder: the DD_DATA_DIR environment variable -------------------------

fn custom_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os(VAR).filter(|dir| !dir.is_empty())?);
    // A relative value is taken relative to the exe folder.
    Some(if dir.is_absolute() { dir } else { exe_dir().join(dir) })
}

/// Saves the custom folder as a user environment variable (`None` removes it).
fn set_custom_dir(path: Option<&Path>) -> Result<(), String> {
    use winreg::{RegKey, enums::*};
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)
        .map_err(err_str)?;
    match path {
        Some(path) => key.set_value(VAR, &path.to_string_lossy().as_ref()).map_err(err_str)?,
        None => {
            if let Err(error) = key.delete_value(VAR)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return Err(err_str(error));
            }
        }
    }
    broadcast_environment_change();
    Ok(())
}

/// Tells Explorer and other apps that the user environment changed (like `setx` does).
fn broadcast_environment_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };
    let name: Vec<u16> = "Environment".encode_utf16().chain([0]).collect();
    // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call.
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            name.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            2000,
            std::ptr::null_mut(),
        );
    }
}

// --- stale webview profiles ---------------------------------------------------

fn read_stale(dir: &Path) -> Vec<PathBuf> {
    fs::read(dir.join(STALE_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_stale(dir: &Path, stale: &[PathBuf]) {
    let path = dir.join(STALE_FILE);
    if stale.is_empty() {
        let _ = fs::remove_file(path);
    } else if let Ok(json) = serde_json::to_vec_pretty(stale) {
        let _ = fs::write(path, json);
    }
}

/// Picks the data folder for this launch and deletes profiles abandoned by earlier switches.
/// Falls back to the default folder if the custom one can't be created.
pub fn resolve() -> PathBuf {
    let mut dir = custom_dir().unwrap_or_else(default_dir);
    if fs::create_dir_all(&dir).is_err() {
        dir = default_dir();
        let _ = fs::create_dir_all(&dir);
    }

    let active = dir.join(WEBVIEW_DIR);
    let mut stale = read_stale(&dir);
    stale.retain(|old| !same_path(old, &active) && fs::remove_dir_all(old).is_err() && old.exists());
    write_stale(&dir, &stale);
    dir
}

// --- moving -------------------------------------------------------------------

const MOVE_TIMEOUT: Duration = Duration::from_secs(5);
const MOVE_RETRY: Duration = Duration::from_millis(200);
/// Windows `ERROR_NOT_SAME_DEVICE`: rename can't cross volumes.
const NOT_SAME_DEVICE: i32 = 17;

fn move_entries(from: &Path, to: &Path) {
    for entry in ENTRIES {
        let src = from.join(entry);
        if src.exists()
            && let Err(error) = move_entry(&src, &to.join(entry), entry == crate::store::FILE)
        {
            eprintln!("failed to move {}: {error}", src.display());
        }
    }
}

/// Moves a file or folder. Folders that already exist at the target are merged item by item;
/// an existing file is kept unless `overwrite` (the current settings always win).
fn move_entry(src: &Path, dst: &Path, overwrite: bool) -> std::io::Result<()> {
    if src.is_dir() && dst.is_dir() {
        for child in fs::read_dir(src)? {
            let child = child?;
            move_entry(&child.path(), &dst.join(child.file_name()), overwrite)?;
        }
        let _ = fs::remove_dir(src); // only succeeds once it is empty
        return Ok(());
    }
    if dst.exists() {
        if !overwrite {
            return Ok(());
        }
        fs::remove_file(dst)?;
    }

    // Short retries cover files still held for a moment (e.g. by an antivirus scan).
    let deadline = Instant::now() + MOVE_TIMEOUT;
    loop {
        let result = match fs::rename(src, dst) {
            Err(error) if error.raw_os_error() == Some(NOT_SAME_DEVICE) => copy_then_remove(src, dst),
            other => other,
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(MOVE_RETRY),
        }
    }
}

fn copy_then_remove(src: &Path, dst: &Path) -> std::io::Result<()> {
    let copied = if src.is_dir() { copy_dir(src, dst) } else { fs::copy(src, dst).map(drop) };
    match copied {
        Ok(()) => {
            // The copy is complete; a leftover original is only wasted space.
            let _ = if src.is_dir() { fs::remove_dir_all(src) } else { fs::remove_file(src) };
            Ok(())
        }
        Err(error) => {
            let _ = if dst.is_dir() { fs::remove_dir_all(dst) } else { fs::remove_file(dst) };
            Err(error)
        }
    }
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn normalize(path: &Path) -> PathBuf {
    canonical(path).unwrap_or_else(|| path.to_path_buf())
}

/// Canonical path without the `\\?\` prefix Windows adds, so it can be compared and shown.
fn canonical(path: &Path) -> Option<PathBuf> {
    let canonical = fs::canonicalize(path).ok()?;
    let text = canonical.to_string_lossy();
    Some(PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()))
}

fn same_path(a: &Path, b: &Path) -> bool {
    normalize(a) == normalize(b)
}

fn display(path: &Path) -> String {
    normalize(path).to_string_lossy().into_owned()
}

// --- commands ------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataDirInfo {
    path: String,
    /// True when the user picked the folder; false when it is the one next to the exe.
    is_custom: bool,
}

#[tauri::command]
pub fn data_dir_info(dir: State<'_, DataDir>) -> DataDirInfo {
    let current = dir.get();
    DataDirInfo {
        path: display(&current),
        is_custom: !same_path(&current, &default_dir()),
    }
}

/// Native folder picker; `None` when dismissed.
#[tauri::command]
pub async fn data_dir_pick(app: AppHandle, title: String) -> Result<Option<String>, String> {
    let start = app.state::<DataDir>().get();
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app.dialog().file().set_title(title).set_directory(start);
        if let Some(window) = app.get_webview_window("main") {
            dialog = dialog.set_parent(&window);
        }
        dialog.blocking_pick_folder()
    })
    .await
    .map_err(err_str)?
    .map(|picked| picked.into_path().map(|path| display(&path)).map_err(err_str))
    .transpose()
}

/// Moves all app data to `path` (`None` = drop the custom folder and go back to the one next to
/// the exe) without restarting: the main window is closed, the data moved, and a new window opens
/// on the new folder in the same place. The calling page goes away with the old window, so on
/// success nothing is left to receive the result.
#[tauri::command]
pub async fn data_dir_apply(app: AppHandle, path: Option<String>) -> Result<(), String> {
    let current = app.state::<DataDir>().get();
    let custom = path.map(PathBuf::from);
    let target = custom.clone().unwrap_or_else(default_dir);
    if !target.is_absolute() {
        return Err("path must be absolute".into());
    }
    if same_path(&target, &current) {
        // Same place (e.g. a custom folder that is the exe folder): only the setting changes.
        return set_custom_dir(custom.as_deref());
    }

    fs::create_dir_all(&target).map_err(err_str)?;
    let probe = target.join(".dd-write-test");
    fs::write(&probe, b"").map_err(|e| format!("folder is not writable: {e}"))?;
    let _ = fs::remove_file(probe);

    let target_norm = normalize(&target);
    if ENTRIES
        .iter()
        .chain([&WEBVIEW_DIR])
        .any(|entry| target_norm.starts_with(normalize(&current.join(entry))))
    {
        return Err("folder is inside the current data folder".into());
    }

    set_custom_dir(custom.as_deref())?;

    SWITCHING.store(true, Ordering::SeqCst);
    let geometry = app.get_webview_window("main").map(|window| {
        let geometry = WindowGeometry::of(&window);
        let _ = window.destroy();
        geometry
    });
    // Destruction finishes on the event loop; the label must be free before it is reused.
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.get_webview_window("main").is_some() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The old window is gone by now, so never bail out before a new one is opened.
    let (from, to) = (current, target.clone());
    let moved = tauri::async_runtime::spawn_blocking(move || {
        move_entries(&from, &to);
        // The old profile stays locked until exit; the new folder remembers to delete it.
        let mut stale = read_stale(&to);
        stale.extend(read_stale(&from));
        let _ = fs::remove_file(from.join(STALE_FILE));
        stale.push(from.join(WEBVIEW_DIR));
        let active = to.join(WEBVIEW_DIR);
        stale.retain(|old| !same_path(old, &active));
        write_stale(&to, &stale);
    });
    if let Err(error) = moved.await {
        eprintln!("moving app data failed: {error}");
    }

    app.state::<DataDir>().set(target.clone());
    app.state::<crate::store::Store>().reload(&target);
    crate::background::allow(&app, &target);
    let result = crate::create_main_window(&app, &target.join(WEBVIEW_DIR), geometry.flatten());
    SWITCHING.store(false, Ordering::SeqCst);
    result.map_err(err_str)
}

/// Size, position and maximized state, carried over to the recreated window.
pub struct WindowGeometry {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    maximized: bool,
}

impl WindowGeometry {
    fn of(window: &tauri::WebviewWindow) -> Option<Self> {
        Some(Self {
            position: window.outer_position().ok()?,
            size: window.inner_size().ok()?,
            maximized: window.is_maximized().unwrap_or(false),
        })
    }

    pub fn apply(&self, window: &tauri::WebviewWindow) {
        let _ = window.set_size(self.size);
        let _ = window.set_position(self.position);
        if self.maximized {
            let _ = window.maximize();
        }
    }
}
