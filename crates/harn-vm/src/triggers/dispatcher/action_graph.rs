use std::collections::BTreeMap;
use std::time::Duration;

use crate::orchestration::{
    append_action_graph_update, RunActionGraphEdgeRecord, RunActionGraphNodeRecord,
    RunObservabilityRecord, ACTION_GRAPH_EDGE_KIND_A2A_DISPATCH,
    ACTION_GRAPH_EDGE_KIND_PREDICATE_GATE, ACTION_GRAPH_EDGE_KIND_TRIGGER_DISPATCH,
    ACTION_GRAPH_NODE_KIND_A2A_HOP, ACTION_GRAPH_NODE_KIND_DISPATCH,
    ACTION_GRAPH_NODE_KIND_WORKER_ENQUEUE,
};
use crate::triggers::registry::TriggerBinding;

use super::types::{DispatchError, Dispatcher};
use super::uri::DispatchUri;
use super::TriggerEvent;

impl Dispatcher {
    pub(super) async fn emit_action_graph(
        &self,
        event: &TriggerEvent,
        nodes: Vec<RunActionGraphNodeRecord>,
        edges: Vec<RunActionGraphEdgeRecord>,
        extra: serde_json::Value,
    ) -> Result<(), DispatchError> {
        let mut headers = BTreeMap::new();
        headers.insert("trace_id".to_string(), event.trace_id.0.clone());
        headers.insert("event_id".to_string(), event.id.0.clone());
        let observability = RunObservabilityRecord {
            schema_version: 1,
            action_graph_nodes: nodes,
            action_graph_edges: edges,
            ..Default::default()
        };
        append_action_graph_update(
            headers,
            serde_json::json!({
                "source": "dispatcher",
                "trace_id": event.trace_id.0,
                "event_id": event.id.0,
                "observability": observability,
                "context": extra,
            }),
        )
        .await
        .map_err(DispatchError::from)
    }
}

pub(super) fn dispatch_error_label(error: &DispatchError) -> &'static str {
    match error {
        DispatchError::Denied(_) => "denied",
        DispatchError::Timeout(_) => "timeout",
        DispatchError::Waiting(_) => "waiting",
        DispatchError::Cancelled(_) => "cancelled",
        _ => "failed",
    }
}

pub(super) fn dispatch_success_outcome(
    route: &DispatchUri,
    result: &serde_json::Value,
) -> &'static str {
    match route {
        DispatchUri::Worker { .. } => "enqueued",
        DispatchUri::Persona { .. } => "recorded",
        DispatchUri::A2a { .. }
            if result.get("kind").and_then(|value| value.as_str()) == Some("a2a_task_handle") =>
        {
            "pending"
        }
        DispatchUri::A2a { .. } => "completed",
        DispatchUri::Local { .. } => "success",
        DispatchUri::AutoResume { .. } => "resumed",
    }
}

pub(super) fn dispatch_node_id(
    route: &DispatchUri,
    binding_key: &str,
    event_id: &str,
    attempt: u32,
) -> String {
    let prefix = match route {
        DispatchUri::A2a { .. } => "a2a",
        _ => "dispatch",
    };
    format!("{prefix}:{binding_key}:{event_id}:{attempt}")
}

pub(super) fn dispatch_node_kind(route: &DispatchUri) -> &'static str {
    match route {
        DispatchUri::A2a { .. } => ACTION_GRAPH_NODE_KIND_A2A_HOP,
        DispatchUri::Worker { .. } => ACTION_GRAPH_NODE_KIND_WORKER_ENQUEUE,
        _ => ACTION_GRAPH_NODE_KIND_DISPATCH,
    }
}

pub(super) fn dispatch_node_label(route: &DispatchUri) -> String {
    match route {
        DispatchUri::A2a { target, .. } => crate::a2a::target_agent_label(target),
        _ => route.target_uri(),
    }
}

pub(super) fn dispatch_target_agent(route: &DispatchUri) -> Option<String> {
    match route {
        DispatchUri::A2a { target, .. } => Some(crate::a2a::target_agent_label(target)),
        _ => None,
    }
}

pub(super) fn dispatch_entry_edge_kind(route: &DispatchUri, has_predicate: bool) -> &'static str {
    match route {
        DispatchUri::A2a { .. } => ACTION_GRAPH_EDGE_KIND_A2A_DISPATCH,
        _ if has_predicate => ACTION_GRAPH_EDGE_KIND_PREDICATE_GATE,
        _ => ACTION_GRAPH_EDGE_KIND_TRIGGER_DISPATCH,
    }
}

pub(super) fn signature_status_label(status: &crate::triggers::SignatureStatus) -> &'static str {
    match status {
        crate::triggers::SignatureStatus::Verified => "verified",
        crate::triggers::SignatureStatus::Unsigned => "unsigned",
        crate::triggers::SignatureStatus::Failed { .. } => "failed",
    }
}

pub(super) fn trigger_node_metadata(event: &TriggerEvent) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "provider".to_string(),
        serde_json::json!(event.provider.as_str()),
    );
    metadata.insert("event_kind".to_string(), serde_json::json!(event.kind));
    metadata.insert(
        "dedupe_key".to_string(),
        serde_json::json!(event.dedupe_key),
    );
    metadata.insert(
        "signature_status".to_string(),
        serde_json::json!(signature_status_label(&event.signature_status)),
    );
    metadata
}

