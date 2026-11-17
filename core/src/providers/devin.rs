//! The Devin provider: Cognition's Cascade API over Connect-RPC with protobuf bodies — the
//! same endpoints the Devin CLI uses. Sign-in is the CLI's OAuth PKCE flow (browser to the
//! Devin sign-in page, code back to a loopback listener); the session token then
//! authenticates every call as `Basic <token>-<token>`.
//!
//! Wire format: unary calls are `application/proto` with a bare message; `GetChatMessage`
//! is `application/connect+proto` — envelopes `flag(1) + len(4, BE) + payload`, each
//! payload gzipped (`connect-content-encoding: gzip`), the last one a JSON trailer.

use std::{
    collections::HashMap,
    io::{Read as _, Write as _},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    body::{Body, Bytes},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest as _;
use tokio::sync::oneshot;

use super::{
    Account, Limit, Plan,
};
use crate::{
    events::{Event, Events},
    paths::{Paths, write_atomic},
    proto::{self, Msg, Val},
    store::{DevinCredentials as Credentials, Store},
    util::{iso, now_ms, random_hex},
};

/// Provider prefix of the model ids the gateway serves, e.g. `devin/swe-2-max`.
pub const PROVIDER: &str = "devin";
pub const PROVIDER_NAME: &str = "Devin";

const API_SERVER: &str = "https://server.codeium.com";
const DEVIN_API: &str = "https://api.devin.ai";
const AUTH_PAGE: &str = "https://app.devin.ai/auth/cli/continue";
/// Loopback port the Devin CLI registers for its OAuth callback.
const CALLBACK_PORT: u16 = 59653;
const TOKEN_PREFIX: &str = "devin-session-token$";
/// Client identity the API expects; version matches the captured CLI build.
const CLIENT_VERSION: &str = "3000.12.57";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// How long a fetched account answer is reused.
const ACCOUNT_TTL_MS: u64 = 30_000;

fn api_server(creds: &Credentials) -> &str {
    if creds.api_server_url.is_empty() { API_SERVER } else { &creds.api_server_url }
}

/// A remote model as the catalog lists it (cached in `<data>/core/cache/devin-models.json`).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DevinModel {
    pub uid: String,
    pub label: String,
    pub context: Option<u64>,
    /// Longest reply the model can give; caps `max_tokens` the client asks for.
    pub max_output: Option<u64>,
}

/// What clients see about the provider (`GET /dd/providers/devin`).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DevinStatus {
    pub signed_in: bool,
    pub email: Option<String>,
    pub models: usize,
    /// A sign-in is waiting on the browser.
    pub signing_in: bool,
    /// The sign-in page of the flow in progress.
    pub login_url: Option<String>,
}

struct Pending {
    cancel: oneshot::Sender<()>,
    url: String,
}

/// Remote provider state: credentials in the store, the fetched model list, an in-flight login.
#[derive(Clone)]
pub struct Devin {
    store: Arc<Store>,
    paths: Paths,
    events: Events,
    models: Arc<Mutex<Vec<DevinModel>>>,
    signing_in: Arc<Mutex<Option<Pending>>>,
    account: Arc<Mutex<Option<Account>>>,
    /// Remote calls go through the system proxy, unlike the gateway's loopback client.
    client: reqwest::Client,
}

