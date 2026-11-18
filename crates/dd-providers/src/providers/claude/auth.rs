//! OAuth Claude (как у Claude Code):
//! `GET {AUTHORIZE}?code=true&client_id=…&response_type=code&redirect_uri=…
//!   &scope=…&code_challenge=…&code_challenge_method=S256&state=…`
//! → код (loopback `?code&state`, либо ручная вставка `code#state`)
//! → `POST {TOKEN_URL} {grant_type:"authorization_code", code, redirect_uri,
//!    client_id, code_verifier, state}` → `{access_token, refresh_token, expires_in}`.

use serde::Deserialize;

use crate::credentials::CredentialsStore;
use crate::error::{Error, Result};
use crate::oauth::{self, CallbackServer};
use crate::provider::LOGIN_TIMEOUT;
use crate::types::{PendingLogin, ProviderKind};

use super::{AUTHORIZE_URL, CLIENT_ID, MANUAL_REDIRECT, SCOPES, TOKEN_URL};

/// `manual=false` — loopback `http://localhost:<port>/callback`;
/// `manual=true` — redirect на MANUAL_REDIRECT, пользователь вставляет `code#state`.
pub fn begin_login(manual: bool) -> Result<PendingLogin> {
    let (verifier, challenge) = oauth::pkce_pair();
    let state = oauth::random_state();
    let server = if manual {
        None
    } else {
        Some(CallbackServer::bind("/callback")?)
    };
    let redirect_uri = server
        .as_ref()
        .map(|s| format!("http://localhost:{}{}", s.port, s.path))
        .unwrap_or_else(|| MANUAL_REDIRECT.to_string());
    let url = format!(
        "{AUTHORIZE_URL}?{}",
        oauth::query_string(&[
            ("code", "true"),
            ("client_id", CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", &redirect_uri),
            ("scope", SCOPES),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
        ])
    );
    Ok(PendingLogin {
        provider: ProviderKind::Claude,
        authorize_url: url,
        manual,
        state,
        code_verifier: verifier,
        server,
    })
}

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: i64,
    #[serde(default)]
    pub refresh_token_expires_in: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
}

/// Обмен `authorization_code` на токены.
pub async fn exchange_code(
    client: &reqwest::Client,
    code: &str,
    state: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse> {
    token_request(
        client,
        &serde_json::json!({
            "grant_type": "authorization_code",
            "code": code,
            "state": state,
            "redirect_uri": redirect_uri,
            "client_id": CLIENT_ID,
            "code_verifier": verifier,
        }),
    )
    .await
}

/// Обмен `refresh_token` на новые токены.
pub async fn exchange_refresh(
    client: &reqwest::Client,
    refresh_token: &str,
    scopes: &str,
) -> Result<TokenResponse> {
    token_request(
        client,
        &serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
            "scope": scopes,
        }),
    )
    .await
}

async fn token_request(client: &reqwest::Client, body: &serde_json::Value) -> Result<TokenResponse> {
    let resp = client.post(TOKEN_URL).json(body).send().await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(Error::OAuth(format!(
            "token request failed ({}): {}",
            status.as_u16(),
            text.chars().take(500).collect::<String>()
        )));
    }
    serde_json::from_str(&text).map_err(|e| Error::Parse(format!("token response: {e}")))
}

/// Сохранить токены + профильные поля в credentials.toml.
pub async fn save_tokens(
    client: &reqwest::Client,
    store: &CredentialsStore,
    t: &TokenResponse,
) -> Result<()> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let mut vals = vec![
        ("access_token".to_string(), t.access_token.clone()),
        (
            "expires_at_ms".to_string(),
            (now_ms + t.expires_in * 1000).to_string(),
        ),
    ];
    if let Some(rt) = &t.refresh_token {
        vals.push(("refresh_token".to_string(), rt.clone()));
        if let Some(rte) = t.refresh_token_expires_in {
            vals.push((
                "refresh_expires_at_ms".to_string(),
                (now_ms + rte * 1000).to_string(),
            ));
        }
    }
    if let Some(s) = &t.scope {
        vals.push(("scopes".to_string(), s.clone()));
    }

    // Профиль → subscription_type / rate_limit_tier (best-effort).
    if let Ok(resp) = client
        .get(format!("{}/api/oauth/profile", super::BASE_API))
        .bearer_auth(&t.access_token)
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(v) = resp.json::<serde_json::Value>().await {
                let org = &v["organization"];
                if let Some(st) = org["organization_type"].as_str() {
                    vals.push(("subscription_type".to_string(), st.to_string()));
                }
                if let Some(rlt) = org["rate_limit_tier"].as_str() {
                    vals.push(("rate_limit_tier".to_string(), rlt.to_string()));
                }
                if let Some(email) = v["account"]["email"].as_str() {
                    vals.push(("email".to_string(), email.to_string()));
                }
            }
        }
    }

    store.set(ProviderKind::Claude, &vals)
}

pub async fn finish_login(
    client: &reqwest::Client,
    store: &CredentialsStore,
    pending: PendingLogin,
    manual_code: Option<String>,
) -> Result<()> {
    let redirect_uri = if pending.manual {
        MANUAL_REDIRECT.to_string()
    } else {
        pending
            .server
            .as_ref()
            .map(|s| format!("http://localhost:{}{}", s.port, s.path))
            .unwrap_or_default()
    };

    let (code, state) = if pending.manual {
        // Формат вставки: "code#state" (как у Claude Code).
        let pasted = manual_code
            .ok_or_else(|| Error::OAuth("paste the authorization code".into()))?;
        let pasted = pasted.trim();
        match pasted.split_once('#') {
            Some((c, s)) => (c.to_string(), s.to_string()),
            None => (pasted.to_string(), String::new()),
        }
    } else {
        let server = pending
            .server
            .ok_or_else(|| Error::OAuth("no callback server".into()))?;
        let cb = server.wait(LOGIN_TIMEOUT).await?;
        (cb.code, cb.state)
    };

    if code.is_empty() {
        return Err(Error::OAuth("empty authorization code".into()));
    }
    if !pending.state.is_empty() && !state.is_empty() && state != pending.state {
        return Err(Error::OAuth("state mismatch".into()));
    }

    let tokens = exchange_code(
        client,
        &code,
        &state,
        &pending.code_verifier,
        &redirect_uri,
    )
    .await?;
    save_tokens(client, store, &tokens).await
}
