use crate::event_log::{EventId, LogEvent, Topic};
use crate::value::VmError;

use super::StoredChannelEvent;

pub(super) fn receipt_value(
    topic: &Topic,
    event_id: EventId,
    event: &LogEvent,
    inserted: bool,
) -> Result<serde_json::Value, VmError> {
    let record = stored_record(event)?;
    let execution_id = event.headers.get(crate::tracing::meta::EXECUTION_ID);
    Ok(serde_json::json!({
        "event_id": event_id,
        "cursor": event_id,
        "id": record.id,
        "name": record.name,
        "name_resolved": record.name,
        "scope": record.scope,
        "scope_id": record.scope_id,
        "payload": record.payload,
        "emitted_at": record.emitted_at,
        "emitted_by": record.emitted_by,
        "pipeline_id": record.pipeline_id,
        "session_id": record.session_id,
        "tenant_id": record.tenant_id,
        "retention": record.retention,
        "ttl_ms": record.ttl_ms,
        "topic": topic.as_str(),
        "inserted": inserted,
        "duplicate": !inserted,
        "execution_id": execution_id,
    }))
}

pub(super) fn event_value(
    topic: &Topic,
    event_id: EventId,
    event: LogEvent,
) -> Result<serde_json::Value, VmError> {
    let record = stored_record(&event)?;
    let execution_id = event
        .headers
        .get(crate::tracing::meta::EXECUTION_ID)
        .cloned();
    Ok(serde_json::json!({
        "event_id": event_id,
        "cursor": event_id,
        "topic": topic.as_str(),
        "kind": event.kind,
        "headers": event.headers,
        "occurred_at_ms": event.occurred_at_ms,
        "id": record.id,
        "name": record.name,
        "name_resolved": record.name,
        "scope": record.scope,
        "scope_id": record.scope_id,
        "payload": record.payload,
        "emitted_at": record.emitted_at,
        "emitted_by": record.emitted_by,
        "execution_id": execution_id,
        "pipeline_id": record.pipeline_id,
        "session_id": record.session_id,
        "tenant_id": record.tenant_id,
        "retention": record.retention,
        "ttl_ms": record.ttl_ms,
    }))
}

fn stored_record(event: &LogEvent) -> Result<StoredChannelEvent, VmError> {
    serde_json::from_value(event.payload.clone()).map_err(|error| {
        VmError::Runtime(format!(
            "channel event store contained malformed channel payload: {error}"
        ))
    })
}
