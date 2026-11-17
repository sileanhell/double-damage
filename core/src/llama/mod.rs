//! Local models through llama.cpp's `llama-server`: installing it, model files, and the running
//! processes.

pub mod files;
pub mod install;
pub mod runtime;

pub use runtime::{InstanceInfo, LaunchSpec, Llama, PROVIDER, State, model_id};
