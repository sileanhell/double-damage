//! The catalog: every provider with the models it can answer with right now. Local models are
//! listed once they are loaded (or loading / failed); files that aren't loaded are on
//! `GET /dd/llama/models` instead.

use serde::Serialize;

use crate::{
    caps::{Capabilities, Reasoning},
    gguf::Sampling,
    llama::{self, Llama},
    providers::{
        claude::{self, Claude},
        devin::{self, Devin},
    },
};

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
    /// Local models: the file that runs (the first part of a split model) and the whole size.
    pub file: Option<String>,
    pub size: Option<u64>,
    /// The most context the model was trained for.
    pub context_length: Option<u64>,
    /// Context of the running instance.
    pub loaded_context: Option<u32>,
    pub capabilities: Capabilities,
    /// Author-recommended sampling from the file (local models).
    pub sampling: Option<Sampling>,
    /// Longest reply the model allows (remote models).
    pub max_output: Option<u64>,
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

/// What remote chat models can do: tools, and reasoning steered the provider's way.
pub fn remote_caps(provider: &str) -> Capabilities {
    if provider == claude::PROVIDER {
        Capabilities {
            tools: true,
            reasoning: Reasoning::Effort,
            effort_levels: ["low", "medium", "high"].iter().map(|l| l.to_string()).collect(),
            confirmed: true,
        }
    } else {
        Capabilities { tools: true, reasoning: Reasoning::Always, effort_levels: Vec::new(), confirmed: true }
    }
}

pub fn build(llama: &Llama, llama_installed: bool, devin: &Devin, claude: &Claude) -> Catalog {
    let mut providers = vec![llama_entry(llama, llama_installed)];
    if devin.creds().is_some() {
        providers.push(ProviderEntry {
            id: devin::PROVIDER,
            name: devin::PROVIDER_NAME,
            status: ProviderStatus::Ready,
            models: devin
                .models()
                .into_iter()
                .map(|model| ModelEntry {
                    id: format!("{}/{}", devin::PROVIDER, model.uid),
                    name: model.label,
                    provider: devin::PROVIDER,
                    state: ModelState::Loaded,
                    file: None,
                    size: None,
                    context_length: model.context,
                    loaded_context: model.context.map(|c| c as u32),
                    capabilities: remote_caps(devin::PROVIDER),
                    sampling: None,
                    max_output: model.max_output,
                    error: None,
                })
                .collect(),
        });
    }
    if claude.creds().is_some() {
        providers.push(ProviderEntry {
            id: claude::PROVIDER,
            name: claude::PROVIDER_NAME,
            status: ProviderStatus::Ready,
            models: claude
                .models()
                .into_iter()
                .map(|model| ModelEntry {
                    id: format!("{}/{}", claude::PROVIDER, model.id),
                    name: model.label,
                    provider: claude::PROVIDER,
                    state: ModelState::Loaded,
                    file: None,
                    size: None,
                    context_length: model.context,
                    loaded_context: model.context.map(|c| c as u32),
                    capabilities: remote_caps(claude::PROVIDER),
                    sampling: None,
                    max_output: model.max_output,
                    error: None,
                })
                .collect(),
        });
    }
    Catalog { providers }
}

fn llama_entry(llama: &Llama, installed: bool) -> ProviderEntry {
    let models = llama
        .list()
        .into_iter()
        .map(|instance| {
            let (state, error) = match &instance.state {
                llama::State::Loading => (ModelState::Loading, None),
                llama::State::Ready => (ModelState::Loaded, None),
                llama::State::Failed { error } => (ModelState::Failed, Some(error.clone())),
            };
            ModelEntry {
                name: instance.id.strip_prefix(&format!("{}/", llama::PROVIDER)).unwrap_or(&instance.id).to_string(),
                id: instance.id,
                provider: llama::PROVIDER,
                state,
                file: Some(instance.spec.file),
                size: Some(instance.size),
                context_length: instance.context_length,
                loaded_context: instance.loaded_context,
                capabilities: instance.capabilities,
                sampling: Some(instance.sampling),
                max_output: None,
                error,
            }
        })
        .collect();
    ProviderEntry {
        id: llama::PROVIDER,
        name: "llama-server",
        status: if installed { ProviderStatus::Ready } else { ProviderStatus::NotInstalled },
        models,
    }
}
