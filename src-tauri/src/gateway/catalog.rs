//! The catalog: every provider with every model it offers, including local models that aren't
//! loaded yet. The app builds its model picker from it; `/dd/catalog` serves the same thing.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};

use serde::Serialize;

use super::{
    caps::{self, Capabilities},
    llama::{self, Llama},
};
use crate::gguf::Sampling;

/// Where things are on disk; asked for on every use, since the data folder can move.
#[derive(Clone)]
pub struct Paths {
    pub models_dir: PathBuf,
    pub llama_exe: Option<PathBuf>,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderStatus {
    Ready,
    NotInstalled,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ModelState {
    Unloaded,
    Loading,
    Loaded,
    Failed,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    /// Gateway model id, `provider/name`.
    pub id: String,
    pub name: String,
    pub provider: &'static str,
    pub state: ModelState,
    /// Local models: the file to load (the first part of a split model) and the whole size.
    pub file: Option<String>,
    pub size: Option<u64>,
    /// The most context the model was trained for.
    pub context_length: Option<u64>,
    /// Context of the running instance.
    pub loaded_context: Option<u32>,
    pub capabilities: Capabilities,
    /// Author-recommended sampling from the file (local models).
    pub sampling: Option<Sampling>,
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub status: ProviderStatus,
    pub models: Vec<ModelEntry>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Catalog {
    pub providers: Vec<ProviderEntry>,
}

/// What a model file says about itself, cached by size + modification time.
#[derive(Clone)]
struct FileMeta {
    stamp: (u64, u64),
    context_length: Option<u64>,
    caps: Capabilities,
    sampling: Sampling,
}

pub struct Source {
    paths: Box<dyn Fn() -> Paths + Send + Sync>,
    llama: Llama,
    cache: Mutex<HashMap<PathBuf, FileMeta>>,
}

const SHARD_MARK: &str = "-of-";

/// `name-00002-of-00003.gguf` -> Some((`name`, 2, 3)).
fn shard(name: &str) -> Option<(&str, u32, u32)> {
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

impl Source {
    pub fn new(llama: Llama, paths: impl Fn() -> Paths + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { paths: Box::new(paths), llama, cache: Default::default() })
    }

    pub fn paths(&self) -> Paths {
        (self.paths)()
    }

    fn file_meta(&self, path: &Path, size: u64, modified: u64) -> FileMeta {
        let stamp = (size, modified);
        if let Some(meta) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).get(path)
            && meta.stamp == stamp
        {
            return meta.clone();
        }
        let meta = match crate::gguf::read_meta(path) {
            Ok(meta) => FileMeta {
                stamp,
                context_length: meta.info.context_length,
                caps: caps::from_template(meta.chat_template.as_deref()),
                sampling: meta.sampling,
            },
            Err(_) => FileMeta { stamp, context_length: None, caps: Capabilities::default(), sampling: Sampling::default() },
        };
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).insert(path.to_path_buf(), meta.clone());
        meta
    }

    /// Builds the catalog (reads model file headers; call off the async threads).
    pub fn build(&self) -> Catalog {
        Catalog { providers: vec![self.llama_entry()] }
    }

    fn llama_entry(&self) -> ProviderEntry {
        let paths = self.paths();
        let running: HashMap<String, llama::InstanceInfo> =
            self.llama.list().into_iter().map(|i| (i.id.clone(), i)).collect();

        // Group split models into one entry (sizes add up), skip projectors and partial downloads.
        let mut files: Vec<(String, u64, u64)> = Vec::new();
        let mut shard_sizes: HashMap<(String, u32), u64> = HashMap::new();
        for entry in std::fs::read_dir(&paths.models_dir).into_iter().flatten().flatten() {
            let Ok(name) = entry.file_name().into_string() else { continue };
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() || name.starts_with('.') || !name.to_ascii_lowercase().ends_with(".gguf") || is_projector(&name) {
                continue;
            }
            let modified = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
            match shard(&name) {
                Some((base, index, count)) => {
                    *shard_sizes.entry((base.to_string(), count)).or_default() += meta.len();
                    if index == 1 {
                        files.push((name, 0, modified));
                    }
                }
                None => files.push((name, meta.len(), modified)),
            }
        }
        files.sort_by_key(|file| file.0.to_lowercase());

        let mut models = Vec::new();
        for (file, size, modified) in files {
            let size = match shard(&file) {
                Some((base, _, count)) => shard_sizes.get(&(base.to_string(), count)).copied().unwrap_or(0),
                None => size,
            };
            let meta = self.file_meta(&paths.models_dir.join(&file), size, modified);
            let id = llama::model_id(&file);
            let instance = running.get(&id);
            let (state, error) = match instance.map(|i| &i.state) {
                None => (ModelState::Unloaded, None),
                Some(llama::State::Loading) => (ModelState::Loading, None),
                Some(llama::State::Ready) => (ModelState::Loaded, None),
                Some(llama::State::Failed { error }) => (ModelState::Failed, Some(error.clone())),
            };
            let capabilities = match instance.and_then(|i| i.caps.as_ref()) {
                Some(runtime) => caps::with_runtime(meta.caps.clone(), Some(runtime)),
                None => meta.caps.clone(),
            };
            models.push(ModelEntry {
                name: id.strip_prefix(&format!("{}/", llama::PROVIDER)).unwrap_or(&id).to_string(),
                id,
                provider: llama::PROVIDER,
                state,
                file: Some(file),
                size: Some(size),
                context_length: meta.context_length,
                loaded_context: instance.map(|i| i.spec.ctx),
                capabilities,
                sampling: Some(meta.sampling),
                error,
            });
        }

        ProviderEntry {
            id: llama::PROVIDER,
            name: "llama-server",
            status: if paths.llama_exe.is_some() { ProviderStatus::Ready } else { ProviderStatus::NotInstalled },
            models,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::shard;

    #[test]
    fn shards() {
        assert_eq!(shard("big-00001-of-00003.gguf"), Some(("big", 1, 3)));
        assert_eq!(shard("big-00003-of-00003.gguf"), Some(("big", 3, 3)));
        assert_eq!(shard("plain.gguf"), None);
        assert_eq!(shard("x-1-of-2.gguf"), None);
    }
}
