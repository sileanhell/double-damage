#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod attachments;
mod background;
mod bash;
mod data_dir;
mod gateway;
mod gguf;
mod hardware;
mod llama_server;
mod models;
mod store;
mod tasks;
mod tools;

use std::{path::Path, sync::atomic::Ordering};

use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, window::Color};

/// Builds the (initially hidden) main window; the page shows it once it has rendered.
/// Created in code rather than tauri.conf.json so its webview profile lives in the app data
/// folder (which can change at runtime) and the page gets the settings before it starts.
pub fn create_main_window(
    app: &AppHandle,
    webview_data: &Path,
    geometry: Option<data_dir::WindowGeometry>,
) -> tauri::Result<()> {
    let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
        .title("Double Damage")
        .visible(false)
        .background_color(Color(0, 0, 0, 255))
        .inner_size(1280.0, 720.0)
        .min_inner_size(1280.0, 720.0)
        .data_directory(webview_data.to_path_buf())
        .initialization_script(app.state::<store::Store>().init_script())
        .build()?;
    if let Some(geometry) = geometry {
        geometry.apply(&window);
    }
    Ok(())
}

fn main() {
    let data = data_dir::resolve();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_sql::Builder::new().build())
        .manage(tasks::Tasks::default())
        .manage(bash::Runs::default())
        .manage(store::Store::load(&data))
        .manage(data_dir::DataDir::new(data.clone()))
        .setup(move |app| {
            background::allow(app.handle(), &data);
            gateway::commands::setup(app.handle());
            create_main_window(app.handle(), &data.join(data_dir::WEBVIEW_DIR), None)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            attachments::attachment_pick_folder,
            attachments::attachment_pick_files,
            attachments::attachment_kinds,
            attachments::clipboard_file_paths,
            data_dir::data_dir_info,
            data_dir::data_dir_pick,
            data_dir::data_dir_apply,
            data_dir::sessions_db_path,
            store::store_set,
            hardware::hardware_info,
            background::background_save,
            background::background_path,
            background::background_clear,
            llama_server::llama_server_status,
            llama_server::llama_server_devices,
            llama_server::llama_server_install,
            llama_server::llama_server_uninstall,
            models::models_list,
            models::model_download,
            models::model_import,
            models::model_delete,
            models::models_dir,
            gguf::model_info,
            tasks::task_cancel,
            bash::bash_status,
            bash::bash_latest,
            bash::bash_install,
            bash::bash_uninstall,
            bash::bash_run,
            bash::bash_cancel,
            gateway::commands::gateway_info,
            gateway::commands::gateway_catalog,
            gateway::commands::gateway_restart,
            gateway::commands::gateway_regenerate_token,
            gateway::commands::llama_instances,
            gateway::commands::llama_load,
            gateway::commands::llama_unload,
            tools::tool_read,
            tools::tool_write,
            tools::tool_edit,
            tools::tool_list,
            tools::tool_glob,
            tools::tool_grep,
            tools::workspace_instructions,
            tools::tool_sensitive,
            tools::tool_patch_paths,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                gateway::commands::shutdown(app);
            }
            // Closing the last window normally quits; not while it is being swapped for a new one.
            if let RunEvent::ExitRequested { api, code: None, .. } = event
                && data_dir::SWITCHING.load(Ordering::SeqCst)
            {
                api.prevent_exit();
            }
        });
}