fn err_str(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn uuid() -> String {
    let hex = random_hex(16);
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    let _ = encoder.write_all(data);
    encoder.finish().unwrap_or_default()
}

fn gunzip(data: &[u8]) -> Result<Vec<u8>, String> {
    if !data.starts_with(b"\x1f\x8b") {
        return Ok(data.to_vec());
    }
    let mut out = Vec::new();
    GzDecoder::new(data).read_to_end(&mut out).map_err(err_str)?;
    Ok(out)
}

/// `flag + len(4, BE) + payload`; `compressed` sets the gzip bit the server reads.
fn envelope(payload: &[u8], compressed: bool, trailer: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 5);
    out.push((compressed as u8) | ((trailer as u8) << 1));
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// `Metadata` for released-CLI calls (`GetChatMessage`, `GetUserStatus`).
fn cli_metadata(creds: &Credentials) -> Msg {
    let mut meta = Msg::new();
    meta.string(1, "devin-cli")
        .string(2, CLIENT_VERSION)
        .string(3, &creds.token)
        .string(4, "en")
        .string(5, "windows")
        .string(7, CLIENT_VERSION)
        .string(12, "chisel")
        .string(28, "chisel")
        .string(31, &creds.fingerprint);
    meta
}

/// `Metadata` for `GetCliModelConfigs`: the dev-channel identity unlocks the full catalog.
fn discovery_metadata(creds: &Credentials) -> Msg {
    let mut meta = Msg::new();
    meta.string(1, "chisel")
        .string(2, "0.0.0-dev")
        .string(3, &creds.token)
        .string(4, "en")
        .string(5, "windows")
        .string(12, "chisel")
        .string(31, &creds.fingerprint)
        .packed(30, &[3, 4, 6, 7, 8]);
    meta
}

fn auth_header(creds: &Credentials) -> String {
    format!("Basic {}-{}", creds.token, creds.token)
}

impl Devin {
    pub fn new(store: Arc<Store>, paths: Paths, events: Events) -> Self {
        let devin = Self {
            store,
            paths,
            events,
            models: Default::default(),
            signing_in: Default::default(),
            account: Default::default(),
            client: reqwest::Client::new(),
        };
        if let Ok(bytes) = std::fs::read(devin.models_path())
            && let Ok(saved) = serde_json::from_slice::<Vec<DevinModel>>(&bytes)
        {
            *devin.models.lock().unwrap_or_else(|e| e.into_inner()) = saved;
        }
        devin
    }

    fn changed(&self) {
        self.events.send(Event::Provider { id: PROVIDER.into() });
    }

    fn models_path(&self) -> PathBuf {
        self.paths.cache("devin-models.json")
    }

    pub fn creds(&self) -> Option<Credentials> {
        self.store.get().devin.filter(|c| !c.token.is_empty())
    }

    fn save_creds(&self, creds: &Credentials) -> Result<(), String> {
        self.store.update(|all| all.devin = Some(creds.clone()))
    }

    pub fn status(&self) -> DevinStatus {
        let creds = self.creds();
        let pending = self.signing_in.lock().unwrap_or_else(|e| e.into_inner());
        DevinStatus {
            signed_in: creds.is_some(),
            email: creds.and_then(|c| c.email),
            models: self.models.lock().unwrap_or_else(|e| e.into_inner()).len(),
            signing_in: pending.is_some(),
            login_url: pending.as_ref().map(|p| p.url.clone()),
        }
    }

    pub fn models(&self) -> Vec<DevinModel> {
        self.models.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Signs out: drops the credentials and the fetched model list.
    pub fn logout(&self) -> DevinStatus {
        self.cancel_login();
        if let Err(error) = self.store.update(|all| all.devin = None) {
            eprintln!("dd-core: can't save the sign-out: {error}");
        }
        let _ = std::fs::remove_file(self.models_path());
        self.models.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.account.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.changed();
        self.status()
    }

    pub fn cancel_login(&self) {
        if let Some(pending) = self.signing_in.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = pending.cancel.send(());
        }
    }

    /// The full PKCE sign-in: loopback listener, browser sign-in page, token exchange.
    /// `open` hands the sign-in URL to the user's browser; an error aborts the login.
    pub async fn login(&self, open: impl FnOnce(&str) -> Result<(), String>) -> Result<DevinStatus, String> {
        if self.signing_in.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
            return Err("sign-in is already in progress".into());
        }
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", CALLBACK_PORT)).await {
            Ok(listener) => listener,
            Err(_) => tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|e| format!("can't listen on 127.0.0.1: {e}"))?,
        };
        let port = listener.local_addr().map_err(err_str)?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");

        let verifier = random_hex(48); // 96 chars, within the 43..128 the spec allows
        let challenge = URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
        let state = random_hex(16);
        let url = format!(
            "{AUTH_PAGE}?redirect_uri={}&state={state}&prompt=select_account\
             &code_challenge={challenge}&code_challenge_method=S256&cli_pkce_marker=1",
            percent_encoding::utf8_percent_encode(&redirect_uri, percent_encoding::NON_ALPHANUMERIC),
        );

        let (cancel, cancelled) = oneshot::channel();
        *self.signing_in.lock().unwrap_or_else(|e| e.into_inner()) = Some(Pending { cancel, url: url.clone() });
        self.changed();
        if let Err(error) = open(&url) {
            self.signing_in.lock().unwrap_or_else(|e| e.into_inner()).take();
            self.changed();
            return Err(error);
        }
        let result = self.wait_for_code(&listener, &state, cancelled).await;
        self.signing_in.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.changed();

        let code = result?;
        let mut creds = self.exchange(&code, &verifier, &redirect_uri).await?;
        if creds.fingerprint.is_empty() {
            creds.fingerprint = random_hex(366); // 732 hex chars, like the CLI's own
        }
        // Email and the user ref ride along for the account card and the chat envelope.
        if let Ok(info) = self.user_status(&creds).await {
            creds.email = info.0;
            creds.user_id = info.1.clone();
            if let Some(id) = info.1.filter(|id| !id.is_empty()) {
                creds.user_ref = Some(format!("user-{id}:buttercup-bite"));
            }
        }
        self.save_creds(&creds)?;
        let _ = self.refresh_models().await;
        self.changed();
        Ok(self.status())
    }

    /// Waits for the browser to redirect to the loopback listener; answers a friendly page.
    async fn wait_for_code(
        &self,
        listener: &tokio::net::TcpListener,
        state: &str,
        mut cancelled: oneshot::Receiver<()>,
    ) -> Result<String, String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let deadline = Instant::now() + LOGIN_TIMEOUT;
        loop {
            let accept = tokio::time::timeout_at(deadline.into(), listener.accept());
            let mut stream = tokio::select! {
                _ = &mut cancelled => return Err("sign-in cancelled".into()),
                result = accept => result.map_err(|_| "sign-in timed out".to_string())?.map_err(err_str)?.0,
            };
            let mut buf = vec![0u8; 8192];
            let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await.map_err(err_str)?.map_err(err_str)?;
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let path = request.split_whitespace().nth(1).unwrap_or("");
            let reply = |title: &str, text: &str| {
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\nconnection: close\r\n\r\n\
                     <!doctype html><title>{title}</title><body style=\"font-family:system-ui;margin:3em\">\
                     <h3>{title}</h3><p>{text}</p></body>"
                )
            };
            let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
            let params: HashMap<String, String> = percent_encoding::percent_decode_str(query)
                .decode_utf8_lossy()
                .split('&')
                .filter_map(|pair| pair.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
                .collect();
            if let Some(error) = params.get("error").filter(|e| !e.is_empty()) {
                let _ = stream.write_all(reply("Sign-in failed", error).as_bytes()).await;
                return Err(format!("sign-in failed: {error}"));
            }
            let ok = params.get("state").is_some_and(|s| s == state);
            let (title, text) = if ok {
                ("Signed in to Devin", "You can close this tab and go back to Double Damage.")
            } else {
                ("Sign-in failed", "State mismatch — try signing in again.")
            };
            let _ = stream.write_all(reply(title, text).as_bytes()).await;
            if ok
                && let Some(code) = params.get("code")
            {
                return Ok(code.clone());
            }
        }
    }

    /// Swaps the authorization code for a session token: the CLI's REST exchange first,
    /// then the Connect-RPC one as a fallback.
    async fn exchange(&self, code: &str, verifier: &str, redirect_uri: &str) -> Result<Credentials, String> {
        let body = json!({ "code": code, "code_verifier": verifier, "redirect_uri": redirect_uri });
        let rest = self
            .client
            .post(format!("{DEVIN_API}/auth/cli/token"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await;
        let mut http_error = String::new();
        if let Ok(response) = rest {
            if response.status().is_success() {
                if let Ok(value) = serde_json::from_slice::<Value>(&response.bytes().await.map_err(err_str)?) {
                    let token = ["token", "api_key", "apiKey", "session_token", "sessionToken"]
                        .iter()
                        .find_map(|key| value.get(*key).and_then(|v| v.as_str()))
                        .unwrap_or("");
                    if !token.is_empty() {
                        let api_server_url = ["api_server_url", "apiServerUrl"]
                            .iter()
                            .find_map(|key| value.get(*key).and_then(|v| v.as_str()))
                            .unwrap_or("")
                            .to_string();
                        return Ok(Credentials { token: normalize_token(token), api_server_url, ..Default::default() });
                    }
                }
            } else {
                http_error = format!("HTTP {}", response.status());
            }
        }

        // Connect-RPC exchange on the API server (protobuf body).
        let mut req = Msg::new();
        req.string(1, code).string(2, verifier);
        let req = req.into_vec();
        for rpc in ["ExchangeDevinCLIPKCECode", "ExchangePKCEAuthorizationCode"] {
            let path = format!("/exa.seat_management_pb.SeatManagementService/{rpc}");
            let response = self
                .client
                .post(format!("{API_SERVER}{path}"))
                .header(header::CONTENT_TYPE, "application/proto")
                .header("connect-protocol-version", "1")
                .body(req.clone())
                .send()
                .await;
            let Ok(response) = response else { continue };
            if !response.status().is_success() {
                continue;
            }
            let bytes = response.bytes().await.map_err(err_str)?;
            let token = proto::get(&bytes, 1).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
            if token.is_empty() {
                continue;
            }
            return Ok(Credentials {
                token: normalize_token(&token),
                api_server_url: proto::get(&bytes, 3).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default(),
                ..Default::default()
            });
        }
        Err(if http_error.is_empty() { "token exchange failed".into() } else { format!("token exchange failed ({http_error})") })
    }

    /// A unary Connect call: bare protobuf body in, bare protobuf body out.
    async fn unary(&self, creds: &Credentials, path: &str, request: Msg) -> Result<Vec<u8>, String> {
        let response = self
            .client
            .post(format!("{}{path}", api_server(creds)))
            .header(header::AUTHORIZATION, auth_header(creds))
            .header(header::CONTENT_TYPE, "application/proto")
            .header("connect-protocol-version", "1")
            .header(header::ACCEPT, "*/*")
            .body(request.into_vec())
            .send()
            .await
            .map_err(err_str)?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(err_str)?;
        if !status.is_success() {
            return Err(connect_error(status, &gunzip(&bytes).unwrap_or_else(|_| bytes.to_vec())));
        }
        Ok(bytes.to_vec())
    }

    /// GetUserStatus → (email, user_id). Best effort: callers tolerate a failure.
    async fn user_status(&self, creds: &Credentials) -> Result<(Option<String>, Option<String>), String> {
        let mut req = Msg::new();
        req.msg(1, &cli_metadata(creds));
        let bytes = self.unary(creds, "/exa.seat_management_pb.SeatManagementService/GetUserStatus", req).await?;
        let Some(Val::Bytes(status)) = proto::get(&bytes, 1) else { return Ok((None, None)) };
        let email = proto::get(status, 7).and_then(|v| v.as_str().map(str::to_owned));
        let user_id = proto::get(status, 36).and_then(|v| v.as_str().map(str::to_owned));
        Ok((email, user_id))
    }

    /// GetCliModelConfigs → the account's model catalog; falls back to a small built-in list.
    pub async fn refresh_models(&self) -> Result<usize, String> {
        let Some(creds) = self.creds() else { return Err("not signed in".into()) };
        let mut req = Msg::new();
        req.msg(1, &discovery_metadata(&creds));
        let result = self.unary(&creds, "/exa.api_server_pb.ApiServerService/GetCliModelConfigs", req).await;
        let models = match result {
            Ok(bytes) => {
                let parsed = parse_model_configs(&bytes);
                if parsed.is_empty() { fallback_models() } else { parsed }
            }
            Err(_) => fallback_models(),
        };
        let count = models.len();
        let _ = write_atomic(&self.models_path(), &serde_json::to_vec_pretty(&models).unwrap_or_default());
        *self.models.lock().unwrap_or_else(|e| e.into_inner()) = models;
        self.changed();
        Ok(count)
    }

    /// The plan (name, period) and the daily / weekly quotas from `GetUserStatus`.
    /// Reused for a while unless `fresh`.
    pub async fn account(&self, fresh: bool) -> Result<Account, String> {
        if !fresh
            && let Some(cached) = self.account.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
            && now_ms().saturating_sub(cached.fetched_at) < ACCOUNT_TTL_MS
        {
            return Ok(cached.clone());
        }
        let creds = self.creds().ok_or("not signed in")?;
        let mut req = Msg::new();
        req.msg(1, &cli_metadata(&creds));
        let bytes = self.unary(&creds, "/exa.seat_management_pb.SeatManagementService/GetUserStatus", req).await?;
        let bytes = gunzip(&bytes)?;
        let account = account_from(&bytes).ok_or("GetUserStatus returned no user status")?;
        *self.account.lock().unwrap_or_else(|e| e.into_inner()) = Some(account.clone());
        Ok(account)
    }
}

/// `{1: secs, 2: nanos}` → ISO time.
fn timestamp(value: Option<Val<'_>>) -> Option<String> {
    let Some(Val::Bytes(ts)) = value else { return None };
    let secs = proto::get(ts, 1)?.as_u64();
    (secs > 0).then(|| iso(secs as i64))
}

/// Unix seconds as a varint → ISO time.
fn unix_secs(value: Option<Val<'_>>) -> Option<String> {
    let secs = value?.as_u64();
    (secs > 0 && secs < u64::MAX).then(|| iso(secs as i64))
}

/// `GetUserStatusResponse` → the normalized account. Field numbers from the captured CLI traffic:
///
/// ```text
/// 1: UserStatus { 3: username, 5: team id, 7: email, 10: teams tier,
///                 13: PlanStatus { 1: PlanInfo { 1: tier, 2: plan name },
///                                  2: plan start {1: secs}, 3: plan end {1: secs},
///                                  14: daily quota left (%), 15: weekly quota left (%),
///                                  17: daily reset (unix secs), 18: weekly reset (unix secs) } }
/// ```
///
/// A quota left at 0 is dropped from the wire (proto3), so a reset time without its percentage
/// means the quota is used up.
fn account_from(response: &[u8]) -> Option<Account> {
    let Some(Val::Bytes(status)) = proto::get(response, 1) else { return None };
    let text = |msg: &[u8], field: u32| proto::get(msg, field).and_then(|v| v.as_str().map(str::to_owned)).filter(|s| !s.is_empty());
    let plan_status = match proto::get(status, 13) {
        Some(Val::Bytes(plan)) => plan,
        _ => &[][..],
    };
    let plan_info = match proto::get(plan_status, 1) {
        Some(Val::Bytes(info)) => info,
        _ => &[][..],
    };

    let mut limits = Vec::new();
    for (id, label, left_field, reset_field) in [("daily", "Daily quota", 14, 17), ("weekly", "Weekly quota", 15, 18)] {
        let left = proto::get(plan_status, left_field).map(|v| v.as_u64());
        let resets_at = unix_secs(proto::get(plan_status, reset_field));
        if left.is_none() && resets_at.is_none() {
            continue;
        }
        let left = left.unwrap_or(0).min(100) as f64;
        limits.push(Limit {
            id: id.into(),
            label: label.into(),
            used_percent: Some(100.0 - left),
            resets_at,
            status: None,
        });
    }

    // The bulky model catalog (field 33) and team config are left out of the raw dump.
    let raw_status: serde_json::Map<String, serde_json::Value> = match proto::to_json(status, 0) {
        serde_json::Value::Object(map) => map.into_iter().filter(|(k, _)| k != "32" && k != "33").collect(),
        _ => Default::default(),
    };
    Some(Account {
        provider: PROVIDER.into(),
        email: text(status, 7),
        // PlanInfo.33 is the team: {4: org id, 8: team name, …}.
        organization: match proto::get(plan_info, 33) {
            Some(Val::Bytes(team)) => text(team, 8),
            _ => None,
        },
        plan: Plan {
            name: text(plan_info, 2),
            tier: proto::get(plan_info, 1).map(|v| v.as_u64().to_string()),
            billing: None,
            started_at: timestamp(proto::get(plan_status, 2)),
            expires_at: timestamp(proto::get(plan_status, 3)),
            trial_ends_at: None,
        },
        balance: None,
        limits,
        fetched_at: now_ms(),
        raw: serde_json::json!({
            "userStatus": raw_status,
            "planStatus": proto::to_json(plan_status, 4),
        }),
    })
}

/// `devin-session-token$…` is the shape every call expects; bare JWTs get the prefix.
fn normalize_token(token: &str) -> String {
    let token = token.trim();
    if token.starts_with(TOKEN_PREFIX) || token.split('.').count() != 3 {
        token.to_string()
    } else {
        format!("{TOKEN_PREFIX}{token}")
    }
}

/// Reads `repeated ClientModelConfig` entries out of the GetCliModelConfigs response.
fn parse_model_configs(bytes: &[u8]) -> Vec<DevinModel> {
    let mut models = Vec::new();
    for entry in proto::get_all(bytes, 1) {
        let Val::Bytes(config) = entry else { continue };
        let disabled = proto::get(config, 4).is_some_and(|v| v.as_u64() != 0);
        let uid = proto::get(config, 22).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
        if disabled || uid.is_empty() {
            continue;
        }
        let info = proto::get(config, 23);
        let (context, max_output) = match info {
            Some(Val::Bytes(info)) => (
                proto::get(info, 4).map(|v| v.as_u64()),
                proto::get(info, 13).map(|v| v.as_u64()),
            ),
            _ => (None, None),
        };
        let label = proto::get(config, 1).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_else(|| uid.clone());
        models.push(DevinModel { uid, label, context, max_output });
    }
    models.sort_by_key(|m| m.label.to_lowercase());
    models
}

/// Used until the live catalog arrives (or when it can't be fetched).
fn fallback_models() -> Vec<DevinModel> {
    [
        ("swe-2-max", "SWE-2 Max", 262000, 128000),
        ("swe-2-high", "SWE-2 High", 262000, 128000),
        ("swe-2-medium", "SWE-2 Medium", 262000, 128000),
        ("swe-2-max-lightning", "SWE-2 Max Lightning", 262144, 128000),
        ("swe-2-high-lightning", "SWE-2 High Lightning", 262144, 128000),
        ("swe-2-medium-lightning", "SWE-2 Medium Lightning", 262144, 128000),
        ("swe-2-flash-max", "SWE-2 Flash Max", 262144, 131072),
        ("swe-2-flash-high", "SWE-2 Flash High", 262144, 131072),
        ("swe-2-flash-medium", "SWE-2 Flash Medium", 262144, 131072),
        ("claude-fable-5-2-max", "Claude Fable 5.2 Max", 1000000, 128000),
        ("claude-fable-5-2-high", "Claude Fable 5.2 High", 1000000, 128000),
        ("claude-fable-5-2-medium", "Claude Fable 5.2 Medium", 1000000, 128000),
        ("claude-fable-5-2-low", "Claude Fable 5.2 Low", 1000000, 128000),
    ]
    .into_iter()
    .map(|(uid, label, context, max_output)| DevinModel {
        uid: uid.into(),
        label: label.into(),
        context: Some(context),
        max_output: Some(max_output),
    })
    .collect()
}

// --- chat --------------------------------------------------------------------

/// Connect's JSON error body on a failed call (or the end-of-stream trailer's `error`).
fn connect_error(status: StatusCode, body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    if let Ok(value) = serde_json::from_str::<Value>(&text) {
        let error = value.get("error").unwrap_or(&value);
        if let Some(message) = error.get("message").and_then(|m| m.as_str()) {
            let code = error.get("code").and_then(|c| c.as_str()).unwrap_or("");
            return if code.is_empty() { message.to_string() } else { format!("{code}: {message}") };
        }
    }
    let text = text.trim();
    if text.is_empty() { format!("HTTP {status}") } else { text.chars().take(400).collect() }
}

/// Anthropic `system` (a string or a list of text blocks) → one prompt string.
fn system_text(system: &Value) -> String {
    match system {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// Content of a message as blocks (the API also accepts a bare string).
fn content_blocks<'a>(message: &'a Value) -> Vec<Value> {
    match message.get("content") {
        Some(Value::String(text)) => vec![json!({ "type": "text", "text": text })],
        Some(Value::Array(blocks)) => blocks.clone(),
        _ => Vec::new(),
    }
}

fn block_text(block: &Value) -> String {
    block.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string()
}

fn encode_images(prompt: &mut Msg, block: &Value) {
    // Anthropic: {type:"image", source:{type:"base64", media_type, data}} → ImageData{1,2}
    let source = block.get("source").unwrap_or(&Value::Null);
    let data = source.get("data").and_then(|d| d.as_str()).unwrap_or("");
    let mime = source.get("media_type").and_then(|m| m.as_str()).unwrap_or("");
    if data.is_empty() {
        return;
    }
    let mut image = Msg::new();
    image.string(1, data).string(2, mime);
    prompt.msg(10, &image);
}

/// One conversation entry → `ChatMessagePrompt` (field 3 of the request).
/// Roles: USER = 1, SYSTEM (assistant) = 2, TOOL = 4.
fn chat_prompt(message: &Value, out: &mut Vec<Msg>) {
    let role = message.get("role").and_then(|r| r.as_str()).unwrap_or("user");
    let blocks = content_blocks(message);
    if role == "assistant" {
        let mut text = String::new();
        let mut thinking = String::new();
        let mut signature = String::new();
        let mut calls = Vec::new();
        for block in &blocks {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&block_text(block));
                }
                Some("thinking") => {
                    if !thinking.is_empty() {
                        thinking.push('\n');
                    }
                    thinking.push_str(block.get("thinking").and_then(|t| t.as_str()).unwrap_or(""));
                    if let Some(sig) = block.get("signature").and_then(|s| s.as_str()).filter(|s| !s.is_empty()) {
                        signature = sig.to_string();
                    }
                }
                Some("tool_use") => {
                    let mut call = Msg::new();
                    call.string(1, block.get("id").and_then(|v| v.as_str()).unwrap_or(""))
                        .string(2, block.get("name").and_then(|v| v.as_str()).unwrap_or(""))
                        .string(3, &serde_json::to_string(block.get("input").unwrap_or(&json!({}))).unwrap_or_default());
                    calls.push(call);
                }
                _ => {}
            }
        }
        if text.is_empty() && thinking.is_empty() && calls.is_empty() {
            return;
        }
        let mut prompt = Msg::new();
        prompt.string(1, &uuid()).varint(2, 2).string(3, &text);
        for call in &calls {
            prompt.msg(6, call);
        }
        prompt.string(11, &thinking);
        if !signature.is_empty() {
            prompt.string(12, &signature).string(18, "sealed");
        }
        out.push(prompt);
        return;
    }

    // User turn: text and images form a USER prompt; each tool_result is its own TOOL prompt.
    let mut pending_text = String::new();
    let mut pending = Msg::new();
    let mut has_pending = false;
    let flush = |pending: &mut Msg, text: &mut String, has: &mut bool, out: &mut Vec<Msg>| {
        if !*has && text.is_empty() {
            return;
        }
        pending.string(1, &uuid()).varint(2, 1).string(3, text);
        out.push(std::mem::take(pending));
        text.clear();
        *has = false;
    };
    for block in &blocks {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if !pending_text.is_empty() {
                    pending_text.push('\n');
                }
                pending_text.push_str(&block_text(block));
            }
            Some("image") => {
                encode_images(&mut pending, block);
                has_pending = true;
            }
            Some("tool_result") => {
                flush(&mut pending, &mut pending_text, &mut has_pending, out);
                let mut prompt = Msg::new();
                prompt.string(1, &uuid()).varint(2, 4);
                let content = block.get("content").unwrap_or(&Value::Null);
                let text = match content {
                    Value::String(text) => text.clone(),
                    Value::Array(parts) => parts
                        .iter()
                        .filter_map(|part| {
                            if part.get("type").and_then(|t| t.as_str()) == Some("image") {
                                encode_images(&mut prompt, part);
                                None
                            } else {
                                part.get("text").and_then(|t| t.as_str()).map(str::to_owned)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                };
                prompt
                    .string(3, &text)
                    .string(7, block.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or(""));
                if block.get("is_error").and_then(|v| v.as_bool()) == Some(true) {
                    prompt.varint(9, 1);
                }
                out.push(prompt);
            }
            _ => {}
        }
    }
    flush(&mut pending, &mut pending_text, &mut has_pending, out);
}

