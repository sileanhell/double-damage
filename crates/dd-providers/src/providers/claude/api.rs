//! REST-клиент Anthropic API поверх OAuth-токена:
//! профиль/лимиты, /v1/models, /v1/messages (+SSE), refresh-токен.

use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::credentials::CredentialsStore;
use crate::error::{ensure_ok, Error, Result};
use crate::provider::EventSink;
use crate::types::{
    AccountInfo, ContentBlock, Message, MessagesRequest, MessagesResponse, ModelInfo,
    ProviderKind, Quota, Role, StopReason, StreamEvent, Usage,
};

use super::fingerprint::{self, ModelTraits, ThinkingMode};
use super::{auth, ClaudeCreds, API_VERSION, BASE_API, OAUTH_BETA, SCOPES};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// --- refresh -----------------------------------------------------------------

/// Обновить access_token, если до истечения < 60 c. Возвращает свежие креды.
pub async fn fresh_token(client: &reqwest::Client, store: &CredentialsStore) -> Result<ClaudeCreds> {
    refresh_if_needed(client, store).await?;
    let s = store.section(ProviderKind::Claude)?;
    let access_token = s
        .get("access_token")
        .cloned()
        .ok_or(Error::NotAuthenticated)?;
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

/// Если access_token протух и есть refresh_token — обновить и сохранить.
/// Возвращает true, если рефреш реально выполнялся.
pub async fn refresh_if_needed(
    client: &reqwest::Client,
    store: &CredentialsStore,
) -> Result<bool> {
    let s = store.section(ProviderKind::Claude)?;
    if s.get("access_token").is_none() {
        return Err(Error::NotAuthenticated);
    }
    let expires: i64 = s
        .get("expires_at_ms")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if expires - 60_000 > now_ms() {
        return Ok(false);
    }
    let Some(rt) = s.get("refresh_token").cloned() else {
        // Рефреша нет — работаем на том что есть (пусть сервер сам решит).
        return Ok(false);
    };
    let scopes = s.get("scopes").cloned().unwrap_or_else(|| SCOPES.to_string());
    let tokens = auth::exchange_refresh(client, &rt, &scopes).await?;
    auth::save_tokens(client, store, &tokens).await?;
    Ok(true)
}

// --- account / limits --------------------------------------------------------

#[derive(Deserialize)]
struct Profile {
    account: Option<Value>,
    organization: Option<ProfileOrg>,
}

#[derive(Deserialize)]
struct ProfileOrg {
    name: Option<String>,
    organization_type: Option<String>,
    subscription_created_at: Option<String>,
    claude_code_trial_ends_at: Option<String>,
}

/// Период оплаты из organization_type: "claude_max" → "Max", "pro" → "Pro", ...
fn plan_name(org_type: &str) -> String {
    match org_type {
        "claude_max" => "Max".into(),
        "claude_pro" | "pro" => "Pro".into(),
        "team" => "Team".into(),
        "enterprise" => "Enterprise".into(),
        other => other.to_string(),
    }
}

#[derive(Deserialize)]
struct UsageWindow {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct UsageResponse {
    five_hour: Option<UsageWindow>,
    seven_day: Option<UsageWindow>,
    seven_day_oauth_apps: Option<UsageWindow>,
    seven_day_opus: Option<UsageWindow>,
    extra_usage: Option<ExtraUsage>,
}

#[derive(Deserialize)]
struct ExtraUsage {
    is_enabled: Option<bool>,
    monthly_limit: Option<f64>,
    used_credits: Option<f64>,
    utilization: Option<f64>,
}

fn ts_rfc3339(s: &str) -> Option<i64> {
    let (date, time) = s.split_once('T')?;
    let mut it = date.split('-');
    let (y, m, d) = (
        it.next()?.parse::<i64>().ok()?,
        it.next()?.parse::<i64>().ok()?,
        it.next()?.parse::<i64>().ok()?,
    );
    let t = time.trim_end_matches('Z').split(['.', '+']).next()?;
    let mut it = t.split(':');
    let (hh, mm, ss) = (
        it.next()?.parse::<i64>().ok()?,
        it.next()?.parse::<i64>().ok()?,
        it.next()?.parse::<i64>().ok()?,
    );
    Some(days_from_civil(y, m, d) * 86400 + hh * 3600 + mm * 60 + ss)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Дни от эпохи → (год, месяц, день) — обратная `days_from_civil`.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as i64;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as i64;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        _ => 28,
    }
}

/// Ближайшее ежемесячное продление после `now`: то же число и время,
/// что у `created` (день подписки — якорь биллинга), с зажимом на конец
/// короткого месяца.
fn next_monthly(created: i64, now: i64) -> i64 {
    let (mut y, mut m, d) = civil_from_days(created.div_euclid(86400));
    let secs = created.rem_euclid(86400);
    loop {
        let ts = days_from_civil(y, m, d.min(days_in_month(y, m))) * 86400 + secs;
        if ts > now {
            return ts;
        }
        (m, y) = if m == 12 { (1, y + 1) } else { (m + 1, y) };
    }
}

pub async fn account_info(
    client: &reqwest::Client,
    token: &str,
    creds: &ClaudeCreds,
) -> Result<AccountInfo> {
    let resp = client
        .get(format!("{BASE_API}/api/oauth/profile"))
        .bearer_auth(token)
        .send()
        .await?;
    let profile: Profile = ensure_ok(resp).await?.json().await?;
    let org = profile.organization.as_ref();
    let email = profile
        .account
        .as_ref()
        .and_then(|a| a["email"].as_str())
        .map(str::to_string)
        .or_else(|| profile.account.as_ref().and_then(|a| a["uuid"].as_str()).map(str::to_string));

    let mut quotas = Vec::new();
    // Лимиты — best-effort: /api/oauth/usage может не отвечать на части тарифов.
    if let Ok(resp) = client
        .get(format!("{BASE_API}/api/oauth/usage"))
        .bearer_auth(token)
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(u) = resp.json::<UsageResponse>().await {
                let mut push = |name: &str, w: &Option<UsageWindow>| {
                    if let Some(w) = w {
                        quotas.push(Quota {
                            name: name.to_string(),
                            used: None,
                            limit: Some(100.0),
                            remaining: w.utilization.map(|u| (100.0 - u).max(0.0)),
                            resets_at: w.resets_at.as_deref().and_then(ts_rfc3339),
                            utilization: w.utilization,
                        });
                    }
                };
                push("five_hour", &u.five_hour);
                push("seven_day", &u.seven_day);
                push("seven_day_oauth_apps", &u.seven_day_oauth_apps);
                push("seven_day_opus", &u.seven_day_opus);
                if let Some(e) = &u.extra_usage {
                    if e.is_enabled == Some(true) || e.used_credits.is_some() {
                        quotas.push(Quota {
                            name: "extra_usage".into(),
                            used: e.used_credits,
                            limit: e.monthly_limit,
                            remaining: match (e.monthly_limit, e.used_credits) {
                                (Some(l), Some(u)) => Some(l - u),
                                _ => None,
                            },
                            resets_at: None,
                            utilization: e.utilization,
                        });
                    }
                }
            }
        }
    }

    let plan = org
        .and_then(|o| o.organization_type.as_deref())
        .map(plan_name)
        .or_else(|| creds.subscription_type.as_deref().map(plan_name));

    // «Когда кончается»: конец триала, иначе ближайшее ежемесячное
    // продление от даты подписки (API точного period_end не отдаёт).
    let now = now_ms() / 1000;
    let expires_at = org
        .and_then(|o| o.claude_code_trial_ends_at.as_deref())
        .and_then(ts_rfc3339)
        .filter(|at| *at > now)
        .or_else(|| {
            org.and_then(|o| o.subscription_created_at.as_deref())
                .and_then(ts_rfc3339)
                .map(|created| next_monthly(created, now))
        });

    Ok(AccountInfo {
        provider: ProviderKind::Claude,
        label: email.or_else(|| org.and_then(|o| o.name.clone())),
        plan,
        expires_at,
        started_at: None,
        balance_usd: None,
        spent_usd: None,
        credit_limit_usd: None,
        quotas,
    })
}

