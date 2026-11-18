//! ConnectRPC-клиент Devin: унарные proto-вызовы и стриминг GetChatMessage.
//! Поля protobuf восстановлены из живого дампа (см. devin-dump/PROTOCOL_SPEC.md).

use std::io::Read as _;
use std::sync::OnceLock;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use futures_util::StreamExt;
use rand::RngCore;
use serde_json::Value;

use crate::error::{ensure_ok, Error, Result};
use crate::proto;
use crate::provider::EventSink;
use crate::types::{
    AccountInfo, ContentBlock, MessagesRequest, MessagesResponse, ModelInfo, ProviderKind,
    Quota, Role, StopReason, StreamEvent, Usage,
};

use super::{catalog, DevinConfig};

const CLIENT_NAME: &str = "devin-cli";
const CLIENT_VERSION: &str = "3000.12.57";
const EXTENSION_NAME: &str = "chisel";

const SVC_SEAT: &str = "/exa.seat_management_pb.SeatManagementService";
const SVC_API: &str = "/exa.api_server_pb.ApiServerService";

// --- RequestMetadata ---------------------------------------------------------

fn fingerprint() -> &'static str {
    static FP: OnceLock<String> = OnceLock::new();
    FP.get_or_init(|| {
        let mut buf = vec![0u8; 366];
        rand::thread_rng().fill_bytes(&mut buf);
        buf.iter().map(|b| format!("{b:02x}")).collect()
    })
}

fn os_name() -> &'static str {
    #[cfg(target_os = "windows")]
    return "windows";
    #[cfg(target_os = "macos")]
    return "macos";
    #[cfg(all(unix, not(target_os = "macos")))]
    return "linux";
}

/// RequestMetadata (поле 1 всех запросов). `with_caps` — для GetCliModelConfigs.
fn metadata(api_key: &str, with_caps: bool) -> Vec<u8> {
    let mut m = Vec::new();
    m.extend(proto::str_field(1, CLIENT_NAME));
    m.extend(proto::str_field(2, CLIENT_VERSION));
    m.extend(proto::str_field(3, api_key));
    m.extend(proto::str_field(4, "en"));
    m.extend(proto::str_field(5, os_name()));
    m.extend(proto::str_field(7, CLIENT_VERSION));
    m.extend(proto::str_field(12, EXTENSION_NAME));
    m.extend(proto::str_field(28, EXTENSION_NAME));
    if with_caps {
        m.extend(proto::packed_varints(30, &[3, 4, 6, 7, 8]));
    }
    m.extend(proto::str_field(31, fingerprint()));
    m
}

// --- ConnectRPC transport ----------------------------------------------------

async fn unary(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    token: &str,
    extra: &[u8],
    caps: bool,
) -> Result<Vec<u8>> {
    let mut body = proto::len_field(1, &metadata(token, caps));
    body.extend_from_slice(extra);
    let resp = client
        .post(format!("{base}{path}"))
        .header("authorization", format!("Basic {token}"))
        .header("content-type", "application/proto")
        .header("connect-protocol-version", "1")
        .body(body)
        .send()
        .await?;
    let resp = ensure_ok(resp).await?;
    Ok(resp.bytes().await?.to_vec())
}

fn gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut e = GzEncoder::new(Vec::new(), Compression::fast());
    use std::io::Write as _;
    e.write_all(data)
        .map_err(|err| Error::Transport(format!("gzip: {err}")))?;
    e.finish()
        .map_err(|err| Error::Transport(format!("gzip: {err}")))
}

fn gunzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut d = GzDecoder::new(data);
    let mut out = Vec::new();
    d.read_to_end(&mut out)
        .map_err(|e| Error::Parse(format!("gunzip: {e}")))?;
    Ok(out)
}

