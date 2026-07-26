//! What the trigger stdlib writes to (and reads back from) the event log.
//!
//! Owns the serialized record shapes — recorded trigger events, dispatch
//! handles, dead-letter entries and their retry history, lifecycle and
//! action-graph entries — plus the queries behind `trigger_inspect_dlq`,
//! `trigger_inspect_lifecycle`, and `trigger_inspect_action_graph`, and the
//! dead-letter upsert/resolve pair the dispatch path drives. The log itself is
//! reached through [`ensure_trigger_event_log`], which installs a per-thread
//! in-memory log when a script runs without one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event_log::{
    active_event_log, install_memory_for_current_thread, EventLog, LogEvent, Topic,
};
use crate::stdlib::macros::harn_builtin;
use crate::triggers::test_util::clock;
use crate::triggers::{
    TriggerBindingSnapshot, TriggerEvent, TRIGGERS_LIFECYCLE_TOPIC, TRIGGER_DLQ_TOPIC,
};
use crate::value::{VmError, VmValue};

use super::args::value_from_serde;
use super::{ACTION_GRAPH_TOPIC, TRIGGER_EVENTS_TOPIC, TRIGGER_EVENT_LOG_QUEUE_DEPTH};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct TriggerEventRecord {
    pub(super) binding_id: String,
    pub(super) binding_version: u32,
    pub(super) replay_of_event_id: Option<String>,
    pub(super) event: TriggerEvent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct DispatchHandleRecord {
    pub(super) event_id: String,
    pub(super) binding_id: String,
    pub(super) binding_version: u32,
    pub(super) status: String,
    pub(super) replay_of_event_id: Option<String>,
    pub(super) dlq_entry_id: Option<String>,
    pub(super) error: Option<String>,
    pub(super) result: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct DlqAttemptRecord {
    pub(super) attempt: u32,
    pub(super) at: String,
    pub(super) status: String,
    pub(super) error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct DlqEntryRecord {
    pub(super) id: String,
    pub(super) event_id: String,
    pub(super) binding_id: String,
    pub(super) binding_version: u32,
    pub(super) provider: String,
    pub(super) kind: String,
    pub(super) state: String,
    pub(super) error: String,
    #[serde(default = "default_dlq_error_class")]
    pub(super) error_class: String,
    pub(super) event: TriggerEvent,
    pub(super) retry_history: Vec<DlqAttemptRecord>,
}

fn default_dlq_error_class() -> String {
    "unknown".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LifecycleEventRecord {
    kind: String,
    headers: BTreeMap<String, String>,
    payload: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ActionGraphEventRecord {
    kind: String,
    headers: BTreeMap<String, String>,
    payload: serde_json::Value,
}

#[harn_builtin(
    sig = "trigger_inspect_dlq(...args: any) -> list",
    kind = "async",
    category = "triggers"
)]
async fn trigger_inspect_dlq_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    _args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let entries = inspect_dlq_entries().await?;
    Ok(VmValue::List(std::sync::Arc::new(
        entries
            .into_iter()
            .map(|entry| value_from_serde(&entry))
            .collect(),
    )))
}

#[harn_builtin(
    sig = "trigger_inspect_lifecycle(...args: any) -> list",
    kind = "async",
    category = "triggers"
)]
async fn trigger_inspect_lifecycle_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let kind = args.first().and_then(|value| match value {
        VmValue::String(text) => Some(text.to_string()),
        VmValue::Nil => None,
        _ => None,
    });
    let entries = inspect_lifecycle_events(kind.as_deref()).await?;
    Ok(VmValue::List(std::sync::Arc::new(
        entries
            .into_iter()
            .map(|entry| value_from_serde(&entry))
            .collect(),
    )))
}

#[harn_builtin(
    sig = "trigger_inspect_action_graph(...args: any) -> list",
    kind = "async",
    category = "triggers"
)]
async fn trigger_inspect_action_graph_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let trace_id = args.first().and_then(|value| match value {
        VmValue::String(text) => Some(text.to_string()),
        VmValue::Nil => None,
        _ => None,
    });
    let entries = inspect_action_graph_events(trace_id.as_deref()).await?;
    Ok(VmValue::List(std::sync::Arc::new(
        entries
            .into_iter()
            .map(|entry| value_from_serde(&entry))
            .collect(),
    )))
}

