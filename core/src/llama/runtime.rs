//! Running llama-server processes: one per loaded model, each on its own random port with its
//! own random API key, so only the gateway can talk to it. Loading and unloading are explicit
//! (`dd-core load` / `POST /dd/llama/load`); the gateway only routes requests to models that are
//! ready, and only those show up in the model lists.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

use crate::{
    caps::{self, Capabilities},
    events::{Event, Events},
    gguf::Sampling,
    job,
    util::{now_ms, random_hex},
};

/// Provider prefix of the model ids the gateway serves, e.g. `llama-server/Qwen3-8B-Q4_K_M`.
pub const PROVIDER: &str = "llama-server";
const HEALTH_INTERVAL: Duration = Duration::from_millis(250);
const WATCH_INTERVAL: Duration = Duration::from_secs(1);
/// Log lines kept per process, to explain a failed start.
const LOG_LINES: usize = 200;

/// Flags the core sets itself: where the server listens, its key, the model and its name.
const RESERVED: [&str; 9] = ["--host", "--port", "--api-key", "--api-key-file", "-m", "--model", "--alias", "-a", "--path"];

/// How to start a model. Everything but the model is optional: what isn't given is left to
/// llama-server's own defaults. `extra_args` go to llama-server as they are.
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct LaunchSpec {
    /// What to run: a URL (downloaded into the models folder), a path, or a file name in the
    /// models folder (the first part of a split model).
    pub model: String,
    /// The model file the core resolved `model` to (kept for clients; also accepted in place of
    /// `model`).
    pub file: String,
    /// Model id suffix (`llama-server/<id>`); defaults to the file name without extension.
    pub id: Option<String>,
    /// Context size (`-c`).
    pub ctx: Option<u32>,
    /// llama-server device id (`CUDA0`, `Vulkan1`, a comma list), or `cpu` / `none`.
    pub device: Option<String>,
    /// Layers on the GPU (`-ngl`).
    pub gpu_layers: Option<u32>,
    /// KV cache type (`-ctk` / `-ctv`), e.g. `f16`, `q8_0`.
    pub kv: Option<String>,
    /// `auto`, `on` or `off` (`-fa`).
    pub flash_attn: Option<String>,
    pub threads: Option<u32>,
    /// Parallel slots (`-np`); 1 unless given, so one conversation owns the whole context.
    pub parallel: Option<u32>,
    pub extra_args: Vec<String>,
}

impl LaunchSpec {
    /// The reference to resolve: `model`, or `file` from older clients.
    pub fn reference(&self) -> &str {
        if self.model.trim().is_empty() { self.file.trim() } else { self.model.trim() }
    }

