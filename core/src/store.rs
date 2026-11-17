//! The core's files: secrets in `credentials.toml`, settings in `config.toml`.
//!
//! Secrets are kept in memory and written back (atomically) on every change; the running server
//! is the only writer, CLI commands go through it.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::paths::{Paths, write_atomic};

/// The local API token clients present.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct GatewaySecrets {
    pub token: String,
}

/// Claude (claude.ai OAuth) tokens and the account bits that came with them.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct ClaudeCredentials {
    pub access_token: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub refresh_token: String,
    /// ms epoch when the access token stops working.
    pub expires_at: u64,
    /// ms epoch when the refresh token stops working, when the server said.
    pub refresh_token_expires_at: Option<u64>,
    pub scopes: Vec<String>,
    pub email: Option<String>,
    /// `pro`, `max`, `team`, `enterprise`.
    pub subscription: Option<String>,
    pub organization: Option<String>,
}

/// Devin (Cognition) session token and what it was issued for.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct DevinCredentials {
    pub token: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub api_server_url: String,
    /// `Metadata.f`: a random hex blob generated once per sign-in.
    pub fingerprint: String,
    pub email: Option<String>,
    pub user_id: Option<String>,
    pub user_ref: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct Credentials {
    pub gateway: GatewaySecrets,
    pub claude: Option<ClaudeCredentials>,
    pub devin: Option<DevinCredentials>,
}

pub struct Store {
    path: std::path::PathBuf,
    data: Mutex<Credentials>,
}

/// camelCase object keys → snake_case (the old JSON credential files used camelCase).
fn snake_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let mut snake = String::with_capacity(key.len() + 4);
                    for c in key.chars() {
                        if c.is_ascii_uppercase() {
                            snake.push('_');
                            snake.push(c.to_ascii_lowercase());
                        } else {
                            snake.push(c);
                        }
                    }
                    (snake, snake_keys(value))
                })
                .collect(),
        ),
        other => other,
    }
}

fn old_json<T: serde::de::DeserializeOwned>(path: std::path::PathBuf) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_value(snake_keys(serde_json::from_slice(&bytes).ok()?)).ok()
}

impl Store {
    /// Loads `credentials.toml`; on first use, takes over the sign-ins (and the gateway token)
    /// the desktop app kept before the core existed. A token is generated when there is none.
    pub fn open(paths: &Paths) -> Self {
        let path = paths.credentials();
        let existing = std::fs::read_to_string(&path).ok();
        let mut data: Credentials = existing.as_deref().and_then(|text| toml::from_str(text).ok()).unwrap_or_default();
        let mut dirty = existing.is_none();

        if existing.is_none() {
            data.claude = old_json::<ClaudeCredentials>(paths.data.join("claude").join("credentials.json"))
                .filter(|c| !c.access_token.is_empty());
            data.devin = old_json::<DevinCredentials>(paths.data.join("devin").join("credentials.json"))
                .filter(|c| !c.token.is_empty());
            let settings: Option<Value> =
                std::fs::read(paths.data.join("settings.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
            if let Some(token) = settings.as_ref().and_then(|s| s.get("gateway.token")).and_then(Value::as_str) {
                data.gateway.token = token.to_string();
            }
        }
        if data.gateway.token.len() < 32 {
            data.gateway.token = new_token();
            dirty = true;
        }
        let store = Self { path, data: Mutex::new(data) };
        if dirty && let Err(error) = store.save() {
            eprintln!("dd-core: can't write {}: {error}", store.path.display());
        }
        store
    }

    fn save(&self) -> Result<(), String> {
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let text = toml::to_string_pretty(&*data).map_err(|e| e.to_string())?;
        let text = format!(
            "# Double Damage core secrets: the local API token and provider sign-ins.\n\
             # Keep this file private.\n\n{text}"
        );
        write_atomic(&self.path, text.as_bytes())
    }

    pub fn get(&self) -> Credentials {
        self.data.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Changes the secrets and writes the file.
    pub fn update<R>(&self, change: impl FnOnce(&mut Credentials) -> R) -> Result<R, String> {
        let result = change(&mut self.data.lock().unwrap_or_else(|e| e.into_inner()));
        self.save()?;
        Ok(result)
    }

    pub fn token(&self) -> String {
        self.data.lock().unwrap_or_else(|e| e.into_inner()).gateway.token.clone()
    }
}

pub fn new_token() -> String {
    format!("dd-{}", crate::util::random_hex(24))
}

// --- config.toml ----------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { host: "127.0.0.1".into(), port: crate::DEFAULT_PORT }
    }
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
}

impl Config {
    pub fn load(paths: &Paths) -> Self {
        std::fs::read_to_string(paths.config()).ok().and_then(|text| toml::from_str(&text).ok()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_old_json_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        std::fs::create_dir_all(dir.path().join("claude")).unwrap();
        std::fs::write(
            dir.path().join("claude/credentials.json"),
            r#"{"accessToken":"a","refreshToken":"r","expiresAt":5,"scopes":["x"],"subscription":"max"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("settings.json"), r#"{"gateway.token":"dd-0123456789abcdef0123456789abcdef"}"#).unwrap();

        let store = Store::open(&paths);
        let creds = store.get();
        let claude = creds.claude.unwrap();
        assert_eq!(claude.access_token, "a");
        assert_eq!(claude.refresh_token, "r");
        assert_eq!(claude.subscription.as_deref(), Some("max"));
        assert_eq!(creds.gateway.token, "dd-0123456789abcdef0123456789abcdef");
        assert!(creds.devin.is_none());

        store.update(|c| c.devin = Some(DevinCredentials { token: "t".into(), ..Default::default() })).unwrap();
        let again = Store::open(&paths).get();
        assert_eq!(again.devin.unwrap().token, "t");
        assert_eq!(again.claude.unwrap().expires_at, 5);
    }
}
