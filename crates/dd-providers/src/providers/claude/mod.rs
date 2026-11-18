//! Claude: OAuth (claude.com/cai) → access/refresh token в credentials.toml,
//! подписка через /api/oauth/profile, лимиты /api/oauth/usage,
//! модели /v1/models, инференс /v1/messages (Anthropic Messages API + SSE).

mod api;
mod auth;
mod fingerprint;

use async_trait::async_trait;

use crate::credentials::CredentialsStore;
use crate::error::Result;
use crate::provider::{EventSink, Provider};
use crate::types::{
    AccountInfo, MessagesRequest, MessagesResponse, ModelInfo, PendingLogin, ProviderKind,
};

pub const BASE_API: &str = "https://api.anthropic.com";
pub const AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
pub const MANUAL_REDIRECT: &str = "https://platform.claude.com/oauth/code/callback";
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const OAUTH_BETA: &str = "oauth-2025-04-20";
pub const API_VERSION: &str = "2023-06-01";

/// Скоупы consumer-логина (claude.ai): union из org:create_api_key + user:*.
pub const SCOPES: &str = "org:create_api_key user:profile user:inference \
user:sessions:claude_code user:mcp_servers user:file_upload user:plugins \
user:privacy_consents:write";

pub struct Claude {
    store: CredentialsStore,
    client: reqwest::Client,
}

pub struct ClaudeCreds {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_ms: i64,
    pub scopes: Vec<String>,
    pub subscription_type: Option<String>,
    pub rate_limit_tier: Option<String>,
}

impl Claude {
    pub fn new(store: CredentialsStore) -> Self {
        Self {
            store,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .unwrap_or_default(),
        }
    }

    pub(crate) fn creds(&self) -> Result<ClaudeCreds> {
        let s = self.store.section(ProviderKind::Claude)?;
        let access_token = s
            .get("access_token")
            .cloned()
            .ok_or(crate::error::Error::NotAuthenticated)?;
        Ok(ClaudeCreds {
            access_token,
            refresh_token: s.get("refresh_token").cloned(),
            expires_at_ms: s
                .get("expires_at_ms")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            scopes: s
                .get("scopes")
                .map(|v| v.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default(),
            subscription_type: s.get("subscription_type").cloned(),
            rate_limit_tier: s.get("rate_limit_tier").cloned(),
        })
    }
}

#[async_trait]
impl Provider for Claude {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Claude
    }

    fn display_name(&self) -> &'static str {
        "Claude"
    }

    fn is_authenticated(&self) -> bool {
        self.creds().is_ok()
    }

    /// Claude-логин — локальный callback. Для вставки кода руками —
    /// `begin_login_manual()` (redirect_uri = platform.claude.com/.../callback).
    fn begin_login(&self) -> Result<PendingLogin> {
        auth::begin_login(false)
    }

    fn begin_login_manual(&self) -> Result<PendingLogin> {
        auth::begin_login(true)
    }

    async fn finish_login(&self, pending: PendingLogin, manual_code: Option<String>) -> Result<()> {
        auth::finish_login(&self.client, &self.store, pending, manual_code).await
    }

    async fn refresh(&self) -> Result<()> {
        api::refresh_if_needed(&self.client, &self.store).await.map(|_| ())
    }

    async fn account_info(&self) -> Result<AccountInfo> {
        let creds = api::fresh_token(&self.client, &self.store).await?;
        api::account_info(&self.client, &creds.access_token, &creds).await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let creds = api::fresh_token(&self.client, &self.store).await?;
        api::list_models(&self.client, &creds.access_token).await
    }

    async fn messages(&self, req: &MessagesRequest) -> Result<MessagesResponse> {
        let creds = api::fresh_token(&self.client, &self.store).await?;
        api::messages(&self.client, &creds.access_token, req).await
    }

    async fn stream_messages(
        &self,
        req: &MessagesRequest,
        sink: EventSink,
    ) -> Result<MessagesResponse> {
        let creds = api::fresh_token(&self.client, &self.store).await?;
        api::stream_messages(&self.client, &creds.access_token, req, sink).await
    }

    fn logout(&self) -> Result<()> {
        self.store.remove(ProviderKind::Claude)
    }
}
