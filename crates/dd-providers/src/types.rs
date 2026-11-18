use serde::{Deserialize, Serialize};

/// Идентификатор провайдера.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenRouter,
    Devin,
    Claude,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 3] =
        [ProviderKind::OpenRouter, ProviderKind::Devin, ProviderKind::Claude];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter",
            Self::Devin => "devin",
            Self::Claude => "claude",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::OpenRouter => "OpenRouter",
            Self::Devin => "Devin",
            Self::Claude => "Claude",
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ProviderKind {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "openrouter" | "or" => Ok(Self::OpenRouter),
            "devin" => Ok(Self::Devin),
            "claude" | "anthropic" => Ok(Self::Claude),
            other => Err(format!("unknown provider '{other}' (openrouter|devin|claude)")),
        }
    }
}

// ---------------------------------------------------------------------------
// Messages API (модель по Anthropic Messages API)
// ---------------------------------------------------------------------------

/// Роль сообщения. `System` при маппинге уходит в системный промпт либо
/// в role=system — зависит от провайдера.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

/// Блок контента сообщения (как у Anthropic: text / thinking / tool_use / tool_result).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    Thinking { thinking: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, #[serde(default)] is_error: bool },
}

impl ContentBlock {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn new(role: Role, content: Vec<ContentBlock>) -> Self {
        Self { role, content }
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, content: vec![ContentBlock::text(text)] }
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: vec![ContentBlock::text(text)] }
    }
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: Role::System, content: vec![ContentBlock::text(text)] }
    }
    /// Весь текст сообщения склеенный (для провайдеров без блоков).
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Описание инструмента (JSON Schema на входе — как у Anthropic `input_schema`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Унифицированный запрос к модели (поля = Anthropic Messages API).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    pub messages: Vec<Message>,
    /// Системный промпт (у Anthropic — отдельное поле).
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub max_tokens: u32,
    #[serde(default)]
    pub tools: Vec<Tool>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub stop_sequences: Vec<String>,
    #[serde(default)]
    pub stream: bool,
    /// Сколько модель рассуждает перед ответом; `None` — не просить
    /// рассуждений (поведение модели по умолчанию).
    #[serde(default)]
    pub effort: Option<Effort>,
}

/// Уровень рассуждений: у Claude — `thinking` / `output_config.effort` по
/// режиму модели, у OpenRouter — `reasoning.effort`, у Devin — выбор
/// конфига семейства (`… Low` / `… High`). `Minimal` и `XHigh` бывают
/// только у моделей, которые сами их объявляют ([`ModelInfo::efforts`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    /// Уровни, которые понимает любая модель без собственного списка.
    pub const STANDARD: [Effort; 4] = [Effort::Low, Effort::Medium, Effort::High, Effort::Max];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Бюджет `thinking.budget_tokens` для Anthropic (минимум у API — 1024).
    pub fn thinking_budget(&self) -> u32 {
        match self {
            Self::Minimal => 1024,
            Self::Low => 2048,
            Self::Medium => 8192,
            Self::High => 16384,
            Self::XHigh => 24576,
            Self::Max => 31999,
        }
    }

    /// Значение `reasoning.effort` у OpenRouter.
    pub fn openrouter(&self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh | Self::Max => "xhigh",
        }
    }
}

impl std::str::FromStr for Effort {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            other => Err(format!(
                "unknown effort '{other}' (minimal|low|medium|high|xhigh|max)"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    Refusal,
    #[serde(untagged)]
    Other(String),
}

impl StopReason {
    pub fn from_openai(s: &str) -> Self {
        match s {
            "stop" => Self::EndTurn,
            "length" => Self::MaxTokens,
            "tool_calls" | "function_call" => Self::ToolUse,
            "content_filter" => Self::Refusal,
            other => Self::Other(other.to_string()),
        }
    }
    pub fn from_anthropic(s: &str) -> Self {
        match s {
            "end_turn" => Self::EndTurn,
            "max_tokens" => Self::MaxTokens,
            "stop_sequence" => Self::StopSequence,
            "tool_use" => Self::ToolUse,
            "refusal" => Self::Refusal,
            other => Self::Other(other.to_string()),
        }
    }
}

/// Унифицированный ответ (Anthropic-shaped).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MessagesResponse {
    pub id: String,
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<StopReason>,
    pub usage: Usage,
}

impl MessagesResponse {
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

/// Событие стриминга (унифицированное поверх SSE Anthropic / SSE OpenAI / connect-фреймов Devin).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart { message_id: Option<String>, model: Option<String> },
    TextDelta { text: String },
    ThinkingDelta { thinking: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    MessageDelta { stop_reason: Option<StopReason>, usage: Option<Usage> },
    MessageStop,
    Error { message: String },
}

// ---------------------------------------------------------------------------
// Модели и аккаунт
// ---------------------------------------------------------------------------

/// Информация о модели провайдера.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Ид, который передаётся в `MessagesRequest.model`.
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    /// USD за 1M входных токенов.
    #[serde(default)]
    pub input_price: Option<f64>,
    /// USD за 1M выходных токенов.
    #[serde(default)]
    pub output_price: Option<f64>,
    /// Уровни рассуждений модели; `None` — стандартные ([`Effort::STANDARD`]).
    #[serde(default)]
    pub efforts: Option<Vec<Effort>>,
    /// Можно ли выключить рассуждения («выкл» в `/effort`).
    #[serde(default = "yes")]
    pub effort_off: bool,
    /// Уровень, с которого модель начинает, если текущий ей не подходит.
    #[serde(default)]
    pub default_effort: Option<Effort>,
}

fn yes() -> bool {
    true
}

impl Default for ModelInfo {
    fn default() -> Self {
        Self {
            id: String::new(),
            display_name: String::new(),
            description: None,
            context_window: None,
            max_output_tokens: None,
            input_price: None,
            output_price: None,
            efforts: None,
            effort_off: true,
            default_effort: None,
        }
    }
}

impl ModelInfo {
    /// Уровни, которые предлагает `/effort` (без «выкл»).
    pub fn effort_levels(&self) -> &[Effort] {
        self.efforts.as_deref().unwrap_or(&Effort::STANDARD)
    }

