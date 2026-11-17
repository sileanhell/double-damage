//! Talking to a running `dd-core serve` from the CLI: where it is (`<data>/core/server.json`, or
//! `--url` / `DD_CORE_URL`) and its token (`credentials.toml`, or `--token` / `DD_CORE_TOKEN`).

use futures_util::StreamExt;
use serde_json::Value;

use crate::{ServerInfo, paths::Paths, store::Credentials, util::err_str};

pub struct Client {
    pub base: String,
    token: String,
    http: reqwest::Client,
}

impl Client {
    pub fn locate(paths: &Paths, url: Option<String>, token: Option<String>) -> Result<Self, String> {
        let base = match url {
            Some(url) => url,
            None => {
                let info: ServerInfo = std::fs::read(paths.server_file())
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                    .ok_or_else(|| format!("dd-core isn't running for {} (start it with `dd-core serve`)", paths.data.display()))?;
                info.url
            }
        };
        let token = match token {
            Some(token) => token,
            None => {
                let text = std::fs::read_to_string(paths.credentials()).map_err(|e| format!("can't read {}: {e}", paths.credentials().display()))?;
                let creds: Credentials = toml::from_str(&text).map_err(err_str)?;
                creds.gateway.token
            }
        };
        let http = reqwest::Client::builder().no_proxy().build().map_err(err_str)?;
        Ok(Self { base: base.trim_end_matches('/').to_string(), token, http })
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Value, String> {
        let response = request.bearer_auth(&self.token).send().await.map_err(|e| format!("can't reach dd-core at {}: {e}", self.base))?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(err_str)?;
        let value: Value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap_or(Value::Null) };
        if status.is_success() {
            return Ok(value);
        }
        let message = value.pointer("/error/message").and_then(Value::as_str).map(str::to_owned);
        Err(message.unwrap_or_else(|| format!("HTTP {status}: {}", String::from_utf8_lossy(&bytes))))
    }

    pub async fn get(&self, path: &str) -> Result<Value, String> {
        self.send(self.http.get(format!("{}{path}", self.base))).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        self.send(self.http.post(format!("{}{path}", self.base)).json(&body)).await
    }

    /// Follows `/dd/events`, calling `on_event(name, data)` for each until the stream ends.
    pub async fn events(&self, mut on_event: impl FnMut(&str, Value)) -> Result<(), String> {
        let response = self
            .http
            .get(format!("{}/dd/events", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(err_str)?
            .error_for_status()
            .map_err(err_str)?;
        let mut stream = response.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk.map_err(err_str)?));
            while let Some(end) = buf.find("\n\n") {
                let block: String = buf.drain(..end + 2).collect();
                let mut name = "message";
                let mut data = String::new();
                for line in block.lines() {
                    if let Some(rest) = line.strip_prefix("event:") {
                        name = rest.trim();
                    } else if let Some(rest) = line.strip_prefix("data:") {
                        data.push_str(rest.trim_start());
                    }
                }
                if let Ok(value) = serde_json::from_str(&data) {
                    on_event(name, value);
                }
            }
        }
        Ok(())
    }
}
