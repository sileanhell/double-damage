//! REST-клиент OpenRouter: ключ/баланс, список моделей, chat/completions + SSE.

use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ensure_ok, Error, Result};
use crate::provider::EventSink;
use crate::types::{
    AccountInfo, ContentBlock, Message, MessagesRequest, MessagesResponse, ModelInfo,
    ProviderKind, Role, StopReason, StreamEvent, Tool, Usage,
};

use super::API_BASE;

// --- account / limits --------------------------------------------------------

#[derive(Deserialize)]
struct KeyInfo {
    label: Option<String>,
    usage: Option<f64>,
    limit: Option<f64>,
    limit_remaining: Option<f64>,
    limit_reset: Option<String>,
    is_free_tier: Option<bool>,
    expires_at: Option<String>,
}

#[derive(Deserialize)]
struct Data<T> {
    data: T,
}

/// `/credits`: баланс аккаунта (куплено минус израсходовано).
#[derive(Deserialize)]
struct Credits {
    total_credits: Option<f64>,
    total_usage: Option<f64>,
}

fn parse_ts(s: &str) -> Option<i64> {
    // RFC3339 → unix seconds (простой разбор без chrono).
    let (date, time) = s.split_once('T')?;
    let (y, m, d) = {
        let mut it = date.split('-');
        (
            it.next()?.parse::<i64>().ok()?,
            it.next()?.parse::<i64>().ok()?,
            it.next()?.parse::<i64>().ok()?,
        )
    };
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
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub async fn account_info(client: &reqwest::Client, api_key: &str) -> Result<AccountInfo> {
    let resp = client
        .get(format!("{API_BASE}/key"))
        .bearer_auth(api_key)
        .send()
        .await?;
    let d = ensure_ok(resp)
        .await?
        .json::<Data<KeyInfo>>()
        .await
        .map_err(|e| Error::Parse(format!("/key: {e}")))?
        .data;

    // Периодические квоты не показываем: нужен только баланс аккаунта.
    let plan = match d.is_free_tier {
        Some(true) => Some("free tier".to_string()),
        _ => d.limit_reset.clone().map(|r| format!("credits ({r})")),
    };

    // Баланс аккаунта — /credits (total_credits − total_usage);
    // запасной вариант — остаток лимита самого ключа.
    let mut balance_usd = d
        .limit_remaining
        .or_else(|| match (d.limit, d.usage) {
            (Some(l), Some(u)) => Some((l - u).max(0.0)),
            _ => None,
        });
    if let Ok(resp) = client
        .get(format!("{API_BASE}/credits"))
        .bearer_auth(api_key)
        .send()
        .await
        && resp.status().is_success()
        && let Ok(c) = resp.json::<Data<Credits>>().await
        && let (Some(credits), Some(used)) = (c.data.total_credits, c.data.total_usage)
    {
        balance_usd = Some((credits - used).max(0.0));
    }

    Ok(AccountInfo {
        provider: ProviderKind::OpenRouter,
        label: d.label,
        plan,
        expires_at: d.expires_at.as_deref().and_then(parse_ts),
        started_at: None,
        balance_usd,
        spent_usd: None,
        credit_limit_usd: d.limit,
        quotas: Vec::new(),
    })
}

// --- models ------------------------------------------------------------------

#[derive(Deserialize)]
struct OrModel {
    id: String,
    name: Option<String>,
    description: Option<String>,
    context_length: Option<u64>,
    pricing: Option<OrPricing>,
    top_provider: Option<OrTopProvider>,
}

#[derive(Deserialize)]
struct OrPricing {
    prompt: Option<String>,
    completion: Option<String>,
}

#[derive(Deserialize)]
struct OrTopProvider {
    max_completion_tokens: Option<u64>,
}

pub async fn list_models(client: &reqwest::Client, api_key: &str) -> Result<Vec<ModelInfo>> {
    let resp = client
        .get(format!("{API_BASE}/models"))
        .bearer_auth(api_key)
        .send()
        .await?;
    let d = ensure_ok(resp)
        .await?
        .json::<Data<Vec<OrModel>>>()
        .await
        .map_err(|e| Error::Parse(format!("/models: {e}")))?
        .data;

    Ok(d.into_iter()
        .map(|m| {
            let pf = |s: &Option<String>| {
                s.as_deref()
                    .and_then(|v| v.parse::<f64>().ok())
                    .map(|v| v * 1e6) // цена за токен → за 1M
            };
            let pricing = m.pricing.as_ref();
            ModelInfo {
                id: m.id.clone(),
                display_name: m.name.clone().unwrap_or_else(|| m.id.clone()),
                description: m.description,
                context_window: m.context_length,
                max_output_tokens: m.top_provider.and_then(|t| t.max_completion_tokens),
                input_price: pricing.and_then(|p| pf(&p.prompt)),
                output_price: pricing.and_then(|p| pf(&p.completion)),
                ..Default::default()
            }
        })
        .collect())
}

// --- messages (OpenAI chat completions) ---------------------------------------

fn role_str(r: Role) -> &'static str {
    match r {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    }
}

