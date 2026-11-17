//! `/dd/*`: managing the core over HTTP — providers and their sign-ins, account plans and
//! limits, llama-server and local models, change events. The CLI and the desktop app both use
//! these routes.

use std::{convert::Infallible, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_stream::wrappers::BroadcastStream;

use crate::{
    Core, VERSION, browser,
    llama::{self, LaunchSpec, files},
    providers::{ProviderSummary, claude, devin},
    server::{Ctx, error},
};

type Result<T> = std::result::Result<T, Response>;

fn bad(message: impl Into<String>) -> Response {
    error(StatusCode::BAD_REQUEST, "invalid_request_error", message)
}

fn failed(message: impl Into<String>) -> Response {
    error(StatusCode::BAD_GATEWAY, "api_error", message)
}

fn not_found(message: impl Into<String>) -> Response {
    error(StatusCode::NOT_FOUND, "not_found_error", message)
}

fn ok<T: serde::Serialize>(value: T) -> Result<Json<T>> {
    Ok(Json(value))
}

pub fn routes() -> Router<Ctx> {
    Router::new()
        .route("/dd/info", get(info))
        .route("/dd/catalog", get(catalog))
        .route("/dd/events", get(events))
        .route("/dd/hardware", get(hardware))
        .route("/dd/token/regenerate", post(regenerate_token))
        .route("/dd/shutdown", post(shutdown))
        .route("/dd/tasks/{id}/cancel", post(cancel_task))
        .route("/dd/providers", get(providers))
        .route("/dd/accounts", get(accounts))
        .route("/dd/providers/{id}", get(provider_status))
        .route("/dd/providers/{id}/account", get(account))
        .route("/dd/providers/{id}/login", post(login))
        .route("/dd/providers/{id}/login/cancel", post(cancel_login))
        .route("/dd/providers/{id}/code", post(submit_code))
        .route("/dd/providers/{id}/logout", post(logout))
        .route("/dd/providers/{id}/models/refresh", post(refresh_models))
        .route("/dd/llama", get(llama_status))
        .route("/dd/llama/install", post(llama_install))
        .route("/dd/llama/uninstall", post(llama_uninstall))
        .route("/dd/llama/devices", get(llama_devices))
        .route("/dd/llama/models", get(llama_models))
        .route("/dd/llama/inspect", post(llama_inspect))
        .route("/dd/llama/instances", get(llama_instances))
        .route("/dd/llama/load", post(llama_load))
        .route("/dd/llama/unload", post(llama_unload))
}

// --- core ----------------------------------------------------------------------

async fn info(State(ctx): State<Ctx>) -> Json<Value> {
    let core = &ctx.core;
    Json(json!({
        "version": VERSION,
        "pid": std::process::id(),
        "dataDir": core.paths.data.to_string_lossy(),
        "modelsDir": core.paths.models_dir().to_string_lossy(),
        "llamaInstalled": core.llama_installed(),
    }))
}

async fn catalog(State(ctx): State<Ctx>) -> Json<crate::catalog::Catalog> {
    Json(ctx.core.catalog())
}

/// Server-Sent Events: `instances`, `provider`, `task`, `token` (see `events::Event`).
async fn events(State(ctx): State<Ctx>) -> Sse<impl Stream<Item = std::result::Result<SseEvent, Infallible>>> {
    // The current instance list first, so a client starts in sync.
    let first = SseEvent::default().event("instances").data(serde_json::to_string(&ctx.core.llama.list()).unwrap_or_default());
    let updates = BroadcastStream::new(ctx.core.events.subscribe()).filter_map(|event| async move {
        let event = event.ok()?;
        Some(Ok(SseEvent::default().event(event.name()).data(event.data().to_string())))
    });
    Sse::new(futures_util::stream::once(async { Ok(first) }).chain(updates))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

async fn hardware() -> Result<Json<crate::hardware::HardwareInfo>> {
    let info = tokio::task::spawn_blocking(crate::hardware::info).await.map_err(|e| failed(e.to_string()))?;
    ok(info)
}

async fn regenerate_token(State(ctx): State<Ctx>) -> Result<Json<Value>> {
    let token = ctx.core.regenerate_token().map_err(failed)?;
    ok(json!({ "token": token }))
}

async fn shutdown(State(ctx): State<Ctx>) -> StatusCode {
    ctx.core.request_shutdown();
    StatusCode::ACCEPTED
}

async fn cancel_task(State(ctx): State<Ctx>, Path(id): Path<String>) -> Result<StatusCode> {
    if ctx.core.tasks.cancel(&id) { Ok(StatusCode::NO_CONTENT) } else { Err(not_found(format!("no task {id:?}"))) }
}

// --- providers -----------------------------------------------------------------

fn summaries(core: &Core) -> Vec<ProviderSummary> {
    let claude = core.claude.status();
    let devin = core.devin.status();
    vec![
        ProviderSummary {
            id: llama::PROVIDER,
            name: "llama-server",
            kind: "local",
            signed_in: core.llama_installed(),
            models: core.llama.ready_all().len(),
        },
        ProviderSummary {
            id: claude::PROVIDER,
            name: claude::PROVIDER_NAME,
            kind: "remote",
            signed_in: claude.signed_in,
            models: claude.models,
        },
        ProviderSummary {
            id: devin::PROVIDER,
            name: devin::PROVIDER_NAME,
            kind: "remote",
            signed_in: devin.signed_in,
            models: devin.models,
        },
    ]
}

async fn providers(State(ctx): State<Ctx>) -> Json<Vec<ProviderSummary>> {
    Json(summaries(&ctx.core))
}

async fn provider_status(State(ctx): State<Ctx>, Path(id): Path<String>) -> Result<Json<Value>> {
    let core = &ctx.core;
    let value = match id.as_str() {
        claude::PROVIDER => serde_json::to_value(core.claude.status()),
        devin::PROVIDER => serde_json::to_value(core.devin.status()),
        llama::PROVIDER => {
            let status = llama::install::status(&core.paths).await.map_err(failed)?;
            let mut value = serde_json::to_value(status).unwrap_or_default();
            value["instances"] = json!(core.llama.list().len());
            Ok(value)
        }
        _ => return Err(not_found(format!("unknown provider {id:?}"))),
    };
    ok(value.unwrap_or_default())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Fresh {
    refresh: Option<String>,
}

impl Fresh {
    fn wanted(&self) -> bool {
        self.refresh.as_deref().is_some_and(|v| v != "0" && v != "false")
    }
}

async fn account_of(core: &Core, id: &str, fresh: bool) -> Result<crate::providers::Account> {
    match id {
        claude::PROVIDER => core.claude.account(fresh).await.map_err(failed),
        devin::PROVIDER => core.devin.account(fresh).await.map_err(failed),
        llama::PROVIDER => Err(bad("local models have no account")),
        _ => Err(not_found(format!("unknown provider {id:?}"))),
    }
}

/// Plan, balance and usage limits of one provider's account.
async fn account(State(ctx): State<Ctx>, Path(id): Path<String>, Query(fresh): Query<Fresh>) -> Result<Json<crate::providers::Account>> {
    ok(account_of(&ctx.core, &id, fresh.wanted()).await?)
}

/// Every signed-in provider's account, side by side; a failure stays with its provider.
async fn accounts(State(ctx): State<Ctx>, Query(fresh): Query<Fresh>) -> Json<Value> {
    let core = &ctx.core;
    let fresh = fresh.wanted();
    let mut out = serde_json::Map::new();
    let (claude, devin) = tokio::join!(
        async { core.claude.creds().is_some().then_some(core.claude.account(fresh).await) },
        async { core.devin.creds().is_some().then_some(core.devin.account(fresh).await) },
    );
    for (id, result) in [(claude::PROVIDER, claude), (devin::PROVIDER, devin)] {
        if let Some(result) = result {
            out.insert(
                id.into(),
                match result {
                    Ok(account) => serde_json::to_value(account).unwrap_or_default(),
                    Err(error) => json!({ "provider": id, "error": error }),
                },
            );
        }
    }
    Json(Value::Object(out))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LoginRequest {
    /// Open the sign-in page in the browser of the machine the core runs on (default).
    open: Option<bool>,
}

/// Runs a sign-in and answers when it's over. While it waits, the provider's status carries
/// `loginUrl` (and for Claude `manualUrl`, whose page shows a code for `POST …/code`).
async fn login(State(ctx): State<Ctx>, Path(id): Path<String>, body: Option<Json<LoginRequest>>) -> Result<Json<Value>> {
    let open = body.and_then(|b| b.0.open).unwrap_or(true);
    let opener = move |url: &str| if open { browser::open(url) } else { Ok(()) };
    let core = &ctx.core;
    let cancelled = |e: &String| e == "sign-in cancelled";
    let value = match id.as_str() {
        claude::PROVIDER => match core.claude.login(opener).await {
            Err(e) if cancelled(&e) => serde_json::to_value(core.claude.status()),
            result => serde_json::to_value(result.map_err(failed)?),
        },
        devin::PROVIDER => match core.devin.login(opener).await {
            Err(e) if cancelled(&e) => serde_json::to_value(core.devin.status()),
            result => serde_json::to_value(result.map_err(failed)?),
        },
        _ => return Err(not_found(format!("{id:?} has no sign-in"))),
    };
    ok(value.unwrap_or_default())
}

async fn cancel_login(State(ctx): State<Ctx>, Path(id): Path<String>) -> Result<Json<Value>> {
    let core = &ctx.core;
    let value = match id.as_str() {
        claude::PROVIDER => {
            core.claude.cancel_login();
            serde_json::to_value(core.claude.status())
        }
        devin::PROVIDER => {
            core.devin.cancel_login();
            serde_json::to_value(core.devin.status())
        }
        _ => return Err(not_found(format!("{id:?} has no sign-in"))),
    };
    ok(value.unwrap_or_default())
}

#[derive(Deserialize)]
struct CodeRequest {
    code: String,
}

async fn submit_code(State(ctx): State<Ctx>, Path(id): Path<String>, Json(body): Json<CodeRequest>) -> Result<Json<claude::ClaudeStatus>> {
    if id != claude::PROVIDER {
        return Err(not_found(format!("{id:?} takes no pasted code")));
    }
    ctx.core.claude.submit_code(&body.code).map_err(bad)?;
    ok(ctx.core.claude.status())
}

async fn logout(State(ctx): State<Ctx>, Path(id): Path<String>) -> Result<Json<Value>> {
    let core = &ctx.core;
    let value = match id.as_str() {
        claude::PROVIDER => serde_json::to_value(core.claude.logout()),
        devin::PROVIDER => serde_json::to_value(core.devin.logout()),
        _ => return Err(not_found(format!("{id:?} has no sign-in"))),
    };
    ok(value.unwrap_or_default())
}

async fn refresh_models(State(ctx): State<Ctx>, Path(id): Path<String>) -> Result<Json<Value>> {
    let count = match id.as_str() {
        claude::PROVIDER => ctx.core.claude.refresh_models().await,
        devin::PROVIDER => ctx.core.devin.refresh_models().await,
        _ => return Err(not_found(format!("{id:?} has no model list to fetch"))),
    }
    .map_err(failed)?;
    ok(json!({ "count": count }))
}

// --- llama-server ----------------------------------------------------------------

async fn llama_status(State(ctx): State<Ctx>) -> Result<Json<llama::install::LlamaServerStatus>> {
    ok(llama::install::status(&ctx.core.paths).await.map_err(failed)?)
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct InstallRequest {
    /// Only these backends (`cuda13`, `cuda12`, `vulkan`, `opencl`, `cpu`); default: detected.
    backends: Option<Vec<String>>,
    /// A release tag, e.g. `b6500`; default: the newest with the needed builds.
    version: Option<String>,
}

/// Installs (or replaces) llama-server; progress goes out as `task` events (id `llama-server`).
async fn llama_install(State(ctx): State<Ctx>, body: Option<Json<InstallRequest>>) -> Result<Json<llama::install::LlamaServerStatus>> {
    let body = body.map(|b| b.0).unwrap_or_default();
    if !ctx.core.llama.is_empty() {
        return Err(bad("unload the running models first"));
    }
    ok(llama::install::install(&ctx.core.paths, &ctx.core.tasks, body.backends, body.version).await.map_err(failed)?)
}

/// Removes llama-server; stops the running models first.
async fn llama_uninstall(State(ctx): State<Ctx>) -> Result<StatusCode> {
    for instance in ctx.core.llama.list() {
        ctx.core.llama.unload(&instance.id).await;
    }
    llama::install::uninstall(&ctx.core.paths).await.map_err(failed)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn llama_devices(State(ctx): State<Ctx>) -> Result<Json<Vec<llama::install::Device>>> {
    ok(llama::install::devices(&ctx.core.paths).await.map_err(failed)?)
}

/// Model files in the models folder, whether loaded or not (not part of the model lists).
async fn llama_models(State(ctx): State<Ctx>) -> Result<Json<Vec<files::ModelFile>>> {
    let (paths, meta) = (ctx.core.paths.clone(), ctx.core.meta.clone());
    ok(tokio::task::spawn_blocking(move || files::list(&paths, &meta)).await.map_err(|e| failed(e.to_string()))?)
}

#[derive(Deserialize)]
struct ModelRequest {
    model: String,
}

/// What a local model file says about itself: context, tools/reasoning, sampling, and the
/// architecture numbers for memory estimates.
async fn llama_inspect(State(ctx): State<Ctx>, Json(body): Json<ModelRequest>) -> Result<Json<files::Inspection>> {
    let path = match files::resolve(&ctx.core.paths, &body.model).map_err(bad)? {
        files::Reference::Local(path) => path,
        files::Reference::Remote { file, .. } => {
            let path = ctx.core.paths.models_dir().join(&file);
            if !path.is_file() {
                return Err(bad(format!("{file} isn't downloaded yet")));
            }
            path
        }
    };
    let meta = ctx.core.meta.clone();
    ok(tokio::task::spawn_blocking(move || meta.inspect(&path)).await.map_err(|e| failed(e.to_string()))?.map_err(bad)?)
}

async fn llama_instances(State(ctx): State<Ctx>) -> Json<Vec<llama::InstanceInfo>> {
    Json(ctx.core.llama.list())
}

/// Downloads the model if it's a URL, starts llama-server with the given settings, and answers
/// once it's ready (or failed).
async fn llama_load(State(ctx): State<Ctx>, Json(mut spec): Json<LaunchSpec>) -> Result<Json<llama::InstanceInfo>> {
    let core = &ctx.core;
    let exe = llama::install::installed_exe(&core.paths).ok_or_else(|| bad("llama-server is not installed (dd-core llama install)"))?;
    let reference = files::resolve(&core.paths, spec.reference()).map_err(bad)?;
    let path = files::fetch(&core.paths, &core.tasks, reference).await.map_err(failed)?;
    if spec.model.is_empty() {
        spec.model = spec.file.clone();
    }
    // Clients get the file name for models in the models folder, the full path otherwise.
    spec.file = match path.strip_prefix(core.paths.models_dir()) {
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    };
    let meta = {
        let (cache, path) = (core.meta.clone(), path.clone());
        tokio::task::spawn_blocking(move || cache.file_meta(&path)).await.unwrap_or_default()
    };
    ok(core.llama.load(exe, path, spec, meta).await.map_err(failed)?)
}

#[derive(Deserialize)]
struct UnloadRequest {
    id: String,
}

async fn llama_unload(State(ctx): State<Ctx>, Json(body): Json<UnloadRequest>) -> Result<StatusCode> {
    if ctx.core.llama.unload(&body.id).await { Ok(StatusCode::NO_CONTENT) } else { Err(not_found(format!("{} is not loaded", body.id))) }
}
