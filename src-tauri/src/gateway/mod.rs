//! The provider gateway: model servers and the local OpenAI/Anthropic-compatible HTTP endpoint
//! in front of them. Nothing in here depends on the webview, so it can later run on its own
//! (a headless CLI build); `commands` is the thin Tauri layer on top.

pub mod caps;
pub mod catalog;
pub mod commands;
pub mod job;
pub mod llama;
mod server;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};

use serde::Serialize;

pub use llama::Llama;

/// Port used until the user picks another one.
pub const DEFAULT_PORT: u16 = 11488;

/// Random bytes as lowercase hex (`bytes` long before encoding).
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("system random number generator unavailable");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GatewayInfo {
    /// Base URL, e.g. `http://127.0.0.1:11488`; None while not listening.
    pub url: Option<String>,
    pub port: u16,
    pub token: String,
    /// Why it isn't listening (port taken, …).
    pub error: Option<String>,
}

pub struct Gateway {
    pub llama: Llama,
    pub catalog: Arc<catalog::Source>,
    server: tokio::sync::Mutex<Option<server::Server>>,
    info: Mutex<GatewayInfo>,
}

impl Gateway {
    pub fn new(llama: Llama, paths: impl Fn() -> catalog::Paths + Send + Sync + 'static, token: String) -> Self {
        Self {
            catalog: catalog::Source::new(llama.clone(), paths),
            llama,
            server: Default::default(),
            info: Mutex::new(GatewayInfo { url: None, port: DEFAULT_PORT, token, error: None }),
        }
    }

    pub fn info(&self) -> GatewayInfo {
        self.info.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// (Re)starts the HTTP endpoint on `port` with `token`. Model servers keep running.
    pub async fn listen(&self, port: u16, token: String) -> GatewayInfo {
        let mut server = self.server.lock().await;
        if let Some(old) = server.take() {
            old.stop();
            // Give the old listener a moment to release the port before binding it again.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let result = server::start(port, token.clone(), self.llama.clone(), self.catalog.clone()).await;
        let info = match result {
            Ok(started) => {
                let info = GatewayInfo {
                    url: Some(format!("http://127.0.0.1:{}", started.port)),
                    port,
                    token,
                    error: None,
                };
                *server = Some(started);
                info
            }
            Err(error) => GatewayInfo { url: None, port, token, error: Some(error) },
        };
        *self.info.lock().unwrap_or_else(|e| e.into_inner()) = info.clone();
        info
    }

    pub fn shutdown(&self) {
        self.llama.shutdown();
        if let Ok(mut server) = self.server.try_lock()
            && let Some(server) = server.take()
        {
            server.stop();
        }
    }
}
