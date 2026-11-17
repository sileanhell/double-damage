//! Tauri commands and wiring for the gateway.

use tauri::{AppHandle, Emitter, Manager, State};

use super::{DEFAULT_PORT, Gateway, GatewayInfo, Llama, catalog, llama, random_hex};
use crate::{store::Store, tasks::storage_root};

/// Settings keys. The port belongs to the settings page; the token is the backend's own.
const PORT_KEY: &str = "dd:gateway-port";
const TOKEN_KEY: &str = "gateway.token";
/// Sent with the full list of running models whenever anything about them changes.
pub const INSTANCES_EVENT: &str = "llama-instances";

fn port_setting(store: &Store) -> u16 {
    store.get(PORT_KEY).and_then(|p| p.trim().parse().ok()).filter(|p| *p > 0).unwrap_or(DEFAULT_PORT)
}

fn token_setting(app: &AppHandle) -> String {
    let store = app.state::<Store>();
    if let Some(token) = store.get(TOKEN_KEY).filter(|t| t.len() >= 32) {
        return token;
    }
    new_token(app)
}

fn new_token(app: &AppHandle) -> String {
    let token = format!("dd-{}", random_hex(24));
    let dir = app.state::<crate::data_dir::DataDir>().get();
    if let Err(error) = app.state::<Store>().set(&dir, TOKEN_KEY, Some(token.clone())) {
        eprintln!("failed to save the gateway token: {error}");
    }
    token
}

/// Creates the gateway state and starts listening in the background.
pub fn setup(app: &AppHandle) {
    let emitter = app.clone();
    let llama = Llama::new(move |list| {
        let _ = emitter.emit(INSTANCES_EVENT, list);
    });
    let token = token_setting(app);
    let paths_app = app.clone();
    let paths = move || {
        let root = paths_app.state::<crate::data_dir::DataDir>().get();
        catalog::Paths {
            models_dir: root.join(crate::models::DIR_NAME),
            llama_exe: crate::llama_server::installed_exe(&root),
        }
    };
    app.manage(Gateway::new(llama, paths, token.clone()));
    let port = port_setting(&app.state::<Store>());
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        app.state::<Gateway>().listen(port, token).await;
    });
}

pub fn shutdown(app: &AppHandle) {
    if let Some(gateway) = app.try_state::<Gateway>() {
        gateway.shutdown();
    }
}

#[tauri::command]
pub fn gateway_info(gateway: State<'_, Gateway>) -> GatewayInfo {
    gateway.info()
}

/// Re-reads the port setting and restarts the endpoint on it.
#[tauri::command]
pub async fn gateway_restart(app: AppHandle) -> Result<GatewayInfo, String> {
    let port = port_setting(&app.state::<Store>());
    let token = app.state::<Gateway>().info().token;
    Ok(app.state::<Gateway>().listen(port, token).await)
}

/// Issues a new token; clients holding the old one are locked out.
#[tauri::command]
pub async fn gateway_regenerate_token(app: AppHandle) -> Result<GatewayInfo, String> {
    let token = new_token(&app);
    let port = app.state::<Gateway>().info().port;
    Ok(app.state::<Gateway>().listen(port, token).await)
}

/// Every provider with its models (local ones included whether loaded or not).
#[tauri::command]
pub async fn gateway_catalog(app: AppHandle) -> Result<catalog::Catalog, String> {
    let source = app.state::<Gateway>().catalog.clone();
    tauri::async_runtime::spawn_blocking(move || source.build()).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn llama_instances(gateway: State<'_, Gateway>) -> Vec<llama::InstanceInfo> {
    gateway.llama.list()
}

/// Loads a model and resolves once it is ready to answer.
#[tauri::command]
pub async fn llama_load(app: AppHandle, spec: llama::LaunchSpec) -> Result<llama::InstanceInfo, String> {
    crate::models::validate_name(&spec.file)?;
    let root = storage_root(&app)?;
    let exe = crate::llama_server::installed_exe(&root).ok_or("llama-server is not installed")?;
    let model = root.join(crate::models::DIR_NAME).join(&spec.file);
    let llama = app.state::<Gateway>().llama.clone();
    llama.load(exe, model, spec).await
}

#[tauri::command]
pub async fn llama_unload(app: AppHandle, id: String) -> Result<(), String> {
    let llama = app.state::<Gateway>().llama.clone();
    llama.unload(&id).await;
    Ok(())
}