/// Builds the `GetChatMessageRequest` protobuf from an Anthropic Messages body.
fn build_request(creds: &Credentials, model_uid: &str, body: &Value, max_output: Option<u64>) -> Msg {
    let mut request = Msg::new();
    request.msg(1, &cli_metadata(creds));
    request.string(2, &system_text(body.get("system").unwrap_or(&Value::Null)));

    let mut prompts = Vec::new();
    for message in body.get("messages").and_then(|m| m.as_array()).map(|m| m.as_slice()).unwrap_or_default() {
        chat_prompt(message, &mut prompts);
    }
    // The cache breakpoint marks the end of the conversation, like the CLI does.
    if let Some(last) = prompts.last_mut() {
        let mut cache = Msg::new();
        cache.varint(1, 1);
        last.msg(8, &cache);
    }
    for prompt in &prompts {
        request.msg(3, prompt);
    }

    request.varint(7, 5); // CHAT_MESSAGE_REQUEST_TYPE_CASCADE
    let mut config = Msg::new();
    let pick_f64 = |key: &str, default: f64| body.get(key).and_then(|v| v.as_f64()).unwrap_or(default);
    let pick_u64 = |key: &str, default: u64| body.get(key).and_then(|v| v.as_u64()).unwrap_or(default);
    let cap = max_output.unwrap_or(128000);
    config
        .varint(1, 1)
        .varint(2, pick_u64("max_tokens", cap).min(cap))
        .varint(3, 400)
        .f64(5, pick_f64("temperature", 1.0))
        .varint(7, pick_u64("top_k", 40))
        .f64(8, pick_f64("top_p", 0.95));
    for stop in body.get("stop_sequences").and_then(|s| s.as_array()).map(|s| s.as_slice()).unwrap_or_default() {
        if let Some(stop) = stop.as_str() {
            config.string(9, stop);
        }
    }
    request.msg(8, &config);

    for tool in body.get("tools").and_then(|t| t.as_array()).map(|t| t.as_slice()).unwrap_or_default() {
        let mut def = Msg::new();
        def.string(1, tool.get("name").and_then(|v| v.as_str()).unwrap_or(""))
            .string(2, tool.get("description").and_then(|v| v.as_str()).unwrap_or(""))
            .string(3, &serde_json::to_string(tool.get("input_schema").unwrap_or(&json!({}))).unwrap_or_default());
        request.msg(10, &def);
    }

    let mut trajectory = Msg::new(); // trajectory_reference: CASCADE / USER_INPUT, as captured
    trajectory.varint(3, 4).varint(4, 14);
    request.msg(15, &trajectory);
    request.string(16, &uuid()); // cascade_id
    request.varint(20, 1); // CONVERSATIONAL_PLANNER_MODE_DEFAULT
    request.string(21, model_uid);
    request.string(22, &uuid()); // execution_id
    if let Some(user_ref) = &creds.user_ref {
        request.string(28, user_ref);
    }
    request
}

