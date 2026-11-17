//! Change notifications: everything a client may want to redraw on. Served as Server-Sent Events
//! on `GET /dd/events` (`event: <type>`, `data: <json>`).

use serde::Serialize;
use tokio::sync::broadcast;

use crate::llama::InstanceInfo;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase", tag = "type", content = "data")]
pub enum Event {
    /// The full list of running local models, after any change (state, activity, load, unload).
    Instances(Vec<InstanceInfo>),
    /// A provider's sign-in state or model list changed; ask `/dd/providers/{id}`.
    Provider { id: String },
    /// Progress of a long task (llama-server install, model download).
    Task(TaskProgress),
    /// The local API token was replaced.
    Token,
}

impl Event {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Instances(_) => "instances",
            Self::Provider { .. } => "provider",
            Self::Task(_) => "task",
            Self::Token => "token",
        }
    }

    /// Just the payload, as the SSE `data:` line carries it.
    pub fn data(&self) -> serde_json::Value {
        match self {
            Self::Instances(list) => serde_json::to_value(list).unwrap_or_default(),
            Self::Provider { id } => serde_json::json!({ "id": id }),
            Self::Task(task) => serde_json::to_value(task).unwrap_or_default(),
            Self::Token => serde_json::json!({}),
        }
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgress {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub received: u64,
    pub total: Option<u64>,
    /// Set on the last event of a task.
    pub done: bool,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct Events(broadcast::Sender<Event>);

impl Default for Events {
    fn default() -> Self {
        Self(broadcast::channel(256).0)
    }
}

impl Events {
    pub fn send(&self, event: Event) {
        let _ = self.0.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.0.subscribe()
    }
}
