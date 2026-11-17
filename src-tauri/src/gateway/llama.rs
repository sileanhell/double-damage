//! Running llama-server processes: one per loaded model, each on its own random port with its
//! own random API key, so only the gateway can talk to it. Loading and unloading are explicit
//! (the app decides); the gateway only routes requests to models that are ready.

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

use super::{job, random_hex};

/// Provider prefix of the model ids the gateway serves, e.g. `llama-server/Qwen3-8B-Q4_K_M`.
pub const PROVIDER: &str = "llama-server";
const HEALTH_INTERVAL: Duration = Duration::from_millis(250);
const WATCH_INTERVAL: Duration = Duration::from_secs(1);
/// Log lines kept per process, to explain a failed start.
const LOG_LINES: usize = 200;

/// How to start a model. Built by the app from the model's launch settings; every field is
/// checked here, the app never passes raw command-line arguments.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSpec {
    /// Model file name in the models folder (the first part of a split model).
    pub file: String,
    pub ctx: u32,
    /// llama-server device id ("CUDA0", "Vulkan1"); None runs on the CPU.
    pub device: Option<String>,
    pub gpu_layers: u32,
    pub kv: String,
    pub flash_attn: String,
    pub threads: Option<u32>,
}

impl LaunchSpec {
    fn validate(&self) -> Result<(), String> {
        if !["f16", "q8_0", "q4_0"].contains(&self.kv.as_str()) {
            return Err(format!("unsupported KV cache type {:?}", self.kv));
        }
        if !["auto", "on", "off"].contains(&self.flash_attn.as_str()) {
            return Err(format!("unsupported flash attention mode {:?}", self.flash_attn));
        }
        if let Some(device) = &self.device
            && (device.is_empty() || !device.chars().all(|c| c.is_ascii_alphanumeric()))
        {
            return Err(format!("invalid device {device:?}"));
        }
        if self.ctx == 0 {
            return Err("context size must be positive".into());
        }
        Ok(())
    }

    fn args(&self, path: &str, port: u16, key: &str, alias: &str) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-m".into(),
            path.into(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            port.to_string(),
            "--api-key".into(),
            key.into(),
            "--alias".into(),
            alias.into(),
            "--jinja".into(),
            "--no-webui".into(),
            "-c".into(),
            self.ctx.to_string(),
            // One slot: the whole context belongs to one conversation; requests queue up.
            "-np".into(),
            "1".into(),
            "-fa".into(),
            self.flash_attn.clone(),
        ];
        match &self.device {
            Some(device) => args.extend(["--device".into(), device.clone(), "-ngl".into(), self.gpu_layers.to_string()]),
            None => args.extend(["--device".into(), "none".into(), "-ngl".into(), "0".into()]),
        }
        if self.kv != "f16" {
            args.extend(["-ctk".into(), self.kv.clone(), "-ctv".into(), self.kv.clone()]);
        }
        if let Some(threads) = self.threads.filter(|t| *t > 0) {
            args.extend(["-t".into(), threads.to_string()]);
        }
        args
    }
}

/// Model id as clients see it: provider prefix + file name without the extension and shard suffix.
pub fn model_id(file: &str) -> String {
    let stem = file.strip_suffix(".gguf").or_else(|| file.strip_suffix(".GGUF")).unwrap_or(file);
    // `name-00001-of-00003` -> `name`
    let stem = match stem.rsplit_once("-00001-of-") {
        Some((base, count)) if count.len() == 5 && count.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => stem,
    };
    format!("{PROVIDER}/{stem}")
}

#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum State {
    Loading,
    Ready,
    Failed { error: String },
}

/// What the app sees about a running model.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct InstanceInfo {
    pub id: String,
    pub spec: LaunchSpec,
    pub state: State,
    /// Unix ms.
    pub started_at: u64,
    /// Unix ms of the last request start or end; the app's idle timer counts from here.
    pub last_used: u64,
    /// Requests being answered right now.
    pub active: usize,
    /// `chat_template_caps` from llama-server's `/props` once ready (tools, reasoning support).
    pub caps: Option<serde_json::Value>,
}