// --- stream translation --------------------------------------------------------

enum Block {
    Thinking { index: usize },
    Text { index: usize },
}

/// Turns `GetChatMessageResponse` deltas into Anthropic SSE events. Blocks open lazily in
/// arrival order (thinking, then text, then tool calls) and close as the next one opens;
/// tool_use blocks all close at the end.
struct Streamer {
    out: Vec<String>,
    message: Option<String>,
    next_index: usize,
    open: Option<Block>,
    tools: HashMap<String, (usize, String)>,
    tool_order: Vec<String>,
    last_tool: Option<String>,
    stop_reason: Option<u64>,
    input: u64,
    output: u64,
    cache_read: u64,
}

impl Streamer {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            message: None,
            next_index: 0,
            open: None,
            tools: HashMap::new(),
            tool_order: Vec::new(),
            last_tool: None,
            stop_reason: None,
            input: 0,
            output: 0,
            cache_read: 0,
        }
    }

    fn sse(&mut self, event: &str, data: Value) {
        self.out.push(format!("event: {event}\ndata: {data}\n\n"));
    }

    /// `message_start` rides on the first frame that knows anything — usage arrives
    /// ahead of the first text, so input tokens usually make it in.
    fn ensure_started(&mut self, message_id: &str) {
        if self.message.is_none() {
            self.message = Some(message_id.to_string());
            self.sse(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": message_id, "type": "message", "role": "assistant", "content": [],
                        "model": "devin", "stop_reason": null, "stop_sequence": null,
                        "usage": { "input_tokens": self.input, "cache_read_input_tokens": self.cache_read, "output_tokens": 0 },
                    },
                }),
            );
        }
    }

    fn close_open(&mut self) {
        if let Some(block) = self.open.take() {
            let index = match &block {
                Block::Thinking { index } | Block::Text { index } => *index,
            };
            self.sse("content_block_stop", json!({ "type": "content_block_stop", "index": index }));
        }
    }

    /// Opens a thinking or text block (or keeps the one that's open) and returns its index.
    fn open_block(&mut self, want: &'static str) -> usize {
        let matches = matches!(
            (&self.open, want),
            (Some(Block::Thinking { .. }), "thinking") | (Some(Block::Text { .. }), "text")
        );
        if matches {
            return match self.open.as_ref().unwrap() {
                Block::Thinking { index } | Block::Text { index } => *index,
            };
        }
        self.close_open();
        let index = self.next_index;
        self.next_index += 1;
        let block = match want {
            "thinking" => json!({ "type": "thinking", "thinking": "" }),
            _ => json!({ "type": "text", "text": "" }),
        };
        self.sse("content_block_start", json!({ "type": "content_block_start", "index": index, "content_block": block }));
        self.open = Some(if want == "thinking" { Block::Thinking { index } } else { Block::Text { index } });
        index
    }

    fn delta(&mut self, index: usize, delta: Value) {
        self.sse("content_block_delta", json!({ "type": "content_block_delta", "index": index, "delta": delta }));
    }

    /// One `GetChatMessageResponse` frame.
    fn frame(&mut self, payload: &[u8]) {
        let fields: Vec<(u32, Val)> = proto::fields(payload).collect();
        let get = |want: u32| fields.iter().find(|(f, _)| *f == want).map(|(_, v)| v);
        if let Some(usage) = get(7) {
            if let Val::Bytes(usage) = usage {
                self.input = proto::get(usage, 2).map(|v| v.as_u64()).unwrap_or(self.input);
                self.output = proto::get(usage, 3).map(|v| v.as_u64()).unwrap_or(self.output);
                self.cache_read = proto::get(usage, 5).map(|v| v.as_u64()).unwrap_or(self.cache_read);
            }
        }
        let id = get(1).and_then(|v| v.as_str()).filter(|id| !id.is_empty()).unwrap_or("msg_devin");
        self.ensure_started(id);
        if let Some(thinking) = get(9).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            let index = self.open_block("thinking");
            self.delta(index, json!({ "type": "thinking_delta", "thinking": thinking }));
        }
        if let Some(signature) = get(10).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            let index = self.open_block("thinking");
            self.delta(index, json!({ "type": "signature_delta", "signature": signature }));
        }
        if let Some(text) = get(3).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            let index = self.open_block("text");
            self.delta(index, json!({ "type": "text_delta", "text": text }));
        }
        for call in fields.iter().filter(|(f, _)| *f == 6) {
            let Val::Bytes(call) = &call.1 else { continue };
            let id = proto::get(call, 1)
                .and_then(|v| v.as_str().map(str::to_owned))
                .filter(|id| !id.is_empty())
                .or_else(|| self.last_tool.clone())
                .unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            if !self.tools.contains_key(&id)
                && let Some(name) = proto::get(call, 2).and_then(|v| v.as_str()).filter(|n| !n.is_empty())
            {
                // A new id opens a new tool_use block.
                self.close_open();
                let index = self.next_index;
                self.next_index += 1;
                self.sse(
                    "content_block_start",
                    json!({
                        "type": "content_block_start", "index": index,
                        "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} },
                    }),
                );
                self.tools.insert(id.clone(), (index, String::new()));
                self.tool_order.push(id.clone());
            }
            self.last_tool = Some(id.clone());
            if let Some(args) = proto::get(call, 3).and_then(|v| v.as_str()).filter(|a| !a.is_empty()) {
                if let Some((index, accumulated)) = self.tools.get_mut(&id) {
                    // The server may resend the whole JSON or only the new tail.
                    let delta = args.strip_prefix(accumulated.as_str()).unwrap_or(args);
                    *accumulated = if delta.len() == args.len() { format!("{accumulated}{args}") } else { args.to_string() };
                    let index = *index;
                    self.delta(index, json!({ "type": "input_json_delta", "partial_json": delta }));
                }
            }
        }
        if let Some(reason) = get(5).map(|v| v.as_u64()) {
            self.stop_reason = Some(reason);
        }
    }

    /// Closes whatever is open and emits the trailing events.
    fn finish(mut self) -> Vec<String> {
        let id = self.message.clone().unwrap_or_else(|| format!("msg_{}", random_hex(12)));
        self.ensure_started(&id);
        self.close_open();
        for id in self.tool_order.clone() {
            if let Some((index, _)) = self.tools.get(&id) {
                self.sse("content_block_stop", json!({ "type": "content_block_stop", "index": index }));
            }
        }
        let stop = if !self.tool_order.is_empty() {
            "tool_use"
        } else {
            match self.stop_reason {
                Some(3) => "max_tokens",
                Some(10) => "tool_use",
                _ => "end_turn",
            }
        };
        self.sse(
            "message_delta",
            json!({ "type": "message_delta", "delta": { "stop_reason": stop }, "usage": { "output_tokens": self.output } }),
        );
        self.sse("message_stop", json!({ "type": "message_stop" }));
        self.out
    }
}