// --- models ------------------------------------------------------------------

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<AnthropicModel>,
}

#[derive(Deserialize)]
struct AnthropicModel {
    id: String,
    display_name: Option<String>,
    created_at: Option<String>,
}

pub async fn list_models(client: &reqwest::Client, token: &str) -> Result<Vec<ModelInfo>> {
    let resp = headers(client.get(format!("{BASE_API}/v1/models?limit=1000")), token)
        .send()
        .await?;
    let d = ensure_ok(resp).await?.json::<ModelsResponse>().await?;
    Ok(d.data
        .into_iter()
        .map(|m| ModelInfo {
            id: m.id.clone(),
            display_name: m.display_name.unwrap_or_else(|| m.id.clone()),
            description: m.created_at.map(|c| format!("created {c}")),
            ..Default::default()
        })
        .collect())
}

// --- messages ----------------------------------------------------------------

fn headers(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    req.bearer_auth(token)
        .header("anthropic-version", API_VERSION)
        .header("anthropic-beta", OAUTH_BETA)
}

/// Заголовки инференса в отпечатке Claude Code: без них OAuth-токен
/// подписки не пускает к Sonnet/Opus.
fn inference_headers(
    req: reqwest::RequestBuilder,
    token: &str,
    body: &Value,
) -> reqwest::RequestBuilder {
    let effort = body["output_config"]["effort"].is_string();
    req.bearer_auth(token)
        .header("anthropic-version", API_VERSION)
        .header("anthropic-beta", fingerprint::betas(effort))
        .header("user-agent", fingerprint::user_agent())
        .header("x-app", "cli")
        .header("anthropic-dangerous-direct-browser-access", "true")
        .header("content-type", "application/json")
}

