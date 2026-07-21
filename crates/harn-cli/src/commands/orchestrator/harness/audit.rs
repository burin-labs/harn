//! State-snapshot serialization and event-log lifecycle helpers.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value as JsonValue;
use tokio::sync::watch;

use harn_vm::event_log::{AnyEventLog, EventLog};

use super::super::errors::OrchestratorError;
use super::{LIFECYCLE_TOPIC, MANIFEST_TOPIC};

pub(super) fn validate_mcp_paths(
    path: &str,
    sse_path: &str,
    messages_path: &str,
) -> Result<(), OrchestratorError> {
    let reserved = [
        "/health",
        "/healthz",
        "/readyz",
        "/metrics",
        "/admin/reload",
        "/acp",
    ];
    let mut seen = BTreeSet::new();
    for (label, value) in [
        ("--mcp-path", path),
        ("--mcp-sse-path", sse_path),
        ("--mcp-messages-path", messages_path),
    ] {
        if !value.starts_with('/') {
            return Err(format!("{label} must start with '/'").into());
        }
        if value == "/" {
            return Err(format!("{label} cannot be '/'").into());
        }
        if reserved.contains(&value) {
            return Err(format!("{label} cannot use reserved listener path '{value}'").into());
        }
        if !seen.insert(value) {
            return Err(format!("embedded MCP paths must be unique; duplicate '{value}'").into());
        }
    }
    Ok(())
}

pub(super) async fn append_lifecycle_event(
    log: &Arc<AnyEventLog>,
    kind: &str,
    payload: JsonValue,
) -> Result<(), OrchestratorError> {
    let topic =
        harn_vm::event_log::Topic::new(LIFECYCLE_TOPIC).map_err(|error| error.to_string())?;
    log.append(&topic, harn_vm::event_log::LogEvent::new(kind, payload))
        .await
        .map(|_| ())
        .map_err(|error| {
            OrchestratorError::Serve(format!(
                "failed to append orchestrator lifecycle event: {error}"
            ))
        })
}

pub(super) async fn append_pump_lifecycle_event(
    log: &Arc<AnyEventLog>,
    kind: &str,
    payload: JsonValue,
) -> Result<(), OrchestratorError> {
    append_lifecycle_event(log, kind, payload).await
}

pub(super) async fn wait_for_pump_drain_release(
    log: &Arc<AnyEventLog>,
    topic: &harn_vm::event_log::Topic,
    event_id: u64,
    gate_rx: &mut watch::Receiver<bool>,
) -> Result<(), OrchestratorError> {
    if !*gate_rx.borrow() {
        return Ok(());
    }
    append_pump_lifecycle_event(
        log,
        "pump_drain_waiting",
        serde_json::json!({
            "topic": topic.as_str(),
            "event_log_id": event_id,
        }),
    )
    .await?;
    while *gate_rx.borrow() {
        if gate_rx.changed().await.is_err() {
            break;
        }
    }
    Ok(())
}

#[cfg_attr(not(unix), allow(dead_code))]
pub(super) async fn append_manifest_event(
    log: &Arc<AnyEventLog>,
    kind: &str,
    payload: JsonValue,
) -> Result<(), OrchestratorError> {
    let topic =
        harn_vm::event_log::Topic::new(MANIFEST_TOPIC).map_err(|error| error.to_string())?;
    log.append(&topic, harn_vm::event_log::LogEvent::new(kind, payload))
        .await
        .map(|_| ())
        .map_err(|error| {
            OrchestratorError::Serve(format!(
                "failed to append orchestrator manifest event: {error}"
            ))
        })
}

pub(super) async fn record_pump_metrics(
    metrics: &harn_vm::MetricsRegistry,
    log: &Arc<AnyEventLog>,
    topic: &harn_vm::event_log::Topic,
    last_seen: u64,
    outstanding: usize,
) -> Result<(), OrchestratorError> {
    let latest = log.latest(topic).await.ok().flatten().unwrap_or(last_seen);
    let backlog = latest.saturating_sub(last_seen);
    metrics.set_orchestrator_pump_outstanding(topic.as_str(), outstanding);
    metrics.set_orchestrator_pump_backlog(topic.as_str(), backlog);
    append_pump_lifecycle_event(
        log,
        "pump_metrics_recorded",
        serde_json::json!({
            "topic": topic.as_str(),
            "latest": latest,
            "last_seen": last_seen,
            "backlog": backlog,
            "outstanding": outstanding,
        }),
    )
    .await
}

pub(super) fn admission_delay(occurred_at_ms: i64) -> Duration {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    Duration::from_millis(now.saturating_sub(occurred_at_ms).max(0) as u64)
}

/// Current UTC time as RFC3339. Infallible; see
/// `harn_vm::clock::format_rfc3339` for why the old `Result` was vestigial.
pub(super) fn now_rfc3339() -> String {
    harn_vm::clock::system_now_rfc3339()
}

pub(super) fn write_state_snapshot(
    path: &Path,
    snapshot: &ServeStateSnapshot,
) -> Result<(), OrchestratorError> {
    let encoded = serde_json::to_vec_pretty(snapshot)
        .map_err(|error| format!("failed to encode orchestrator state snapshot: {error}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    std::fs::write(path, encoded).map_err(|error| {
        OrchestratorError::Serve(format!("failed to write {}: {error}", path.display()))
    })
}

// ── Snapshot types ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub(super) struct ServeStateSnapshot {
    pub(super) status: String,
    pub(super) role: String,
    pub(super) bind: String,
    pub(super) listener_url: String,
    pub(super) manifest_path: String,
    pub(super) state_dir: String,
    pub(super) started_at: String,
    pub(super) stopped_at: Option<String>,
    pub(super) secret_provider_chain: String,
    pub(super) event_log_backend: String,
    pub(super) event_log_location: Option<String>,
    pub(super) triggers: Vec<TriggerStateSnapshot>,
    pub(super) connectors: Vec<String>,
    pub(super) activations: Vec<ConnectorActivationSnapshot>,
}

#[derive(Debug, Serialize)]
pub(super) struct TriggerStateSnapshot {
    pub(super) id: String,
    pub(super) provider: String,
    pub(super) kind: String,
    pub(super) handler: String,
    pub(super) version: Option<u32>,
    pub(super) state: Option<String>,
    pub(super) received: u64,
    pub(super) dispatched: u64,
    pub(super) failed: u64,
    pub(super) in_flight: u64,
}

#[derive(Debug, Serialize)]
pub(super) struct ConnectorActivationSnapshot {
    pub(super) provider: String,
    pub(super) binding_count: usize,
}
