//! Double Damage core: model providers (local llama-server, Claude, Devin), their sign-ins, and
//! the local HTTP endpoint in front of them: Anthropic- and OpenAI-compatible model routes plus
//! the `/dd/*` management routes. Runs as `dd-core serve`; every other `dd-core` command is a
//! client of a running server.

pub mod admin;
pub mod browser;
pub mod caps;
pub mod catalog;
pub mod client;
pub mod events;
pub mod gguf;
pub mod hardware;
pub mod job;
pub mod llama;
pub mod paths;
pub mod proto;
pub mod providers;
pub mod server;
pub mod store;
pub mod tasks;
pub mod util;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, RwLock};

use serde::Serialize;

use crate::{
    events::{Event, Events},
    llama::{Llama, files::MetaCache},
    paths::Paths,
    providers::{Claude, Devin},
    store::Store,
    tasks::Tasks,
};

/// Port used unless `--port` or `config.toml` says otherwise.
pub const DEFAULT_PORT: u16 = 11488;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the server listens; written to `<data>/core/server.json` while it runs and printed as
/// the first stdout line of `dd-core serve` (`{"event":"ready",…}`).
#[derive(Serialize, serde::Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub url: String,
    pub port: u16,
    pub pid: u32,
    pub version: String,
    pub data_dir: String,
}

/// Everything the server shares between requests. Cheap to clone.
#[derive(Clone)]
pub struct Core {
    pub paths: Paths,
    pub store: Arc<Store>,
    pub events: Events,
    pub tasks: Tasks,
    pub llama: Llama,
    pub devin: Devin,
    pub claude: Claude,
    pub meta: Arc<MetaCache>,
    /// The token clients present; replaceable while running.
    token: Arc<RwLock<Arc<str>>>,
    /// Set by `POST /dd/shutdown`; the server stops serving when it fires.
    shutdown: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl Core {
    pub fn new(paths: Paths) -> Self {
        let events = Events::default();
        let store = Arc::new(Store::open(&paths));
        let token: Arc<str> = store.token().into();
        Self {
            tasks: Tasks::new(events.clone()),
            llama: Llama::new(events.clone()),
            devin: Devin::new(store.clone(), paths.clone(), events.clone()),
            claude: Claude::new(store.clone(), paths.clone(), events.clone()),
            meta: Default::default(),
            token: Arc::new(RwLock::new(token)),
            shutdown: Default::default(),
            events,
            store,
            paths,
        }
    }

    pub fn token(&self) -> Arc<str> {
        self.token.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Issues a new token; clients holding the old one are locked out at once.
    pub fn regenerate_token(&self) -> Result<String, String> {
        let token = store::new_token();
        self.store.update(|c| c.gateway.token = token.clone())?;
        *self.token.write().unwrap_or_else(|e| e.into_inner()) = token.as_str().into();
        self.events.send(Event::Token);
        Ok(token)
    }

    pub fn llama_installed(&self) -> bool {
        llama::install::installed_exe(&self.paths).is_some()
    }

    pub fn catalog(&self) -> catalog::Catalog {
        catalog::build(&self.llama, self.llama_installed(), &self.devin, &self.claude)
    }

    /// Arms the shutdown request; the server waits on the receiver.
    pub fn on_shutdown(&self) -> tokio::sync::oneshot::Receiver<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        *self.shutdown.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        rx
    }

    pub fn request_shutdown(&self) {
        if let Some(tx) = self.shutdown.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(());
        }
    }

    /// Stops every model server.
    pub fn stop_models(&self) {
        self.llama.shutdown();
    }

    /// Background refresh of the remote model lists at startup.
    pub fn refresh_remote_models(&self) {
        if self.claude.creds().is_some() {
            let claude = self.claude.clone();
            tokio::spawn(async move {
                let _ = claude.refresh_models().await;
            });
        }
        if self.devin.creds().is_some() {
            let devin = self.devin.clone();
            tokio::spawn(async move {
                let _ = devin.refresh_models().await;
            });
        }
    }
}
