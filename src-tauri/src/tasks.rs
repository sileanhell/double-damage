//! Shared infrastructure: storage paths, the running-task registry, progress events and HTTP helpers.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

pub const CANCELLED: &str = "cancelled";
pub const ALREADY_RUNNING: &str = "already running";
pub const PROGRESS_EVENT: &str = "task-progress";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

pub fn err_str<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Storage root (the app data folder chosen in settings), created on demand.
pub fn storage_root(app: &AppHandle) -> Result<PathBuf, String> {
    let root = app.state::<crate::data_dir::DataDir>().get();
    std::fs::create_dir_all(&root).map_err(err_str)?;
    Ok(root)
}

/// A subdirectory of the storage root, created on demand.
pub fn storage_subdir(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    let dir = storage_root(app)?.join(name);
    std::fs::create_dir_all(&dir).map_err(err_str)?;
    Ok(dir)
}

/// Removes a file, ignoring errors (used for cleanup).
pub fn remove_file_quiet(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Removes a directory tree, ignoring errors (used for cleanup).
pub fn remove_dir_quiet(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

type TaskMap = Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>;

/// Registry of running tasks: id -> cancellation flag.
#[derive(Default)]
pub struct Tasks(TaskMap);

impl Tasks {
    /// Registers a task; fails with `"already running"` if the id is taken.
    /// The task stays registered until the returned guard is dropped.
    pub fn start(&self, id: &str) -> Result<TaskGuard, String> {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if map.contains_key(id) {
            return Err(ALREADY_RUNNING.to_string());
        }
        let flag = Arc::new(AtomicBool::new(false));
        map.insert(id.to_string(), flag.clone());
        Ok(TaskGuard {
            map: self.0.clone(),
            id: id.to_string(),
            flag,
        })
    }

    pub fn cancel(&self, id: &str) {
        let map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(flag) = map.get(id) {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

pub struct TaskGuard {
    map: TaskMap,
    id: String,
    flag: Arc<AtomicBool>,
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if map.get(&self.id).is_some_and(|f| Arc::ptr_eq(f, &self.flag)) {
            map.remove(&self.id);
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProgressPayload<'a> {
    id: &'a str,
    kind: &'a str,
    label: &'a str,
    received: u64,
    total: Option<u64>,
}

/// Throttled progress reporter + cancellation check for one task.
pub struct Progress {
    app: AppHandle,
    id: String,
    kind: &'static str,
    label: String,
    received: u64,
    total: Option<u64>,
    last_emit: Option<Instant>,
    cancel: Arc<AtomicBool>,
}

impl Progress {
    pub fn new(app: &AppHandle, guard: &TaskGuard, kind: &'static str, label: impl Into<String>) -> Self {
        Self {
            app: app.clone(),
            id: guard.id.clone(),
            kind,
            label: label.into(),
            received: 0,
            total: None,
            last_emit: None,
            cancel: guard.flag.clone(),
        }
    }

    pub fn set_label(&mut self, label: impl Into<String>) {
        self.label = label.into();
    }

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn check_cancel(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::SeqCst) {
            Err(CANCELLED.to_string())
        } else {
            Ok(())
        }
    }

    /// Records progress; emits at most ~10 events per second.
    pub fn update(&mut self, received: u64, total: Option<u64>) {
        self.received = received;
        self.total = total;
        let now = Instant::now();
        if self
            .last_emit
            .is_none_or(|t| now.duration_since(t) >= PROGRESS_INTERVAL)
        {
            self.last_emit = Some(now);
            self.emit();
        }
    }

    /// Unconditionally emits the latest recorded progress.
    pub fn finish(&mut self) {
        self.last_emit = Some(Instant::now());
        self.emit();
    }

    fn emit(&self) {
        let _ = self.app.emit(
            PROGRESS_EVENT,
            ProgressPayload {
                id: &self.id,
                kind: self.kind,
                label: &self.label,
                received: self.received,
                total: self.total,
            },
        );
    }
}

pub fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("double-damage/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .build()
        .map_err(err_str)
}

/// Streams a response body into `path`, reporting `base + bytes written` as progress and
/// checking for cancellation between chunks. Returns the number of bytes written.
/// The caller is responsible for deleting `path` on error.
pub async fn stream_to_file(
    resp: reqwest::Response,
    path: &Path,
    progress: &mut Progress,
    base: u64,
    total: Option<u64>,
) -> Result<u64, String> {
    let file = tokio::fs::File::create(path).await.map_err(err_str)?;
    let mut writer = tokio::io::BufWriter::with_capacity(1 << 20, file);
    let mut stream = resp.bytes_stream();
    let mut written = 0u64;
    progress.update(base, total);
    while let Some(chunk) = stream.next().await {
        progress.check_cancel()?;
        let chunk = chunk.map_err(err_str)?;
        writer.write_all(&chunk).await.map_err(err_str)?;
        written += chunk.len() as u64;
        progress.update(base + written, total);
    }
    progress.check_cancel()?;
    writer.flush().await.map_err(err_str)?;
    let file = writer.into_inner();
    file.sync_all().await.map_err(err_str)?;
    drop(file);
    Ok(written)
}

#[tauri::command]
pub async fn task_cancel(app: AppHandle, id: String) -> Result<(), String> {
    app.state::<Tasks>().cancel(&id);
    Ok(())
}
