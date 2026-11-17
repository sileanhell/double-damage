//! Remote model providers and what the core reports about their accounts.

pub mod claude;
pub mod devin;

use serde::Serialize;

pub use claude::Claude;
pub use devin::Devin;

/// An account's plan, money and rate limits, normalized across providers
/// (`GET /dd/providers/{id}/account`). `raw` keeps the provider's own answer.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub provider: String,
    pub email: Option<String>,
    pub organization: Option<String>,
    pub plan: Plan,
    /// Money on the account (extra usage / credits), when the provider has any.
    pub balance: Option<Balance>,
    /// Usage windows: how much of each is used and when it resets.
    pub limits: Vec<Limit>,
    /// ms epoch the data was fetched at.
    pub fetched_at: u64,
    pub raw: serde_json::Value,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// Subscription type as the provider names it: `max`, `pro`, `Pro`, `Teams`, …
    pub name: Option<String>,
    /// Finer tier (Claude's `rate_limit_tier`, Devin's teams tier).
    pub tier: Option<String>,
    pub billing: Option<String>,
    /// ISO 8601 times.
    pub started_at: Option<String>,
    /// When the paid period ends (renewal or expiry); None when the provider doesn't say.
    pub expires_at: Option<String>,
    pub trial_ends_at: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Balance {
    pub enabled: bool,
    pub currency: Option<String>,
    /// In currency units (not cents).
    pub used: Option<f64>,
    pub limit: Option<f64>,
    pub remaining: Option<f64>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Limit {
    /// Stable key, e.g. `five_hour`, `seven_day`, `daily`, `weekly`.
    pub id: String,
    pub label: String,
    /// 0–100 (above 100 once over).
    pub used_percent: Option<f64>,
    pub resets_at: Option<String>,
    /// Provider's own state for the window (`allowed`, `rejected`, `normal`, …).
    pub status: Option<String>,
}

/// What `GET /dd/providers` lists.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    pub id: &'static str,
    pub name: &'static str,
    /// Local models need no sign-in; remote ones do.
    pub kind: &'static str,
    pub signed_in: bool,
    pub models: usize,
}
