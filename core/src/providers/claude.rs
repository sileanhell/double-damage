//! The Claude provider: Anthropic's OAuth for claude.ai accounts — the same
//! authorization-code + PKCE flow the Claude Code CLI runs. The browser lands on the
//! hosted sign-in page; the code comes back to a loopback listener, or the user pastes
//! the `code#state` the hosted callback page shows when no listener can take it. The
//! token endpoint then swaps it for an access/refresh pair; API calls authenticate as
//! `Authorization: Bearer <token>` with `anthropic-beta: oauth-2025-04-20`.
//!
//! Chat is a passthrough: the account's tokens are bound to the Claude Code product, so
//! the request goes to the public Messages endpoint with the CLI's identity system block
//! first, the user-agent it sends, and the beta flags the request's features need.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest as _;
use tokio::sync::{mpsc, oneshot};

use super::{Account, Balance, Limit, Plan};
use crate::{
    events::{Event, Events},
    paths::{Paths, write_atomic},
    store::{ClaudeCredentials as Credentials, Store},
    util::now_ms,
};

/// Provider prefix of the model ids the gateway serves, e.g. `claude/sonnet`.
pub const PROVIDER: &str = "claude";
pub const PROVIDER_NAME: &str = "Claude";

const API_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
/// The identity Anthropic requires as the first system block on claude.ai OAuth calls —
/// without it the Messages endpoint refuses the subscription token.
const IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

const AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// Subscription usage windows (`five_hour`, `seven_day`, …) and extra-usage spend, as the
/// CLI's `/usage` shows them.
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// How long a fetched account answer is reused.
const ACCOUNT_TTL_MS: u64 = 30_000;
/// The hosted page that displays the code for pasting, when no loopback listener
/// can take the redirect (and always offered alongside it).
const MANUAL_REDIRECT: &str = "https://platform.claude.com/oauth/code/callback";
/// Hosted "you can go back" page the loopback listener redirects the browser to.
const SUCCESS_URL: &str = "https://platform.claude.com/oauth/code/success?app=claude-code";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// The scope set the CLI asks for on a claude.ai sign-in.
const SCOPES: &str = "org:create_api_key user:profile user:inference \
    user:sessions:claude_code user:mcp_servers user:file_upload user:plugins \
    user:privacy_consents:write";
const USER_AGENT: &str = "claude-cli/2.1.325 (external, cli)";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// Refresh this many ms ahead of the recorded expiry, like the CLI.
const REFRESH_AHEAD_MS: u64 = 300_000;

/// A remote model as the catalog lists it (cached in `<data>/core/cache/claude-models.json`).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ClaudeModel {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub context: Option<u64>,
    /// Longest reply the API allows; caps `max_tokens`.
    #[serde(default)]
    pub max_output: Option<u64>,
}

/// What clients see about the provider (`GET /dd/providers/claude`).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeStatus {
    pub signed_in: bool,
    pub email: Option<String>,
    pub subscription: Option<String>,
    /// ms epoch the access token dies at.
    pub expires_at: Option<u64>,
    /// Models the account's catalog currently lists.
    pub models: usize,
    /// A sign-in is waiting on the browser or a pasted code.
    pub signing_in: bool,
    /// The sign-in page of the flow in progress.
    pub login_url: Option<String>,
    /// The sign-in page variant whose callback shows the code for pasting.
    pub manual_url: Option<String>,
    /// The system block every request starts with, so settings can show it.
    pub system_prefix: &'static str,
}

/// An in-flight sign-in: dropping it ends the wait, `manual` delivers a pasted code.
struct Pending {
    _cancel: oneshot::Sender<()>,
    manual: mpsc::UnboundedSender<String>,
    url: String,
    manual_url: String,
}

/// Remote provider state: credentials in the store, the fetched model list (cached on disk),
/// an in-flight login, and the last account answer.
#[derive(Clone)]
pub struct Claude {
    store: Arc<Store>,
    paths: Paths,
    events: Events,
    models: Arc<Mutex<Vec<ClaudeModel>>>,
    signing_in: Arc<Mutex<Option<Pending>>>,
    account: Arc<Mutex<Option<Account>>>,
    /// Remote calls go through the system proxy, unlike the gateway's loopback client.
    client: reqwest::Client,
}