/// Content → OpenAI parts (text + image нет, tool_result → tool message).
fn message_json(m: &Message) -> Vec<Value> {
    let mut out = Vec::new();
    let mut tool_results: Vec<Value> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut text_parts: Vec<String> = Vec::new();

    for b in &m.content {
        match b {
            ContentBlock::Text { text } => text_parts.push(text.clone()),
            ContentBlock::Thinking { thinking } => {
                text_parts.push(format!("<think>{thinking}</think>"))
            }
            ContentBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": input.to_string() },
            })),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => tool_results.push(json!({
                "role": "tool",
                "tool_call_id": tool_use_id,
                "content": content,
            })),
        }
    }

    let mut msg = json!({ "role": role_str(m.role) });
    if m.role == Role::Assistant && !tool_calls.is_empty() {
        msg["content"] = if text_parts.is_empty() {
            Value::Null
        } else {
            json!(text_parts.join("\n"))
        };
        msg["tool_calls"] = json!(tool_calls);
    } else {
        msg["content"] = json!(text_parts.join("\n"));
    }
    out.push(msg);
    out.extend(tool_results);
    out
}

fn tools_json(tools: &[Tool]) -> Value {
    json!(tools
        .iter()
        .map(|t| json!({
            "type": "function",
            "function": {
                "name": t.name,
                "description": t.description,
                "parameters": t.input_schema,
            }
        }))
        .collect::<Vec<_>>())
}

fn request_json(req: &MessagesRequest, stream: bool) -> Value {
    let mut msgs: Vec<Value> = Vec::new();
    if let Some(sys) = &req.system {
        msgs.push(json!({ "role": "system", "content": sys }));
    }
    for m in &req.messages {
        match m.role {
            Role::System => msgs.push(json!({ "role": "system", "content": m.text() })),
            _ => msgs.extend(message_json(m)),
        }
    }
    let mut body = json!({
        "model": req.model,
        "messages": msgs,
        "stream": stream,
    });
    if req.max_tokens > 0 {
        body["max_tokens"] = json!(req.max_tokens);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(t) = req.top_p {
        body["top_p"] = json!(t);
    }
    if !req.stop_sequences.is_empty() {
        body["stop"] = json!(req.stop_sequences);
    }
    if !req.tools.is_empty() {
        body["tools"] = tools_json(&req.tools);
    }
    if stream {
        body["stream_options"] = json!({ "include_usage": true });
    }
    // Без effort рассуждения выключаются явно: модели вроде DeepSeek
    // думают по умолчанию (oh-my-pi `openrouter-enabled-false`).
    body["reasoning"] = match req.effort {
        Some(effort) => json!({ "effort": effort.openrouter() }),
        None => json!({ "enabled": false }),
    };
    body
}

#[derive(Deserialize)]
struct ChatResponse {
    id: Option<String>,
    model: Option<String>,
    choices: Option<Vec<ChatChoice>>,
    usage: Option<OrUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: Option<ChatMessage>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<Value>,
    tool_calls: Option<Vec<OrToolCall>>,
}

#[derive(Deserialize)]
struct OrToolCall {
    id: Option<String>,
    function: Option<OrFunction>,
}

#[derive(Deserialize)]
struct OrFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct OrUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    prompt_tokens_details: Option<PromptDetails>,
}

