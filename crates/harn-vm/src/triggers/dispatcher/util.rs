use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures::pin_mut;
use tokio::sync::broadcast;

use crate::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use crate::stdlib::json_to_vm_value;
use crate::value::{error_to_category, ErrorCategory, VmError, VmValue};

use super::state::{ACTIVE_DISPATCHER_STATE, ACTIVE_DISPATCH_WAIT_LEASE};
use super::types::{
    DispatchCancelRequest, DispatchError, DispatchOutcome, DispatchStatus, DispatcherRuntimeState,
    DispatcherStatsSnapshot,
};
use super::uri::DispatchUri;
use super::TriggerEvent;
use super::{
    TRIGGER_ACCEPTED_AT_MS_HEADER, TRIGGER_CANCEL_REQUESTS_TOPIC, TRIGGER_INBOX_ENVELOPES_TOPIC,
    TRIGGER_QUEUE_APPENDED_AT_MS_HEADER,
};
use crate::triggers::registry::TriggerBinding;

pub(super) async fn dispatch_cancel_requested(
    event_log: &Arc<AnyEventLog>,
    binding_key: &str,
    event_id: &str,
    replay_of_event_id: Option<&String>,
) -> Result<bool, DispatchError> {
    if replay_of_event_id.is_some() {
        return Ok(false);
    }
    let topic = Topic::new(TRIGGER_CANCEL_REQUESTS_TOPIC)
        .expect("static trigger cancel topic should always be valid");
    let events = event_log.read_range(&topic, None, usize::MAX).await?;
    let requested = events
        .into_iter()
        .filter(|(_, event)| event.kind == "dispatch_cancel_requested")
        .filter_map(|(_, event)| {
            serde_json::from_value::<DispatchCancelRequest>(event.payload).ok()
        })
        .collect::<BTreeSet<_>>();
    Ok(requested
        .iter()
        .any(|request| request.binding_key == binding_key && request.event_id == event_id))
}

pub(super) async fn sleep_or_cancel_or_request(
    event_log: &Arc<AnyEventLog>,
    delay: Duration,
    binding_key: &str,
    event_id: &str,
    replay_of_event_id: Option<&String>,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<(), DispatchError> {
    let sleep = tokio::time::sleep(delay);
    pin_mut!(sleep);
    let mut poll = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            _ = &mut sleep => return Ok(()),
            _ = recv_cancel(cancel_rx) => {
                return Err(DispatchError::Cancelled(
                    "dispatcher shutdown cancelled retry wait".to_string(),
                ));
            }
            _ = poll.tick() => {
                if dispatch_cancel_requested(event_log, binding_key, event_id, replay_of_event_id).await? {
                    return Err(DispatchError::Cancelled(
                        "trigger cancel request cancelled retry wait".to_string(),
                    ));
                }
            }
        }
    }
}

pub(crate) fn build_batched_event(
    events: Vec<TriggerEvent>,
) -> Result<TriggerEvent, DispatchError> {
    let mut iter = events.into_iter();
    let Some(mut root) = iter.next() else {
        return Err(DispatchError::Registry(
            "batch dispatch produced an empty event list".to_string(),
        ));
    };
    let mut batch = Vec::new();
    batch.push(
        serde_json::to_value(&root).map_err(|error| DispatchError::Serde(error.to_string()))?,
    );
    for event in iter {
        batch.push(
            serde_json::to_value(&event)
                .map_err(|error| DispatchError::Serde(error.to_string()))?,
        );
    }
    root.batch = Some(batch);
    Ok(root)
}

pub(super) fn json_value_to_gate(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "unserializable".to_string()),
    }
}

pub(super) fn event_to_handler_value(event: &TriggerEvent) -> Result<VmValue, DispatchError> {
    let json =
        serde_json::to_value(event).map_err(|error| DispatchError::Serde(error.to_string()))?;
    let value = json_to_vm_value(&json);
    match (&event.raw_body, value) {
        (Some(raw_body), VmValue::Dict(dict)) => {
            let mut map = (*dict).clone();
            map.insert(
                "raw_body".to_string(),
                VmValue::Bytes(Rc::new(raw_body.clone())),
            );
            Ok(VmValue::Dict(Rc::new(map)))
        }
        (_, other) => Ok(other),
    }
}