fn err_str(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// base64url without padding, as the CLI encodes the verifier and state.
fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn random_b64(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("system random number generator unavailable");
    b64(&buf)
}

/// The sign-in URL; `redirect_uri` is the loopback listener or the hosted paste page.
fn authorize_url(challenge: &str, state: &str, redirect_uri: &str) -> String {
    let encode = |s: &str| percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string();
    format!(
        "{AUTHORIZE_URL}?code=true&client_id={CLIENT_ID}&response_type=code\
         &redirect_uri={}&scope={}&code_challenge={challenge}\
         &code_challenge_method=S256&state={state}",
        encode(redirect_uri),
        encode(SCOPES),
    )
}

/// `claude_*` organization_type → the subscription name the CLI shows.
fn subscription_of(organization_type: Option<&str>) -> Option<String> {
    let mapped = match organization_type? {
        "claude_pro" => "pro",
        "claude_max" => "max",
        "claude_team" => "team",
        "claude_enterprise" => "enterprise",
        _ => return None,
    };
    Some(mapped.to_string())
}

impl Claude {
    pub fn new(store: Arc<Store>, paths: Paths, events: Events) -> Self {
        Self {
            store,
            paths,
            events,
            models: Default::default(),
            signing_in: Default::default(),
            account: Default::default(),
            client: reqwest::Client::new(),
        }
    }

    fn changed(&self) {
        self.events.send(Event::Provider { id: PROVIDER.into() });
    }

    pub(crate) fn creds(&self) -> Option<Credentials> {
        self.store.get().claude.filter(|c| !c.access_token.is_empty())
    }

    fn save_creds(&self, creds: &Credentials) -> Result<(), String> {
        self.store.update(|all| all.claude = Some(creds.clone()))
    }

    pub fn status(&self) -> ClaudeStatus {
        let creds = self.creds();
        let pending = self.signing_in.lock().unwrap_or_else(|e| e.into_inner());
        ClaudeStatus {
            signed_in: creds.is_some(),
            email: creds.as_ref().and_then(|c| c.email.clone()),
            subscription: creds.as_ref().and_then(|c| c.subscription.clone()),
            expires_at: creds.as_ref().map(|c| c.expires_at).filter(|e| *e > 0),
            models: self.models().len(),
            signing_in: pending.is_some(),
            login_url: pending.as_ref().map(|p| p.url.clone()),
            manual_url: pending.as_ref().map(|p| p.manual_url.clone()),
            system_prefix: IDENTITY,
        }
    }

    /// Signs out: drops the tokens, the fetched model list, and any in-flight sign-in.
    pub fn logout(&self) -> ClaudeStatus {
        self.cancel_login();
        if let Err(error) = self.store.update(|all| all.claude = None) {
            eprintln!("dd-core: can't save the sign-out: {error}");
        }
        let _ = std::fs::remove_file(self.models_path());
        self.models.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.account.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.changed();
        self.status()
    }

    /// The fetched model list; an empty memory reloads the cache file once.
    pub fn models(&self) -> Vec<ClaudeModel> {
        let mut models = self.models.lock().unwrap_or_else(|e| e.into_inner());
        if models.is_empty()
            && let Ok(bytes) = std::fs::read(self.models_path())
            && let Ok(saved) = serde_json::from_slice::<Vec<ClaudeModel>>(&bytes)
        {
            *models = saved;
        }
        models.clone()
    }

    fn models_path(&self) -> PathBuf {
        self.paths.cache("claude-models.json")
    }

    fn save_models(&self, models: &[ClaudeModel]) {
        let _ = write_atomic(&self.models_path(), &serde_json::to_vec_pretty(models).unwrap_or_default());
    }

    pub fn cancel_login(&self) {
        // Dropping the senders resolves both wait channels as cancelled.
        self.signing_in.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    /// Hands a pasted `code#state` (as the hosted callback page shows it) to an
    /// in-flight sign-in; the pasted state is ignored — the exchange validates it.
    pub fn submit_code(&self, blob: &str) -> Result<(), String> {
        let code = blob.trim();
        let Some((code, _)) = code.split_once('#').filter(|(c, s)| !c.is_empty() && !s.is_empty()) else {
            return Err("invalid code — paste the whole code, with the part after '#'".into());
        };
        let pending = self.signing_in.lock().unwrap_or_else(|e| e.into_inner());
        let Some(pending) = pending.as_ref() else {
            return Err("no sign-in in progress".into());
        };
        pending.manual.send(code.to_string()).map_err(|_| "no sign-in in progress".into())
    }

    /// The full PKCE sign-in: loopback listener when it binds, the paste fallback
    /// always. `open` hands the sign-in URL to the user's browser; an error aborts.
    pub async fn login(&self, open: impl FnOnce(&str) -> Result<(), String>) -> Result<ClaudeStatus, String> {
        if self.signing_in.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
            return Err("sign-in is already in progress".into());
        }
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.ok();
        let port = listener.as_ref().and_then(|l| l.local_addr().ok()).map(|a| a.port());
        let loopback = port.map(|p| format!("http://localhost:{p}/callback"));

        let verifier = random_b64(32);
        let challenge = b64(&sha2::Sha256::digest(verifier.as_bytes()));
        let state = random_b64(32);
        let url = authorize_url(&challenge, &state, loopback.as_deref().unwrap_or(MANUAL_REDIRECT));
        let manual_url = authorize_url(&challenge, &state, MANUAL_REDIRECT);

        let (cancel_tx, cancel_rx) = oneshot::channel();
        let (manual_tx, mut manual_rx) = mpsc::unbounded_channel();
        *self.signing_in.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Pending { _cancel: cancel_tx, manual: manual_tx, url: url.clone(), manual_url });
        self.changed();
        if let Err(error) = open(&url) {
            self.signing_in.lock().unwrap_or_else(|e| e.into_inner()).take();
            self.changed();
            return Err(error);
        }

        let deadline = Instant::now() + LOGIN_TIMEOUT;
        let wait_listener = async {
            match &listener {
                Some(listener) => Self::wait_for_code(listener, &state, deadline).await,
                // No listener: this arm simply never fires.
                None => std::future::pending::<Result<String, String>>().await,
            }
        };
        let arrived: Result<(String, bool), String> = tokio::select! {
            result = wait_listener => result.map(|code| (code, true)),
            code = manual_rx.recv() => code.map(|code| (code, false)).ok_or_else(|| "sign-in cancelled".to_string()),
            _ = cancel_rx => Err("sign-in cancelled".into()),
            _ = tokio::time::sleep_until(deadline.into()) => Err("sign-in timed out".into()),
        };
        self.signing_in.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.changed();

        let (code, via_listener) = arrived?;
        let redirect_uri = if via_listener {
            loopback.unwrap_or_else(|| MANUAL_REDIRECT.to_string())
        } else {
            MANUAL_REDIRECT.to_string()
        };
        let mut creds = self.exchange(&code, &verifier, &state, &redirect_uri).await?;
        // Email, subscription, and the org name ride along for the account card.
        if let Ok(profile) = self.profile(&creds.access_token).await {
            creds.email = profile.0;
            creds.subscription = profile.1;
            creds.organization = profile.2;
        }
        self.save_creds(&creds)?;
        let _ = self.refresh_models().await;
        self.changed();
        Ok(self.status())
    }

    /// Waits for the browser to redirect to the loopback listener. A successful code
    /// gets a 302 to the hosted success page; an OAuth error, a readable reply.
    async fn wait_for_code(
        listener: &tokio::net::TcpListener,
        state: &str,
        deadline: Instant,
    ) -> Result<String, String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let accept = tokio::time::timeout_at(deadline.into(), listener.accept());
            let mut stream = accept.await.map_err(|_| "sign-in timed out".to_string())?.map_err(err_str)?.0;
            let mut buf = vec![0u8; 8192];
            let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await.map_err(err_str)?.map_err(err_str)?;
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let path = request.split_whitespace().nth(1).unwrap_or("");
            if !path.starts_with("/callback") {
                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nconnection: close\r\n\r\n").await;
                continue;
            }
            let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
            let params: HashMap<String, String> = percent_encoding::percent_decode_str(query)
                .decode_utf8_lossy()
                .split('&')
                .filter_map(|pair| pair.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
                .collect();
            let text_reply = |status: &str, text: &str| {
                format!("HTTP/1.1 {status}\r\ncontent-type: text/plain; charset=utf-8\r\nconnection: close\r\n\r\n{text}")
            };
            if let Some(error) = params.get("error").filter(|e| !e.is_empty()) {
                let detail = params.get("error_description").map(String::as_str).unwrap_or(error);
                let _ = stream.write_all(text_reply("200 OK", detail).as_bytes()).await;
                return Err(format!("sign-in failed: {detail}"));
            }
            if params.get("state").is_some_and(|s| s != state) || params.get("state").is_none() {
                let _ = stream.write_all(text_reply("400 Bad Request", "Invalid state parameter").as_bytes()).await;
                return Err("sign-in failed: state mismatch".into());
            }
            if let Some(code) = params.get("code").filter(|c| !c.is_empty()) {
                let redirect = format!("HTTP/1.1 302 Found\r\nlocation: {SUCCESS_URL}\r\nconnection: close\r\n\r\n");
                let _ = stream.write_all(redirect.as_bytes()).await;
                return Ok(code.clone());
            }
            let _ = stream.write_all(text_reply("400 Bad Request", "Authorization code not found").as_bytes()).await;
        }
    }

    /// Swaps the authorization code for tokens: JSON POST to the token endpoint.
    async fn exchange(&self, code: &str, verifier: &str, state: &str, redirect_uri: &str) -> Result<Credentials, String> {
        let body = json!({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": redirect_uri,
            "client_id": CLIENT_ID,
            "code_verifier": verifier,
            "state": state,
        });
        let response = self
            .client
            .post(TOKEN_URL)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(err_str)?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(err_str)?;
        if status.as_u16() == 401 {
            return Err("invalid or expired authorization code".into());
        }
        if !status.is_success() {
            return Err(format!("token exchange failed (HTTP {status})"));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(err_str)?;
        let access_token = value.get("access_token").and_then(Value::as_str).unwrap_or_default();
        if access_token.is_empty() {
            return Err("token exchange returned no access token".into());
        }
        let expires_at = now_ms() + value.get("expires_in").and_then(Value::as_u64).unwrap_or(0) * 1000;
        let refresh_expires = value
            .get("refresh_token_expires_in")
            .and_then(Value::as_u64)
            .map(|secs| now_ms() + secs * 1000);
        Ok(Credentials {
            access_token: access_token.to_string(),
            refresh_token: value.get("refresh_token").and_then(Value::as_str).unwrap_or_default().to_string(),
            expires_at,
            refresh_token_expires_at: refresh_expires,
            scopes: value
                .get("scope")
                .and_then(Value::as_str)
                .map(|s| s.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default(),
            email: value
                .get("account")
                .and_then(|a| a.get("email_address"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            ..Default::default()
        })
    }

    /// GET /api/oauth/profile → (email, subscription, organization). Best effort.
    async fn profile(&self, access_token: &str) -> Result<(Option<String>, Option<String>, Option<String>), String> {
        let response = self
            .client
            .get(PROFILE_URL)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {access_token}"))
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .send()
            .await
            .map_err(err_str)?;
        if !response.status().is_success() {
            return Err(format!("profile fetch failed (HTTP {})", response.status()));
        }
        let value: Value = serde_json::from_slice(&response.bytes().await.map_err(err_str)?).map_err(err_str)?;
        let account = value.get("account");
        let email = account
            .and_then(|a| a.get("email_address"))
            .or_else(|| account.and_then(|a| a.get("full_name")))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let organization = value.get("organization");
        let subscription = subscription_of(
            organization.and_then(|o| o.get("organization_type")).and_then(Value::as_str),
        );
        let org_name = organization.and_then(|o| o.get("name")).and_then(Value::as_str).map(str::to_owned);
        Ok((email, subscription, org_name))
    }

    /// GET /v1/models → the account's model catalog; falls back to a small built-in list
    /// and persists the result so the gateway catalog survives a restart.
    pub async fn refresh_models(&self) -> Result<usize, String> {
        let token = self.access_token().await?;
        let result = self
            .client
            .get(format!("{API_URL}/v1/models?limit=1000"))
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .header("anthropic-version", API_VERSION)
            .header("anthropic-beta", "oauth-2025-04-20")
            .send()
            .await;
        let models = match result {
            Ok(response) if response.status().is_success() => {
                let value: Value = serde_json::from_slice(&response.bytes().await.map_err(err_str)?).map_err(err_str)?;
                let parsed = parse_models(&value);
                if parsed.is_empty() { fallback_models() } else { parsed }
            }
            _ => fallback_models(),
        };
        let count = models.len();
        self.save_models(&models);
        *self.models.lock().unwrap_or_else(|e| e.into_inner()) = models;
        self.changed();
        Ok(count)
    }

    /// A usable access token, refreshing through the stored refresh token first.
    pub async fn access_token(&self) -> Result<String, String> {
        let creds = self.creds().ok_or("not signed in")?;
        if now_ms() + REFRESH_AHEAD_MS < creds.expires_at {
            return Ok(creds.access_token);
        }
        self.refresh(creds).await
    }

    /// Swaps the refresh token for a fresh pair; persists the result.
    async fn refresh(&self, creds: Credentials) -> Result<String, String> {
        if creds.refresh_token.is_empty() {
            return Err("session expired — sign in again".into());
        }
        let scopes = if creds.scopes.is_empty() { SCOPES.to_string() } else { creds.scopes.join(" ") };
        let body = json!({
            "grant_type": "refresh_token",
            "refresh_token": creds.refresh_token,
            "client_id": CLIENT_ID,
            "scope": scopes,
        });
        let response = self
            .client
            .post(TOKEN_URL)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(err_str)?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(err_str)?;
        if !status.is_success() {
            return Err(format!("token refresh failed (HTTP {status})"));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(err_str)?;
        let access_token = value
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or("token refresh returned no access token")?
            .to_string();
        let mut next = creds;
        next.access_token = access_token.clone();
        if let Some(token) = value.get("refresh_token").and_then(Value::as_str) {
            next.refresh_token = token.to_string();
        }
        if let Some(secs) = value.get("expires_in").and_then(Value::as_u64) {
            next.expires_at = now_ms() + secs * 1000;
        }
        if let Some(secs) = value.get("refresh_token_expires_in").and_then(Value::as_u64) {
            next.refresh_token_expires_at = Some(now_ms() + secs * 1000);
        }
        if let Some(scope) = value.get("scope").and_then(Value::as_str) {
            next.scopes = scope.split_whitespace().map(str::to_owned).collect();
        }
        self.save_creds(&next)?;
        Ok(access_token)
    }

    /// GET on an OAuth account endpoint, as JSON.
    async fn get_json(&self, url: &str, token: &str) -> Result<Value, String> {
        let response = self
            .client
            .get(url)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .header("anthropic-beta", "oauth-2025-04-20")
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .send()
            .await
            .map_err(err_str)?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(err_str)?;
        if !status.is_success() {
            return Err(format!("{url} answered HTTP {status}"));
        }
        serde_json::from_slice(&bytes).map_err(err_str)
    }

    /// Plan, extra-usage spend and the subscription's usage windows: the profile and
    /// `/api/oauth/usage` (what the CLI's `/usage` shows). Reused for a while unless `fresh`.
    pub async fn account(&self, fresh: bool) -> Result<Account, String> {
        if !fresh
            && let Some(cached) = self.account.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
            && now_ms().saturating_sub(cached.fetched_at) < ACCOUNT_TTL_MS
        {
            return Ok(cached.clone());
        }
        let token = self.access_token().await?;
        let (profile, usage) = tokio::join!(self.get_json(PROFILE_URL, &token), self.get_json(USAGE_URL, &token));
        let profile = profile?;
        let mut account = account_from(&profile, usage.as_ref().ok());
        if let Err(error) = &usage {
            account.raw["usageError"] = json!(error);
        }
        // The profile is the freshest word on the plan; keep the stored copy in step.
        if let Some(mut creds) = self.creds()
            && (creds.subscription != account.plan.name || creds.email != account.email)
        {
            creds.subscription = account.plan.name.clone().or(creds.subscription);
            creds.email = account.email.clone().or(creds.email);
            let _ = self.save_creds(&creds);
        }
        *self.account.lock().unwrap_or_else(|e| e.into_inner()) = Some(account.clone());
        Ok(account)
    }
}

/// Profile + usage answers → the normalized account.
fn account_from(profile: &Value, usage: Option<&Value>) -> Account {
    let text = |v: Option<&Value>| v.and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned);
    let account = profile.get("account");
    let org = profile.get("organization");
    let plan = Plan {
        name: subscription_of(org.and_then(|o| o.get("organization_type")).and_then(Value::as_str)),
        tier: text(org.and_then(|o| o.get("rate_limit_tier"))),
        billing: text(org.and_then(|o| o.get("billing_type"))),
        started_at: text(org.and_then(|o| o.get("subscription_created_at"))),
        // Anthropic doesn't publish the renewal date through the OAuth API.
        expires_at: text(org.and_then(|o| o.get("subscription_ends_at")))
            .or_else(|| text(org.and_then(|o| o.get("current_period_end")))),
        trial_ends_at: text(org.and_then(|o| o.get("claude_code_trial_ends_at"))),
    };

    let mut limits = Vec::new();
    let mut balance = None;
    if let Some(usage) = usage {
        const WINDOWS: [(&str, &str); 5] = [
            ("five_hour", "Session (5 hours)"),
            ("seven_day", "Week (all models)"),
            ("seven_day_opus", "Week (Opus)"),
            ("seven_day_sonnet", "Week (Sonnet)"),
            ("seven_day_oauth_apps", "Week (OAuth apps)"),
        ];
        for (id, label) in WINDOWS {
            let Some(window) = usage.get(id).filter(|w| w.is_object()) else { continue };
            limits.push(Limit {
                id: id.into(),
                label: label.into(),
                used_percent: window.get("utilization").and_then(Value::as_f64),
                resets_at: text(window.get("resets_at")),
                status: None,
            });
        }
        // Newer answers add per-model and spend rows.
        for row in usage.get("limits").and_then(Value::as_array).into_iter().flatten() {
            let kind = row.get("kind").and_then(Value::as_str).unwrap_or("limit");
            let group = row.get("group").and_then(Value::as_str).unwrap_or("");
            let id = if group.is_empty() { kind.to_string() } else { format!("{kind}:{group}") };
            if limits.iter().any(|l| l.id == id) {
                continue;
            }
            let label = text(row.get("display_name")).unwrap_or_else(|| format!("{kind} ({group})"));
            limits.push(Limit {
                id,
                label,
                used_percent: row.get("percent").or_else(|| row.get("utilization")).and_then(Value::as_f64),
                resets_at: text(row.get("resets_at")),
                status: text(row.get("severity")),
            });
        }
        if let Some(extra) = usage.get("extra_usage").filter(|e| e.is_object()) {
            let cents = |key: &str| extra.get(key).and_then(Value::as_f64).map(|c| c / 100.0);
            let (used, limit) = (cents("used_credits"), cents("monthly_limit"));
            balance = Some(Balance {
                enabled: extra.get("is_enabled").and_then(Value::as_bool).unwrap_or(false),
                currency: text(extra.get("currency")).or(Some("USD".into())),
                used,
                limit,
                remaining: limit.zip(used).map(|(limit, used)| (limit - used).max(0.0)),
            });
        }
    }

    Account {
        provider: PROVIDER.into(),
        email: text(account.and_then(|a| a.get("email_address")).or_else(|| account.and_then(|a| a.get("email")))),
        organization: text(org.and_then(|o| o.get("name"))),
        plan,
        balance,
        limits,
        fetched_at: now_ms(),
        raw: json!({ "profile": profile, "usage": usage }),
    }
}

// --- chat --------------------------------------------------------------------

/// The `/v1/models` answer → the account's catalog. Aliases keep their nicer label.
fn parse_models(value: &Value) -> Vec<ClaudeModel> {
    let mut models = Vec::new();
    for entry in value.get("data").and_then(|d| d.as_array()).into_iter().flatten() {
        let Some(id) = entry.get("id").and_then(|i| i.as_str()).filter(|id| id.starts_with("claude")) else { continue };
        let label = entry.get("display_name").and_then(|n| n.as_str()).filter(|n| !n.is_empty()).unwrap_or(id).to_string();
        models.push(ClaudeModel {
            id: id.to_string(),
            label,
            context: Some(context_of(id)),
            max_output: Some(64_000),
        });
    }
    models
}

/// Trained context by model id; the `[1m]` variant the CLI offers doubles the window.
fn context_of(id: &str) -> u64 {
    if id.contains("[1m]") { 1_000_000 } else { 200_000 }
}

/// Used until the live catalog arrives (or when it can't be fetched).
fn fallback_models() -> Vec<ClaudeModel> {
    [
        ("claude-opus-5-5", "Claude Opus 5.5"),
        ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
        ("claude-haiku-5-5", "Claude Haiku 5.5"),
    ]
    .into_iter()
    .map(|(id, label)| ClaudeModel {
        id: id.into(),
        label: label.into(),
        context: Some(context_of(id)),
        max_output: Some(64_000),
    })
    .collect()
}

/// `anthropic-beta` for the request's features, the way the CLI composes it.
fn betas(path: &str, model: &str, body: &Value) -> String {
    let mut betas = vec!["oauth-2025-04-20"];
    if path.ends_with("count_tokens") {
        betas.push("token-counting-2024-11-01");
    }
    let thinking = matches!(body.pointer("/thinking/type").and_then(|t| t.as_str()), Some("enabled" | "adaptive"))
        || body
            .get("messages")
            .and_then(|m| m.as_array())
            .is_some_and(|msgs| msgs.iter().any(|m| m.pointer("/content").is_some_and(|c| thinking_blocks(c))));
    if thinking {
        betas.push("interleaved-thinking-2025-05-14");
    }
    let effort = body.pointer("/output_config/effort").is_some()
        || body.get("reasoning_effort").is_some()
        || body.pointer("/reasoning/effort").is_some();
    if effort {
        betas.push("effort-2025-11-24");
    }
    if model.contains("[1m]") {
        betas.push("context-1m-2025-08-07");
    }
    betas.join(",")
}

fn thinking_blocks(content: &Value) -> bool {
    content.as_array().is_some_and(|blocks| {
        blocks.iter().any(|b| b.get("type").and_then(|t| t.as_str()) == Some("thinking"))
    })
}

/// Rewrites the incoming Messages body for upstream: real model id, the required
/// identity block first in `system`, and `max_tokens` inside the model's allowance.
fn adapt_body(model: &str, body: &Value, max_output: Option<u64>) -> Value {
    let mut out = body.clone();
    let Some(obj) = out.as_object_mut() else { return out };
    obj.insert("model".into(), json!(model));

    let identity = json!({ "type": "text", "text": IDENTITY });
    let already = |blocks: &[Value]| {
        blocks.first().and_then(|b| b.get("text")).and_then(|t| t.as_str()).is_some_and(|t| t.starts_with(IDENTITY))
    };
    let system = match obj.get("system") {
        Some(Value::String(text)) => json!([identity, { "type": "text", "text": text }]),
        Some(Value::Array(blocks)) if already(blocks) => Value::Array(blocks.clone()),
        Some(Value::Array(blocks)) => {
            let mut with_identity = vec![identity];
            with_identity.extend(blocks.iter().cloned());
            Value::Array(with_identity)
        }
        _ => json!([identity]),
    };
    obj.insert("system".into(), system);

    // max_tokens is required upstream; the app asks for the whole window — clamp it.
    let cap = max_output.unwrap_or(64_000);
    match obj.get("max_tokens").and_then(|m| m.as_u64()) {
        Some(asked) if asked <= cap => {}
        _ => {
            obj.insert("max_tokens".into(), json!(cap));
        }
    }
    out
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

/// `/v1/messages` (and `…/count_tokens`) for a `claude/*` model: the request is already
/// Anthropic-shaped, so after the model/system/`max_tokens` adaptation it relays both
/// the streamed SSE and the one-shot answer — and Anthropic's own error bodies — as is.
pub async fn chat(claude: &Claude, path: &str, model: &str, body: &Value) -> Response {
    let model_uid = model.strip_prefix("claude/").unwrap_or(model);
    let max_output = claude.models().iter().find(|m| m.id == model_uid).and_then(|m| m.max_output);
    let out = adapt_body(model_uid, body, max_output);
    let betas = betas(path, model_uid, &out);

    let send = |token: String, body: Value| {
        claude
            .client
            .post(format!("{API_URL}{path}"))
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .header("anthropic-version", API_VERSION)
            .header("anthropic-beta", betas.clone())
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
    };

    let token = match claude.access_token().await {
        Ok(token) => token,
        Err(error) => return error_response(StatusCode::UNAUTHORIZED, "authentication_error", error),
    };
    let mut upstream = match send(token, out.clone()).await {
        Ok(upstream) => upstream,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, "api_error", format!("Anthropic API unreachable: {e}")),
    };
    if upstream.status().as_u16() == 401
        && let Some(creds) = claude.creds()
        && let Ok(token) = claude.refresh(creds).await
    {
        // A revoked or early-expired token: refresh once and retry.
        upstream = match send(token, out).await {
            Ok(upstream) => upstream,
            Err(e) => {
                return error_response(StatusCode::BAD_GATEWAY, "api_error", format!("Anthropic API unreachable: {e}"));
            }
        };
    }

    let status = StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = upstream.headers().get(reqwest::header::CONTENT_TYPE).cloned();
    let stream = upstream.bytes_stream().map(|chunk| {
        chunk.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    });
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    if let Some(content_type) = content_type.and_then(|v| HeaderValue::from_bytes(v.as_bytes()).ok()) {
        response.headers_mut().insert(header::CONTENT_TYPE, content_type);
    }
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_carries_the_pkce_and_scope_params() {
        let url = authorize_url("CHALLENGE", "STATE", "http://localhost:1234/callback");
        for part in [
            "code=true",
            "client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e",
            "response_type=code",
            "redirect_uri=http%3A%2F%2Flocalhost%3A1234%2Fcallback",
            "code_challenge=CHALLENGE",
            "code_challenge_method=S256",
            "state=STATE",
        ] {
            assert!(url.contains(part), "missing {part} in {url}");
        }
        assert!(url.contains("user%3Ainference"), "scopes missing: {url}");
        assert!(url.contains("org%3Acreate%5Fapi%5Fkey"), "scopes missing: {url}");
    }

    #[test]
    fn subscription_maps_the_org_type() {
        assert_eq!(subscription_of(Some("claude_max")).as_deref(), Some("max"));
        assert_eq!(subscription_of(Some("claude_pro")).as_deref(), Some("pro"));
        assert_eq!(subscription_of(Some("other")), None);
        assert_eq!(subscription_of(None), None);
    }

    #[test]
    fn b64url_has_no_padding() {
        let out = random_b64(32);
        assert_eq!(out.len(), 43);
        assert!(!out.contains('=') && !out.contains('+') && !out.contains('/'));
    }

    #[test]
    fn submit_code_requires_the_pasted_shape() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let claude = Claude::new(Arc::new(Store::open(&paths)), paths, Events::default());
        assert!(claude.submit_code("no-hash-here").is_err());
        assert!(claude.submit_code("#no-code").is_err());
        // Well-formed but nothing is listening.
        assert!(claude.submit_code("code#state").is_err());
    }

    #[test]
    fn account_normalizes_profile_and_usage() {
        let profile = json!({
            "account": { "email_address": "a@b.c" },
            "organization": {
                "name": "Org", "organization_type": "claude_max", "rate_limit_tier": "default_claude_max_20x",
                "billing_type": "stripe_subscription", "subscription_created_at": "2026-01-02T00:00:00Z",
            },
        });
        let usage = json!({
            "five_hour": { "utilization": 12.0, "resets_at": "2026-11-18T05:00:00Z" },
            "seven_day": { "utilization": 40.5, "resets_at": "2026-11-20T00:00:00Z" },
            "seven_day_opus": null,
            "extra_usage": { "is_enabled": true, "monthly_limit": 50000, "used_credits": 27140, "utilization": 54, "currency": "USD" },
            "limits": [{ "kind": "spend", "group": "monthly", "percent": 54, "resets_at": "2026-12-01T00:00:00Z", "severity": "normal" }],
        });
        let account = account_from(&profile, Some(&usage));
        assert_eq!(account.email.as_deref(), Some("a@b.c"));
        assert_eq!(account.plan.name.as_deref(), Some("max"));
        assert_eq!(account.plan.tier.as_deref(), Some("default_claude_max_20x"));
        let ids: Vec<_> = account.limits.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["five_hour", "seven_day", "spend:monthly"]);
        assert_eq!(account.limits[1].used_percent, Some(40.5));
        let balance = account.balance.unwrap();
        assert_eq!(balance.used, Some(271.4));
        assert_eq!(balance.limit, Some(500.0));
        assert!((balance.remaining.unwrap() - 228.6).abs() < 1e-9);
        // No usage answer: the plan alone.
        assert!(account_from(&profile, None).limits.is_empty());
    }

    #[test]
    fn adapt_puts_the_identity_first() {
        let body = json!({
            "model": "claude/claude-sonnet-5-5",
            "max_tokens": 200_000,
            "system": "Be brief.",
            "messages": [{ "role": "user", "content": "hi" }],
        });
        let out = adapt_body("claude-sonnet-5-5", &body, Some(64_000));
        assert_eq!(out["model"], "claude-sonnet-5-5");
        assert_eq!(out["system"][0]["text"], IDENTITY);
        assert_eq!(out["system"][1]["text"], "Be brief.");
        // The app's whole-window ask is clamped to the model's output allowance.
        assert_eq!(out["max_tokens"], 64_000);
        assert_eq!(out["messages"], body["messages"]);
    }

    #[test]
    fn adapt_keeps_an_existing_identity_and_small_max() {
        let system = json!([
            { "type": "text", "text": format!("{IDENTITY} extra") },
            { "type": "text", "text": "Rules." },
        ]);
        let body = json!({ "system": system, "max_tokens": 4096 });
        let out = adapt_body("claude-haiku-5-5", &body, Some(64_000));
        assert_eq!(out["system"].as_array().unwrap().len(), 2);
        assert_eq!(out["max_tokens"], 4096);
        // No system at all still gets the identity.
        let out = adapt_body("claude-haiku-5-5", &json!({}), Some(64_000));
        assert_eq!(out["system"][0]["text"], IDENTITY);
    }

    #[test]
    fn betas_follow_the_request_features() {
        let plain = json!({ "messages": [] });
        assert_eq!(betas("/v1/messages", "claude-sonnet-5-5", &plain), "oauth-2025-04-20");

        let thinking = json!({ "thinking": { "type": "enabled" }, "messages": [] });
        assert_eq!(betas("/v1/messages", "m", &thinking), "oauth-2025-04-20,interleaved-thinking-2025-05-14");

        // Thinking blocks in the history count too.
        let history = json!({ "messages": [{ "role": "assistant", "content": [{ "type": "thinking" }] }] });
        assert!(betas("/v1/messages", "m", &history).contains("interleaved-thinking"));

        let effort = json!({ "output_config": { "effort": "high" }, "messages": [] });
        assert_eq!(betas("/v1/messages", "m", &effort), "oauth-2025-04-20,effort-2025-11-24");

        assert_eq!(
            betas("/v1/messages/count_tokens", "claude-sonnet-5-5[1m]", &plain),
            "oauth-2025-04-20,token-counting-2024-11-01,context-1m-2025-08-07"
        );
    }

    #[test]
    fn parse_models_keeps_claude_entries() {
        let value = json!({ "data": [
            { "id": "claude-opus-5-5", "display_name": "Claude Opus 5.5", "type": "model" },
            { "id": "not-a-model", "display_name": "Nope" },
            { "id": "claude-haiku-5-5[1m]", "type": "model" },
        ]});
        let models = parse_models(&value);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].label, "Claude Opus 5.5");
        assert_eq!(models[0].context, Some(200_000));
        assert_eq!(models[1].context, Some(1_000_000));
        // Fallback fills in when the server answers oddly.
        assert_eq!(parse_models(&json!({})).len(), 0);
        assert_eq!(fallback_models().len(), 3);
    }

    #[tokio::test]
    async fn listener_rejects_a_bad_state() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let deadline = Instant::now() + Duration::from_secs(10);
        let wait = tokio::spawn(async move { Claude::wait_for_code(&listener, "s1", deadline).await });

        let mut bad = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        bad.write_all(b"GET /callback?code=x&state=wrong HTTP/1.0\r\n\r\n").await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = bad.read(&mut buf).await.unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("400"));
        assert!(wait.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn listener_takes_a_good_code() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let deadline = Instant::now() + Duration::from_secs(10);
        let wait = tokio::spawn(async move { Claude::wait_for_code(&listener, "s1", deadline).await });

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut good = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        good.write_all(b"GET /callback?code=the-code&state=s1 HTTP/1.0\r\n\r\n").await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = good.read(&mut buf).await.unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("302"));
        assert_eq!(wait.await.unwrap().unwrap(), "the-code");
    }
}