#[derive(Deserialize)]
struct PromptDetails {
    cached_tokens: Option<u64>,
}

fn content_to_blocks(msg: &ChatMessage) -> Vec<ContentBlock> {
    let mut out = Vec::new();
    if let Some(c) = &msg.content {
        match c {
            Value::String(s) => {
                if !s.is_empty() {
                    out.push(ContentBlock::text(s.clone()));
                }
            }
            Value::Array(parts) => {
                for p in parts {
                    if p.get("type").and_then(Value::as_str) == Some("text") {
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            out.push(ContentBlock::text(t.to_string()));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(calls) = &msg.tool_calls {
        for (i, c) in calls.iter().enumerate() {
            let f = c.function.as_ref();
            out.push(ContentBlock::ToolUse {
                id: c.id.clone().unwrap_or_else(|| format!("call_{i}")),
                name: f.and_then(|f| f.name.clone()).unwrap_or_default(),
                input: f
                    .and_then(|f| f.arguments.as_deref())
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or(Value::Null),
            });
        }
    }
    out
}

fn usage_of(u: &OrUsage) -> Usage {
    Usage {
        input_tokens: u.prompt_tokens.unwrap_or(0),
        output_tokens: u.completion_tokens.unwrap_or(0),
        cached_input_tokens: u
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens)
            .unwrap_or(0),
    }
}

/// Отказ эндпоинта выключить рассуждения («Reasoning is mandatory…»).
fn reasoning_is_mandatory(err: &Error) -> bool {
    let Error::Http { status: 400, body } = err else {
        return false;
    };
    let body = body.to_ascii_lowercase();
    body.contains("reasoning") && (body.contains("mandatory") || body.contains("cannot be disabled"))
}

/// POST /chat/completions. Если модель не умеет думать «без рассуждений»,
/// повторяет запрос без поля `reasoning` (поведение модели по умолчанию).
async fn post_chat(
    client: &reqwest::Client,
    api_key: &str,
    req: &MessagesRequest,
    stream: bool,
) -> Result<reqwest::Response> {
    let send = |body: Value| {
        client
            .post(format!("{API_BASE}/chat/completions"))
            .bearer_auth(api_key)
            .header("HTTP-Referer", "https://github.com/double-damage")
            .header("X-OpenRouter-Title", "double-damage")
            .json(&body)
            .send()
    };
    let mut body = request_json(req, stream);
    match ensure_ok(send(body.clone()).await?).await {
        Err(err) if req.effort.is_none() && reasoning_is_mandatory(&err) => {
            if let Some(obj) = body.as_object_mut() {
                obj.remove("reasoning");
            }
            ensure_ok(send(body).await?).await
        }
        other => other,
    }
}

pub async fn messages(
    client: &reqwest::Client,
    api_key: &str,
    req: &MessagesRequest,
) -> Result<MessagesResponse> {
    let r: ChatResponse = post_chat(client, api_key, req, false).await?.json().await?;
    let choice = r
        .choices
        .and_then(|mut c| c.drain(..).next())
        .ok_or_else(|| Error::Parse("empty choices".into()))?;
    let msg = choice.message.unwrap_or(ChatMessage {
        content: None,
        tool_calls: None,
    });
    Ok(MessagesResponse {
        id: r.id.unwrap_or_default(),
        model: r.model.unwrap_or_else(|| req.model.clone()),
        content: content_to_blocks(&msg),
        stop_reason: choice.finish_reason.map(|s| StopReason::from_openai(&s)),
        usage: r.usage.as_ref().map(usage_of).unwrap_or_default(),
    })
}

// --- streaming ---------------------------------------------------------------

#[derive(Deserialize)]
struct StreamChunk {
    id: Option<String>,
    model: Option<String>,
    choices: Option<Vec<StreamChoice>>,
    usage: Option<OrUsage>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: Option<StreamDelta>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct StreamDelta {
    content: Option<String>,
    reasoning: Option<String>,
    /// Так рассуждения шлют некоторые апстримы (DeepSeek).
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Deserialize)]
struct StreamToolCall {
    index: Option<usize>,
    id: Option<String>,
    function: Option<OrFunction>,
}

pub async fn stream_messages(
    client: &reqwest::Client,
    api_key: &str,
    req: &MessagesRequest,
    sink: EventSink,
) -> Result<MessagesResponse> {
    let resp = post_chat(client, api_key, req, true).await?;

    let mut acc = MessagesResponse {
        model: req.model.clone(),
        ..Default::default()
    };
    let mut text = String::new();
    let mut thinking = String::new();
    let mut tool_calls: Vec<(String, String, String)> = Vec::new(); // id, name, args
    let mut sent_start = false;
    let mut sse = String::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(Error::from)?;
        sse.push_str(&String::from_utf8_lossy(&chunk));
        // SSE: события разделены пустой строкой.
        while let Some(i) = sse.find("\n\n") {
            let event = sse[..i].to_string();
            sse.drain(..i + 2);
            for line in event.lines() {
                let Some(data) = line.strip_prefix("data:") else { continue };
                let data = data.trim();
                if data == "[DONE]" {
                    continue;
                }
                let Ok(c) = serde_json::from_str::<StreamChunk>(data) else {
                    continue;
                };
                if let Some(id) = &c.id {
                    if acc.id.is_empty() {
                        acc.id = id.clone();
                    }
                }
                if let Some(m) = &c.model {
                    acc.model = m.clone();
                }
                if !sent_start {
                    sent_start = true;
                    let _ = sink.send(StreamEvent::MessageStart {
                        message_id: c.id.clone(),
                        model: c.model.clone(),
                    });
                }
                if let Some(u) = &c.usage {
                    acc.usage = usage_of(u);
                }
                let Some(choices) = c.choices else { continue };
                for ch in choices {
                    if let Some(d) = &ch.delta {
                        let reasoning = d
                            .reasoning
                            .as_ref()
                            .or(d.reasoning_content.as_ref())
                            .filter(|t| !t.is_empty());
                        if let Some(t) = reasoning {
                            thinking.push_str(t);
                            let _ = sink.send(StreamEvent::ThinkingDelta {
                                thinking: t.clone(),
                            });
                        }
                        if let Some(t) = &d.content {
                            if !t.is_empty() {
                                text.push_str(t);
                                let _ = sink.send(StreamEvent::TextDelta { text: t.clone() });
                            }
                        }
                        if let Some(calls) = &d.tool_calls {
                            for tc in calls {
                                let idx = tc.index.unwrap_or(tool_calls.len());
                                while tool_calls.len() <= idx {
                                    tool_calls.push((String::new(), String::new(), String::new()));
                                }
                                let e = &mut tool_calls[idx];
                                if let Some(id) = &tc.id {
                                    e.0 = id.clone();
                                }
                                if let Some(f) = &tc.function {
                                    if let Some(n) = &f.name {
                                        e.1 = n.clone();
                                    }
                                    if let Some(a) = &f.arguments {
                                        e.2.push_str(a);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(fr) = &ch.finish_reason {
                        acc.stop_reason = Some(StopReason::from_openai(fr));
                        let _ = sink.send(StreamEvent::MessageDelta {
                            stop_reason: acc.stop_reason.clone(),
                            usage: Some(acc.usage),
                        });
                    }
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
    for (id, name, args) in tool_calls {
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