pub struct Instance {
    pub id: String,
    spec: LaunchSpec,
    pub port: u16,
    pub key: String,
    started_at: u64,
    state: Mutex<State>,
    last_used: Mutex<u64>,
    active: AtomicUsize,
    caps: Mutex<Option<serde_json::Value>>,
    child: tokio::sync::Mutex<Option<Child>>,
    log: Mutex<VecDeque<String>>,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Instance {
    pub fn state(&self) -> State {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn info(&self) -> InstanceInfo {
        InstanceInfo {
            id: self.id.clone(),
            spec: self.spec.clone(),
            state: self.state(),
            started_at: self.started_at,
            last_used: *self.last_used.lock().unwrap_or_else(|e| e.into_inner()),
            active: self.active.load(Ordering::SeqCst),
            caps: self.caps.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        }
    }

    fn tail(&self, lines: usize) -> String {
        let log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        let skip = log.len().saturating_sub(lines);
        log.iter().skip(skip).cloned().collect::<Vec<_>>().join("\n")
    }
}

/// Marks a request in flight for as long as it lives; touches `last_used` at both ends.
pub struct Busy {
    instance: Arc<Instance>,
    runtime: Llama,
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.instance.active.fetch_sub(1, Ordering::SeqCst);
        *self.instance.last_used.lock().unwrap_or_else(|e| e.into_inner()) = now_ms();
        self.runtime.changed();
    }
}

type Listener = Arc<dyn Fn(Vec<InstanceInfo>) + Send + Sync>;

/// The set of running model servers. Cheap to clone.
#[derive(Clone)]
pub struct Llama {
    instances: Arc<Mutex<HashMap<String, Arc<Instance>>>>,
    listener: Listener,
    /// Held while deciding whether to start a process and registering it, so two loads of the
    /// same model can't both start one (the second would orphan the first).
    starting: Arc<tokio::sync::Mutex<()>>,
}

impl Llama {
    /// `listener` gets the full list after every change (state, activity, load, unload).
    pub fn new(listener: impl Fn(Vec<InstanceInfo>) + Send + Sync + 'static) -> Self {
        Self { instances: Default::default(), listener: Arc::new(listener), starting: Default::default() }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Instance>>> {
        self.instances.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn list(&self) -> Vec<InstanceInfo> {
        let mut list: Vec<_> = self.map().values().map(|i| i.info()).collect();
        list.sort_by_key(|i| i.started_at);
        list
    }

    fn changed(&self) {
        (self.listener)(self.list());
    }

    /// A model that is ready to answer, by model id.
    pub fn ready(&self, id: &str) -> Option<Arc<Instance>> {
        self.map().get(id).filter(|i| i.state() == State::Ready).cloned()
    }

    pub fn busy(&self, instance: Arc<Instance>) -> Busy {
        instance.active.fetch_add(1, Ordering::SeqCst);
        *instance.last_used.lock().unwrap_or_else(|e| e.into_inner()) = now_ms();
        self.changed();
        Busy { instance, runtime: self.clone() }
    }

    /// Starts a model and waits until it is ready (or has failed). Loading a model that is
    /// already loading or loaded just waits for it; a failed one is started again.
    pub async fn load(&self, exe: PathBuf, model_path: PathBuf, spec: LaunchSpec) -> Result<InstanceInfo, String> {
        spec.validate()?;
        let id = model_id(&spec.file);

        let instance = {
            let _starting = self.starting.lock().await;
            let existing = self.map().get(&id).cloned();
            match existing {
                Some(instance) if !matches!(instance.state(), State::Failed { .. }) => instance,
                _ => self.spawn(&id, exe, model_path, spec).await?,
            }
        };

        loop {
            match instance.state() {
                State::Ready => return Ok(instance.info()),
                State::Failed { error } => return Err(error),
                State::Loading => tokio::time::sleep(HEALTH_INTERVAL).await,
            }
            // Unloaded while we waited.
            if !self.map().get(&id).is_some_and(|i| Arc::ptr_eq(i, &instance)) {
                return Err("unloaded".into());
            }
        }
    }

    async fn spawn(&self, id: &str, exe: PathBuf, model_path: PathBuf, spec: LaunchSpec) -> Result<Arc<Instance>, String> {
        if !exe.is_file() {
            return Err("llama-server is not installed".into());
        }
        if !model_path.is_file() {
            return Err(format!("model file not found: {}", spec.file));
        }
        let port = free_port()?;
        let key = random_hex(24);
        let path = model_path.to_string_lossy().into_owned();
        let args = spec.args(&path, port, &key, id);

        let mut command = Command::new(&exe);
        command
            .args(&args)
            .current_dir(exe.parent().unwrap_or(&exe))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
        let mut child = command.spawn().map_err(|e| format!("failed to start llama-server: {e}"))?;
        job::adopt(&child);
        let stderr = child.stderr.take();

        let now = now_ms();
        let instance = Arc::new(Instance {
            id: id.to_string(),
            spec,
            port,
            key,
            started_at: now,
            state: Mutex::new(State::Loading),
            last_used: Mutex::new(now),
            active: AtomicUsize::new(0),
            caps: Mutex::new(None),
            child: tokio::sync::Mutex::new(Some(child)),
            log: Mutex::new(VecDeque::with_capacity(LOG_LINES)),
        });
        // Replaces a failed instance with the same id.
        self.map().insert(id.to_string(), instance.clone());
        self.changed();

        if let Some(stderr) = stderr {
            let log_target = instance.clone();
            tokio::spawn(async move {
                // Byte lines, decoded lossily: llama.cpp's metadata dump can cut a character in
                // half, and a strict reader would stop there and lose the error at the end.
                let mut reader = BufReader::new(stderr);
                let mut buf = Vec::new();
                while reader.read_until(b'\n', &mut buf).await.is_ok_and(|n| n > 0) {
                    let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                    buf.clear();
                    let mut log = log_target.log.lock().unwrap_or_else(|e| e.into_inner());
                    if log.len() == LOG_LINES {
                        log.pop_front();
                    }
                    log.push_back(line);
                }
            });
        }

        let runtime = self.clone();
        let watched = instance.clone();
        tokio::spawn(async move { runtime.watch(watched).await });
        Ok(instance)
    }

    /// Waits for the server to come up, then keeps an eye on it until it exits.
    async fn watch(&self, instance: Arc<Instance>) {
        let client = reqwest::Client::builder().no_proxy().build().unwrap_or_default();
        let base = format!("http://127.0.0.1:{}", instance.port);
        loop {
            if !self.is_current(&instance) {
                return Self::kill(&instance).await;
            }
            if let Some(failure) = self.exited(&instance).await {
                return self.fail(&instance, failure);
            }
            let health = client.get(format!("{base}/health")).timeout(Duration::from_secs(2)).send().await;
            if health.is_ok_and(|r| r.status().is_success()) {
                break;
            }
            tokio::time::sleep(HEALTH_INTERVAL).await;
        }

        let caps = async {
            let bytes = client
                .get(format!("{base}/props"))
                .bearer_auth(&instance.key)
                .send()
                .await
                .ok()?
                .bytes()
                .await
                .ok()?;
            let props: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            props.get("chat_template_caps").cloned()
        }
        .await;
        *instance.caps.lock().unwrap_or_else(|e| e.into_inner()) = caps;
        // The idle timer starts now, not when loading began (a big model can take minutes).
        *instance.last_used.lock().unwrap_or_else(|e| e.into_inner()) = now_ms();
        self.set_state(&instance, State::Ready);

        loop {
            tokio::time::sleep(WATCH_INTERVAL).await;
            if !self.is_current(&instance) {
                return Self::kill(&instance).await;
            }
            if let Some(failure) = self.exited(&instance).await {
                return self.fail(&instance, failure);
            }
        }
    }

    /// Stops a process that is no longer tracked (unloaded, or replaced by a newer start).
    async fn kill(instance: &Instance) {
        if let Some(mut child) = instance.child.lock().await.take() {
            let _ = child.kill().await;
        }
    }

    fn is_current(&self, instance: &Arc<Instance>) -> bool {
        self.map().get(&instance.id).is_some_and(|i| Arc::ptr_eq(i, instance))
    }

    /// `Some(reason)` once the process is gone.
    async fn exited(&self, instance: &Instance) -> Option<String> {
        let mut child = instance.child.lock().await;
        let status = match child.as_mut() {
            None => return Some("stopped".into()),
            Some(child) => child.try_wait().ok()??,
        };
        child.take();
        Some(format!("llama-server exited ({status})"))
    }

    fn fail(&self, instance: &Arc<Instance>, reason: String) {
        if !self.is_current(instance) {
            return;
        }
        let tail = instance.tail(12);
        let error = if tail.is_empty() { reason } else { format!("{reason}\n{tail}") };
        self.set_state(instance, State::Failed { error });
    }

    fn set_state(&self, instance: &Instance, state: State) {
        *instance.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
        self.changed();
    }

    /// Stops a model and forgets it. Unknown ids are ignored.
    pub async fn unload(&self, id: &str) {
        let removed = self.map().remove(id);
        let Some(instance) = removed else { return };
        self.changed();
        if let Some(mut child) = instance.child.lock().await.take() {
            let _ = child.kill().await;
        }
    }

    /// Stops everything (app exit).
    pub fn shutdown(&self) {
        let all: Vec<_> = self.map().drain().map(|(_, i)| i).collect();
        for instance in all {
            if let Ok(mut child) = instance.child.try_lock()
                && let Some(child) = child.as_mut()
            {
                let _ = child.start_kill();
            }
        }
    }
}

fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    Ok(listener.local_addr().map_err(|e| e.to_string())?.port())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> LaunchSpec {
        LaunchSpec {
            file: "m.gguf".into(),
            ctx: 4096,
            device: Some("CUDA0".into()),
            gpu_layers: 33,
            kv: "q8_0".into(),
            flash_attn: "on".into(),
            threads: Some(6),
        }
    }

    #[test]
    fn ids() {
        assert_eq!(model_id("Qwen3-8B-Q4_K_M.gguf"), "llama-server/Qwen3-8B-Q4_K_M");
        assert_eq!(model_id("big-00001-of-00003.gguf"), "llama-server/big");
        assert_eq!(model_id("weird-00001-of-3.gguf"), "llama-server/weird-00001-of-3");
    }

    #[test]
    fn spec_is_checked() {
        assert!(spec().validate().is_ok());
        assert!(LaunchSpec { kv: "--host".into(), ..spec() }.validate().is_err());
        assert!(LaunchSpec { flash_attn: "maybe".into(), ..spec() }.validate().is_err());
        assert!(LaunchSpec { device: Some("CUDA0 --host 0.0.0.0".into()), ..spec() }.validate().is_err());
        assert!(LaunchSpec { device: Some(String::new()), ..spec() }.validate().is_err());
        assert!(LaunchSpec { ctx: 0, ..spec() }.validate().is_err());
    }

    #[test]
    fn args_stay_local() {
        let args = spec().args("C:/m.gguf", 1234, "key", "llama-server/m");
        let at = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].as_str());
        assert_eq!(at("--host"), Some("127.0.0.1"));
        assert_eq!(at("--port"), Some("1234"));
        assert_eq!(at("--api-key"), Some("key"));
        assert_eq!(at("--device"), Some("CUDA0"));
        assert_eq!(at("-ngl"), Some("33"));
        assert_eq!(at("-ctk"), Some("q8_0"));
        assert_eq!(at("-t"), Some("6"));
        let cpu = LaunchSpec { device: None, kv: "f16".into(), threads: None, ..spec() }.args("m", 1, "k", "a");
        assert!(cpu.windows(2).any(|w| w == ["--device", "none"]));
        assert!(!cpu.iter().any(|a| a == "-ctk" || a == "-t"));
    }
}