pub(super) fn decrement_in_flight(state: &DispatcherRuntimeState) {
    let previous = state.in_flight.fetch_sub(1, Ordering::Relaxed);
    if previous == 1 && state.retry_queue_depth.load(Ordering::Relaxed) == 0 {
        state.idle_notify.notify_waiters();
    }
}

pub(super) fn decrement_retry_queue_depth(state: &DispatcherRuntimeState) {
    let previous = state.retry_queue_depth.fetch_sub(1, Ordering::Relaxed);
    if previous == 1 && state.in_flight.load(Ordering::Relaxed) == 0 {
        state.idle_notify.notify_waiters();
    }
}

pub async fn enqueue_trigger_event<L: EventLog + ?Sized>(
    event_log: &L,
    event: &TriggerEvent,
) -> Result<u64, DispatchError> {
    let topic = topic_for_event(event, TRIGGER_INBOX_ENVELOPES_TOPIC)?;
    let headers = event_headers(event, None, None, None);
    let payload =
        serde_json::to_value(event).map_err(|error| DispatchError::Serde(error.to_string()))?;
    event_log
        .append(
            &topic,
            LogEvent::new("event_ingested", payload).with_headers(headers),
        )
        .await
        .map_err(DispatchError::from)
}

pub fn snapshot_dispatcher_stats() -> DispatcherStatsSnapshot {
    ACTIVE_DISPATCHER_STATE.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|state| DispatcherStatsSnapshot {
                in_flight: state.in_flight.load(Ordering::Relaxed),
                retry_queue_depth: state.retry_queue_depth.load(Ordering::Relaxed),
                dlq_depth: state.dlq.lock().expect("dispatcher dlq poisoned").len() as u64,
            })
            .unwrap_or_default()
    })
}

pub fn clear_dispatcher_state() {
    ACTIVE_DISPATCHER_STATE.with(|slot| {
        *slot.borrow_mut() = None;
    });
    ACTIVE_DISPATCH_WAIT_LEASE.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

pub(super) fn dispatch_error_from_vm_error(error: VmError) -> DispatchError {
    if let Some(wait_id) = crate::stdlib::waitpoint::is_waitpoint_suspension(&error) {
        return DispatchError::Waiting(format!("waitpoint suspended: {wait_id}"));
    }
    if is_cancelled_vm_error(&error) {
        return DispatchError::Cancelled("dispatcher shutdown cancelled local handler".to_string());
    }
    if let VmError::Thrown(VmValue::String(message)) = &error {
        return DispatchError::Local(message.to_string());
    }
    match error_to_category(&error) {
        ErrorCategory::Timeout => DispatchError::Timeout(error.to_string()),
        ErrorCategory::ToolRejected => DispatchError::Denied(error.to_string()),
        ErrorCategory::Cancelled => {
            DispatchError::Cancelled("dispatcher shutdown cancelled local handler".to_string())
        }
        _ => DispatchError::Local(error.to_string()),
    }
}

pub(super) fn split_binding_key(binding_key: &str) -> (String, u32) {
    let Some((binding_id, suffix)) = binding_key.rsplit_once("@v") else {
        return (binding_key.to_string(), 0);
    };
    let version = suffix.parse::<u32>().unwrap_or(0);
    (binding_id.to_string(), version)
}

pub(super) fn binding_key_from_parts(trigger_id: &str, binding_version: Option<u32>) -> String {
    match binding_version {
        Some(version) => format!("{trigger_id}@v{version}"),
        None => trigger_id.to_string(),
    }
}

pub(super) fn tenant_id(event: &TriggerEvent) -> Option<&str> {
    event.tenant_id.as_ref().map(|tenant| tenant.0.as_str())
}

pub(super) fn current_unix_ms() -> i64 {
    unix_ms(crate::triggers::test_util::clock::now_utc())
}

pub(super) fn unix_ms(timestamp: time::OffsetDateTime) -> i64 {
    (timestamp.unix_timestamp_nanos() / 1_000_000) as i64
}

pub(super) fn accepted_at_ms(
    headers: Option<&BTreeMap<String, String>>,
    event: &TriggerEvent,
) -> i64 {
    lifecycle_header_ms(headers, TRIGGER_ACCEPTED_AT_MS_HEADER)
        .unwrap_or_else(|| unix_ms(event.received_at))
}

pub(super) fn queue_appended_at_ms(
    headers: Option<&BTreeMap<String, String>>,
    event: &TriggerEvent,
) -> i64 {
    lifecycle_header_ms(headers, TRIGGER_QUEUE_APPENDED_AT_MS_HEADER)
        .unwrap_or_else(|| accepted_at_ms(headers, event))
}

pub(super) fn lifecycle_header_ms(
    headers: Option<&BTreeMap<String, String>>,
    name: &str,
) -> Option<i64> {
    headers
        .and_then(|headers| headers.get(name))
        .and_then(|value| value.parse::<i64>().ok())
}

pub(super) fn duration_between_ms(later_ms: i64, earlier_ms: i64) -> Duration {
    Duration::from_millis(later_ms.saturating_sub(earlier_ms).max(0) as u64)
}

pub(super) fn dispatch_result_status<T>(result: &Result<T, DispatchError>) -> &'static str {
    match result {
        Ok(_) => "succeeded",
        Err(DispatchError::Waiting(_)) => "waiting",
        Err(DispatchError::Cancelled(_)) => "cancelled",
        Err(DispatchError::Denied(_)) => "denied",
        Err(DispatchError::Timeout(_)) => "timeout",
        Err(_) => "failed",
    }
}

