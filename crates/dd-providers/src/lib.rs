//! # dd-providers
//!
//! Единый слой LLM-провайдеров для `dd-desktop` и `dd-cli`.
//! Сейчас: **OpenRouter**, **Devin**, **Claude** — по папке на провайдера.
//!
//! Возможности каждого провайдера:
//! - OAuth-логин (PKCE + loopback callback; у Claude ещё ручная вставка `code#state`)
//! - хранение кредов в `credentials.toml` рядом с бинарником
//! - `account_info()` — баланс ($ у OpenRouter) / подписка + срок (Devin, Claude)
//! - `list_models()` — каталог моделей
//! - `messages()` / `stream_messages()` — унифицированный Messages API
//!   (типы повторяют Anthropic Messages API: `ContentBlock`, `Tool`,
//!   `StopReason`, `Usage`, SSE-подобные `StreamEvent`).
//!
//! ```no_run
//! use dd_providers::{provider, ProviderKind, MessagesRequest, Message};
//!
//! # async fn demo() -> dd_providers::Result<()> {
//! let p = provider(ProviderKind::OpenRouter);
//! p.login().await?;                                  // OAuth в браузере
//! let info = p.account_info().await?;                // баланс/подписка
//! let models = p.list_models().await?;               // каталог
//! let resp = p.messages(&MessagesRequest {
//!     model: "openai/gpt-5.2".into(),
//!     messages: vec![Message::user("hi")],
//!     max_tokens: 256,
//!     ..Default::default()
//! }).await?;
//! println!("{}", resp.text());
//! # Ok(())
//! # }
//! ```

pub mod credentials;
pub mod error;
pub mod oauth;
mod provider;
pub mod providers;
pub mod proto;
pub mod types;

pub use credentials::{credentials_path, CredentialsStore};
pub use error::{Error, Result};
pub use provider::{provider, provider_with_store, providers, EventSink, Provider, LOGIN_TIMEOUT};
pub use types::{
    AccountInfo, AuthCallback, ContentBlock, Effort, Message, MessagesRequest, MessagesResponse,
    ModelInfo, PendingLogin, ProviderKind, Quota, Role, StopReason, StreamEvent, Tool, Usage,
};

/// Простой метод крейта dd-providers.
pub fn hello() -> String {
    "Привет из dd-providers!".to_string()
}
