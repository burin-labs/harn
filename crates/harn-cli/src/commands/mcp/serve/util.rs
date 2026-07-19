use std::collections::BTreeMap;

use harn_vm::event_log::LogEvent;
use serde_json::{json, Value as JsonValue};
use time::OffsetDateTime;

use crate::package::CollectedTriggerHandler;

use super::types::{QueuePreviewEntry, TriggerReplayRequest};

pub(super) fn parse_trust_query_timestamp(raw: &str) -> Result<OffsetDateTime, String> {
    if let Ok(parsed) = OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339) {
        return Ok(parsed);
    }
    if let Ok(unix) = raw.parse::<i64>() {
        let parsed = if raw.len() > 10 {
            OffsetDateTime::from_unix_timestamp_nanos(unix as i128 * 1_000_000)
        } else {
            OffsetDateTime::from_unix_timestamp(unix)
        };
        return parsed.map_err(|error| format!("invalid timestamp '{raw}': {error}"));
    }
    Err(format!(
        "invalid timestamp '{raw}': expected RFC3339 or unix seconds/milliseconds"
    ))
}

pub(super) fn now_rfc3339() -> String {
    harn_vm::clock::system_now_rfc3339()
}

/// Emit a `notifications/progress` update from a built-in tool when the
/// caller opted in via `_meta.progressToken`. Silently no-ops otherwise,
/// so call sites can sprinkle milestones without conditional logic.
pub(super) fn report_milestone(progress: f64, message: &str) {
    if let Some(ctx) = harn_vm::mcp_progress::current_context() {
        ctx.report(progress, Some(1.0), Some(message.to_string()));
    }
}

pub(super) fn handler_json(handler: &CollectedTriggerHandler) -> JsonValue {
    match handler {
        CollectedTriggerHandler::Local { reference, .. } => json!({
            "kind": "local",
            "reference": reference.raw,
        }),
        CollectedTriggerHandler::A2a { target, .. } => json!({
            "kind": "a2a",
            "target": target,
        }),
        CollectedTriggerHandler::Worker { queue } => json!({
            "kind": "worker",
            "queue": queue,
        }),
        CollectedTriggerHandler::Persona { binding, .. } => json!({
            "kind": "persona",
            "name": binding.name,
            "entry_workflow": binding.entry_workflow,
        }),
        CollectedTriggerHandler::EvalPack {
            target, manifest, ..
        } => json!({
            "kind": "eval_pack",
            "target": target,
            "pack_id": manifest.id.clone(),
        }),
    }
}

pub(super) fn preview_events(events: Vec<(u64, LogEvent)>) -> Vec<QueuePreviewEntry> {
    let mut preview = events
        .into_iter()
        .map(|(event_id, event)| QueuePreviewEntry {
            event_id,
            kind: event.kind,
            occurred_at_ms: event.occurred_at_ms,
            headers: event.headers,
            payload: event.payload,
        })
        .collect::<Vec<_>>();
    preview.sort_by_key(|entry| entry.event_id);
    preview
        .into_iter()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

pub(super) fn trigger_kind_name(kind: crate::package::TriggerKind) -> &'static str {
    match kind {
        crate::package::TriggerKind::Webhook => "webhook",
        crate::package::TriggerKind::Cron => "cron",
        crate::package::TriggerKind::Poll => "poll",
        crate::package::TriggerKind::Stream => "stream",
        crate::package::TriggerKind::Predicate => "predicate",
        crate::package::TriggerKind::A2aPush => "a2a-push",
    }
}

pub(super) fn trigger_replay_steering_from_request(
    request: &TriggerReplayRequest,
) -> Result<Option<crate::commands::trigger::replay::ReplaySteering>, String> {
    let Some(step) = request.steer_from.as_ref() else {
        if request.to_decision.is_some()
            || request.reason.is_some()
            || request.applied_by.is_some()
            || request.scope.is_some()
        {
            return Err(
                "harn.trigger.replay: steer_from is required for replay steering fields"
                    .to_string(),
            );
        }
        return Ok(None);
    };
    let to_decision = request
        .to_decision
        .clone()
        .ok_or_else(|| "harn.trigger.replay: steer_from requires to_decision".to_string())?;
    crate::commands::trigger::replay::ReplaySteering::new(
        step.clone(),
        to_decision,
        request.reason.clone(),
        request.applied_by.clone(),
        request.scope.as_deref(),
    )
    .map(Some)
}

pub(super) fn auth_event_log(
    state_dir: &std::path::Path,
) -> Result<std::sync::Arc<harn_vm::event_log::AnyEventLog>, String> {
    let config = harn_vm::event_log::EventLogConfig::for_base_dir(state_dir)
        .map_err(|error| format!("failed to build auth event log config: {error}"))?;
    harn_vm::event_log::open_event_log(&config)
        .map_err(|error| format!("failed to open auth event log: {error}"))
}

pub(super) fn inject_trace_headers(event: &mut JsonValue, client_identity: &str, trace_id: &str) {
    let Some(object) = event.as_object_mut() else {
        return;
    };
    object.insert("trace_id".to_string(), json!(trace_id));
    let headers = object
        .entry("headers")
        .or_insert_with(|| json!({}))
        .as_object_mut();
    if let Some(headers) = headers {
        headers.insert("x-harn-mcp-client".to_string(), json!(client_identity));
        headers.insert("x-harn-mcp-trace-id".to_string(), json!(trace_id));
    }
}

pub(super) fn merge_json_object(target: &mut JsonValue, patch: JsonValue) {
    let Some(target) = target.as_object_mut() else {
        return;
    };
    if let Some(patch) = patch.as_object() {
        for (key, value) in patch {
            target.insert(key.clone(), value.clone());
        }
    }
}

pub(super) fn normalized_headers(headers: &axum::http::HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

pub(super) fn filter_related_events(
    events: Vec<(u64, LogEvent)>,
    event_id: &str,
    trace_id: &str,
) -> Vec<JsonValue> {
    events
        .into_iter()
        .filter_map(|(id, event)| {
            let matches_event = event
                .headers
                .get("event_id")
                .is_some_and(|value| value == event_id)
                || event
                    .headers
                    .get("trace_id")
                    .is_some_and(|value| value == trace_id)
                || event
                    .payload
                    .pointer("/context/event_id")
                    .and_then(JsonValue::as_str)
                    == Some(event_id);
            matches_event.then_some(json!({
                "id": id,
                "kind": event.kind,
                "occurred_at_ms": event.occurred_at_ms,
                "headers": event.headers,
                "payload": event.payload,
            }))
        })
        .collect()
}