/// Connect-конверт: flag(1) + len(4, BE) + payload. flag&1 = gzip, flag&2 = trailer.
fn envelope(payload: &[u8], compress: bool) -> Vec<u8> {
    let (flag, body) = if compress {
        (1u8, gzip(payload).unwrap_or_else(|_| payload.to_vec()))
    } else {
        (0u8, payload.to_vec())
    };
    let mut out = vec![flag];
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

// --- account_info (GetUserStatus) ---------------------------------------------

pub async fn account_info(client: &reqwest::Client, cfg: &DevinConfig) -> Result<AccountInfo> {
    let resp = unary(
        client,
        &cfg.api_server_url,
        &format!("{SVC_SEAT}/GetUserStatus"),
        &cfg.session_token,
        &[],
        false,
    )
    .await?;

    // Ответ: {1: user_status{3: username, 5: team_id, 7: email, 13: plan_status{…}}}
    let us = proto::msgs(&resp, 1).into_iter().next().unwrap_or(&resp[..]);
    let username = proto::str_val(us, 3);
    let email = proto::str_val(us, 7);
    let team = proto::str_val(us, 5);

    let mut plan = None;
    let mut started_at = None;
    let mut expires_at = None;
    let mut quotas = Vec::new();

    if let Some(ps) = proto::msgs(us, 13).into_iter().next() {
        if let Some(pi) = proto::msgs(ps, 1).into_iter().next() {
            plan = proto::str_val(pi, 2).or_else(|| proto::str_val(pi, 1));
        }
        // plan_start/plan_end — Timestamp {1: seconds}
        started_at = proto::msgs(ps, 2)
            .into_iter()
            .next()
            .and_then(|t| proto::varint_val(t, 1))
            .map(|v| v as i64);
        expires_at = proto::msgs(ps, 3)
            .into_iter()
            .next()
            .and_then(|t| proto::varint_val(t, 1))
            .map(|v| v as i64);
        // 14/15 — daily/weekly_quota_remaining_percent (остаток в %),
        // 17/18 — *_quota_reset_at_unix. Бар показывает израсходованное.
        let daily = proto::varint_val(ps, 14);
        let weekly = proto::varint_val(ps, 15);
        let daily_reset = proto::varint_val(ps, 17).map(|v| v as i64);
        let weekly_reset = proto::varint_val(ps, 18).map(|v| v as i64);
        let mut push_quota = |name: &str, remaining: Option<u64>, resets_at| {
            if let Some(r) = remaining {
                quotas.push(Quota {
                    name: name.into(),
                    used: None,
                    limit: None,
                    remaining: Some(r as f64),
                    resets_at,
                    utilization: Some((100.0 - r as f64).clamp(0.0, 100.0)),
                });
            }
        };
        push_quota("daily", daily, daily_reset);
        push_quota("weekly", weekly, weekly_reset);
    }

    Ok(AccountInfo {
        provider: ProviderKind::Devin,
        label: email.or(username).or(team),
        plan,
        expires_at,
        started_at,
        balance_usd: None,
        spent_usd: None,
        credit_limit_usd: None,
        quotas,
    })
}

// --- list_models (GetCliModelConfigs) -----------------------------------------

/// Каталог `GetCliModelConfigs`, свёрнутый в семейства (см. [`catalog`]).
pub async fn list_models(
    client: &reqwest::Client,
    cfg: &DevinConfig,
) -> Result<(Vec<ModelInfo>, std::collections::HashMap<String, catalog::Routes>)> {
    let resp = unary(
        client,
        &cfg.api_server_url,
        &format!("{SVC_API}/GetCliModelConfigs"),
        &cfg.session_token,
        &[],
        true,
    )
    .await?;
    Ok(catalog::collapse(catalog::parse_configs(&resp)))
}

// --- messages (GetChatMessage, connect stream) ---------------------------------

fn chat_role(r: Role) -> u64 {
    match r {
        Role::User => 1,
        Role::Assistant => 2,
        Role::System => 3,
    }
}

fn build_chat_request(cfg: &DevinConfig, req: &MessagesRequest) -> Vec<u8> {
    let mut body = proto::len_field(1, &metadata(&cfg.session_token, true));

    // Системный промпт: req.system + Role::System-сообщения.
    let mut system = req.system.clone().unwrap_or_default();
    let mut chat_msgs: Vec<u8> = Vec::new();
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
        // ToolUse/ToolResult сериализуем текстом — протокол их принимает строками.
        let mut content = String::new();
        for b in &m.content {
            match b {
                ContentBlock::Text { text } => content.push_str(text),
                ContentBlock::Thinking { thinking } => {
                    content.push_str(&format!("<think>{thinking}</think>"))
                }
                ContentBlock::ToolUse { id: _, name, input } => {
                    content.push_str(&format!("[tool_use {name}: {input}]"))
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content: c,
                    ..
                } => content.push_str(&format!("[tool_result {tool_use_id}: {c}]")),
            }
        }
        let mut cm = Vec::new();
        cm.extend(proto::str_field(1, &uuid::Uuid::new_v4().to_string()));
        cm.extend(proto::varint_field(2, chat_role(m.role)));
        cm.extend(proto::str_field(3, &content));
        cm.extend(proto::len_field(8, &proto::varint_field(1, 1)));
        chat_msgs.extend(proto::len_field(3, &cm));
    }
    if !system.is_empty() {
        body.extend(proto::str_field(2, &system));
    }
    body.extend(chat_msgs);

    body.extend(proto::varint_field(7, 5));

    // InferenceParams: {1:1, 2:max_tokens, 3:400, 5:top_p, 7:40, 8:temperature}
    let mut ip = Vec::new();
    ip.extend(proto::varint_field(1, 1));
    ip.extend(proto::varint_field(
        2,
        if req.max_tokens > 0 { req.max_tokens as u64 } else { 128000 },
    ));
    ip.extend(proto::varint_field(3, 400));
    ip.extend(proto::f64_field(5, req.top_p.unwrap_or(1.0)));
    ip.extend(proto::varint_field(7, 40));
    ip.extend(proto::f64_field(8, req.temperature.unwrap_or(0.95)));
    body.extend(proto::len_field(8, &ip));

    for t in &req.tools {
        let mut tm = Vec::new();
        tm.extend(proto::str_field(1, &t.name));
        tm.extend(proto::str_field(2, &t.description));
        tm.extend(proto::str_field(3, &t.input_schema.to_string()));
        body.extend(proto::len_field(10, &tm));
    }

    // Маркер возможностей из дампа: {3:4, 4:14}.
    let mut caps = Vec::new();
    caps.extend(proto::varint_field(3, 4));
    caps.extend(proto::varint_field(4, 14));
    body.extend(proto::len_field(15, &caps));

    body.extend(proto::str_field(16, &uuid::Uuid::new_v4().to_string()));
    body.extend(proto::varint_field(20, 1));
    body.extend(proto::str_field(21, &req.model));
    if let Some(ur) = &cfg.user_ref {
        body.extend(proto::str_field(28, ur));
    }
    body
}

