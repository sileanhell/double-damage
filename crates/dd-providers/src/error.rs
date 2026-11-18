use std::fmt;

/// Ошибка провайдера: сеть, авторизация, парсинг ответа, неподдерживаемое действие.
#[derive(Debug)]
pub enum Error {
    /// Нет сохранённых кредов (нужен `login`).
    NotAuthenticated,
    /// Токен/ключ отклонён сервером (401/403) — нужна повторная авторизация.
    Unauthorized(String),
    /// HTTP-ошибка с кодом и телом ответа.
    Http { status: u16, body: String },
    /// Сетевая/транспортная ошибка.
    Transport(String),
    /// Ошибка разбора ответа.
    Parse(String),
    /// Ошибка чтения/записи credentials.toml.
    Store(String),
    /// OAuth-флоу завершился ошибкой (пользователь отменил, таймаут, обмен кода).
    OAuth(String),
    /// Функция не поддерживается провайдером.
    Unsupported(&'static str),
    /// Запрос нельзя выполнить с такими параметрами.
    Request(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAuthenticated => write!(f, "not authenticated (run auth first)"),
            Self::Unauthorized(m) => write!(f, "unauthorized: {m}"),
            Self::Http { status, body } => write!(f, "http {status}: {body}"),
            Self::Transport(m) => write!(f, "transport: {m}"),
            Self::Parse(m) => write!(f, "parse: {m}"),
            Self::Store(m) => write!(f, "credentials store: {m}"),
            Self::OAuth(m) => write!(f, "oauth: {m}"),
            Self::Unsupported(m) => write!(f, "unsupported: {m}"),
            Self::Request(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        if let Some(s) = e.status() {
            Self::Http { status: s.as_u16(), body: e.to_string() }
        } else {
            Self::Transport(e.to_string())
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Parse(e.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Store(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Помощник: если статус не успешен — читает тело и делает Error::Http/Unauthorized.
pub async fn ensure_ok(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    let body = body.chars().take(2000).collect();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(Error::Unauthorized(body));
    }
    Err(Error::Http { status: status.as_u16(), body })
}
