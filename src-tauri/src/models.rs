//! Local GGUF model management: listing, Hugging Face downloads, imports and deletion.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::tasks::{
    Progress, Tasks, err_str, http_client, remove_file_quiet, storage_subdir, stream_to_file,
};

const DIR_NAME: &str = "models";
const PART_SUFFIX: &str = ".part";
const IMPORT_BUF_SIZE: usize = 8 << 20;

/// Characters left unencoded in a URL path segment (RFC 3986 unreserved).
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModel {
    name: String,
    size: u64,
    modified: u64,
}

fn models_path(app: &AppHandle) -> Result<PathBuf, String> {
    storage_subdir(app, DIR_NAME)
}

fn local_model(path: &Path, name: String) -> Result<LocalModel, String> {
    let meta = std::fs::metadata(path).map_err(err_str)?;
    Ok(LocalModel {
        name,
        size: meta.len(),
        modified: modified_ms(&meta),
    })
}

fn modified_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A model file name must be a single, plain path component.
fn validate_name(name: &str) -> Result<(), String> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name
            .chars()
            .any(|c| c == '/' || c == '\\' || c == ':' || c.is_control());
    if bad {
        Err(format!("invalid model file name: {name:?}"))
    } else {
        Ok(())
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(|seg| utf8_percent_encode(seg, SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

#[tauri::command]
pub async fn models_dir(app: AppHandle) -> Result<String, String> {
    Ok(models_path(&app)?.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn models_list(app: AppHandle) -> Result<Vec<LocalModel>, String> {
    let dir = models_path(&app)?;
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(err_str)? {
        let entry = entry.map_err(err_str)?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') || name.ends_with(PART_SUFFIX) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        out.push(LocalModel {
            name,
            size: meta.len(),
            modified: modified_ms(&meta),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[tauri::command]
pub async fn model_delete(app: AppHandle, name: String) -> Result<(), String> {
    validate_name(&name)?;
    let path = models_path(&app)?.join(&name);
    tokio::fs::remove_file(&path).await.map_err(err_str)
}

#[tauri::command]
pub async fn model_download(
    app: AppHandle,
    id: String,
    repo: String,
    files: Vec<String>,
    total: Option<u64>,
) -> Result<Vec<LocalModel>, String> {
    let guard = app.state::<Tasks>().start(&id)?;
    let label = files
        .first()
        .map(|f| basename(f).to_string())
        .unwrap_or_else(|| repo.clone());
    let mut progress = Progress::new(&app, &guard, "download", label);

    let mut completed = Vec::new();
    let mut current_part = None;
    let result = download_inner(
        &app,
        &repo,
        &files,
        total,
        &mut progress,
        &mut completed,
        &mut current_part,
    )
    .await;
    progress.finish();

    if result.is_err() {
        if let Some(part) = &current_part {
            remove_file_quiet(part);
        }
        for path in &completed {
            remove_file_quiet(path);
        }
    }
    drop(guard);
    result
}

async fn download_inner(
    app: &AppHandle,
    repo: &str,
    files: &[String],
    total: Option<u64>,
    progress: &mut Progress,
    completed: &mut Vec<PathBuf>,
    current_part: &mut Option<PathBuf>,
) -> Result<Vec<LocalModel>, String> {
    for file in files {
        validate_name(basename(file))?;
    }
    let dir = models_path(app)?;
    let client = http_client()?;

    let mut out = Vec::new();
    let mut received = 0u64;
    let mut seen_total: Option<u64> = None;
    for file in files {
        progress.check_cancel()?;
        let name = basename(file).to_string();
        let url = format!(
            "https://huggingface.co/{}/resolve/main/{}",
            encode_path(repo),
            encode_path(file)
        );
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(err_str)?
            .error_for_status()
            .map_err(|e| format!("failed to download {file}: {e}"))?;
        if let Some(len) = resp.content_length() {
            seen_total = Some(seen_total.unwrap_or(0) + len);
        }
        let file_total = total.or(seen_total);

        let part = dir.join(format!("{name}{PART_SUFFIX}"));
        *current_part = Some(part.clone());
        received += stream_to_file(resp, &part, progress, received, file_total).await?;

        let dest = dir.join(&name);
        tokio::fs::rename(&part, &dest).await.map_err(err_str)?;
        *current_part = None;
        completed.push(dest.clone());
        out.push(local_model(&dest, name)?);
    }
    Ok(out)
}

#[tauri::command]
pub async fn model_import(
    app: AppHandle,
    id: String,
    filter_name: String,
    all_files_name: String,
) -> Result<Option<LocalModel>, String> {
    let guard = app.state::<Tasks>().start(&id)?;

    let dialog_app = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut builder = dialog_app
            .dialog()
            .file()
            .add_filter(filter_name, &["gguf"])
            .add_filter(all_files_name, &["*"]);
        if let Some(window) = dialog_app.get_webview_window("main") {
            builder = builder.set_parent(&window);
        }
        builder.blocking_pick_file()
    })
    .await
    .map_err(err_str)?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let src = picked.into_path().map_err(err_str)?;
    let name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "invalid file name".to_string())?
        .to_string();
    validate_name(&name)?;

    let dir = models_path(&app)?;
    let dest = dir.join(&name);
    if dest.exists() {
        return Err("exists".to_string());
    }
    let part = dir.join(format!("{name}{PART_SUFFIX}"));
    let mut progress = Progress::new(&app, &guard, "import", name.clone());

    let result = tauri::async_runtime::spawn_blocking(move || {
        let r = copy_with_progress(&src, &part, &dest, &mut progress);
        progress.finish();
        if r.is_err() {
            remove_file_quiet(&part);
        }
        r.and_then(|()| local_model(&dest, name))
    })
    .await
    .map_err(err_str)?;
    drop(guard);
    result.map(Some)
}

fn copy_with_progress(
    src: &Path,
    part: &Path,
    dest: &Path,
    progress: &mut Progress,
) -> Result<(), String> {
    let mut input = std::fs::File::open(src).map_err(err_str)?;
    let total = input.metadata().map_err(err_str)?.len();
    let mut output = std::fs::File::create(part).map_err(err_str)?;
    let mut buf = vec![0u8; IMPORT_BUF_SIZE];
    let mut copied = 0u64;
    progress.update(0, Some(total));
    loop {
        progress.check_cancel()?;
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(err_str(e)),
        };
        output.write_all(&buf[..n]).map_err(err_str)?;
        copied += n as u64;
        progress.update(copied, Some(total));
    }
    progress.check_cancel()?;
    output.sync_all().map_err(err_str)?;
    drop(output);
    std::fs::rename(part, dest).map_err(err_str)
}
