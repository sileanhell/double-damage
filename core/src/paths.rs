//! Where the core keeps its files.
//!
//! The data folder is, in order: `--data-dir`, the `DD_DATA_DIR` environment variable (relative
//! values are taken from the exe folder), or the folder the exe runs from. The desktop app starts
//! the core with its own data folder, so both see the same models and llama-server install.
//!
//! ```text
//! <data>/
//!   core/credentials.toml   gateway token + provider sign-ins (secrets)
//!   core/config.toml        listen address
//!   core/server.json        written while `serve` runs: where clients find it
//!   core/cache/             fetched model lists
//!   llama-server/           installed llama.cpp build
//!   models/                 GGUF files (downloads land here)
//! ```

use std::path::{Path, PathBuf};

const VAR: &str = "DD_DATA_DIR";

#[derive(Clone, Debug)]
pub struct Paths {
    pub data: PathBuf,
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

impl Paths {
    /// Picks the data folder (see the module docs) and creates it.
    pub fn resolve(explicit: Option<PathBuf>) -> Self {
        let from_env = std::env::var_os(VAR).filter(|v| !v.is_empty()).map(PathBuf::from);
        let dir = explicit.or(from_env).map(|dir| if dir.is_absolute() { dir } else { exe_dir().join(dir) });
        let data = dir.unwrap_or_else(exe_dir);
        let _ = std::fs::create_dir_all(&data);
        Self { data }
    }

    pub fn new(data: impl Into<PathBuf>) -> Self {
        Self { data: data.into() }
    }

    pub fn core_dir(&self) -> PathBuf {
        self.data.join("core")
    }

    pub fn credentials(&self) -> PathBuf {
        self.core_dir().join("credentials.toml")
    }

    pub fn config(&self) -> PathBuf {
        self.core_dir().join("config.toml")
    }

    pub fn server_file(&self) -> PathBuf {
        self.core_dir().join("server.json")
    }

    pub fn cache(&self, name: &str) -> PathBuf {
        self.core_dir().join("cache").join(name)
    }

    pub fn models_dir(&self) -> PathBuf {
        self.data.join("models")
    }

    pub fn llama_dir(&self) -> PathBuf {
        self.data.join("llama-server")
    }
}

/// Writes `bytes` to `path` through a temporary file, so readers never see half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}