/// Разобрать один фрейм-дельту GetChatMessageResponse в события.
fn delta_events(msg: &[u8], acc: &mut MessagesResponse, sink: &EventSink) {
    // 1: message_id, 3: text_delta, 5: finish, 7: usage{2:in,3:out,5:cached,9:model},
    // 9: reasoning_delta, 12: f64 cost
    if let Some(id) = proto::str_val(msg, 1) {
        if acc.id.is_empty() {
            acc.id = id.clone();
            let _ = sink.send(StreamEvent::MessageStart {
                message_id: Some(id),
                model: None,
            });
        }
    }
    if let Some(t) = proto::str_val(msg, 3) {
        if !t.is_empty() {
            let _ = sink.send(StreamEvent::TextDelta { text: t.clone() });
            if let Some(ContentBlock::Text { text }) = acc.content.last_mut() {
                text.push_str(&t);
            } else {
                acc.content.push(ContentBlock::text(t));
            }
        }
    }
    if let Some(r) = proto::str_val(msg, 9) {
        if !r.is_empty() {
            let _ = sink.send(StreamEvent::ThinkingDelta { thinking: r });
        }
    }
    if let Some(u) = proto::msgs(msg, 7).into_iter().next() {
        let usage = Usage {
            input_tokens: proto::varint_val(u, 2).unwrap_or(0),
            output_tokens: proto::varint_val(u, 3).unwrap_or(0),
            cached_input_tokens: proto::varint_val(u, 5).unwrap_or(0),
        };
        acc.usage = usage;
        if let Some(m) = proto::str_val(u, 9) {
            acc.model = m;
        }
        // Usage-тики едут весь стрим, в т.ч. пока модель ещё «думает» и
        // текст не начался: единственный живой сигнал прогресса на проводе.
        let _ = sink.send(StreamEvent::MessageDelta {
            stop_reason: None,
            usage: Some(usage),
        });
    }
    if let Some(fin) = proto::varint_val(msg, 5) {
        // 2 = нормальное завершение (из дампа). Остальное маппим грубо.
        let sr = match fin {
            1 => StopReason::EndTurn,
            2 => StopReason::EndTurn,
            3 => StopReason::MaxTokens,
            4 => StopReason::ToolUse,
            n => StopReason::Other(n.to_string()),
        };
        acc.stop_reason = Some(sr.clone());
        let _ = sink.send(StreamEvent::MessageDelta {
            stop_reason: Some(sr),
            usage: Some(acc.usage),
        });
    }
}

