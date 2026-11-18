//! OAuth PKCE Devin:
//! `https://app.devin.ai/auth/cli/continue?redirect_uri=http://127.0.0.1:<port>/callback
//!   &state=…&prompt=select_account&code_challenge=…&code_challenge_method=S256&cli_pkce_marker=1`
//! → `?code&state` → обмен `POST https://api.devin.ai/auth/cli/token`
//! `{code, code_verifier}` (как oh-my-pi `auth/devin.kdl`; запасной путь —
//! Connect `ExchangeDevinCLIPKCECode`) → `devin-session-token$<JWT>`.

use serde_json::{json, Value};

use crate::credentials::CredentialsStore;
use crate::error::{Error, Result};
use crate::oauth::{self, CallbackServer};
use crate::provider::LOGIN_TIMEOUT;
use crate::types::{AuthCallback, PendingLogin, ProviderKind};

use super::{DEFAULT_API_SERVER, DEFAULT_DEVIN_API, DEFAULT_WEBAPP};

/// Обмен кода: `api.devin.ai`, не `app.devin.ai` — веб-приложение стоит за
/// CloudFront, который пропускает только GET (403 на POST).

const SESSION_PREFIX: &str = "devin-session-token$";

pub fn begin_login() -> Result<PendingLogin> {
    let (verifier, challenge) = oauth::pkce_pair();
    let server = CallbackServer::bind("/callback")?;
    let state = oauth::random_state();
    let url = format!(
        "{DEFAULT_WEBAPP}/auth/cli/continue?{}",
        oauth::query_string(&[
            ("redirect_uri", &server.redirect_uri()),
            ("state", &state),
            ("prompt", "select_account"),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("cli_pkce_marker", "1"),
        ])
    );
    Ok(PendingLogin {
        provider: ProviderKind::Devin,
        authorize_url: url,
        manual: false,
        state,
        code_verifier: verifier,
        server: Some(server),
    })
}

/// Достать session_token из любого разумного JSON-ответа.
fn extract_token(v: &Value) -> Option<String> {
    for key in ["session_token", "sessionToken", "token", "access_token", "api_key", "key"] {
        if let Some(s) = v.get(key).and_then(Value::as_str) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    // Иногда оборачивают: {"data": {...}} / {"result": {...}}
    for wrap in ["data", "result", "response"] {
        if let Some(inner) = v.get(wrap) {
            if let Some(t) = extract_token(inner) {
                return Some(t);
            }
        }
    }
    None
}

async fn exchange_rest(
    client: &reqwest::Client,
    code: &str,
    verifier: &str,
) -> Result<String> {
    let body = json!({
        "code": code,
        "code_verifier": verifier,
    });
    let resp = client
        .post(format!("{DEFAULT_DEVIN_API}/auth/cli/token"))
        .header("accept", "application/json")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(Error::Http {
            status: status.as_u16(),
            body: text.chars().take(2000).collect(),
        });
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| Error::Parse(format!("/auth/cli/token: {e}: {text}")))?;
    extract_token(&v).ok_or_else(|| Error::Parse(format!("no token in response: {text}")))
}

async fn exchange_connect(
    client: &reqwest::Client,
    code: &str,
    verifier: &str,
    state: &str,
    redirect_uri: &str,
) -> Result<String> {
    // Proto-JSON: `code_verifier` и `codeVerifier` — одно поле, оба сразу
    // сервер отвергает как «duplicate field».
    let body = json!({
        "code": code,
        "codeVerifier": verifier,
        "state": state,
        "redirectUri": redirect_uri,
    });
    let resp = client
        .post(format!(
            "{DEFAULT_API_SERVER}/exa.seat_management_pb.SeatManagementService/ExchangeDevinCLIPKCECode"
        ))
        .header("content-type", "application/json")
        .header("connect-protocol-version", "1")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(Error::Http {
            status: status.as_u16(),
            body: text.chars().take(2000).collect(),
        });
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| Error::Parse(format!("ExchangeDevinCLIPKCECode: {e}: {text}")))?;
    extract_token(&v).ok_or_else(|| Error::Parse(format!("no token in response: {text}")))
}

pub async fn finish_login(
    client: &reqwest::Client,
    store: &CredentialsStore,
    pending: PendingLogin,
    manual_code: Option<String>,
) -> Result<()> {
    let redirect_uri = pending
        .server
        .as_ref()
        .map(CallbackServer::redirect_uri)
        .unwrap_or_default();

    let (code, state) = match manual_code {
        Some(c) if !c.trim().is_empty() => {
            // Ручной ввод: принимаем и «code», и полный redirect-URL с query.
            let c = c.trim();
            if let Some(q) = c.split('?').nth(1) {
                let params: std::collections::HashMap<String, String> = q
                    .split('&')
                    .filter_map(|p| {
                        p.split_once('=')
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                    })
                    .collect();
                (
                    params.get("code").cloned().unwrap_or_default(),
                    params.get("state").cloned().unwrap_or_default(),
                )
            } else {
                (c.to_string(), String::new())
            }
        }
        _ => {
            let server = pending
                .server
                .ok_or_else(|| Error::OAuth("no callback server".into()))?;
            let cb: AuthCallback = server.wait(LOGIN_TIMEOUT).await?;
            if !pending.state.is_empty() && !cb.state.is_empty() && cb.state != pending.state {
                return Err(Error::OAuth("state mismatch".into()));
            }
            if cb.code.is_empty() {
                return Err(Error::OAuth("empty authorization code".into()));
            }
            (cb.code, cb.state)
        }
    };
    if code.is_empty() {
        return Err(Error::OAuth("empty authorization code".into()));
    }

    // Основной путь — REST /auth/cli/token; запасной — ConnectRPC.
    let token = match exchange_rest(client, &code, &pending.code_verifier).await
    {
        Ok(t) => t,
        Err(e1) => match exchange_connect(client, &code, &pending.code_verifier, &state, &redirect_uri).await {
            Ok(t) => t,
            Err(e2) => {
                return Err(Error::OAuth(format!(
                    "token exchange failed: rest: {e1}; connect: {e2}"
                )))
            }
        },
    };

    // Как oh-my-pi `devinCliMetadata`: префикс получает только голый JWT,
    // остальные ключи (`sk-ws-…`) с префиксом сервер отвергает.
    let token = if token.starts_with(SESSION_PREFIX) || token.split('.').count() != 3 {
        token
    } else {
        format!("{SESSION_PREFIX}{token}")
    };

    store.set(
        ProviderKind::Devin,
        &[
            ("session_token".to_string(), token),
            ("api_server_url".to_string(), DEFAULT_API_SERVER.to_string()),
            ("devin_api_url".to_string(), DEFAULT_DEVIN_API.to_string()),
        ],
    )
}
