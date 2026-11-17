//! The local HTTP gateway: OpenAI- and Anthropic-compatible endpoints on 127.0.0.1 that route
//! each request to the provider serving its `model`. Every request needs the local API token
//! (`Authorization: Bearer …` or `x-api-key: …`), so other programs and web pages can't use the
//! gateway without being given it.

use std::sync::Arc;

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::StreamExt;
use serde_json::json;
use tokio::sync::oneshot;

use super::{
    catalog,
    llama::{self, Llama},
};

/// Endpoints passed through to the model's server.
const PROXIED: [&str; 5] =
    ["/v1/messages", "/v1/messages/count_tokens", "/v1/chat/completions", "/v1/completions", "/v1/embeddings"];

/// Providers the gateway knows (scoped paths for anything else are 404).
const PROVIDERS: [&str; 1] = [llama::PROVIDER];

/// Origins of the app's own page (release build, dev server); only these get CORS headers.
const APP_ORIGINS: [&str; 3] = ["http://tauri.localhost", "tauri://localhost", "http://localhost:1420"];

#[derive(Clone)]
struct Ctx {
    token: Arc<str>,
    llama: Llama,
    catalog: Arc<catalog::Source>,
    client: reqwest::Client,
}

/// A running server; dropping it (or calling `stop`) shuts it down.
pub struct Server {
    pub port: u16,
    stop: Option<oneshot::Sender<()>>,
}

impl Server {
    pub fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

pub async fn start(port: u16, token: String, llama: Llama, catalog: Arc<catalog::Source>) -> Result<Server, String> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| format!("can't listen on 127.0.0.1:{port}: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let client = reqwest::Client::builder().no_proxy().build().map_err(|e| e.to_string())?;
    let ctx = Ctx { token: token.into(), llama, catalog, client };

    // Every endpoint twice: for all providers, and under `/{provider}` for just one of them.
    let mut api = Router::new()
        .route("/v1/models", get(models))
        .route("/{provider}/v1/models", get(models_scoped))
        .route("/dd/catalog", get(catalog_handler));
    for path in PROXIED {
        api = api.route(path, post(proxy)).route(&format!("/{{provider}}{path}"), post(proxy_scoped));
    }
    let api = api.route_layer(middleware::from_fn_with_state(ctx.clone(), auth));
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(api)
        .layer(middleware::from_fn(cors))
        .with_state(ctx);

    let (stop, stopped) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await;
    });
    Ok(Server { port, stop: Some(stop) })
}

/// Errors in a shape both OpenAI and Anthropic clients understand well enough.
fn error(status: StatusCode, kind: &str, message: impl Into<String>) -> Response {
    let message = message.into();
    (
        status,
        Json(json!({
            "type": "error",
            "error": { "type": kind, "message": message, "code": kind },
        })),
    )
        .into_response()
}

async fn cors(request: Request, next: Next) -> Response {
    let origin = request.headers().get(header::ORIGIN).cloned();
    let allowed = origin.filter(|o| o.to_str().is_ok_and(|o| APP_ORIGINS.contains(&o)));
    let mut response = if request.method() == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };
    if let Some(origin) = allowed {
        let headers = response.headers_mut();
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST, OPTIONS"));
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("*"));
        headers.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    response
}

fn presented_token(headers: &HeaderMap) -> Option<&str> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    bearer.or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
}

/// Constant-time comparison, so the token can't be guessed byte by byte from response timing.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn auth(State(ctx): State<Ctx>, request: Request, next: Next) -> Response {
    match presented_token(request.headers()) {
        Some(token) if same(token.trim(), &ctx.token) => next.run(request).await,
        _ => error(StatusCode::UNAUTHORIZED, "authentication_error", "invalid or missing API token"),
    }
}

