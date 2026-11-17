//! App settings as a flat key → value map in `settings.json` inside the app data folder.
//! The page gets the whole map through an initialization script, so it can read synchronously
//! (like localStorage), and writes changes back with `store_set`. Keeping settings out of the
//! webview profile is what lets the data folder change at runtime: WebView2 keeps its profile
//! locked for as long as the process lives, so the profile is treated as a disposable cache.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use tauri::{AppHandle, Manager, State};

use crate::tasks::err_str;

pub const FILE: &str = "settings.json";

type Map = BTreeMap<String, String>;

pub struct Store(Mutex<Map>);

fn read(dir: &Path) -> Map {
    let bytes = fs::read(dir.join(FILE)).unwrap_or_default();
    // Tolerate a UTF-8 BOM, e.g. after editing the file in Notepad.
    let json = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    serde_json::from_slice(json).unwrap_or_default()
}

fn write(dir: &Path, map: &Map) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(err_str)?;
    let tmp: PathBuf = dir.join(format!("{FILE}.tmp"));
    fs::write(&tmp, serde_json::to_vec_pretty(map).map_err(err_str)?).map_err(err_str)?;
    fs::rename(&tmp, dir.join(FILE)).map_err(err_str)
}

impl Store {
    pub fn load(dir: &Path) -> Self {
        Self(Mutex::new(read(dir)))
    }

    /// Re-reads the settings after the data folder changed.
    pub fn reload(&self, dir: &Path) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = read(dir);
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned()
    }

    /// Sets (or with `None` removes) one key and saves the file in `dir`.
    pub fn set(&self, dir: &Path, key: &str, value: Option<String>) -> Result<(), String> {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match value {
            Some(value) => map.insert(key.to_string(), value),
            None => map.remove(key),
        };
        write(dir, &map)
    }

    /// Script that hands the settings to the page before any of its own code runs.
    pub fn init_script(&self) -> String {
        let map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let json = serde_json::to_string(&*map).unwrap_or_else(|_| "{}".into());
        format!("window.__DD_STORE__ = {json};")
    }
}

/// Applies `changes` (`null` removes a key) and saves the file.
#[tauri::command]
pub fn store_set(
    app: AppHandle,
    store: State<'_, Store>,
    changes: BTreeMap<String, Option<String>>,
) -> Result<(), String> {
    let mut map = store.0.lock().unwrap_or_else(|e| e.into_inner());
    for (key, value) in changes {
        match value {
            Some(value) => map.insert(key, value),
            None => map.remove(&key),
        };
    }
    write(&app.state::<crate::data_dir::DataDir>().get(), &map)
}
