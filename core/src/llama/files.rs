//! Model files: turning what `load` is given (URL, path, file name) into a file on disk,
//! downloading when needed, and reading what a file says about itself.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
    time::UNIX_EPOCH,
};

use serde::Serialize;

use super::runtime::{FileMeta, model_id};
use crate::{
    caps::{self, Capabilities},
    gguf::{self, ModelInfo, Sampling},
    paths::Paths,
    tasks::{Tasks, http_client, remove_file_quiet, stream_to_file},
    util::err_str,
};

const PART_SUFFIX: &str = ".part";
const SHARD_MARK: &str = "-of-";

/// `name-00002-of-00003.gguf` -> Some((`name`, 2, 3)).
pub fn shard(name: &str) -> Option<(&str, u32, u32)> {
    let stem = name.strip_suffix(".gguf")?;
    let (rest, count) = stem.rsplit_once(SHARD_MARK)?;
    let (base, index) = rest.rsplit_once('-')?;
    if index.len() != 5 || count.len() != 5 {
        return None;
    }
    Some((base, index.parse().ok()?, count.parse().ok()?))
}

/// Multimodal projectors ride along with a model; they don't run on their own.
fn is_projector(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("mmproj")
}

/// A model file name must be a single, plain path component.
pub fn validate_name(name: &str) -> Result<(), String> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.chars().any(|c| c == '/' || c == '\\' || c == ':' || c.is_control());
    if bad { Err(format!("invalid model file name: {name:?}")) } else { Ok(()) }
}

/// Size of a model on disk, all parts of a split model together.
pub fn model_size(path: &Path) -> u64 {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let Some((base, _, count)) = shard(name) else {
        return std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    (1..=count)
        .map(|i| dir.join(format!("{base}-{i:05}-of-{count:05}.gguf")))
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

/// What a reference to a model means.
#[derive(Debug, PartialEq)]
pub enum Reference {
    /// Download from these URLs (more than one for a split model), into the models folder.
    Remote { urls: Vec<String>, file: String },
    /// An existing file.
    Local(PathBuf),
}

fn url_file_name(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let name = path.rsplit('/').next()?;
    let name = percent_encoding::percent_decode_str(name).decode_utf8().ok()?.into_owned();
    validate_name(&name).ok()?;
    Some(name)
}

/// `owner/repo/path/file.gguf` → its Hugging Face download URL.
fn hf_url(spec: &str) -> Option<String> {
    let mut parts = spec.trim_start_matches('/').splitn(3, '/');
    let (owner, repo, file) = (parts.next()?, parts.next()?, parts.next()?);
    if owner.is_empty() || repo.is_empty() || file.is_empty() {
        return None;
    }
    Some(format!("https://huggingface.co/{owner}/{repo}/resolve/main/{file}"))
}

/// Resolves `reference`:
/// - `https://…` (Hugging Face `/blob/` links become `/resolve/`) or `hf:owner/repo/file.gguf`:
///   downloaded into the models folder unless already there; for the first part of a split
///   model, every part is fetched;
/// - an existing path;
/// - a file name in the models folder.
pub fn resolve(paths: &Paths, reference: &str) -> Result<Reference, String> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err("no model given".into());
    }
    let url = if let Some(rest) = reference.strip_prefix("hf:").or_else(|| reference.strip_prefix("hf://")) {
        Some(hf_url(rest).ok_or_else(|| format!("expected hf:owner/repo/file.gguf, got {reference:?}"))?)
    } else if reference.starts_with("https://") || reference.starts_with("http://") {
        // A Hugging Face page link (`/blob/`) downloads from `/resolve/`.
        let hf = reference.starts_with("https://huggingface.co/") || reference.starts_with("https://hf.co/");
        Some(if hf { reference.replacen("/blob/", "/resolve/", 1) } else { reference.to_string() })
    } else {
        None
    };
    if let Some(url) = url {
        let file = url_file_name(&url).ok_or_else(|| format!("can't tell the file name from {url:?}"))?;
        let urls = match shard(&file) {
            Some((base, 1, count)) if count > 1 => {
                let prefix = &url[..url.rfind(&file).unwrap_or(url.len())];
                (1..=count).map(|i| format!("{prefix}{base}-{i:05}-of-{count:05}.gguf")).collect()
            }
            _ => vec![url],
        };
        return Ok(Reference::Remote { urls, file });
    }
    let path = PathBuf::from(reference);
    if path.is_absolute() || reference.contains(['/', '\\']) {
        return if path.is_file() { Ok(Reference::Local(path)) } else { Err(format!("model file not found: {reference}")) };
    }
    let path = paths.models_dir().join(reference);
    if path.is_file() { Ok(Reference::Local(path)) } else { Err(format!("{reference} is not in {}", paths.models_dir().display())) }
}