/// Anthropic-shaped error so the chat surfaces it like any other API failure.
fn error_response(status: StatusCode, kind: &str, message: impl Into<String>) -> Response {
    (
        status,
        axum::Json(json!({
            "type": "error",
            "error": { "type": kind, "message": message.into(), "code": kind },
        })),
    )
        .into_response()
}

/// `/v1/messages` for a `devin/*` model: protobuf out to Cascade, Anthropic SSE back.
pub async fn chat(devin: &Devin, model: &str, body: &Value) -> Response {
    let Some(creds) = devin.creds() else {
        return error_response(StatusCode::UNAUTHORIZED, "authentication_error", "sign in to Devin first (Settings → Providers)");
    };
    let model_uid = model.strip_prefix("devin/").unwrap_or(model);
    let max_output = devin.models().iter().find(|m| m.uid == model_uid).and_then(|m| m.max_output);
    let request = build_request(&creds, model_uid, body, max_output);
    let streamed = body.get("stream").and_then(|v| v.as_bool()) == Some(true);

    let upstream = devin
        .client
        .post(format!("{}/exa.api_server_pb.ApiServerService/GetChatMessage", api_server(&creds)))
        .header(header::AUTHORIZATION, auth_header(&creds))
        .header(header::CONTENT_TYPE, "application/connect+proto")
        .header("connect-content-encoding", "gzip")
        .header("connect-protocol-version", "1")
        .header(header::ACCEPT, "*/*")
        .header("sentry-trace", format!("{}-{}-0", random_hex(16), random_hex(8)))
        .body(envelope(&gzip(&request.into_vec()), true, false))
        .send()
        .await;
    let upstream = match upstream {
        Ok(upstream) => upstream,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, "api_error", format!("Devin API unreachable: {e}")),
    };
    let status = upstream.status();
    if !status.is_success() {
        let bytes = upstream.bytes().await.unwrap_or_default();
        let text = gunzip(&bytes).unwrap_or_else(|_| bytes.to_vec());
        return error_response(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            if status.as_u16() == 401 || status.as_u16() == 403 { "authentication_error" } else { "api_error" },
            connect_error(status, &text),
        );
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<Bytes, std::convert::Infallible>>();
    let mut stream = upstream.bytes_stream();
    tokio::spawn(async move {
        let mut streamer = Streamer::new();
        let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
        let mut trailer_error: Option<String> = None;
        'read: while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else { break };
            buf.extend_from_slice(&chunk);
            loop {
                if buf.len() < 5 {
                    break;
                }
                let flag = buf[0];
                let len = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
                if buf.len() < 5 + len {
                    break;
                }
                let payload = buf[5..5 + len].to_vec();
                buf.drain(..5 + len);
                let payload = match if flag & 1 != 0 { gunzip(&payload) } else { Ok(payload) } {
                    Ok(payload) => payload,
                    Err(e) => {
                        trailer_error = Some(format!("can't decode stream: {e}"));
                        break 'read;
                    }
                };
                if flag & 2 != 0 {
                    // End-of-stream trailer: {} is fine, {"error": …} is a late failure.
                    if let Ok(value) = serde_json::from_slice::<Value>(&payload)
                        && let Some(error) = value.get("error")
                    {
                        let message = error.get("message").and_then(|m| m.as_str()).unwrap_or("stream error");
                        let code = error.get("code").and_then(|c| c.as_str()).unwrap_or("");
                        trailer_error = Some(if code.is_empty() { message.into() } else { format!("{code}: {message}") });
                    }
                    break 'read;
                }
                streamer.frame(&payload);
                for event in streamer.out.drain(..) {
                    let _ = tx.send(Ok(Bytes::from(event)));
                }
            }
        }
        if let Some(error) = trailer_error {
            let _ = tx.send(Ok(Bytes::from(format!(
                "event: error\ndata: {}\n\n",
                json!({ "type": "error", "error": { "type": "api_error", "message": error } })
            ))));
        }
        for event in streamer.finish() {
            let _ = tx.send(Ok(Bytes::from(event)));
        }
    });

    if streamed {
        let body = Body::from_stream(stream::poll_fn(move |cx| rx.poll_recv(cx)));
        let mut response = Response::new(body);
        *response.status_mut() = StatusCode::OK;
        response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
        response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        response
    } else {
        // Non-streaming: gather the SSE text and re-shape it into one message.
        let mut sse = String::new();
        while let Some(Ok(bytes)) = rx.recv().await {
            sse.push_str(&String::from_utf8_lossy(&bytes));
        }
        collect_response(&sse)
    }
}