/// Прочитать Connect-стрим: накапливаем буфер, режем конверты flag+len+payload.
async fn read_connect_stream(
    resp: reqwest::Response,
    acc: &mut MessagesResponse,
    sink: &EventSink,
) -> Result<()> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(Error::from)?;
        buf.extend_from_slice(&chunk);
        loop {
            if buf.len() < 5 {
                break;
            }
            let flag = buf[0];
            let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
            if buf.len() < 5 + len {
                break;
            }
            let frame: Vec<u8> = buf.drain(..5 + len).collect();
            let payload = frame[5..].to_vec();
            if flag & 0x02 != 0 {
                // trailer: JSON {"code":..,"message":..} при ошибке, {} при успехе.
                if let Ok(v) = serde_json::from_slice::<Value>(&payload) {
                    if let Some(code) = v.get("code").and_then(Value::as_str) {
                        let msg = v
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        return Err(Error::Http {
                            status: 500,
                            body: format!("{code}: {msg}"),
                        });
                    }
                }
                continue;
            }
            let msg = if flag & 0x01 != 0 { gunzip(&payload)? } else { payload };
            delta_events(&msg, acc, sink);
        }
    }
    Ok(())
}

pub async fn stream_messages(
    client: &reqwest::Client,
    cfg: &DevinConfig,
    req: &MessagesRequest,
    sink: EventSink,
) -> Result<MessagesResponse> {
    let body = envelope(&build_chat_request(cfg, req), true);
    let resp = client
        .post(format!(
            "{}{SVC_API}/GetChatMessage",
            cfg.api_server_url
        ))
        .header("authorization", format!("Basic {}", cfg.session_token))
        .header("content-type", "application/connect+proto")
        .header("connect-content-encoding", "gzip")
        .header("connect-protocol-version", "1")
        .header("accept", "application/connect+proto")
        .body(body)
        .send()
        .await?;
    let resp = ensure_ok(resp).await?;
    let mut acc = MessagesResponse {
        model: req.model.clone(),
        ..Default::default()
    };
    read_connect_stream(resp, &mut acc, &sink).await?;
    let _ = sink.send(StreamEvent::MessageStop);
    Ok(acc)
}

pub async fn messages(
    client: &reqwest::Client,
    cfg: &DevinConfig,
    req: &MessagesRequest,
) -> Result<MessagesResponse> {
    // У Devin единственный путь инференса — стрим; собираем его в ответ.
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    stream_messages(client, cfg, req, tx).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Фрейм с полем 7 → usage-тик: MessageDelta без stop_reason и с
    /// растущим счётчиком, чтобы UI мог показывать прогресс «думает».
    #[test]
    fn usage_frames_emit_progress_deltas() {
        let (sink, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut acc = MessagesResponse::default();
        let mut usage = Vec::new();
        usage.extend(proto::varint_field(2, 1063));
        usage.extend(proto::varint_field(3, 28));
        usage.extend(proto::varint_field(5, 11008));
        let mut frame = Vec::new();
        frame.extend(proto::len_field(7, &usage));
        delta_events(&frame, &mut acc, &sink);
        let Some(StreamEvent::MessageDelta { stop_reason, usage }) = rx.try_recv().ok()
        else {
            panic!("expected a usage MessageDelta");
        };
        assert!(stop_reason.is_none());
        let usage = usage.expect("usage");
        assert_eq!(usage.input_tokens, 1063);
        assert_eq!(usage.output_tokens, 28);
        assert_eq!(acc.usage.output_tokens, 28);
    }

    /// Финишный фрейм отдаёт и usage-тик, и MessageDelta со stop_reason.
    #[test]
    fn finish_frames_emit_usage_then_stop() {
        let (sink, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut acc = MessagesResponse::default();
        let mut usage = Vec::new();
        usage.extend(proto::varint_field(3, 41));
        let mut frame = Vec::new();
        frame.extend(proto::len_field(7, &usage));
        frame.extend(proto::varint_field(5, 2));
        delta_events(&frame, &mut acc, &sink);
        assert!(matches!(
            rx.try_recv(),
            Ok(StreamEvent::MessageDelta {
                stop_reason: None,
                ..
            })
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(StreamEvent::MessageDelta {
                stop_reason: Some(StopReason::EndTurn),
                ..
            })
        ));
    }
}
