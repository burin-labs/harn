use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::event_log::EventLog;
use crate::value::VmError;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct DaemonSnapshot {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub saved_at: String,
    pub daemon_state: String,
    pub visible_messages: Vec<serde_json::Value>,
    pub recorded_messages: Vec<serde_json::Value>,
    pub transcript_summary: Option<String>,
    pub transcript_events: Vec<serde_json::Value>,
    pub total_text: String,
    pub last_iteration_text: String,
    pub all_tools_used: Vec<String>,
    pub rejected_tools: Vec<String>,
    pub deferred_user_messages: Vec<String>,
    pub total_iterations: usize,
    pub idle_backoff_ms: u64,
    pub last_run_exit_code: Option<i32>,
    pub watch_state: BTreeMap<String, u64>,
}

impl DaemonSnapshot {
    pub(crate) fn normalize(mut self) -> Self {
        if self.type_name.is_empty() {
            self.type_name = "daemon_snapshot".to_string();
        }
        if self.saved_at.is_empty() {
            self.saved_at = crate::orchestration::now_rfc3339();
        }
        if self.daemon_state.is_empty() {
            self.daemon_state = "active".to_string();
        }
        self
    }
}

pub(crate) fn load_snapshot(path: &str) -> Result<DaemonSnapshot, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| VmError::Runtime(format!("daemon snapshot read error: {error}")))?;
    let snapshot: DaemonSnapshot = serde_json::from_str(&content)
        .map_err(|error| VmError::Runtime(format!("daemon snapshot parse error: {error}")))?;
    let snapshot = snapshot.normalize();
    append_daemon_state_event(path, "snapshot_loaded", &snapshot);
    Ok(snapshot)
}

fn append_daemon_state_event(path: &str, kind: &str, snapshot: &DaemonSnapshot) {
    let Some(log) = crate::event_log::active_event_log() else {
        return;
    };
    let stem = Path::new(path)
        .file_stem()
        .and_then(|value| value.to_str())
        .map(crate::event_log::sanitize_topic_component)
        .unwrap_or_else(|| "snapshot".to_string());
    let Ok(topic) = crate::event_log::Topic::new(format!("daemon.{stem}.state")) else {
        return;
    };
    let mut headers = BTreeMap::new();
    headers.insert("path".to_string(), path.to_string());
    let payload = serde_json::to_value(snapshot).unwrap_or(serde_json::Value::Null);
    let event = crate::event_log::LogEvent::new(kind, payload).with_headers(headers);
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let _ = log.append(&topic, event).await;
        });
    } else {
        let _ = futures::executor::block_on(log.append(&topic, event));
    }
}