/// Rebuilds a non-streamed Anthropic message from the SSE a stream would have carried.
fn collect_response(sse: &str) -> Response {
    fn at<'a>(blocks: &'a mut Vec<Value>, index: usize) -> &'a mut Value {
        while blocks.len() <= index {
            blocks.push(Value::Null);
        }
        &mut blocks[index]
    }
    let mut blocks: Vec<Value> = Vec::new();
    let mut partial: HashMap<usize, String> = HashMap::new();
    let mut output = 0u64;
    let mut input = 0u64;
    let mut stop = "end_turn".to_string();
    let mut error: Option<String> = None;
    for chunk in sse.split("\n\n") {
        let data = chunk.lines().find_map(|line| line.strip_prefix("data:").map(str::trim));
        let Some(data) = data else { continue };
        let Ok(event) = serde_json::from_str::<Value>(data) else { continue };
        let index = event.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        match event.get("type").and_then(|t| t.as_str()) {
            Some("message_start") => {
                input = event.pointer("/message/usage/input_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
                    + event.pointer("/message/usage/cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            }
            Some("content_block_start") => {
                if let Some(block) = event.get("content_block") {
                    *at(&mut blocks, index) = block.clone();
                }
            }
            Some("content_block_delta") => {
                let delta = event.get("delta").cloned().unwrap_or_default();
                let block = at(&mut blocks, index);
                if !block.is_object() {
                    continue;
                }
                match delta.get("type").and_then(|t| t.as_str()) {
                    Some("text_delta") | Some("thinking_delta") => {
                        let (key, text) = if delta.get("type").and_then(|t| t.as_str()) == Some("text_delta") {
                            ("text", delta.get("text").and_then(|t| t.as_str()).unwrap_or(""))
                        } else {
                            ("thinking", delta.get("thinking").and_then(|t| t.as_str()).unwrap_or(""))
                        };
                        let existing = block[key].as_str().unwrap_or("").to_string();
                        block[key] = json!(existing + text);
                    }
                    Some("signature_delta") => {
                        block["signature"] = delta.get("signature").cloned().unwrap_or_default();
                    }
                    Some("input_json_delta") => {
                        *partial.entry(index).or_default() += delta.get("partial_json").and_then(|t| t.as_str()).unwrap_or("");
                    }
                    _ => {}
                }
            }
            Some("message_delta") => {
                stop = event.pointer("/delta/stop_reason").and_then(|v| v.as_str()).map(str::to_owned).unwrap_or(stop);
                output = event.pointer("/usage/output_tokens").and_then(|v| v.as_u64()).unwrap_or(output);
            }
            Some("error") => {
                error = event.pointer("/error/message").and_then(|v| v.as_str()).map(str::to_owned);
            }
            _ => {}
        }
    }
    if let Some(error) = error {
        return error_response(StatusCode::BAD_GATEWAY, "api_error", error);
    }
    for (index, json_text) in partial {
        if let Some(block) = blocks.get_mut(index)
            && block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
        {
            block["input"] = serde_json::from_str(&json_text).unwrap_or_else(|_| json!({ "_raw": json_text }));
        }
    }
    axum::Json(json!({
        "id": format!("msg_{}", random_hex(12)),
        "type": "message", "role": "assistant", "model": "devin",
        "content": blocks,
        "stop_reason": stop, "stop_sequence": null,
        "usage": { "input_tokens": input, "output_tokens": output },
    }))
    .into_response()
}


#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> Credentials {
        Credentials { token: format!("{TOKEN_PREFIX}jwt"), fingerprint: "ff".repeat(8), ..Default::default() }
    }

    /// Decodes a built message back into (field, value) pairs; the buffer is leaked so the
    /// returned values can be stored without juggling lifetimes in tests.
    fn decoded(msg: Msg) -> Vec<(u32, Val<'static>)> {
        let bytes: &'static [u8] = Box::leak(msg.into_vec().into_boxed_slice());
        proto::fields(bytes).collect()
    }

    fn bytes_of<'a>(val: &'a Val<'a>) -> &'a [u8] {
        match val {
            Val::Bytes(b) => b,
            _ => &[],
        }
    }

    fn text_of(val: &Val<'_>) -> Option<String> {
        val.as_str().map(str::to_owned)
    }

    #[test]
    fn account_reads_the_plan_status() {
        // Shaped like the captured GetUserStatus answer.
        let mut info = Msg::new();
        info.varint(1, 16).string(2, "Pro");
        let mut start = Msg::new();
        start.varint(1, 1_793_012_228);
        let mut end = Msg::new();
        end.varint(1, 1_795_690_628);
        let mut plan = Msg::new();
        plan.msg(1, &info).msg(2, &start).msg(3, &end).varint(8, u64::MAX).varint(14, 29).varint(15, 64);
        plan.varint(17, 1_794_729_600).varint(18, 1_794_729_600);
        let mut status = Msg::new();
        status.varint(1, 1).string(3, "user").string(7, "u@example.com").varint(10, 16).msg(13, &plan);
        let mut response = Msg::new();
        response.msg(1, &status);

        let account = account_from(&response.into_vec()).unwrap();
        assert_eq!(account.email.as_deref(), Some("u@example.com"));
        assert_eq!(account.plan.name.as_deref(), Some("Pro"));
        assert_eq!(account.plan.started_at.as_deref(), Some("2026-10-26T10:57:08Z"));
        assert_eq!(account.plan.expires_at.as_deref(), Some("2026-11-26T10:57:08Z"));
        assert_eq!(account.limits.len(), 2);
        assert_eq!(account.limits[0].id, "daily");
        assert_eq!(account.limits[0].used_percent, Some(71.0));
        assert_eq!(account.limits[1].used_percent, Some(36.0));
        assert_eq!(account.raw["planStatus"]["8"], json!(-1));
        assert_eq!(account.raw["planStatus"]["1"]["2"], json!("Pro"));
    }

    /// Runs the parser on a real captured answer: `DD_TEST_DEVIN_USERSTATUS=<resp-008.bin>`
    /// (a raw HTTP response; the body starts after the blank line).
    #[test]
    #[ignore]
    fn account_from_capture() {
        let path = std::env::var("DD_TEST_DEVIN_USERSTATUS").expect("DD_TEST_DEVIN_USERSTATUS");
        let raw = std::fs::read(path).unwrap();
        let body = raw.windows(4).position(|w| w == b"\r\n\r\n").map_or(&raw[..], |at| &raw[at + 4..]);
        let account = account_from(&gunzip(body).unwrap()).unwrap();
        println!("{}", serde_json::to_string_pretty(&account).unwrap());
        assert!(account.plan.name.is_some());
    }

    #[test]
    fn envelope_flags_and_length() {
        let frame = envelope(b"abc", true, false);
        assert_eq!(frame, [1, 0, 0, 0, 3, b'a', b'b', b'c']);
        let trailer = envelope(b"{}", false, true);
        assert_eq!(trailer[..5], [2, 0, 0, 0, 2]);
    }

    #[test]
    fn token_prefix_normalized() {
        assert_eq!(normalize_token(&format!("{TOKEN_PREFIX}a.b.c")), format!("{TOKEN_PREFIX}a.b.c"));
        assert_eq!(normalize_token("a.b.c"), format!("{TOKEN_PREFIX}a.b.c"));
        assert_eq!(normalize_token("opaque"), "opaque");
        assert_eq!(normalize_token("  a.b.c "), format!("{TOKEN_PREFIX}a.b.c"));
    }

    #[test]
    fn request_fields() {
        let body = json!({
            "model": "devin/swe-2-max",
            "system": [{ "type": "text", "text": "sys" }],
            "max_tokens": 999_999,
            "messages": [
                { "role": "user", "content": [{ "type": "text", "text": "hi" }] },
                {
                    "role": "assistant",
                    "content": [
                        { "type": "text", "text": "hello" },
                        { "type": "thinking", "thinking": "hmm", "signature": "sig" },
                        { "type": "tool_use", "id": "t1", "name": "read", "input": { "a": 1 } },
                    ],
                },
                { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "done" }] },
            ],
            "tools": [{ "name": "read", "description": "reads", "input_schema": { "type": "object" } }],
        });
        let fields = decoded(build_request(&creds(), "swe-2-max", &body, Some(128_000)));

        // Metadata, system prompt, model uid, request type, and a max_tokens capped at the model's.
        let text = |want: u32| fields.iter().find(|(f, _)| *f == want).and_then(|(_, v)| text_of(v));
        assert!(matches!(fields.iter().find(|(f, _)| *f == 1), Some((_, Val::Bytes(_)))));
        assert_eq!(text(2).as_deref(), Some("sys"));
        assert_eq!(text(21).as_deref(), Some("swe-2-max"));
        assert!(fields.iter().any(|(f, v)| *f == 7 && v.as_u64() == 5));

        // Three chat prompts: user text, assistant (text + thinking + call), tool result.
        let prompts: Vec<&[u8]> = fields.iter().filter(|(f, _)| *f == 3).map(|(_, v)| bytes_of(v)).collect();
        assert_eq!(prompts.len(), 3);
        let str_at = |msg: &[u8], want: u32| proto::get(msg, want).and_then(|v| text_of(&v));
        assert!(matches!(proto::get(prompts[0], 2), Some(Val::Varint(1)))); // USER
        assert_eq!(str_at(prompts[0], 3).as_deref(), Some("hi"));
        assert!(matches!(proto::get(prompts[1], 2), Some(Val::Varint(2)))); // assistant
        assert_eq!(str_at(prompts[1], 11).as_deref(), Some("hmm"));
        assert_eq!(str_at(prompts[1], 12).as_deref(), Some("sig"));
        let calls = proto::get_all(prompts[1], 6);
        assert_eq!(calls.len(), 1);
        assert_eq!(str_at(bytes_of(&calls[0]), 2).as_deref(), Some("read"));
        assert!(matches!(proto::get(prompts[2], 2), Some(Val::Varint(4)))); // TOOL
        assert_eq!(str_at(prompts[2], 7).as_deref(), Some("t1"));
        // The last prompt carries the cache breakpoint.
        assert!(matches!(proto::get(prompts[2], 8), Some(Val::Bytes(_))));

        // One tool definition; the config caps max_tokens at the model's 128k.
        let tool = bytes_of(&fields.iter().find(|(f, _)| *f == 10).unwrap().1);
        assert_eq!(str_at(tool, 1).as_deref(), Some("read"));
        let config = bytes_of(&fields.iter().find(|(f, _)| *f == 8).unwrap().1);
        assert_eq!(proto::get(config, 2).map(|v| v.as_u64()), Some(128_000));
    }

    /// One `GetChatMessageResponse` frame with the given fields filled in.
    fn response_frame(build: impl Fn(&mut Msg)) -> Vec<u8> {
        let mut msg = Msg::new();
        msg.string(1, "msg_1");
        build(&mut msg);
        msg.into_vec()
    }

    fn events(sse: Vec<String>) -> String {
        sse.concat()
    }

    #[test]
    fn streamer_text_thinking_tool() {
        let mut streamer = Streamer::new();

        // Thinking + signature + text on the first frame; usage arrives early, like the captures.
        let usage = {
            let mut usage = Msg::new();
            usage.varint(2, 42).varint(3, 7).varint(5, 5);
            usage
        };
        streamer.frame(&response_frame(|m| {
            m.msg(7, &usage).string(9, "think ").string(10, "sig").string(3, "hello ");
        }));

        // A tool call with arguments split over two frames (field 6 is the call delta itself).
        let call = {
            let mut call = Msg::new();
            call.string(1, "t1").string(2, "read_file").string(3, "{\"path\":\"a");
            call
        };
        streamer.frame(&response_frame(|m| {
            m.msg(6, &call);
        }));
        let tail = {
            let mut tail = Msg::new();
            tail.string(1, "t1").string(3, ".txt\"}");
            tail
        };
        streamer.frame(&response_frame(|m| {
            m.msg(6, &tail);
        }));

        // Stop reason on the last frame.
        streamer.frame(&response_frame(|m| {
            m.varint(5, 10);
        }));

        let out = events(streamer.finish());
        assert!(out.contains("\"type\":\"message_start\""));
        let thinking = out.matches("thinking_delta").count();
        assert_eq!(thinking, 1);
        assert!(out.contains("\"signature\":\"sig\""));
        // serde_json serializes object keys sorted: id, input, name, type.
        assert!(out.contains("\"id\":\"t1\",\"input\":{},\"name\":\"read_file\",\"type\":\"tool_use\""));
        // partial_json rides inside the data JSON, so its quotes arrive escaped.
        assert!(out.contains(r#"{\"path\":\"a"#));
        assert!(out.contains(r#".txt\"}"#));
        // Tool blocks all close before message_delta; stop_reason is tool_use.
        let delta_at = out.find("message_delta").unwrap();
        assert!(out[..delta_at].matches("content_block_stop").count() >= 3);
        assert!(out[delta_at..].contains("\"stop_reason\":\"tool_use\""));
        assert!(out.contains("\"output_tokens\":7"));
        assert!(out.contains("\"input_tokens\":42"));
    }

    #[tokio::test]
    async fn collect_rebuilds_a_message() {
        let sse = [
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"cache_read_input_tokens\":2,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"n\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"x\\\":1}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":9}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ]
        .concat();
        let response = collect_response(&sse);
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["content"][0]["text"], "hi");
        assert_eq!(value["content"][1]["input"], json!({ "x": 1 }));
        assert_eq!(value["stop_reason"], "tool_use");
        assert_eq!(value["usage"], json!({ "input_tokens": 7, "output_tokens": 9 }));
    }
}