/// Makes sure a model is on disk; downloads what's missing (as a `download:<file>` task with
/// progress and cancellation). Returns the path of the file to load.
pub async fn fetch(paths: &Paths, tasks: &Tasks, reference: Reference) -> Result<PathBuf, String> {
    let (urls, file) = match reference {
        Reference::Local(path) => return Ok(path),
        Reference::Remote { urls, file } => (urls, file),
    };
    let dir = paths.models_dir();
    tokio::fs::create_dir_all(&dir).await.map_err(err_str)?;
    let target = dir.join(&file);
    let missing: Vec<(String, PathBuf)> = urls
        .into_iter()
        .filter_map(|url| {
            let name = url_file_name(&url)?;
            let path = dir.join(name);
            (!path.is_file()).then_some((url, path))
        })
        .collect();
    if missing.is_empty() {
        return Ok(target);
    }

    let mut progress = tasks.start(&format!("download:{file}"), "download", file.clone())?;
    let client = http_client()?;
    let mut received = 0u64;
    let mut total: Option<u64> = None;
    let mut done = Vec::new();
    let result: Result<(), String> = async {
        for (url, path) in &missing {
            let resp = client
                .get(url)
                .send()
                .await
                .map_err(err_str)?
                .error_for_status()
                .map_err(|e| format!("failed to download {url}: {e}"))?;
            if let Some(len) = resp.content_length() {
                total = Some(total.unwrap_or(0) + len);
            }
            let part = path.with_file_name(format!("{}{PART_SUFFIX}", path.file_name().unwrap_or_default().to_string_lossy()));
            let written = stream_to_file(resp, &part, &mut progress, received, total).await;
            let written = match written {
                Ok(written) => written,
                Err(error) => {
                    remove_file_quiet(&part);
                    return Err(error);
                }
            };
            received += written;
            tokio::fs::rename(&part, path).await.map_err(err_str)?;
            done.push(path.clone());
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            progress.finish(None);
            Ok(target)
        }
        Err(error) => {
            // A split model is only usable whole.
            for path in &done {
                remove_file_quiet(path);
            }
            progress.finish(Some(error.clone()));
            Err(error)
        }
    }
}

/// Everything `inspect` tells about a model file.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub id: String,
    pub path: String,
    pub size: u64,
    pub context_length: Option<u64>,
    pub capabilities: Capabilities,
    pub sampling: Sampling,
    /// The architecture numbers clients need to estimate memory use.
    pub info: ModelInfo,
}

/// Reads model headers, cached by path + size + modification time.
#[derive(Default)]
pub struct MetaCache(Mutex<HashMap<PathBuf, ((u64, u64), Inspection)>>);

fn stamp(path: &Path) -> (u64, u64) {
    let meta = std::fs::metadata(path).ok();
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64);
    (meta.map_or(0, |m| m.len()), modified)
}