pub(super) fn trigger_event_persona_metadata(event: &TriggerEvent) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    metadata.insert("trigger_event_id".to_string(), event.id.0.clone());
    metadata.insert("event_id".to_string(), event.id.0.clone());
    metadata.insert("dedupe_key".to_string(), event.dedupe_key.clone());
    metadata.insert("trace_id".to_string(), event.trace_id.0.clone());
    if let Some(tenant_id) = &event.tenant_id {
        metadata.insert("tenant_id".to_string(), tenant_id.0.clone());
    }
    for (key, value) in &event.headers {
        metadata.insert(format!("header.{key}"), value.clone());
    }
    if let Ok(payload) = serde_json::to_value(&event.provider_payload) {
        collect_persona_payload_metadata("", &payload, &mut metadata);
        if let Some(raw) = payload.get("raw") {
            collect_persona_payload_metadata("", raw, &mut metadata);
        }
    }
    metadata
}

fn collect_persona_payload_metadata(
    prefix: &str,
    value: &serde_json::Value,
    metadata: &mut BTreeMap<String, String>,
) {
    let serde_json::Value::Object(object) = value else {
        return;
    };
    for (key, value) in object {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            serde_json::Value::String(text) => {
                metadata.insert(path, text.clone());
            }
            serde_json::Value::Number(number) => {
                metadata.insert(path, number.to_string());
            }
            serde_json::Value::Bool(flag) => {
                metadata.insert(path, flag.to_string());
            }
            serde_json::Value::Object(_) if prefix.is_empty() => {
                collect_persona_payload_metadata(&path, value, metadata);
            }
            _ => {}
        }
    }
}

pub(super) fn dispatch_node_metadata(
    route: &DispatchUri,
    binding: &TriggerBinding,
    event: &TriggerEvent,
    attempt: u32,
) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = BTreeMap::new();
    metadata.insert("handler_kind".to_string(), serde_json::json!(route.kind()));
    metadata.insert(
        "target_uri".to_string(),
        serde_json::json!(route.target_uri()),
    );
    metadata.extend(route.dispatch_boundary_metadata());
    metadata.insert("attempt".to_string(), serde_json::json!(attempt));
    metadata.insert(
        "trigger_id".to_string(),
        serde_json::json!(binding.id.as_str()),
    );
    metadata.insert("event_id".to_string(), serde_json::json!(event.id.0));
    if let Some(target_agent) = dispatch_target_agent(route) {
        metadata.insert("target_agent".to_string(), serde_json::json!(target_agent));
    }
    if let DispatchUri::Worker { queue } = route {
        metadata.insert("queue_name".to_string(), serde_json::json!(queue));
    }
    if let DispatchUri::Persona { name } = route {
        metadata.insert("persona".to_string(), serde_json::json!(name));
    }
    metadata
}

pub(super) fn dispatch_success_metadata(
    route: &DispatchUri,
    binding: &TriggerBinding,
    event: &TriggerEvent,
    attempt: u32,
    result: &serde_json::Value,
) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = dispatch_node_metadata(route, binding, event, attempt);
    match route {
        DispatchUri::A2a { .. } => {
            if let Some(task_id) = result
                .get("task_id")
                .or_else(|| result.get("id"))
                .and_then(|value| value.as_str())
            {
                metadata.insert("task_id".to_string(), serde_json::json!(task_id));
            }
            if let Some(state) = result.get("state").and_then(|value| value.as_str()) {
                metadata.insert("state".to_string(), serde_json::json!(state));
            }
        }
        DispatchUri::Worker { .. } => {
            if let Some(job_event_id) = result.get("job_event_id").and_then(|value| value.as_u64())
            {
                metadata.insert("job_event_id".to_string(), serde_json::json!(job_event_id));
            }
            if let Some(response_topic) = result
                .get("response_topic")
                .and_then(|value| value.as_str())
            {
                metadata.insert(
                    "response_topic".to_string(),
                    serde_json::json!(response_topic),
                );
            }
        }
        DispatchUri::Persona { .. } => {
            if let Some(receipt_id) = result.get("receipt_id").and_then(|value| value.as_str()) {
                metadata.insert("receipt_id".to_string(), serde_json::json!(receipt_id));
            }
            if let Some(status) = result.get("status").and_then(|value| value.as_str()) {
                metadata.insert("status".to_string(), serde_json::json!(status));
            }
        }
        DispatchUri::Local { .. } | DispatchUri::AutoResume { .. } => {}
    }
    metadata
}

pub(super) fn dispatch_error_metadata(
    route: &DispatchUri,
    binding: &TriggerBinding,
    event: &TriggerEvent,
    attempt: u32,
    error: &DispatchError,
) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = dispatch_node_metadata(route, binding, event, attempt);
    metadata.insert("error".to_string(), serde_json::json!(error.to_string()));
    metadata
}

pub(super) fn retry_node_metadata(
    binding: &TriggerBinding,
    event: &TriggerEvent,
    attempt: u32,
    delay: Duration,
    error: &DispatchError,
) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "trigger_id".to_string(),
        serde_json::json!(binding.id.as_str()),
    );
    metadata.insert("event_id".to_string(), serde_json::json!(event.id.0));
    metadata.insert("attempt".to_string(), serde_json::json!(attempt));
    metadata.insert("delay_ms".to_string(), serde_json::json!(delay.as_millis()));
    metadata.insert("error".to_string(), serde_json::json!(error.to_string()));
    metadata
}