    fn validate(&self) -> Result<(), String> {
        for arg in &self.extra_args {
            let flag = arg.split('=').next().unwrap_or(arg);
            if RESERVED.contains(&flag) {
                return Err(format!("{flag} is set by dd-core and can't be passed through"));
            }
        }
        if self.ctx == Some(0) {
            return Err("context size must be positive".into());
        }
        if let Some(id) = &self.id
            && (id.is_empty() || id.contains(['/', '\\']) || id.chars().any(char::is_whitespace))
        {
            return Err(format!("invalid model id {id:?}"));
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
        ];
        let given = |flags: &[&str]| self.extra_args.iter().any(|a| flags.contains(&a.split('=').next().unwrap_or(a)));
        if let Some(ctx) = self.ctx {
            args.extend(["-c".into(), ctx.to_string()]);
        }
        if !given(&["-np", "--parallel"]) {
            args.extend(["-np".into(), self.parallel.unwrap_or(1).to_string()]);
        }
        if let Some(fa) = &self.flash_attn {
            args.extend(["-fa".into(), fa.clone()]);
        }
        match self.device.as_deref() {
            Some("cpu" | "none" | "CPU") => args.extend(["--device".into(), "none".into(), "-ngl".into(), "0".into()]),
            Some(device) => {
                args.extend(["--device".into(), device.to_string()]);
                if let Some(layers) = self.gpu_layers {
                    args.extend(["-ngl".into(), layers.to_string()]);
                }
            }
            None => {
                if let Some(layers) = self.gpu_layers {
                    args.extend(["-ngl".into(), layers.to_string()]);
                }
            }
        }
        if let Some(kv) = self.kv.as_deref().filter(|kv| *kv != "f16") {
            args.extend(["-ctk".into(), kv.to_string(), "-ctv".into(), kv.to_string()]);
        }
        if let Some(threads) = self.threads.filter(|t| *t > 0) {
            args.extend(["-t".into(), threads.to_string()]);
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

/// Model id as clients see it: provider prefix + file name without the extension and shard suffix.
pub fn model_id(file: &str) -> String {
    let name = Path::new(file).file_name().and_then(|n| n.to_str()).unwrap_or(file);
    let stem = name.strip_suffix(".gguf").or_else(|| name.strip_suffix(".GGUF")).unwrap_or(name);
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

/// What the model file says about itself, read when it is loaded.
#[derive(Clone, Debug, Default)]
pub struct FileMeta {
    pub context_length: Option<u64>,
    pub template_caps: Capabilities,
    pub sampling: Sampling,
}

/// What clients see about a running model.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct InstanceInfo {
    pub id: String,
    pub spec: LaunchSpec,
    pub state: State,
    /// Full path of the model file.
    pub path: String,
    pub size: u64,
    /// Unix ms.
    pub started_at: u64,
    /// Unix ms of the last request start or end; idle timers count from here.
    pub last_used: u64,
    /// Requests being answered right now.
    pub active: usize,
    /// Context the model was trained for.
    pub context_length: Option<u64>,
    /// Context of the running server (`n_ctx`), once known.
    pub loaded_context: Option<u32>,
    /// Tools and reasoning: guessed from the chat template, confirmed once the server is up.
    pub capabilities: Capabilities,
    /// Author-recommended sampling from the file.
    pub sampling: Sampling,
    /// `chat_template_caps` from llama-server's `/props` once ready.
    pub caps: Option<serde_json::Value>,
}

pub struct Instance {
    pub id: String,
    spec: LaunchSpec,
    path: PathBuf,
    size: u64,
    meta: FileMeta,
    pub port: u16,
    pub key: String,
    started_at: u64,
    state: Mutex<State>,
    last_used: Mutex<u64>,
    active: AtomicUsize,
    caps: Mutex<Option<serde_json::Value>>,
    /// Chat template and context the running server reports.
    props: Mutex<(Option<String>, Option<u32>)>,
    child: tokio::sync::Mutex<Option<Child>>,
    log: Mutex<VecDeque<String>>,
}

impl Instance {
    pub fn state(&self) -> State {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn capabilities(&self) -> Capabilities {
        let runtime = self.caps.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let props = self.props.lock().unwrap_or_else(|e| e.into_inner()).clone();
        // The template the server actually uses beats the one in the file.
        let guess = match props.0.as_deref() {
            Some(template) => caps::from_template(Some(template)),
            None => self.meta.template_caps.clone(),
        };
        caps::with_runtime(guess, runtime.as_ref())
    }

    pub fn info(&self) -> InstanceInfo {
        let props = self.props.lock().unwrap_or_else(|e| e.into_inner()).clone();
        InstanceInfo {
            id: self.id.clone(),
            spec: self.spec.clone(),
            state: self.state(),
            path: self.path.to_string_lossy().into_owned(),
            size: self.size,
            started_at: self.started_at,
            last_used: *self.last_used.lock().unwrap_or_else(|e| e.into_inner()),
            active: self.active.load(Ordering::SeqCst),
            context_length: self.meta.context_length,
            loaded_context: props.1.or(self.spec.ctx),
            capabilities: self.capabilities(),
            sampling: self.meta.sampling.clone(),
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

/// The set of running model servers. Cheap to clone.
#[derive(Clone)]
pub struct Llama {
    instances: Arc<Mutex<HashMap<String, Arc<Instance>>>>,
    events: Events,
    /// Held while deciding whether to start a process and registering it, so two loads of the
    /// same model can't both start one (the second would orphan the first).
    starting: Arc<tokio::sync::Mutex<()>>,
}

impl Llama {
    /// Every change (state, activity, load, unload) goes out as an `instances` event.
    pub fn new(events: Events) -> Self {
        Self { instances: Default::default(), events, starting: Default::default() }
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
        self.events.send(Event::Instances(self.list()));
    }

    /// A model that is ready to answer, by model id.
    pub fn ready(&self, id: &str) -> Option<Arc<Instance>> {
        self.map().get(id).filter(|i| i.state() == State::Ready).cloned()
    }

    /// Every model that is ready to answer.
    pub fn ready_all(&self) -> Vec<Arc<Instance>> {
        let mut list: Vec<_> = self.map().values().filter(|i| i.state() == State::Ready).cloned().collect();
        list.sort_by_key(|i| i.started_at);
        list
    }

    pub fn busy(&self, instance: Arc<Instance>) -> Busy {
        instance.active.fetch_add(1, Ordering::SeqCst);
        *instance.last_used.lock().unwrap_or_else(|e| e.into_inner()) = now_ms();
        self.changed();
        Busy { instance, runtime: self.clone() }
    }

    /// Starts a model and waits until it is ready (or has failed). Loading a model that is
    /// already loading or loaded just waits for it; a failed one is started again.
    pub async fn load(&self, exe: PathBuf, model_path: PathBuf, spec: LaunchSpec, meta: FileMeta) -> Result<InstanceInfo, String> {
        spec.validate()?;
        let id = match &spec.id {
            Some(id) => format!("{PROVIDER}/{id}"),
            None => model_id(&model_path.to_string_lossy()),
        };

        let instance = {
            let _starting = self.starting.lock().await;
            let existing = self.map().get(&id).cloned();
            match existing {
                Some(instance) if !matches!(instance.state(), State::Failed { .. }) => instance,
                _ => self.spawn(&id, exe, model_path, spec, meta).await?,
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

    async fn spawn(&self, id: &str, exe: PathBuf, model_path: PathBuf, spec: LaunchSpec, meta: FileMeta) -> Result<Arc<Instance>, String> {
        if !exe.is_file() {
            return Err("llama-server is not installed (dd-core llama install)".into());
        }
        if !model_path.is_file() {
            return Err(format!("model file not found: {}", model_path.display()));
        }
        let size = crate::llama::files::model_size(&model_path);
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
            path: model_path,
            size,
            meta,
            port,
            key,
            started_at: now,
            state: Mutex::new(State::Loading),
            last_used: Mutex::new(now),
            active: AtomicUsize::new(0),
            caps: Mutex::new(None),
            props: Mutex::new((None, None)),
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

        let props = async {
            let bytes = client.get(format!("{base}/props")).bearer_auth(&instance.key).send().await.ok()?.bytes().await.ok()?;
            serde_json::from_slice::<serde_json::Value>(&bytes).ok()
        }
        .await;
        if let Some(props) = props {
            *instance.caps.lock().unwrap_or_else(|e| e.into_inner()) = props.get("chat_template_caps").cloned();
            let template = props.get("chat_template").and_then(|t| t.as_str()).filter(|t| !t.is_empty()).map(str::to_owned);
            let n_ctx = props
                .pointer("/default_generation_settings/n_ctx")
                .and_then(|n| n.as_u64())
                .map(|n| n as u32);
            *instance.props.lock().unwrap_or_else(|e| e.into_inner()) = (template, n_ctx);
        }
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

    /// Stops a model and forgets it. Returns whether it was known; accepts the id with or
    /// without the provider prefix.
    pub async fn unload(&self, id: &str) -> bool {
        let full = if id.starts_with(&format!("{PROVIDER}/")) { id.to_string() } else { format!("{PROVIDER}/{id}") };
        let removed = self.map().remove(&full);
        let Some(instance) = removed else { return false };
        self.changed();
        if let Some(mut child) = instance.child.lock().await.take() {
            let _ = child.kill().await;
        }
        true
    }

    /// Stops everything (exit).
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

    pub fn is_empty(&self) -> bool {
        self.map().is_empty()
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
            model: "m.gguf".into(),
            ctx: Some(4096),
            device: Some("CUDA0".into()),
            gpu_layers: Some(33),
            kv: Some("q8_0".into()),
            flash_attn: Some("on".into()),
            threads: Some(6),
            ..Default::default()
        }
    }

    #[test]
    fn ids() {
        assert_eq!(model_id("Qwen3-8B-Q4_K_M.gguf"), "llama-server/Qwen3-8B-Q4_K_M");
        assert_eq!(model_id("C:\\models\\big-00001-of-00003.gguf"), "llama-server/big");
        assert_eq!(model_id("weird-00001-of-3.gguf"), "llama-server/weird-00001-of-3");
    }

    #[test]
    fn spec_is_checked() {
        assert!(spec().validate().is_ok());
        assert!(LaunchSpec { extra_args: vec!["--host".into(), "0.0.0.0".into()], ..spec() }.validate().is_err());
        assert!(LaunchSpec { extra_args: vec!["--port=1".into()], ..spec() }.validate().is_err());
        assert!(LaunchSpec { extra_args: vec!["--temp".into(), "0.2".into()], ..spec() }.validate().is_ok());
        assert!(LaunchSpec { id: Some("a/b".into()), ..spec() }.validate().is_err());
        assert!(LaunchSpec { ctx: Some(0), ..spec() }.validate().is_err());
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
        assert_eq!(at("-np"), Some("1"));
        let cpu = LaunchSpec { device: Some("cpu".into()), kv: None, threads: None, ..spec() }.args("m", 1, "k", "a");
        assert!(cpu.windows(2).any(|w| w == ["--device", "none"]));
        assert!(!cpu.iter().any(|a| a == "-ctk" || a == "-t"));
        // Bare spec: llama-server's defaults, plus whatever is passed through.
        let bare = LaunchSpec { model: "m".into(), extra_args: vec!["-np".into(), "4".into(), "--mlock".into()], ..Default::default() }
            .args("m", 1, "k", "a");
        assert!(!bare.iter().any(|a| a == "-c" || a == "--device" || a == "-ngl"));
        assert_eq!(bare.iter().filter(|a| *a == "-np").count(), 1);
        assert!(bare.ends_with(&["-np".into(), "4".into(), "--mlock".into()]));
    }
}