pub(super) async fn find_replayable_event(event_id: &str) -> Result<TriggerEventRecord, VmError> {
    if let Some(record) = find_recorded_event(event_id).await? {
        return Ok(record);
    }
    find_ingested_event(event_id).await
}

async fn find_recorded_event(event_id: &str) -> Result<Option<TriggerEventRecord>, VmError> {
    let log = ensure_trigger_event_log();
    let topic = Topic::new(TRIGGER_EVENTS_TOPIC)
        .map_err(|error| VmError::Runtime(format!("trigger_replay: {error}")))?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| VmError::Runtime(format!("trigger_replay: {error}")))?;
    Ok(events
        .into_iter()
        .filter_map(|(_, event)| serde_json::from_value::<TriggerEventRecord>(event.payload).ok())
        .find(|record| record.event.id.0 == event_id))
}

async fn find_ingested_event(event_id: &str) -> Result<TriggerEventRecord, VmError> {
    let log = ensure_trigger_event_log();
    let envelopes_topic = Topic::new(crate::triggers::TRIGGER_INBOX_ENVELOPES_TOPIC)
        .map_err(|error| VmError::Runtime(format!("trigger_replay: {error}")))?;
    let legacy_topic = Topic::new(crate::triggers::TRIGGER_INBOX_LEGACY_TOPIC)
        .map_err(|error| VmError::Runtime(format!("trigger_replay: {error}")))?;
    let mut events = log
        .read_range(&envelopes_topic, None, usize::MAX)
        .await
        .map_err(|error| VmError::Runtime(format!("trigger_replay: {error}")))?;
    let legacy_events = log
        .read_range(&legacy_topic, None, usize::MAX)
        .await
        .map_err(|error| VmError::Runtime(format!("trigger_replay: {error}")))?;
    events.extend(legacy_events);
    events
        .into_iter()
        .filter_map(|(_, event)| {
            if event.kind != "event_ingested" {
                return None;
            }
            let envelope =
                serde_json::from_value::<crate::triggers::dispatcher::InboxEnvelope>(event.payload)
                    .ok()?;
            let binding_id = envelope.trigger_id?;
            let binding_version = envelope.binding_version?;
            Some(TriggerEventRecord {
                binding_id,
                binding_version,
                replay_of_event_id: None,
                event: envelope.event,
            })
        })
        .find(|record| record.event.id.0 == event_id)
        .ok_or_else(|| VmError::Runtime(format!("trigger_replay: unknown event id '{event_id}'")))
}

async fn inspect_dlq_entries() -> Result<Vec<DlqEntryRecord>, VmError> {
    let log = ensure_trigger_event_log();
    let topic = Topic::new(TRIGGER_DLQ_TOPIC)
        .map_err(|error| VmError::Runtime(format!("trigger_inspect_dlq: {error}")))?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| VmError::Runtime(format!("trigger_inspect_dlq: {error}")))?;
    let mut latest = BTreeMap::new();
    for (_, event) in events {
        let Ok(entry) = serde_json::from_value::<DlqEntryRecord>(event.payload) else {
            continue;
        };
        latest.insert(entry.id.clone(), entry);
    }
    let mut entries: Vec<DlqEntryRecord> = latest
        .into_values()
        .filter(|entry| entry.state == "pending")
        .collect();
    entries.sort_by(|left, right| {
        left.event_id
            .cmp(&right.event_id)
            .then(left.id.cmp(&right.id))
    });
    Ok(entries)
}

async fn inspect_lifecycle_events(
    kind_filter: Option<&str>,
) -> Result<Vec<LifecycleEventRecord>, VmError> {
    let log = ensure_trigger_event_log();
    let topic = Topic::new(TRIGGERS_LIFECYCLE_TOPIC)
        .map_err(|error| VmError::Runtime(format!("trigger_inspect_lifecycle: {error}")))?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| VmError::Runtime(format!("trigger_inspect_lifecycle: {error}")))?;
    Ok(events
        .into_iter()
        .filter_map(|(_, event)| {
            if kind_filter.is_some_and(|expected| expected != event.kind) {
                return None;
            }
            Some(LifecycleEventRecord {
                kind: event.kind,
                headers: event.headers,
                payload: event.payload,
            })
        })
        .collect())
}