/// Тело запроса байтами, с вписанной аттестацией `cch`.
fn encode_body(body: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(body)?;
    fingerprint::patch_cch(&mut bytes);
    Ok(bytes)
}

fn content_json(m: &Message) -> Value {
    let parts: Vec<Value> = m
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => json!({ "type": "text", "text": text }),
            ContentBlock::Thinking { thinking } => {
                json!({ "type": "thinking", "thinking": thinking })
            }
            ContentBlock::ToolUse { id, name, input } => json!({
                "type": "tool_use", "id": id, "name": name, "input": input,
            }),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": [{ "type": "text", "text": content }],
                "is_error": is_error,
            }),
        })
        .collect();
    json!({ "role": role_str(m.role), "content": parts })
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::User => "user",
        Role::Assistant => "assistant",
        // Anthropic не имеет role=system в messages — маппим в user
        // (system-сообщения также собираются в req.system ниже).
        Role::System => "user",
    }
}

fn request_json(req: &MessagesRequest, stream: bool) -> Value {
    let mut system = req.system.clone().unwrap_or_default();
    let mut msgs: Vec<Value> = Vec::new();
    for m in &req.messages {
        if m.role == Role::System {
            let t = m.text();
            if !t.is_empty() {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(&t);
            }
            continue;
        }
        msgs.push(content_json(m));
    }
    let traits = ModelTraits::parse(&req.model);
    let ceiling = traits.max_output_tokens();
    let max_tokens = if req.max_tokens > 0 {
        req.max_tokens.min(ceiling)
    } else {
        ceiling
    };
    let first_user = req
        .messages
        .iter()
        .find(|m| m.role == Role::User)
        .map(Message::text)
        .unwrap_or_default();
    // system[0] — billing-строка, system[1] — identity Claude Code, дальше
    // собственный промпт (как `buildAnthropicSystemBlocks` в oh-my-pi).
    let mut system_blocks = vec![
        json!({ "type": "text", "text": fingerprint::billing_header(&first_user) }),
        json!({ "type": "text", "text": fingerprint::IDENTITY }),
    ];
    if !system.is_empty() {
        system_blocks.push(json!({ "type": "text", "text": system }));
    }
    let mut body = json!({
        "model": req.model,
        "messages": msgs,
        "max_tokens": max_tokens,
        "stream": stream,
        "system": system_blocks,
    });
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(t) = req.top_p {
        body["top_p"] = json!(t);
    }
    if !req.stop_sequences.is_empty() {
        body["stop_sequences"] = json!(req.stop_sequences);
    }
    if !req.tools.is_empty() {
        body["tools"] = json!(req
            .tools
            .iter()
            .map(|t| json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            }))
            .collect::<Vec<_>>());
    }
    apply_thinking(&mut body, req, traits, max_tokens);
    body
}

/// Видимых токенов, которые остаются после бюджета рассуждений.
const OUTPUT_AFTER_THINKING: u32 = 4096;

/// Рассуждения по режиму модели (oh-my-pi `anthropic.ts`, «Pre-compute
/// thinking + output_config effort»):
/// - adaptive (4.6+): `thinking: adaptive` + `output_config.effort`; «выкл»
///   у них невозможен, поэтому без effort шлём `low` и не шлём `thinking`;
/// - budget-effort (Opus 4.5): бюджет + `output_config.effort`;
/// - budget: `thinking.budget_tokens`, ужатый под `max_tokens`.
fn apply_thinking(body: &mut Value, req: &MessagesRequest, traits: ModelTraits, max_tokens: u32) {
    let mode = traits.thinking_mode();
    let Some(effort) = req.effort else {
        if mode == ThinkingMode::Adaptive {
            body["output_config"] = json!({ "effort": "low" });
        }
        return;
    };
    if !traits.supports_thinking() {
        return;
    }
    match mode {
        ThinkingMode::Adaptive => {
            body["thinking"] = json!({ "type": "adaptive" });
            body["output_config"] = json!({ "effort": traits.output_effort(effort) });
        }
        ThinkingMode::Budget | ThinkingMode::BudgetEffort => {
            let budget = effort
                .thinking_budget()
                .min(max_tokens.saturating_sub(OUTPUT_AFTER_THINKING));
            if budget < 1024 {
                return;
            }
            body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
            if mode == ThinkingMode::BudgetEffort {
                body["output_config"] = json!({ "effort": traits.output_effort(effort) });
            }
        }
    }
    // temperature/top_p с рассуждениями не сочетаются.
    if let Some(obj) = body.as_object_mut() {
        obj.remove("temperature");
        obj.remove("top_p");
    }
}

