//! Things attached to a prompt: native pickers, what dropped paths are, files on the clipboard.

use serde::Serialize;
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

/// Native picker for files to attach to one message; empty when dismissed.
#[tauri::command]
pub async fn attachment_pick_files(app: AppHandle, title: String) -> Result<Vec<String>, String> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app.dialog().file().set_title(title);
        if let Some(window) = app.get_webview_window("main") {
            dialog = dialog.set_parent(&window);
        }
        dialog.blocking_pick_files()
    })
    .await
    .map_err(|err| err.to_string())?;
    Ok(picked
        .unwrap_or_default()
        .into_iter()
        .filter_map(|file| file.into_path().ok())
        .map(|path| path.display().to_string())
        .collect())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathKind {
    path: String,
    /// A folder (becomes a workspace folder) rather than a file.
    dir: bool,
}

/// What dropped or pasted paths are; ones that don't exist are left out.
#[tauri::command]
pub async fn attachment_kinds(paths: Vec<String>) -> Result<Vec<PathKind>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        paths
            .into_iter()
            .filter_map(|path| {
                let meta = std::fs::metadata(&path).ok()?;
                Some(PathKind { dir: meta.is_dir(), path })
            })
            .collect()
    })
    .await
    .map_err(|err| err.to_string())
}

/// Files copied in Explorer (the clipboard's CF_HDROP list); empty when it holds none.
#[tauri::command]
pub async fn clipboard_file_paths() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(clipboard_files)
        .await
        .map_err(|err| err.to_string())
}

fn clipboard_files() -> Vec<String> {
    use windows::Win32::{
        Foundation::HWND,
        System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard},
        UI::Shell::{DragQueryFileW, HDROP},
    };
    const CF_HDROP: u32 = 15;
    let mut out = Vec::new();
    // SAFETY: the clipboard is opened and closed here; the HDROP stays owned by the clipboard and
    // is only read while it's open.
    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP).is_err() || OpenClipboard(Some(HWND::default())).is_err() {
            return out;
        }
        if let Ok(handle) = GetClipboardData(CF_HDROP) {
            let drop = HDROP(handle.0);
            let count = DragQueryFileW(drop, u32::MAX, None);
            for index in 0..count {
                let len = DragQueryFileW(drop, index, None) as usize;
                let mut buf = vec![0u16; len + 1];
                let written = DragQueryFileW(drop, index, Some(&mut buf)) as usize;
                out.push(String::from_utf16_lossy(&buf[..written]));
            }
        }
        let _ = CloseClipboard();
    }
    out
}

#[cfg(test)]
mod tests {
    /// Copy a file in Explorer (or `Set-Clipboard -Path`) first; `DD_TEST_CLIPBOARD_FILE` = its path.
    #[test]
    #[ignore]
    fn reads_copied_files() {
        let expected = std::env::var("DD_TEST_CLIPBOARD_FILE").unwrap();
        assert_eq!(super::clipboard_files(), vec![expected]);
    }
}
