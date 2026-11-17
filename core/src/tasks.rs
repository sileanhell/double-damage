//! Long tasks (installs, downloads): a registry of cancellation flags and throttled progress
//! that goes out as `task` events.

use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::{
    events::{Event, Events, TaskProgress},
    util::err_str,
};

pub const CANCELLED: &str = "cancelled";
pub const ALREADY_RUNNING: &str = "already running";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

type TaskMap = Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>;

/// Running tasks: id -> cancellation flag.
#[derive(Clone)]
pub struct Tasks {
    map: TaskMap,
    events: Events,
}

impl Tasks {
    pub fn new(events: Events) -> Self {
        Self { map: Default::default(), events }
    }

    /// Registers a task; fails with `"already running"` if the id is taken. The task stays
    /// registered until the returned progress handle is dropped.
    pub fn start(&self, id: &str, kind: &str, label: impl Into<String>) -> Result<Progress, String> {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if map.contains_key(id) {
            return Err(ALREADY_RUNNING.to_string());
        }
        let flag = Arc::new(AtomicBool::new(false));
        map.insert(id.to_string(), flag.clone());
        Ok(Progress {
            map: self.map.clone(),
            events: self.events.clone(),
            state: TaskProgress {
                id: id.to_string(),
                kind: kind.to_string(),
                label: label.into(),
                received: 0,
                total: None,
                done: false,
                error: None,
            },
            last_emit: None,
            cancel: flag,
            finished: false,
        })
    }

    pub fn cancel(&self, id: &str) -> bool {
        let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.get(id).inspect(|flag| flag.store(true, Ordering::SeqCst)).is_some()
    }
}

/// Throttled progress reporter + cancellation check for one task.
pub struct Progress {
    map: TaskMap,
    events: Events,
    state: TaskProgress,
    last_emit: Option<Instant>,
    cancel: Arc<AtomicBool>,
    finished: bool,
}

impl Progress {
    pub fn set_label(&mut self, label: impl Into<String>) {
        self.state.label = label.into();
        self.emit();
    }

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn check_cancel(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::SeqCst) { Err(CANCELLED.to_string()) } else { Ok(()) }
    }

    /// Records progress; emits at most ~10 events per second.
    pub fn update(&mut self, received: u64, total: Option<u64>) {
        self.state.received = received;
        self.state.total = total;
        let now = Instant::now();
        if self.last_emit.is_none_or(|t| now.duration_since(t) >= PROGRESS_INTERVAL) {
            self.last_emit = Some(now);
            self.emit();
        }
    }

    /// Sends the final event (with the error, if any).
    pub fn finish(&mut self, error: Option<String>) {
        self.state.done = true;
        self.state.error = error;
        self.finished = true;
        self.emit();
    }

    fn emit(&self) {
        self.events.send(Event::Task(self.state.clone()));
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(None);
        }
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if map.get(&self.state.id).is_some_and(|f| Arc::ptr_eq(f, &self.cancel)) {
            map.remove(&self.state.id);
        }
    }
}

/// HTTP client for downloads (through the system proxy).
pub fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("dd-core/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .build()
        .map_err(err_str)
}

/// Streams a response body into `path`, reporting `base + bytes written` as progress and checking
/// for cancellation between chunks. Returns the number of bytes written. The caller deletes
/// `path` on error.
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
    Ok(written)
}

pub fn remove_dir_quiet(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

pub fn remove_file_quiet(path: &Path) {
    let _ = std::fs::remove_file(path);
}