pub async fn messages(
    client: &reqwest::Client,
    token: &str,
    req: &MessagesRequest,
) -> Result<MessagesResponse> {
    let body = request_json(req, false);
    let resp = inference_headers(client.post(format!("{BASE_API}/v1/messages")), token, &body)
        .body(encode_body(&body)?)
        .send()
        .await?;
    let v: Value = ensure_ok(resp).await?.json().await?;
    Ok(parse_response(&v, req))
}

fn parse_response(v: &Value, req: &MessagesRequest) -> MessagesResponse {
    let mut content = Vec::new();
    if let Some(blocks) = v["content"].as_array() {
        for b in blocks {
            match b["type"].as_str() {
                Some("text") => {
                    if let Some(t) = b["text"].as_str() {
                        content.push(ContentBlock::text(t));
                    }
                }
                Some("thinking") => {
                    if let Some(t) = b["thinking"].as_str() {
                        content.push(ContentBlock::Thinking {
                            thinking: t.to_string(),
                        });
                    }
                }
                Some("tool_use") => {
                    content.push(ContentBlock::ToolUse {
                        id: b["id"].as_str().unwrap_or_default().to_string(),
                        name: b["name"].as_str().unwrap_or_default().to_string(),
                        input: b["input"].clone(),
                    });
                }
                _ => {}
            }
        }
    }
    MessagesResponse {
        id: v["id"].as_str().unwrap_or_default().to_string(),
        model: v["model"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| req.model.clone()),
        content,
        stop_reason: v["stop_reason"]
            .as_str()
            .map(StopReason::from_anthropic),
        usage: Usage {
            input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
            cached_input_tokens: v["usage"]["cache_read_input_tokens"]
                .as_u64()
                .unwrap_or(0),
        },
    }
}

// --- SSE stream ---------------------------------------------------------------

