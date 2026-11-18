//! Devin (chisel/Windsurf-протокол): OAuth PKCE → session token,
//! ConnectRPC на server.codeium.com: GetUserStatus (подписка/квоты),
//! GetCliModelConfigs (каталог), GetChatMessage (стриминговый инференс).

mod api;
mod auth;
mod catalog;

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::credentials::CredentialsStore;
use crate::error::Result;
use crate::provider::{EventSink, Provider};
use crate::types::{
    AccountInfo, MessagesRequest, MessagesResponse, ModelInfo, PendingLogin, ProviderKind,
};

pub const DEFAULT_API_SERVER: &str = "https://server.codeium.com";
pub const DEFAULT_DEVIN_API: &str = "https://api.devin.ai";
pub const DEFAULT_WEBAPP: &str = "https://app.devin.ai";

pub struct Devin {
    store: CredentialsStore,
    client: reqwest::Client,
    /// Свёрнутые семейства из последнего каталога: id модели → uid по effort.
    routes: Mutex<HashMap<String, catalog::Routes>>,
}

pub struct DevinConfig {
    pub session_token: String,
    pub api_server_url: String,
    pub devin_api_url: String,
    pub user_ref: Option<String>,
}

impl Devin {
    pub fn new(store: CredentialsStore) -> Self {
        Self {
            store,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .unwrap_or_default(),
            routes: Mutex::new(HashMap::new()),
        }
    }

    /// Запрос с `model` = wire-uid: семейство + effort → конкретный конфиг
    /// (`gpt-5-6` + high → `gpt-5-6-high`). Каталог подтягивается, если его
    /// ещё не загружали; незнакомый id уходит как есть.
    async fn routed(&self, req: &MessagesRequest) -> Result<MessagesRequest> {
        let known = self.routes.lock().map(|r| !r.is_empty()).unwrap_or(false);
        if !known {
            let _ = self.list_models().await;
        }
        let mut req = req.clone();
        if let Ok(routes) = self.routes.lock()
            && let Some(route) = routes.get(&req.model)
        {
            let effort = req.effort.map_or("выкл", |e| e.as_str());
            req.model = route
                .resolve(req.effort)
                .ok_or_else(|| {
                    crate::error::Error::Request(format!(
                        "у модели {} нет уровня рассуждений «{effort}» — выберите другой в /effort",
                        req.model
                    ))
                })?
                .to_string();
        }
        Ok(req)
    }

    pub(crate) fn config(&self) -> Result<DevinConfig> {
        let s = self.store.section(ProviderKind::Devin)?;
        let session_token = s
            .get("session_token")
            .cloned()
            .ok_or(crate::error::Error::NotAuthenticated)?;
        Ok(DevinConfig {
            session_token,
            api_server_url: s
                .get("api_server_url")
                .cloned()
                .unwrap_or_else(|| DEFAULT_API_SERVER.to_string()),
            devin_api_url: s
                .get("devin_api_url")
                .cloned()
                .unwrap_or_else(|| DEFAULT_DEVIN_API.to_string()),
            user_ref: s.get("user_ref").cloned(),
        })
    }
}

#[async_trait]
impl Provider for Devin {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Devin
    }

    fn display_name(&self) -> &'static str {
        "Devin"
    }

    fn is_authenticated(&self) -> bool {
        self.config().is_ok()
    }

    fn begin_login(&self) -> Result<PendingLogin> {
        auth::begin_login()
    }

    async fn finish_login(&self, pending: PendingLogin, manual_code: Option<String>) -> Result<()> {
        auth::finish_login(&self.client, &self.store, pending, manual_code).await
    }

    async fn account_info(&self) -> Result<AccountInfo> {
        api::account_info(&self.client, &self.config()?).await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let (models, routes) = api::list_models(&self.client, &self.config()?).await?;
        if let Ok(mut current) = self.routes.lock() {
            *current = routes;
        }
        Ok(models)
    }

    async fn messages(&self, req: &MessagesRequest) -> Result<MessagesResponse> {
        let req = self.routed(req).await?;
        api::messages(&self.client, &self.config()?, &req).await
    }

    async fn stream_messages(
        &self,
        req: &MessagesRequest,
        sink: EventSink,
    ) -> Result<MessagesResponse> {
        let req = self.routed(req).await?;
        api::stream_messages(&self.client, &self.config()?, &req, sink).await
    }

    fn logout(&self) -> Result<()> {
        self.store.remove(ProviderKind::Devin)
    }
}
