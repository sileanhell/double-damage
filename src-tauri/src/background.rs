//! The user's custom background (image or video) as a file in `<data>/background/`, served to
//! the page through the asset protocol.

use std::{fs, path::PathBuf};

use tauri::{AppHandle, Manager, ipc::InvokeBody};

use crate::tasks::{err_str, storage_root};

pub const DIR: &str = "background";
const STEM: &str = "current";

fn extension(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        _ => return None,
    })
}

fn current(app: &AppHandle) -> Option<PathBuf> {
    let dir = storage_root(app).ok()?.join(DIR);
    fs::read_dir(dir).ok()?.flatten().map(|entry| entry.path()).find(|path| {
        path.is_file() && path.file_stem().is_some_and(|stem| stem == STEM)
    })
}

/// Lets the asset protocol serve the background folder of `data` (called on start and on switch).
pub fn allow(app: &AppHandle, data: &std::path::Path) {
    if let Err(error) = app.asset_protocol_scope().allow_directory(data.join(DIR), true) {
        eprintln!("asset scope: {error}");
    }
}

/// Saves the raw request body as the background; the MIME type comes in the `x-mime` header.
/// Returns the absolute path of the stored file.
#[tauri::command]
pub async fn background_save(app: AppHandle, request: tauri::ipc::Request<'_>) -> Result<String, String> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected a raw body".into());
    };
    let mime = request.headers().get("x-mime").and_then(|value| value.to_str().ok()).unwrap_or("");
    let ext = extension(mime).ok_or_else(|| format!("unsupported type: {mime}"))?;

    let dir = storage_root(&app)?.join(DIR);
    fs::create_dir_all(&dir).map_err(err_str)?;
    let target = dir.join(format!("{STEM}.{ext}"));
    let tmp = dir.join(format!("{STEM}.{ext}.part"));
    fs::write(&tmp, bytes).map_err(err_str)?;
    if let Some(old) = current(&app) {
        let _ = fs::remove_file(old);
    }
    fs::rename(&tmp, &target).map_err(err_str)?;
    Ok(target.to_string_lossy().into_owned())
}

/// Absolute path of the stored background, if there is one.
#[tauri::command]
pub fn background_path(app: AppHandle) -> Option<String> {
    current(&app).map(|path| path.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn background_clear(app: AppHandle) -> Result<(), String> {
    let dir = storage_root(&app)?.join(DIR);
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(err_str)?;
    }
    Ok(())
}