    /// Подходит ли `effort` (`None` — «выкл») этой модели.
    pub fn accepts(&self, effort: Option<Effort>) -> bool {
        match effort {
            None => self.effort_off,
            Some(e) => self.effort_levels().contains(&e),
        }
    }

    /// Уровень модели «по умолчанию»: объявленный в каталоге, иначе
    /// средний, иначе самый низкий из списка. `None`, если уровней нет.
    pub fn default_level(&self) -> Option<Effort> {
        let levels = self.effort_levels();
        self.default_effort
            .filter(|e| levels.contains(e))
            .or_else(|| levels.contains(&Effort::Medium).then_some(Effort::Medium))
            .or_else(|| levels.first().copied())
    }

    /// Ближайший допустимый уровень для перехода на эту модель: текущий,
    /// если подходит, иначе её уровень по умолчанию.
    pub fn fit(&self, effort: Option<Effort>) -> Option<Effort> {
        if self.accepts(effort) {
            return effort;
        }
        self.default_level()
    }
}

/// Одна квота/лимит (дневной, недельный, 5-часовой и т.п.).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Quota {
    /// "daily", "weekly", "five_hour", ...
    pub name: String,
    #[serde(default)]
    pub used: Option<f64>,
    #[serde(default)]
    pub limit: Option<f64>,
    #[serde(default)]
    pub remaining: Option<f64>,
    /// unix-время сброса.
    #[serde(default)]
    pub resets_at: Option<i64>,
    /// 0..100 — загрузка квоты в процентах, если провайдер отдаёт utilization.
    #[serde(default)]
    pub utilization: Option<f64>,
}

/// Сведения об аккаунте: баланс денег (OpenRouter) либо подписка (Devin/Claude).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountInfo {
    pub provider: ProviderKind,
    /// email / username / label ключа.
    #[serde(default)]
    pub label: Option<String>,
    /// "Pro", "Max", "team", "credits", ...
    #[serde(default)]
    pub plan: Option<String>,
    /// unix-время окончания/продления подписки или ключа.
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// unix-время начала подписки.
    #[serde(default)]
    pub started_at: Option<i64>,
    /// Денежный баланс в USD (OpenRouter).
    #[serde(default)]
    pub balance_usd: Option<f64>,
    #[serde(default)]
    pub spent_usd: Option<f64>,
    #[serde(default)]
    pub credit_limit_usd: Option<f64>,
    #[serde(default)]
    pub quotas: Vec<Quota>,
}

// ---------------------------------------------------------------------------
// OAuth
// ---------------------------------------------------------------------------

/// Состояние незавершённого OAuth-флоу. `authorize_url` нужно открыть у пользователя.
pub struct PendingLogin {
    pub provider: ProviderKind,
    /// URL, который открывается в браузере.
    pub authorize_url: String,
    /// `true` = кода на localhost не будет; пользователь вставляет код вручную
    /// (нужен `manual_code` в `finish_login`). `false` = код ловит loopback-сервер.
    pub manual: bool,
    /// Ожидаемый `state` (пустой — не проверять).
    pub(crate) state: String,
    pub(crate) code_verifier: String,
    pub(crate) server: Option<crate::oauth::CallbackServer>,
}

/// Параметры, пришедшие на OAuth-redirect.
#[derive(Clone, Debug, Default)]
pub struct AuthCallback {
    pub code: String,
    pub state: String,
}