pub(super) fn is_cancelled_vm_error(error: &VmError) -> bool {
    matches!(
        error,
        VmError::Thrown(VmValue::String(message))
            if message.starts_with("kind:cancelled:")
    ) || matches!(error_to_category(error), ErrorCategory::Cancelled)
}

pub(super) fn event_headers(
    event: &TriggerEvent,
    binding: Option<&TriggerBinding>,
    attempt: Option<u32>,
    replay_of_event_id: Option<&String>,
) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("event_id".to_string(), event.id.0.clone());
    headers.insert("trace_id".to_string(), event.trace_id.0.clone());
    headers.insert("provider".to_string(), event.provider.as_str().to_string());
    headers.insert("kind".to_string(), event.kind.clone());
    if let Some(replay_of_event_id) = replay_of_event_id {
        headers.insert("replay_of_event_id".to_string(), replay_of_event_id.clone());
    }
    if let Some(tenant_id) = event.tenant_id.as_ref() {
        headers.insert("tenant_id".to_string(), tenant_id.0.clone());
    }
    if let Some(binding) = binding {
        headers.insert("trigger_id".to_string(), binding.id.as_str().to_string());
        headers.insert("binding_key".to_string(), binding.binding_key());
        headers.insert(
            "handler_kind".to_string(),
            DispatchUri::from(&binding.handler).kind().to_string(),
        );
    }
    if let Some(attempt) = attempt {
        headers.insert("attempt".to_string(), attempt.to_string());
    }
    headers
}

pub(super) fn topic_for_event(
    event: &TriggerEvent,
    topic_name: &str,
) -> Result<Topic, DispatchError> {
    let topic = Topic::new(topic_name)
        .expect("static trigger dispatcher topic names should always be valid");
    match event.tenant_id.as_ref() {
        Some(tenant_id) => crate::tenant_topic(tenant_id, &topic).map_err(DispatchError::from),
        None => Ok(topic),
    }
}

pub(super) fn worker_queue_priority(
    binding: &super::super::registry::TriggerBinding,
    event: &TriggerEvent,
) -> crate::WorkerQueuePriority {
    match event
        .headers
        .get("priority")
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("high") => crate::WorkerQueuePriority::High,
        Some("low") => crate::WorkerQueuePriority::Low,
        _ => binding.dispatch_priority,
    }
}

const TEST_FAIL_BEFORE_OUTBOX_ENV: &str = "HARN_TEST_DISPATCHER_FAIL_BEFORE_OUTBOX";

pub(super) fn maybe_fail_before_outbox() {
    if std::env::var_os(TEST_FAIL_BEFORE_OUTBOX_ENV).is_some() {
        std::process::exit(86);
    }
}