pub async fn stream_messages(
    client: &reqwest::Client,
    token: &str,
    req: &MessagesRequest,
    sink: EventSink,
) -> Result<MessagesResponse> {
    let body = request_json(req, true);
    let resp = inference_headers(client.post(format!("{BASE_API}/v1/messages")), token, &body)
        .body(encode_body(&body)?)
        .send()
        .await?;
    let resp = ensure_ok(resp).await?;

    let mut acc = MessagesResponse {
        model: req.model.clone(),
        ..Default::default()
    };
    let mut text = String::new();
    let mut thinking = String::new();
    // Собираем tool_use по индексу блока: (id, name, partial_json).
    let mut tools: Vec<(String, String, String)> = Vec::new();
    let mut sse = String::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(Error::from)?;
        sse.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(i) = sse.find("\n\n") {
            let event = sse[..i].to_string();
            sse.drain(..i + 2);
            let mut ev_type = "";
            for line in event.lines() {
                if let Some(t) = line.strip_prefix("event:") {
                    ev_type = t.trim();
                    continue;
                }
                let Some(data) = line.strip_prefix("data:") else { continue };
                let Ok(v) = serde_json::from_str::<Value>(data.trim()) else {
                    continue;
                };
                match ev_type {
                    "message_start" => {
                        let msg = &v["message"];
                        acc.id = msg["id"].as_str().unwrap_or_default().to_string();
                        if let Some(m) = msg["model"].as_str() {
                            acc.model = m.to_string();
                        }
                        acc.usage.input_tokens =
                            msg["usage"]["input_tokens"].as_u64().unwrap_or(0);
                        let _ = sink.send(StreamEvent::MessageStart {
                            message_id: Some(acc.id.clone()),
                            model: Some(acc.model.clone()),
                        });
                    }
                    "content_block_start" => {
                        let idx = v["index"].as_u64().unwrap_or(0) as usize;
                        let cb = &v["content_block"];
                        if cb["type"].as_str() == Some("tool_use") {
                            while tools.len() <= idx {
                                tools.push((String::new(), String::new(), String::new()));
                            }
                            tools[idx].0 =
                                cb["id"].as_str().unwrap_or_default().to_string();
                            tools[idx].1 =
                                cb["name"].as_str().unwrap_or_default().to_string();
                        }
                    }
                    "content_block_delta" => {
                        let d = &v["delta"];
                        match d["type"].as_str() {
                            Some("text_delta") => {
                                if let Some(t) = d["text"].as_str() {
                                    text.push_str(t);
                                    let _ = sink.send(StreamEvent::TextDelta {
                                        text: t.to_string(),
                                    });
                                }
                            }
                            Some("thinking_delta") => {
                                if let Some(t) = d["thinking"].as_str() {
                                    thinking.push_str(t);
                                    let _ = sink.send(StreamEvent::ThinkingDelta {
                                        thinking: t.to_string(),
                                    });
                                }
                            }
                            Some("input_json_delta") => {
                                let idx = v["index"].as_u64().unwrap_or(0) as usize;
                                while tools.len() <= idx {
                                    tools.push((String::new(), String::new(), String::new()));
                                }
                                if let Some(j) = d["partial_json"].as_str() {
                                    tools[idx].2.push_str(j);
                                }
                            }
                            _ => {}
                        }
                    }
                    "message_delta" => {
                        if let Some(sr) = v["delta"]["stop_reason"].as_str() {
                            acc.stop_reason = Some(StopReason::from_anthropic(sr));
                        }
                        if let Some(u) = v["usage"].as_object() {
                            acc.usage.output_tokens =
                                u["output_tokens"].as_u64().unwrap_or(acc.usage.output_tokens);
                        }
                        let _ = sink.send(StreamEvent::MessageDelta {
                            stop_reason: acc.stop_reason.clone(),
                            usage: Some(acc.usage),
                        });
                    }
                    "error" => {
                        let msg = v["error"]["message"]
                            .as_str()
                            .unwrap_or("stream error")
                            .to_string();
                        let _ = sink.send(StreamEvent::Error {
                            message: msg.clone(),
                        });
                        return Err(Error::Http { status: 500, body: msg });
                    }
                    _ => {}
                }
            }
        }
    }

    if !thinking.is_empty() {
        acc.content.push(ContentBlock::Thinking { thinking });
    }
    if !text.is_empty() {
        acc.content.push(ContentBlock::text(text));
    }
    for (id, name, args) in tools {
        if id.is_empty() {
            continue;
        }
        let input = serde_json::from_str(&args).unwrap_or(Value::Null);
        let b = ContentBlock::ToolUse { id, name, input };
        if let ContentBlock::ToolUse { id, name, input } = &b {
            let _ = sink.send(StreamEvent::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            });
        }
        acc.content.push(b);
    }
    let _ = sink.send(StreamEvent::MessageStop);
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_math_round_trips() {
        for (y, m, d) in [(1970, 1, 1), (2024, 2, 29), (2025, 1, 31), (2000, 12, 31)]
        {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
    }

    #[test]
    fn the_subscription_renews_monthly_on_its_anchor_day() {
        let created = ts_rfc3339("2025-11-20T10:00:00Z").unwrap();
        let now = ts_rfc3339("2026-02-10T00:00:00Z").unwrap();
        assert_eq!(
            next_monthly(created, now),
            ts_rfc3339("2026-02-20T10:00:00Z").unwrap()
        );
        // До якорного времени дня продление значится «сегодня».
        let earlier = ts_rfc3339("2026-02-20T09:59:00Z").unwrap();
        assert_eq!(
            next_monthly(created, earlier),
            ts_rfc3339("2026-02-20T10:00:00Z").unwrap()
        );
    }

    #[test]
    fn the_anchor_day_clamps_to_short_months() {
        // Якорь 31-е, февраль 2026 (не високосный) → 28-е.
        let jan31 = ts_rfc3339("2025-01-31T00:00:00Z").unwrap();
        let feb5 = ts_rfc3339("2026-02-05T00:00:00Z").unwrap();
        assert_eq!(
            next_monthly(jan31, feb5),
            ts_rfc3339("2026-02-28T00:00:00Z").unwrap()
        );
        // Високосный 2024 → 29-е.
        let jan31_24 = ts_rfc3339("2024-01-31T00:00:00Z").unwrap();
        let feb10_24 = ts_rfc3339("2024-02-10T00:00:00Z").unwrap();
        assert_eq!(
            next_monthly(jan31_24, feb10_24),
            ts_rfc3339("2024-02-29T00:00:00Z").unwrap()
        );
    }
}
