//! Native pickers for things attached to a prompt.

use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

/// Native folder picker for attaching a folder to a prompt; `None` when dismissed.
#[tauri::command]
pub async fn attachment_pick_folder(app: AppHandle, title: String) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app.dialog().file().set_title(title);
        if let Some(window) = app.get_webview_window("main") {
            dialog = dialog.set_parent(&window);
        }
        dialog.blocking_pick_folder()
    })
    .await
    .map_err(|err| err.to_string())?
    .map(|picked| {
        picked
            .into_path()
            .map(|path| path.display().to_string())
            .map_err(|err| err.to_string())
    })
    .transpose()
}
