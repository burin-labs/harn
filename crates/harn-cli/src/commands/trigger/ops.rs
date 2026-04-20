use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use harn_vm::{
    append_dispatch_cancel_request, CompiledRecordFilter, DispatchCancelRequest, ProviderPayload,
    RecordedTriggerBinding, TriggerHandlerSpec,
};

use crate::package;

const TRIGGER_EVENTS_TOPIC: &str = "triggers.events";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct TriggerEventRecord {
    pub(crate) binding_id: String,
    pub(crate) binding_version: u32,
    pub(crate) replay_of_event_id: Option<String>,
    pub(crate) event: harn_vm::TriggerEvent,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BulkTriggerTarget {
    pub(crate) event_id: String,
    pub(crate) binding_id: String,
    pub(crate) binding_version: u32,
    pub(crate) binding_key: String,
    pub(crate) handler_kind: String,
    pub(crate) handler: String,
    pub(crate) target_uri: String,
    pub(crate) latest_status: String,
    pub(crate) attempt_count: u32,
    pub(crate) cancel_requested: bool,
    pub(crate) terminal: bool,
    pub(crate) cancellable: bool,
    pub(crate) filter_record: JsonValue,
    pub(crate) record: TriggerEventRecord,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TriggerHistoryView {
    outbox: Vec<(u64, LogEvent)>,
    attempts: Vec<(u64, LogEvent)>,
    dlq: Vec<(u64, LogEvent)>,
    action_graph: Vec<(u64, LogEvent)>,
    cancel_requests: Vec<DispatchCancelRequest>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TriggerOperationAuditEntry {
    pub(crate) id: String,
    pub(crate) operation: String,
    pub(crate) dry_run: bool,
    pub(crate) filter: Option<String>,
    pub(crate) requested_at: String,
    pub(crate) requested_by: Option<String>,
    pub(crate) matched_count: usize,
    pub(crate) executed_count: usize,
    pub(crate) skipped_count: usize,
    pub(crate) rate_limit_per_second: Option<f64>,
    pub(crate) targets: Vec<TriggerOperationAuditTarget>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TriggerOperationAuditTarget {
    pub(crate) event_id: String,
    pub(crate) binding_key: String,
    pub(crate) latest_status: String,
}

#[derive(Clone, Debug, Deserialize)]
struct AttemptRecordPayload {
    attempt: u32,
    outcome: String,
    completed_at: String,
    error_msg: Option<String>,
}

pub(crate) struct RateLimiter {
    rate_limit_per_second: Option<f64>,
    last_tick: Option<Instant>,
}

impl RateLimiter {
    pub(crate) fn new(rate_limit_per_second: Option<f64>) -> Self {
        Self {
            rate_limit_per_second: rate_limit_per_second.filter(|value| *value > 0.0),
            last_tick: None,
        }
    }

    pub(crate) async fn wait(&mut self) {
        let Some(rate_limit_per_second) = self.rate_limit_per_second else {
            return;
        };
        let interval = Duration::from_secs_f64(1.0 / rate_limit_per_second);
        if let Some(last_tick) = self.last_tick {
            let elapsed = last_tick.elapsed();
            if elapsed < interval {
                tokio::time::sleep(interval - elapsed).await;
            }
        }
        self.last_tick = Some(Instant::now());
    }
}

pub(crate) struct ProgressReporter {
    enabled: bool,
    operation: &'static str,
    total: usize,
    processed: usize,
    succeeded: usize,
    failed: usize,
    skipped: usize,
}

impl ProgressReporter {
    pub(crate) fn new(enabled: bool, operation: &'static str, total: usize) -> Self {
        Self {
            enabled,
            operation,
            total,
            processed: 0,
            succeeded: 0,
            failed: 0,
            skipped: 0,
        }
    }

    pub(crate) fn update(&mut self, status: &str) {
        self.processed += 1;
        match status {
            "succeeded" | "requested" => self.succeeded += 1,
            "failed" | "cancelled" => self.failed += 1,
            _ => self.skipped += 1,
        }
        if self.enabled {
            eprintln!(
                "[harn] trigger {} progress {}/{} succeeded={} failed={} skipped={}",
                self.operation,
                self.processed,
                self.total,
                self.succeeded,
                self.failed,
                self.skipped
            );
        }
    }
}

pub(crate) async fn install_trigger_runtime(workspace_root: &Path) -> Result<(), String> {
    let mut vm = crate::commands::trigger::replay::build_replay_vm(workspace_root);
    let extensions = package::load_runtime_extensions(workspace_root);
    package::install_runtime_extensions(&extensions);
    package::install_manifest_triggers(&mut vm, &extensions)
        .await
        .map_err(|error| format!("failed to install manifest triggers: {error}"))
}

pub(crate) fn workspace_root_and_event_log() -> Result<(PathBuf, Arc<AnyEventLog>), String> {
    harn_vm::reset_thread_local_state();
    let cwd = std::env::current_dir().map_err(|error| format!("failed to read cwd: {error}"))?;
    let workspace_root = harn_vm::stdlib::process::find_project_root(&cwd).unwrap_or(cwd);
    let event_log = harn_vm::event_log::install_default_for_base_dir(&workspace_root)
        .map_err(|error| format!("failed to open event log snapshot: {error}"))?;
    Ok((workspace_root, event_log))
}

pub(crate) async fn load_bulk_targets(
    event_log: &Arc<AnyEventLog>,
    where_expr: &str,
    as_of: Option<OffsetDateTime>,
) -> Result<(Vec<BulkTriggerTarget>, String), String> {
    let records = load_original_records(event_log).await?;
    let history = load_history_view(event_log).await?;
    let mut filter = CompiledRecordFilter::compile(where_expr)?;
    let normalized_filter = filter.normalized_expr().to_string();
    let mut targets = Vec::new();
    for record in records {
        let binding = resolve_binding_for_record(&record, as_of)?;
        let target = build_bulk_target(&record, &binding, &history)?;
        if filter
            .matches(&target.filter_record)
            .await
            .map_err(|error| {
                format!(
                    "failed to evaluate filter on '{}': {error}",
                    record.event.id.0
                )
            })?
        {
            targets.push(target);
        }
    }
    targets.sort_by(|left, right| {
        left.event_id
            .cmp(&right.event_id)
            .then(left.binding_key.cmp(&right.binding_key))
    });
    Ok((targets, normalized_filter))
}

pub(crate) async fn load_targets_for_event_id(
    event_log: &Arc<AnyEventLog>,
    event_id: &str,
    as_of: Option<OffsetDateTime>,
) -> Result<Vec<BulkTriggerTarget>, String> {
    let records = load_original_records(event_log).await?;
    let history = load_history_view(event_log).await?;
    let mut targets = Vec::new();
    for record in records
        .into_iter()
        .filter(|record| record.event.id.0 == event_id)
    {
        let binding = resolve_binding_for_record(&record, as_of)?;
        targets.push(build_bulk_target(&record, &binding, &history)?);
    }
    targets.sort_by(|left, right| left.binding_key.cmp(&right.binding_key));
    Ok(targets)
}

pub(crate) async fn append_bulk_cancel_requests(
    event_log: &Arc<AnyEventLog>,
    audit_id: &str,
    requested_by: Option<String>,
    targets: &[BulkTriggerTarget],
) -> Result<usize, String> {
    let mut appended = 0;
    for target in targets {
        if !target.cancellable {
            continue;
        }
        append_dispatch_cancel_request(
            event_log,
            &DispatchCancelRequest {
                binding_key: target.binding_key.clone(),
                event_id: target.event_id.clone(),
                requested_at: OffsetDateTime::now_utc(),
                requested_by: requested_by.clone(),
                audit_id: Some(audit_id.to_string()),
            },
        )
        .await
        .map_err(|error| format!("failed to append cancel request: {error}"))?;
        appended += 1;
    }
    Ok(appended)
}

pub(crate) async fn append_operation_audit(
    event_log: &Arc<AnyEventLog>,
    audit: &TriggerOperationAuditEntry,
) -> Result<u64, String> {
    let topic = Topic::new(harn_vm::TRIGGER_OPERATION_AUDIT_TOPIC)
        .map_err(|error| format!("invalid operation audit topic: {error}"))?;
    event_log
        .append(
            &topic,
            LogEvent::new(
                "trigger_operation_audit",
                serde_json::to_value(audit)
                    .map_err(|error| format!("failed to encode trigger audit entry: {error}"))?,
            ),
        )
        .await
        .map_err(|error| format!("failed to append trigger audit entry: {error}"))
}

pub(crate) fn default_requested_by() -> Option<String> {
    std::env::var("USER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::env::var("LOGNAME").ok())
}

pub(crate) fn build_operation_audit(
    operation: &str,
    dry_run: bool,
    filter: Option<String>,
    rate_limit_per_second: Option<f64>,
    matched_count: usize,
    executed_count: usize,
    skipped_count: usize,
    targets: &[BulkTriggerTarget],
) -> TriggerOperationAuditEntry {
    TriggerOperationAuditEntry {
        id: format!("trigger_audit_{}", Uuid::now_v7()),
        operation: operation.to_string(),
        dry_run,
        filter,
        requested_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| OffsetDateTime::now_utc().to_string()),
        requested_by: default_requested_by(),
        matched_count,
        executed_count,
        skipped_count,
        rate_limit_per_second,
        targets: targets
            .iter()
            .map(|target| TriggerOperationAuditTarget {
                event_id: target.event_id.clone(),
                binding_key: target.binding_key.clone(),
                latest_status: target.latest_status.clone(),
            })
            .collect(),
    }
}

fn build_bulk_target(
    record: &TriggerEventRecord,
    binding: &Arc<harn_vm::triggers::registry::TriggerBinding>,
    history: &TriggerHistoryView,
) -> Result<BulkTriggerTarget, String> {
    let binding_key = format!("{}@v{}", record.binding_id, record.binding_version);
    let handler_kind = handler_kind(binding.as_ref()).to_string();
    let handler = handler_label(binding.as_ref());
    let target_uri = target_uri(binding.as_ref());
    let state = derive_target_state(
        record,
        &binding_key,
        &handler_kind,
        &handler,
        &target_uri,
        history,
    );
    let cancellable = !state.terminal && !state.cancel_requested;
    let latest_status = state.latest_status.clone();
    let error = state.error.clone();
    Ok(BulkTriggerTarget {
        event_id: record.event.id.0.clone(),
        binding_id: record.binding_id.clone(),
        binding_version: record.binding_version,
        binding_key: binding_key.clone(),
        handler_kind: handler_kind.clone(),
        handler: handler.clone(),
        target_uri: target_uri.clone(),
        latest_status: latest_status.clone(),
        attempt_count: state.attempt_count,
        cancel_requested: state.cancel_requested,
        terminal: state.terminal,
        cancellable,
        filter_record: json!({
            "event": event_filter_record(record),
            "binding": {
                "id": record.binding_id.clone(),
                "version": record.binding_version,
                "key": binding_key.clone(),
                "handler_kind": handler_kind.clone(),
                "handler": handler.clone(),
                "target_uri": target_uri.clone(),
            },
            "attempt": {
                "status": latest_status.clone(),
                "attempt": state.attempt_count,
                "count": state.attempt_count,
                "handler": handler.clone(),
                "handler_kind": handler_kind.clone(),
                "target_uri": target_uri.clone(),
                "error": error.clone(),
                "started_at": state.started_at.clone(),
                "completed_at": state.completed_at.clone(),
                "failed_at": state.failed_at.clone(),
                "terminal": state.terminal,
                "cancellable": cancellable,
                "cancel_requested": state.cancel_requested,
            },
            "outcome": {
                "status": latest_status,
                "attempt_count": state.attempt_count,
                "error": error,
                "terminal": state.terminal,
            },
            "audit": {
                "replay_of_event_id": record.replay_of_event_id.clone(),
                "cancel_requested": state.cancel_requested,
            }
        }),
        record: record.clone(),
    })
}

fn event_filter_record(record: &TriggerEventRecord) -> JsonValue {
    json!({
        "id": record.event.id.0.clone(),
        "provider": record.event.provider.as_str(),
        "kind": record.event.kind.clone(),
        "received_at": record.event.received_at.format(&Rfc3339).ok(),
        "occurred_at": record.event.occurred_at.and_then(|value| value.format(&Rfc3339).ok()),
        "dedupe_key": record.event.dedupe_key.clone(),
        "trace_id": record.event.trace_id.0.clone(),
        "tenant": record.event.tenant_id.as_ref().map(|tenant| tenant.0.clone()),
        "headers": record.event.headers.clone(),
        "payload": normalized_event_payload(&record.event.provider_payload),
        "provider_payload": serde_json::to_value(&record.event.provider_payload).unwrap_or(JsonValue::Null),
        "replay_of_event_id": record.replay_of_event_id.clone(),
    })
}

async fn load_original_records(
    event_log: &Arc<AnyEventLog>,
) -> Result<Vec<TriggerEventRecord>, String> {
    let topic = Topic::new(TRIGGER_EVENTS_TOPIC)
        .map_err(|error| format!("invalid trigger events topic: {error}"))?;
    let recorded = event_log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| format!("failed to read trigger events: {error}"))?;
    let envelopes_topic = Topic::new(harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC)
        .map_err(|error| format!("invalid trigger inbox topic: {error}"))?;
    let legacy_topic = Topic::new(harn_vm::TRIGGER_INBOX_LEGACY_TOPIC)
        .map_err(|error| format!("invalid trigger inbox legacy topic: {error}"))?;
    let envelopes = event_log
        .read_range(&envelopes_topic, None, usize::MAX)
        .await
        .map_err(|error| format!("failed to read trigger inbox envelopes: {error}"))?;
    let legacy = event_log
        .read_range(&legacy_topic, None, usize::MAX)
        .await
        .map_err(|error| format!("failed to read legacy trigger inbox envelopes: {error}"))?;

    let mut deduped = BTreeMap::new();
    for (_, event) in recorded {
        let Ok(record) = serde_json::from_value::<TriggerEventRecord>(event.payload) else {
            continue;
        };
        if record.replay_of_event_id.is_some() {
            continue;
        }
        deduped.insert(
            format!(
                "{}:{}:{}",
                record.binding_id, record.binding_version, record.event.id.0
            ),
            record,
        );
    }

    for (_, event) in envelopes.into_iter().chain(legacy) {
        if event.kind != "event_ingested" {
            continue;
        }
        let Ok(envelope) =
            serde_json::from_value::<harn_vm::triggers::dispatcher::InboxEnvelope>(event.payload)
        else {
            continue;
        };
        let (Some(binding_id), Some(binding_version)) =
            (envelope.trigger_id, envelope.binding_version)
        else {
            continue;
        };
        let record = TriggerEventRecord {
            binding_id: binding_id.clone(),
            binding_version,
            replay_of_event_id: None,
            event: envelope.event,
        };
        deduped
            .entry(format!(
                "{}:{}:{}",
                binding_id, binding_version, record.event.id.0
            ))
            .or_insert(record);
    }

    Ok(deduped.into_values().collect())
}

async fn load_history_view(event_log: &Arc<AnyEventLog>) -> Result<TriggerHistoryView, String> {
    let outbox = read_topic(event_log, harn_vm::TRIGGER_OUTBOX_TOPIC).await?;
    let attempts = read_topic(event_log, harn_vm::TRIGGER_ATTEMPTS_TOPIC).await?;
    let dlq = read_topic(event_log, harn_vm::TRIGGER_DLQ_TOPIC).await?;
    let action_graph = read_topic(event_log, "observability.action_graph").await?;
    let cancel_topic = Topic::new(harn_vm::TRIGGER_CANCEL_REQUESTS_TOPIC)
        .map_err(|error| format!("invalid trigger cancel request topic: {error}"))?;
    let cancel_requests = event_log
        .read_range(&cancel_topic, None, usize::MAX)
        .await
        .map_err(|error| format!("failed to read trigger cancel requests: {error}"))?
        .into_iter()
        .filter(|(_, event)| event.kind == "dispatch_cancel_requested")
        .filter_map(|(_, event)| {
            serde_json::from_value::<DispatchCancelRequest>(event.payload).ok()
        })
        .collect();
    Ok(TriggerHistoryView {
        outbox,
        attempts,
        dlq,
        action_graph,
        cancel_requests,
    })
}

async fn read_topic(
    event_log: &Arc<AnyEventLog>,
    topic_name: &str,
) -> Result<Vec<(u64, LogEvent)>, String> {
    let topic = Topic::new(topic_name).map_err(|error| error.to_string())?;
    event_log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| format!("failed to read {topic_name}: {error}"))
}

fn resolve_binding_for_record(
    record: &TriggerEventRecord,
    as_of: Option<OffsetDateTime>,
) -> Result<Arc<harn_vm::triggers::registry::TriggerBinding>, String> {
    if let Some(as_of) = as_of {
        return harn_vm::resolve_trigger_binding_as_of(&record.binding_id, as_of).map_err(
            |error| {
                format!(
                    "failed to resolve binding '{}' as of {}: {}",
                    record.binding_id,
                    as_of.format(&Rfc3339).unwrap_or_else(|_| as_of.to_string()),
                    error
                )
            },
        );
    }
    harn_vm::resolve_live_or_as_of(
        &record.binding_id,
        RecordedTriggerBinding {
            version: record.binding_version,
            received_at: record.event.received_at,
        },
    )
    .map_err(|error| {
        format!(
            "failed to resolve binding '{}' version {}: {}",
            record.binding_id, record.binding_version, error
        )
    })
}

#[derive(Clone, Debug, Default)]
struct DerivedTargetState {
    latest_status: String,
    attempt_count: u32,
    cancel_requested: bool,
    terminal: bool,
    error: Option<String>,
    started_at: String,
    completed_at: String,
    failed_at: String,
}

fn derive_target_state(
    record: &TriggerEventRecord,
    binding_key: &str,
    _handler_kind: &str,
    _handler: &str,
    _target_uri: &str,
    history: &TriggerHistoryView,
) -> DerivedTargetState {
    let event_id = record.event.id.0.as_str();
    let mut state = DerivedTargetState {
        latest_status: "pending".to_string(),
        ..Default::default()
    };

    state.cancel_requested = history
        .cancel_requests
        .iter()
        .any(|request| request.binding_key == binding_key && request.event_id == event_id);

    for (_, event) in &history.outbox {
        if header_text(event, "event_id").as_deref() != Some(event_id)
            || header_text(event, "binding_key").as_deref() != Some(binding_key)
            || header_text(event, "replay_of_event_id").is_some()
        {
            continue;
        }
        if let Some(attempt) = header_u32(event, "attempt") {
            state.attempt_count = state.attempt_count.max(attempt);
        }
        match event.kind.as_str() {
            "dispatch_started" => {
                state.latest_status = "in_progress".to_string();
                state.started_at = format_event_time(event.occurred_at_ms);
            }
            "dispatch_succeeded" => {
                state.latest_status = "succeeded".to_string();
                state.terminal = true;
                state.completed_at = format_event_time(event.occurred_at_ms);
            }
            "dispatch_failed" => {
                let error = event
                    .payload
                    .get("error")
                    .and_then(JsonValue::as_str)
                    .map(str::to_string);
                state.latest_status = if error
                    .as_deref()
                    .is_some_and(|message| message.contains("cancelled"))
                {
                    "cancelled".to_string()
                } else {
                    "failed".to_string()
                };
                state.error = error;
                state.failed_at = format_event_time(event.occurred_at_ms);
                state.completed_at = format_event_time(event.occurred_at_ms);
                state.terminal = state.latest_status == "cancelled";
            }
            _ => {}
        }
    }

    for (_, event) in &history.attempts {
        if header_text(event, "event_id").as_deref() != Some(event_id)
            || header_text(event, "binding_key").as_deref() != Some(binding_key)
            || header_text(event, "replay_of_event_id").is_some()
        {
            continue;
        }
        if event.kind == "retry_scheduled" {
            state.latest_status = "retry_scheduled".to_string();
            state.failed_at = format_event_time(event.occurred_at_ms);
            if let Some(attempt) = header_u32(event, "attempt") {
                state.attempt_count = state.attempt_count.max(attempt.saturating_sub(1));
            }
        }
        if event.kind == "attempt_recorded" {
            let Ok(recorded_attempt) =
                serde_json::from_value::<AttemptRecordPayload>(event.payload.clone())
            else {
                continue;
            };
            state.attempt_count = state.attempt_count.max(recorded_attempt.attempt);
            if recorded_attempt.outcome == "cancelled" {
                state.latest_status = "cancelled".to_string();
                state.terminal = true;
            }
            state.error = recorded_attempt.error_msg;
            state.completed_at = recorded_attempt.completed_at;
        }
    }

    for (_, event) in &history.dlq {
        if header_text(event, "event_id").as_deref() != Some(event_id)
            || header_text(event, "binding_key").as_deref() != Some(binding_key)
            || header_text(event, "replay_of_event_id").is_some()
        {
            continue;
        }
        if event.kind == "dlq_moved" {
            state.latest_status = "dlq".to_string();
            state.terminal = true;
            state.error = event
                .payload
                .get("final_error")
                .and_then(JsonValue::as_str)
                .map(str::to_string);
        }
    }

    if !state.terminal {
        for (_, event) in &history.action_graph {
            let Some(context) = event.payload.get("context") else {
                continue;
            };
            if context.get("event_id").and_then(JsonValue::as_str) != Some(event_id)
                || context.get("binding_key").and_then(JsonValue::as_str) != Some(binding_key)
                || context
                    .get("replay_of_event_id")
                    .and_then(JsonValue::as_str)
                    .is_some()
            {
                continue;
            }
            let Some(nodes) = event.payload["observability"]["action_graph_nodes"].as_array()
            else {
                continue;
            };
            if nodes.iter().any(|node| {
                node.get("kind").and_then(JsonValue::as_str) == Some("predicate")
                    && node.get("outcome").and_then(JsonValue::as_str) == Some("false")
            }) {
                state.latest_status = "skipped".to_string();
                state.terminal = true;
            }
        }
    }

    state
}

fn header_text(event: &LogEvent, key: &str) -> Option<String> {
    event.headers.get(key).cloned()
}

fn header_u32(event: &LogEvent, key: &str) -> Option<u32> {
    event
        .headers
        .get(key)
        .and_then(|value| value.parse::<u32>().ok())
}

fn format_event_time(occurred_at_ms: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(occurred_at_ms as i128 * 1_000_000)
        .ok()
        .and_then(|value| value.format(&Rfc3339).ok())
        .unwrap_or_default()
}

fn handler_kind(binding: &harn_vm::triggers::registry::TriggerBinding) -> &'static str {
    match &binding.handler {
        TriggerHandlerSpec::Local { .. } => "local",
        TriggerHandlerSpec::A2a { .. } => "a2a",
        TriggerHandlerSpec::Worker { .. } => "worker",
    }
}

fn handler_label(binding: &harn_vm::triggers::registry::TriggerBinding) -> String {
    match &binding.handler {
        TriggerHandlerSpec::Local { raw, .. } => raw.clone(),
        TriggerHandlerSpec::A2a { target, .. } => target.clone(),
        TriggerHandlerSpec::Worker { queue } => queue.clone(),
    }
}

fn target_uri(binding: &harn_vm::triggers::registry::TriggerBinding) -> String {
    match &binding.handler {
        TriggerHandlerSpec::Local { raw, .. } => raw.clone(),
        TriggerHandlerSpec::A2a { target, .. } => format!("a2a://{target}"),
        TriggerHandlerSpec::Worker { queue } => format!("worker://{queue}"),
    }
}

fn normalized_event_payload(payload: &ProviderPayload) -> JsonValue {
    match payload {
        ProviderPayload::Known(harn_vm::triggers::event::KnownProviderPayload::GitHub(payload)) => {
            match payload {
                harn_vm::triggers::event::GitHubEventPayload::Issues(value) => {
                    value.common.raw.clone()
                }
                harn_vm::triggers::event::GitHubEventPayload::PullRequest(value) => {
                    value.common.raw.clone()
                }
                harn_vm::triggers::event::GitHubEventPayload::IssueComment(value) => {
                    value.common.raw.clone()
                }
                harn_vm::triggers::event::GitHubEventPayload::PullRequestReview(value) => {
                    value.common.raw.clone()
                }
                harn_vm::triggers::event::GitHubEventPayload::Push(value) => {
                    value.common.raw.clone()
                }
                harn_vm::triggers::event::GitHubEventPayload::WorkflowRun(value) => {
                    value.common.raw.clone()
                }
                harn_vm::triggers::event::GitHubEventPayload::Other(value) => value.raw.clone(),
            }
        }
        ProviderPayload::Known(harn_vm::triggers::event::KnownProviderPayload::Slack(payload)) => {
            match payload.as_ref() {
                harn_vm::triggers::event::SlackEventPayload::MessageChannels(value) => {
                    value.common.raw.clone()
                }
                harn_vm::triggers::event::SlackEventPayload::AppMention(value) => {
                    value.common.raw.clone()
                }
                harn_vm::triggers::event::SlackEventPayload::ReactionAdded(value) => {
                    value.common.raw.clone()
                }
                harn_vm::triggers::event::SlackEventPayload::TeamJoin(value) => {
                    value.common.raw.clone()
                }
                harn_vm::triggers::event::SlackEventPayload::ChannelCreated(value) => {
                    value.common.raw.clone()
                }
                harn_vm::triggers::event::SlackEventPayload::Other(value) => value.raw.clone(),
            }
        }
        ProviderPayload::Known(harn_vm::triggers::event::KnownProviderPayload::Linear(payload)) => {
            payload.raw.clone()
        }
        ProviderPayload::Known(harn_vm::triggers::event::KnownProviderPayload::Notion(payload)) => {
            payload.raw.clone()
        }
        ProviderPayload::Known(harn_vm::triggers::event::KnownProviderPayload::Cron(payload)) => {
            payload.raw.clone()
        }
        ProviderPayload::Known(harn_vm::triggers::event::KnownProviderPayload::Webhook(
            payload,
        )) => payload.raw.clone(),
        ProviderPayload::Known(harn_vm::triggers::event::KnownProviderPayload::A2aPush(
            payload,
        )) => payload.raw.clone(),
        ProviderPayload::Extension(payload) => payload.raw.clone(),
    }
}