pub(super) fn now_rfc3339() -> String {
    crate::triggers::test_util::clock::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

pub(super) fn next_budget_reset_rfc3339(binding: &TriggerBinding) -> String {
    let now = crate::triggers::test_util::clock::now_utc();
    let reset = if binding.hourly_cost_usd.is_some() {
        let next_hour = (now.unix_timestamp() / 3_600 + 1) * 3_600;
        time::OffsetDateTime::from_unix_timestamp(next_hour).unwrap_or(now)
    } else {
        let next_day = ((now.unix_timestamp() / 86_400) + 1) * 86_400;
        time::OffsetDateTime::from_unix_timestamp(next_day).unwrap_or(now)
    };
    reset
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

pub(super) fn now_unix_ms() -> i64 {
    (crate::triggers::test_util::clock::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

pub(super) fn cancelled_dispatch_outcome(
    binding: &TriggerBinding,
    route: &DispatchUri,
    event: &TriggerEvent,
    replay_of_event_id: Option<String>,
    attempt_count: u32,
    error: String,
) -> DispatchOutcome {
    DispatchOutcome {
        trigger_id: binding.id.as_str().to_string(),
        binding_key: binding.binding_key(),
        event_id: event.id.0.clone(),
        attempt_count,
        status: DispatchStatus::Cancelled,
        handler_kind: route.kind().to_string(),
        target_uri: route.target_uri(),
        replay_of_event_id,
        result: None,
        error: Some(error),
    }
}

pub(super) async fn recv_cancel(cancel_rx: &mut broadcast::Receiver<()>) {
    let _ = cancel_rx.recv().await;
}

/// Resolve a simple dotted JSON path (e.g. `"tenant_id"`,
/// `"provider_payload.urgency"`, `"headers.x_priority"`) against the
/// serialized event. Returns `None` when any segment is missing or when an
/// array index is out of range. Used by the SpawnToPool trigger handler
/// (#1889) to pluck priority + fair-queue key from event payloads.
///
/// Numeric segments index into arrays; everything else looks up object
/// fields. Bracketed forms (`payload[0]`) are not supported; users that need
/// richer extraction should pre-shape the event in `task_factory`.
pub(super) fn extract_event_path(
    value: &serde_json::Value,
    path: &str,
) -> Option<serde_json::Value> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut current = value;
    for segment in trimmed.split('.') {
        if segment.is_empty() {
            return None;
        }
        match current {
            serde_json::Value::Object(map) => {
                current = map.get(segment)?;
            }
            serde_json::Value::Array(items) => {
                let index = segment.parse::<usize>().ok()?;
                current = items.get(index)?;
            }
            _ => return None,
        }
    }
    Some(current.clone())
}

/// Coerce an extracted JSON value into an i64 priority. Floats truncate
/// (matching the i64 cast on the Harn side), strings parse, booleans map to
/// 0/1, and null/objects/arrays return `None` so the dispatcher falls back
/// to the default priority instead of failing the dispatch.
pub(super) fn extracted_priority_value(value: serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        serde_json::Value::String(s) => s.trim().parse::<i64>().ok(),
        serde_json::Value::Bool(b) => Some(if b { 1 } else { 0 }),
        _ => None,
    }
}

/// Coerce an extracted JSON value into a fair-queue key string. Scalars
/// stringify; null and complex values return `None` so the pool sees no key
/// (which round-robin treats as the default bucket).
pub(super) fn extracted_key_value(value: serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    }
}

pub async fn append_dispatch_cancel_request(
    event_log: &Arc<AnyEventLog>,
    request: &DispatchCancelRequest,
) -> Result<u64, DispatchError> {
    let topic = Topic::new(TRIGGER_CANCEL_REQUESTS_TOPIC)
        .expect("static trigger cancel topic should always be valid");
    event_log
        .append(
            &topic,
            LogEvent::new(
                "dispatch_cancel_requested",
                serde_json::to_value(request)
                    .map_err(|error| DispatchError::Serde(error.to_string()))?,
            ),
        )
        .await
        .map_err(DispatchError::from)
}
