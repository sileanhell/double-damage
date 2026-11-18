//! Общий интерфейс провайдера: OAuth-логин, информация об аккаунте/лимитах,
//! список моделей и Messages API (Anthropic-shaped) в sync и streaming вариантах.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::credentials::CredentialsStore;
use crate::error::{Error, Result};
use crate::oauth;
use crate::types::{
    AccountInfo, MessagesRequest, MessagesResponse, ModelInfo, PendingLogin, ProviderKind,
    StreamEvent,
};

/// Таймаут ожидания OAuth-redirect'а.
pub const LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Событие стриминга уходит в этот канал по мере поступления.
pub type EventSink = mpsc::UnboundedSender<StreamEvent>;

/// Единый интерфейс LLM-провайдера.
///
/// Жизненный цикл:
/// `login()` (OAuth в браузере) → `account_info()`/`list_models()` → `messages()`/`stream_messages()`.
/// Креды лежат в `credentials.toml` рядом с бинарником и перечитываются при каждом вызове.
#[async_trait]
pub trait Provider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn display_name(&self) -> &'static str;

    /// Есть ли сохранённые креды.
    fn is_authenticated(&self) -> bool;

    // --- OAuth -------------------------------------------------------------

    /// Шаг 1: сформировать `authorize_url` (и поднять loopback-сервер, если не manual).
    fn begin_login(&self) -> Result<PendingLogin>;

    /// Шаг 1 (ручной флоу): `authorize_url` без loopback-сервера — код вставляется
    /// вручную (`finish_login(pending, Some(code))`). По умолчанию = `begin_login()`
    /// (у OpenRouter/Devin redirect_uri не меняется, код можно вставить руками).
    fn begin_login_manual(&self) -> Result<PendingLogin> {
        self.begin_login()
    }

    /// Шаг 2: дождаться кода (loopback) или принять вставленный `manual_code`,
    /// обменять на токены и сохранить в credentials.toml.
    async fn finish_login(&self, pending: PendingLogin, manual_code: Option<String>) -> Result<()>;

    /// Полный интерактивный логин: открывает браузер и ждёт завершения.
    async fn login(&self) -> Result<()> {
        let pending = self.begin_login()?;
        if pending.manual {
            return Err(Error::OAuth(
                "manual flow: call begin_login()/finish_login() with pasted code".into(),
            ));
        }
        oauth::open_browser(&pending.authorize_url)?;
        self.finish_login(pending, None).await
    }

    /// Обновить токен (только Claude; у остальных no-op).
    async fn refresh(&self) -> Result<()> {
        Ok(())
    }

    // --- Данные аккаунта ---------------------------------------------------

    /// Баланс/подписка/квоты.
    async fn account_info(&self) -> Result<AccountInfo>;

    /// Доступные модели.
    async fn list_models(&self) -> Result<Vec<ModelInfo>>;

    // --- Messages API ------------------------------------------------------

    /// Один запрос-ответ (не-стриминг).
    async fn messages(&self, req: &MessagesRequest) -> Result<MessagesResponse>;

    /// Стриминговый ответ; события — в `sink`, возвращает собранный ответ.
    async fn stream_messages(
        &self,
        req: &MessagesRequest,
        sink: EventSink,
    ) -> Result<MessagesResponse>;

    /// Удалить креды (logout).
    fn logout(&self) -> Result<()>;
}

/// Все поддерживаемые провайдеры.
pub fn providers() -> Vec<ProviderKind> {
    ProviderKind::ALL.to_vec()
}

/// Создать провайдера по id.
pub fn provider(kind: ProviderKind) -> Box<dyn Provider> {
    provider_with_store(kind, CredentialsStore::new())
}

/// Создать провайдера с явным путём к credentials.toml (тесты, нестандартное место).
pub fn provider_with_store(kind: ProviderKind, store: CredentialsStore) -> Box<dyn Provider> {
    match kind {
        ProviderKind::OpenRouter => Box::new(crate::providers::openrouter::OpenRouter::new(store)),
        ProviderKind::Devin => Box::new(crate::providers::devin::Devin::new(store)),
        ProviderKind::Claude => Box::new(crate::providers::claude::Claude::new(store)),
    }
}