async fn inspect_action_graph_events(
    trace_id_filter: Option<&str>,
) -> Result<Vec<ActionGraphEventRecord>, VmError> {
    let log = ensure_trigger_event_log();
    let topic = Topic::new(ACTION_GRAPH_TOPIC)
        .map_err(|error| VmError::Runtime(format!("trigger_inspect_action_graph: {error}")))?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| VmError::Runtime(format!("trigger_inspect_action_graph: {error}")))?;
    Ok(events
        .into_iter()
        .filter_map(|(_, event)| {
            if let Some(expected) = trace_id_filter {
                let matches_trace = event
                    .headers
                    .get("trace_id")
                    .is_some_and(|trace| trace == expected)
                    || event
                        .payload
                        .get("trace_id")
                        .and_then(|value| value.as_str())
                        == Some(expected);
                if !matches_trace {
                    return None;
                }
            }
            Some(ActionGraphEventRecord {
                kind: event.kind,
                headers: event.headers,
                payload: event.payload,
            })
        })
        .collect())
}

pub(super) async fn find_pending_dlq_entry_for_event(
    event_id: &str,
) -> Result<Option<DlqEntryRecord>, VmError> {
    Ok(inspect_dlq_entries()
        .await?
        .into_iter()
        .find(|entry| entry.event_id == event_id))
}

pub(super) async fn upsert_dlq_entry(
    log: &std::sync::Arc<crate::event_log::AnyEventLog>,
    binding: &TriggerBindingSnapshot,
    event: &TriggerEvent,
    error: &str,
    replay_of_event_id: Option<String>,
    existing_entry_id: Option<String>,
    mut retry_history: Vec<DlqAttemptRecord>,
) -> Result<DlqEntryRecord, VmError> {
    let mut entry = DlqEntryRecord {
        id: existing_entry_id.unwrap_or_else(|| format!("dlq_{}", Uuid::now_v7())),
        event_id: event.id.0.clone(),
        binding_id: binding.id.clone(),
        binding_version: binding.version,
        provider: event.provider.as_str().to_string(),
        kind: event.kind.clone(),
        state: "pending".to_string(),
        error: error.to_string(),
        error_class: crate::triggers::classify_trigger_dlq_error(error).to_string(),
        event: event.clone(),
        retry_history: Vec::new(),
    };
    entry.error = error.to_string();
    entry.error_class = crate::triggers::classify_trigger_dlq_error(error).to_string();
    retry_history.push(DlqAttemptRecord {
        attempt: (retry_history.len() + 1) as u32,
        at: clock::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        status: match replay_of_event_id {
            Some(_) => "replay_dlq".to_string(),
            None => "dlq".to_string(),
        },
        error: Some(error.to_string()),
    });
    entry.retry_history = retry_history;
    append_log(
        log,
        TRIGGER_DLQ_TOPIC,
        LogEvent::new(
            "dlq_entry",
            serde_json::to_value(&entry).unwrap_or_default(),
        ),
    )
    .await?;
    Ok(entry)
}

pub(super) async fn resolve_dlq_entry(
    log: &std::sync::Arc<crate::event_log::AnyEventLog>,
    mut entry: DlqEntryRecord,
    replay_of_event_id: Option<String>,
) -> Result<(), VmError> {
    entry.state = "resolved".to_string();
    entry.retry_history.push(DlqAttemptRecord {
        attempt: (entry.retry_history.len() + 1) as u32,
        at: clock::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        status: match replay_of_event_id {
            Some(_) => "replay_succeeded".to_string(),
            None => "resolved".to_string(),
        },
        error: None,
    });
    append_log(
        log,
        TRIGGER_DLQ_TOPIC,
        LogEvent::new(
            "dlq_entry",
            serde_json::to_value(&entry).unwrap_or_default(),
        ),
    )
    .await
}

pub(super) fn ensure_trigger_event_log() -> std::sync::Arc<crate::event_log::AnyEventLog> {
    active_event_log()
        .unwrap_or_else(|| install_memory_for_current_thread(TRIGGER_EVENT_LOG_QUEUE_DEPTH))
}

pub(super) async fn append_log(
    log: &std::sync::Arc<crate::event_log::AnyEventLog>,
    topic_name: &str,
    event: LogEvent,
) -> Result<(), VmError> {
    let topic = Topic::new(topic_name)
        .map_err(|error| VmError::Runtime(format!("trigger stdlib: {error}")))?;
    log.append(&topic, event)
        .await
        .map(|_| ())
        .map_err(|error| VmError::Runtime(format!("trigger stdlib: {error}")))
}
