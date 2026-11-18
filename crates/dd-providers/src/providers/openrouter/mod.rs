//! OpenRouter: OAuth PKCE → API key, баланс через /api/v1/key,
//! модели /api/v1/models, инференс /api/v1/chat/completions (OpenAI-shaped + SSE).

mod api;
mod auth;

use async_trait::async_trait;

use crate::credentials::CredentialsStore;
use crate::error::Result;
use crate::provider::{EventSink, Provider};
use crate::types::{
    AccountInfo, MessagesRequest, MessagesResponse, ModelInfo, PendingLogin, ProviderKind,
};

pub const API_BASE: &str = "https://openrouter.ai/api/v1";
pub const AUTH_URL: &str = "https://openrouter.ai/auth";

pub struct OpenRouter {
    store: CredentialsStore,
    client: reqwest::Client,
}

impl OpenRouter {
    pub fn new(store: CredentialsStore) -> Self {
        Self {
            store,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
        }
    }

    pub(crate) fn api_key(&self) -> Result<String> {
        self.store
            .get(ProviderKind::OpenRouter, "api_key")?
            .ok_or(crate::error::Error::NotAuthenticated)
    }
}

#[async_trait]
impl Provider for OpenRouter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenRouter
    }

    fn display_name(&self) -> &'static str {
        "OpenRouter"
    }

    fn is_authenticated(&self) -> bool {
        self.api_key().is_ok()
    }

    fn begin_login(&self) -> Result<PendingLogin> {
        auth::begin_login()
    }

    async fn finish_login(&self, pending: PendingLogin, manual_code: Option<String>) -> Result<()> {
        auth::finish_login(&self.client, &self.store, pending, manual_code).await
    }

    async fn account_info(&self) -> Result<AccountInfo> {
        api::account_info(&self.client, &self.api_key()?).await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        api::list_models(&self.client, &self.api_key()?).await
    }

    async fn messages(&self, req: &MessagesRequest) -> Result<MessagesResponse> {
        api::messages(&self.client, &self.api_key()?, req).await
    }

    async fn stream_messages(
        &self,
        req: &MessagesRequest,
        sink: EventSink,
    ) -> Result<MessagesResponse> {
        api::stream_messages(&self.client, &self.api_key()?, req, sink).await
    }

    fn logout(&self) -> Result<()> {
        self.store.remove(ProviderKind::OpenRouter)
    }
}
