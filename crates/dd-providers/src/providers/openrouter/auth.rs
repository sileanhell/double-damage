//! OAuth PKCE OpenRouter:
//! `GET https://openrouter.ai/auth?callback_url=…&code_challenge=…&code_challenge_method=S256`
//! → redirect с `?code=` → `POST /api/v1/auth/keys {code, code_verifier, code_challenge_method}`
//! → `{key: "sk-or-..."}`.

use serde::Deserialize;

use crate::credentials::CredentialsStore;
use crate::error::{ensure_ok, Error, Result};
use crate::oauth::{self, CallbackServer};
use crate::provider::LOGIN_TIMEOUT;
use crate::types::{AuthCallback, PendingLogin, ProviderKind};

use super::API_BASE;

pub fn begin_login() -> Result<PendingLogin> {
    let (verifier, challenge) = oauth::pkce_pair();
    let server = CallbackServer::bind("/callback")?;
    let url = format!(
        "{}?callback_url={}&code_challenge={}&code_challenge_method=S256",
        super::AUTH_URL,
        oauth::urlencode(&server.redirect_uri()),
        challenge
    );
    Ok(PendingLogin {
        provider: ProviderKind::OpenRouter,
        authorize_url: url,
        manual: false,
        state: String::new(),
        code_verifier: verifier,
        server: Some(server),
    })
}

#[derive(Deserialize)]
struct KeyResponse {
    key: String,
}

pub async fn finish_login(
    client: &reqwest::Client,
    store: &CredentialsStore,
    pending: PendingLogin,
    manual_code: Option<String>,
) -> Result<()> {
    let code = match manual_code {
        Some(c) if !c.trim().is_empty() => c.trim().to_string(),
        _ => {
            let server = pending
                .server
                .ok_or_else(|| Error::OAuth("no callback server".into()))?;
            let AuthCallback { code, .. } = server.wait(LOGIN_TIMEOUT).await?;
            if code.is_empty() {
                return Err(Error::OAuth("empty authorization code".into()));
            }
            code
        }
    };

    let resp = client
        .post(format!("{API_BASE}/auth/keys"))
        .json(&serde_json::json!({
            "code": code,
            "code_verifier": pending.code_verifier,
            "code_challenge_method": "S256",
        }))
        .send()
        .await?;
    let resp = ensure_ok(resp).await?;
    let KeyResponse { key } = resp
        .json::<KeyResponse>()
        .await
        .map_err(|e| Error::Parse(format!("auth/keys response: {e}")))?;

    store.set(
        ProviderKind::OpenRouter,
        &[("api_key".to_string(), key)],
    )
}