/// Loaded models, in a shape that satisfies both the OpenAI and the Anthropic model list.
fn model_list(ctx: &Ctx, provider: Option<&str>) -> Response {
    let data: Vec<_> = ctx
        .llama
        .list()
        .into_iter()
        .filter(|i| i.state == llama::State::Ready)
        .filter(|_| provider.is_none_or(|p| p == llama::PROVIDER))
        .map(|i| {
            json!({
                "id": i.id,
                "object": "model",
                "type": "model",
                "owned_by": llama::PROVIDER,
                "provider": llama::PROVIDER,
                "display_name": i.id,
                "created": i.started_at / 1000,
                "created_at": i.started_at / 1000,
                "context_length": i.spec.ctx,
            })
        })
        .collect();
    Json(json!({ "object": "list", "data": data, "has_more": false })).into_response()
}

async fn models(State(ctx): State<Ctx>) -> Response {
    model_list(&ctx, None)
}

async fn models_scoped(State(ctx): State<Ctx>, Path(provider): Path<String>) -> Response {
    if !PROVIDERS.contains(&provider.as_str()) {
        return error(StatusCode::NOT_FOUND, "not_found_error", format!("unknown provider {provider:?}"));
    }
    model_list(&ctx, Some(&provider))
}

async fn catalog_handler(State(ctx): State<Ctx>) -> Response {
    let source = ctx.catalog.clone();
    match tokio::task::spawn_blocking(move || source.build()).await {
        Ok(catalog) => Json(catalog).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, "api_error", e.to_string()),
    }
}

async fn proxy(State(ctx): State<Ctx>, request: Request) -> Response {
    let path = request.uri().path().to_string();
    route(ctx, None, path, request).await
}

async fn proxy_scoped(State(ctx): State<Ctx>, Path(provider): Path<String>, request: Request) -> Response {
    if !PROVIDERS.contains(&provider.as_str()) {
        return error(StatusCode::NOT_FOUND, "not_found_error", format!("unknown provider {provider:?}"));
    }
    let path = request.uri().path().strip_prefix(&format!("/{provider}")).unwrap_or("/").to_string();
    route(ctx, Some(provider), path, request).await
}

/// Full model id for a request. Under a provider's path the prefix may be left out, and
/// another provider's models are off limits.
fn resolve_model(model: &str, provider: Option<&str>) -> Option<String> {
    match provider {
        None => Some(model.to_string()),
        Some(p) => match model.strip_prefix(p).and_then(|rest| rest.strip_prefix('/')) {
            Some(_) => Some(model.to_string()),
            None if !model.contains('/') => Some(format!("{p}/{model}")),
            None => None,
        },
    }
}

async fn route(ctx: Ctx, provider: Option<String>, path: String, request: Request) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 256 << 20).await {
        Ok(body) => body,
        Err(e) => return error(StatusCode::BAD_REQUEST, "invalid_request_error", e.to_string()),
    };
    let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_request_error", "request body is not JSON");
    };
    let Some(model) = json.get("model").and_then(|m| m.as_str()).map(str::to_owned) else {
        return error(StatusCode::BAD_REQUEST, "invalid_request_error", "request has no `model`");
    };
    let Some(id) = resolve_model(&model, provider.as_deref()) else {
        return error(StatusCode::NOT_FOUND, "not_found_error", format!("model {model:?} is not served here"));
    };
    let Some(instance) = ctx.llama.ready(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found_error", format!("model {id:?} is not loaded"));
    };
    let body = if adapt_for_llama(&mut json) { Bytes::from(json.to_string()) } else { body };
    forward(&ctx, instance, &path, body).await
}