impl MetaCache {
    /// Reads the GGUF header (blocking; call off the async threads).
    pub fn inspect(&self, path: &Path) -> Result<Inspection, String> {
        let stamp = stamp(path);
        if let Some((seen, inspection)) = self.0.lock().unwrap_or_else(|e| e.into_inner()).get(path)
            && *seen == stamp
        {
            return Ok(inspection.clone());
        }
        let meta = gguf::read_meta(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let inspection = Inspection {
            id: model_id(&path.to_string_lossy()),
            path: path.to_string_lossy().into_owned(),
            size: model_size(path),
            context_length: meta.info.context_length,
            capabilities: caps::from_template(meta.chat_template.as_deref()),
            sampling: meta.sampling,
            info: meta.info,
        };
        self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(path.to_path_buf(), (stamp, inspection.clone()));
        Ok(inspection)
    }

    pub fn file_meta(&self, path: &Path) -> FileMeta {
        match self.inspect(path) {
            Ok(i) => FileMeta { context_length: i.context_length, template_caps: i.capabilities, sampling: i.sampling },
            Err(_) => FileMeta::default(),
        }
    }
}

/// A model file in the models folder (the first part for a split model).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModelFile {
    pub file: String,
    pub id: String,
    pub size: u64,
    pub modified: u64,
    pub context_length: Option<u64>,
    pub capabilities: Capabilities,
    pub sampling: Option<Sampling>,
}

/// The models folder: split models as one entry, without projectors and partial downloads.
pub fn list(paths: &Paths, cache: &MetaCache) -> Vec<ModelFile> {
    let dir = paths.models_dir();
    let mut files: Vec<(String, u64)> = Vec::new();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let Ok(name) = entry.file_name().into_string() else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || name.starts_with('.') || !name.to_ascii_lowercase().ends_with(".gguf") || is_projector(&name) {
            continue;
        }
        if shard(&name).is_some_and(|(_, index, _)| index != 1) {
            continue;
        }
        let modified = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
        files.push((name, modified));
    }
    files.sort_by_key(|file| file.0.to_lowercase());
    files
        .into_iter()
        .map(|(file, modified)| {
            let path = dir.join(&file);
            let inspection = cache.inspect(&path).ok();
            ModelFile {
                id: model_id(&file),
                size: model_size(&path),
                modified,
                context_length: inspection.as_ref().and_then(|i| i.context_length),
                capabilities: inspection.as_ref().map(|i| i.capabilities.clone()).unwrap_or_default(),
                sampling: inspection.map(|i| i.sampling),
                file,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shards() {
        assert_eq!(shard("big-00001-of-00003.gguf"), Some(("big", 1, 3)));
        assert_eq!(shard("big-00003-of-00003.gguf"), Some(("big", 3, 3)));
        assert_eq!(shard("plain.gguf"), None);
        assert_eq!(shard("x-1-of-2.gguf"), None);
    }

    #[test]
    fn references() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        std::fs::create_dir_all(paths.models_dir()).unwrap();
        std::fs::write(paths.models_dir().join("a.gguf"), b"x").unwrap();

        assert_eq!(resolve(&paths, "a.gguf").unwrap(), Reference::Local(paths.models_dir().join("a.gguf")));
        assert!(resolve(&paths, "missing.gguf").is_err());
        assert!(resolve(&paths, "").is_err());

        let blob = resolve(&paths, "https://huggingface.co/o/r/blob/main/sub/m-Q4.gguf?download=true").unwrap();
        assert_eq!(
            blob,
            Reference::Remote { urls: vec!["https://huggingface.co/o/r/resolve/main/sub/m-Q4.gguf?download=true".into()], file: "m-Q4.gguf".into() }
        );
        let Reference::Remote { urls, file } = resolve(&paths, "hf:o/r/big-00001-of-00002.gguf").unwrap() else { panic!() };
        assert_eq!(file, "big-00001-of-00002.gguf");
        assert_eq!(
            urls,
            [
                "https://huggingface.co/o/r/resolve/main/big-00001-of-00002.gguf",
                "https://huggingface.co/o/r/resolve/main/big-00002-of-00002.gguf"
            ]
        );
        assert!(resolve(&paths, "hf:o/r").is_err());
    }
}