/// Translates the standard reasoning switches into what llama-server understands (it ignores
/// Anthropic's `thinking` and only reads `chat_template_kwargs`). Explicit kwargs win.
/// Returns whether the body changed.
fn adapt_for_llama(body: &mut serde_json::Value) -> bool {
    let Some(obj) = body.as_object_mut() else { return false };
    let thinking = match obj.get("thinking").and_then(|t| t.get("type")).and_then(|t| t.as_str()) {
        Some("disabled") => Some(false),
        Some("enabled" | "adaptive") => Some(true),
        _ => None,
    };
    let effort = obj
        .get("reasoning_effort")
        .or_else(|| obj.get("output_config").and_then(|c| c.get("effort")))
        .or_else(|| obj.get("reasoning").and_then(|r| r.get("effort")))
        .and_then(|e| e.as_str())
        .map(str::to_owned);
    if thinking.is_none() && effort.is_none() {
        return false;
    }
    let kwargs = obj.entry("chat_template_kwargs").or_insert_with(|| json!({}));
    let Some(kwargs) = kwargs.as_object_mut() else { return false };
    let mut changed = false;
    if let Some(thinking) = thinking
        && !kwargs.contains_key("enable_thinking")
    {
        kwargs.insert("enable_thinking".into(), thinking.into());
        changed = true;
    }
    if let Some(effort) = effort
        && !kwargs.contains_key("reasoning_effort")
    {
        kwargs.insert("reasoning_effort".into(), effort.into());
        changed = true;
    }
    changed
}

/// Sends the request on to the model's server and streams the answer back. When the client
/// goes away the stream is dropped, which closes the upstream connection and stops generation.
async fn forward(ctx: &Ctx, instance: Arc<llama::Instance>, path: &str, body: Bytes) -> Response {
    let busy = ctx.llama.busy(instance.clone());
    let upstream = ctx
        .client
        .post(format!("http://127.0.0.1:{}{path}", instance.port))
        .bearer_auth(&instance.key)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await;
    let upstream = match upstream {
        Ok(upstream) => upstream,
        Err(e) => return error(StatusCode::BAD_GATEWAY, "api_error", format!("model server unreachable: {e}")),
    };

    let status = StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = upstream.headers().get(reqwest::header::CONTENT_TYPE).cloned();
    // `busy` rides along with the stream, so the request counts as active until it ends.
    let stream = upstream.bytes_stream().map(move |chunk| {
        let _ = &busy;
        chunk
    });
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    if let Some(content_type) = content_type.and_then(|v| HeaderValue::from_bytes(v.as_bytes()).ok()) {
        response.headers_mut().insert(header::CONTENT_TYPE, content_type);
    }
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_and_effort_map_to_template_kwargs() {
        let mut body = json!({ "model": "m", "thinking": { "type": "disabled" } });
        assert!(adapt_for_llama(&mut body));
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);

        let mut body = json!({ "thinking": { "type": "enabled", "budget_tokens": 2048 }, "output_config": { "effort": "low" } });
        assert!(adapt_for_llama(&mut body));
        assert_eq!(body["chat_template_kwargs"], json!({ "enable_thinking": true, "reasoning_effort": "low" }));

        // OpenAI's spelling.
        let mut body = json!({ "reasoning_effort": "high" });
        assert!(adapt_for_llama(&mut body));
        assert_eq!(body["chat_template_kwargs"]["reasoning_effort"], "high");

        // Explicit kwargs win; nothing to do leaves the body alone.
        let mut body = json!({ "thinking": { "type": "disabled" }, "chat_template_kwargs": { "enable_thinking": true } });
        assert!(!adapt_for_llama(&mut body));
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
        let mut body = json!({ "model": "m" });
        assert!(!adapt_for_llama(&mut body));
        assert!(body.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn scoped_model_names() {
        assert_eq!(resolve_model("anything/x", None).as_deref(), Some("anything/x"));
        assert_eq!(resolve_model("Qwen", Some("llama-server")).as_deref(), Some("llama-server/Qwen"));
        assert_eq!(resolve_model("llama-server/Qwen", Some("llama-server")).as_deref(), Some("llama-server/Qwen"));
        assert_eq!(resolve_model("anthropic/claude", Some("llama-server")), None);
        // A prefix that merely starts the same isn't the provider.
        assert_eq!(resolve_model("llama-server-x/Qwen", Some("llama-server")), None);
    }

    #[test]
    fn token_checks() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));

        let mut headers = HeaderMap::new();
        assert_eq!(presented_token(&headers), None);
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer t1"));
        assert_eq!(presented_token(&headers), Some("t1"));
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("t2"));
        assert_eq!(presented_token(&headers), Some("t2"));
    }
}
