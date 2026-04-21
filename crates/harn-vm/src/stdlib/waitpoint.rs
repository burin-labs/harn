use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::event_log::{
    active_event_log, install_memory_for_current_thread, sanitize_topic_component, AnyEventLog,
    EventLog, LogEvent, Topic,
};
use crate::triggers::dispatcher::{
    current_dispatch_context, current_dispatch_is_replay, DispatchContext,
};
use crate::triggers::registry::{resolve_live_or_as_of, RecordedTriggerBinding};
use crate::value::{categorized_error, ErrorCategory, VmError, VmValue};
use crate::vm::{clone_async_builtin_child_vm, Vm};

const WAITPOINT_EVENT_LOG_QUEUE_DEPTH: usize = 128;
const WAITPOINT_WAITS_TOPIC: &str = "waitpoint.waits";
pub const WAITPOINT_RESUME_TOPIC: &str = "waitpoint.resumes";
const TRIGGER_EVENTS_TOPIC: &str = "triggers.events";

thread_local! {
    static CREATE_SEQUENCE: RefCell<SequenceState> = RefCell::new(SequenceState::default());
    static WAIT_SEQUENCE: RefCell<SequenceState> = RefCell::new(SequenceState::default());
}

#[derive(Default)]
struct SequenceState {
    instance_key: String,
    next_seq: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitpointStatus {
    Open,
    Completed,
    Cancelled,
}

impl WaitpointStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WaitpointRecord {
    pub id: String,
    pub status: WaitpointStatus,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub cancelled_at: Option<String>,
    pub completed_by: Option<String>,
    pub cancelled_by: Option<String>,
    pub value: Option<JsonValue>,
    pub reason: Option<String>,
    pub metadata: Option<JsonValue>,
}

impl Default for WaitpointRecord {
    fn default() -> Self {
        Self {
            id: String::new(),
            status: WaitpointStatus::Open,
            created_at: String::new(),
            completed_at: None,
            cancelled_at: None,
            completed_by: None,
            cancelled_by: None,
            value: None,
            reason: None,
            metadata: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WaiterStatus {
    Pending,
    Completed,
    Cancelled,
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WaiterRecord {
    wait_id: String,
    waitpoint_ids: Vec<String>,
    binding_id: String,
    binding_version: u32,
    original_event_id: String,
    created_at: String,
    timeout_at: Option<String>,
    resolved_at: Option<String>,
    status: WaiterStatus,
    completed_ids: Vec<String>,
    cancelled_ids: Vec<String>,
    cancel_reason: Option<String>,
    event: crate::TriggerEvent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WaitpointResumeRequest {
    waitpoint_id: String,
    requested_at: String,
    reason: String,
}

#[derive(Clone, Debug)]
struct DispatchKeys {
    instance_key: String,
    stable_base: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct WaitpointWaitOptions {
    pub timeout: Option<StdDuration>,
}

#[derive(Clone, Debug)]
pub(crate) enum WaitpointWaitFailure {
    Timeout {
        wait_id: String,
    },
    Cancelled {
        wait_id: String,
        waitpoint_ids: Vec<String>,
        reason: Option<String>,
    },
    Vm(VmError),
}

#[derive(Clone, Debug)]
enum WaiterResolution {
    NotReady,
    Completed {
        completed_ids: Vec<String>,
    },
    Cancelled {
        cancelled_ids: Vec<String>,
        reason: Option<String>,
    },
    TimedOut,
}

pub(crate) fn register_waitpoint_builtins(vm: &mut Vm) {
    vm.register_async_builtin("__waitpoint_create", |args| {
        Box::pin(async move { waitpoint_create_builtin(&args).await })
    });
    vm.register_async_builtin("__waitpoint_wait", |args| {
        Box::pin(async move { waitpoint_wait_builtin(&args).await })
    });
    vm.register_async_builtin("__waitpoint_complete", |args| {
        Box::pin(async move { waitpoint_complete_builtin(&args).await })
    });
    vm.register_async_builtin("__waitpoint_cancel", |args| {
        Box::pin(async move { waitpoint_cancel_builtin(&args).await })
    });
}

pub(crate) fn reset_waitpoint_state() {
    CREATE_SEQUENCE.with(|slot| *slot.borrow_mut() = SequenceState::default());
    WAIT_SEQUENCE.with(|slot| *slot.borrow_mut() = SequenceState::default());
}

pub(crate) async fn create_waitpoint(
    explicit_id: Option<String>,
    metadata: Option<JsonValue>,
) -> Result<WaitpointRecord, VmError> {
    let log = ensure_waitpoint_event_log();
    create_waitpoint_on(&log, explicit_id, metadata).await
}

pub(crate) async fn create_waitpoint_on(
    log: &Arc<AnyEventLog>,
    explicit_id: Option<String>,
    metadata: Option<JsonValue>,
) -> Result<WaitpointRecord, VmError> {
    let id = explicit_id.unwrap_or_else(next_waitpoint_id);
    if let Some(existing) = read_waitpoint_record(&log, &id).await? {
        return Ok(existing);
    }
    let record = WaitpointRecord {
        id: id.clone(),
        status: WaitpointStatus::Open,
        created_at: now_rfc3339(),
        metadata,
        ..WaitpointRecord::default()
    };
    append_waitpoint_record(&log, &record).await?;
    Ok(record)
}

pub(crate) async fn wait_on_waitpoints(
    waitpoint_ids: Vec<String>,
    options: WaitpointWaitOptions,
) -> Result<Vec<WaitpointRecord>, WaitpointWaitFailure> {
    let waitpoint_ids = normalize_waitpoint_ids(waitpoint_ids).map_err(WaitpointWaitFailure::Vm)?;
    let log = ensure_waitpoint_event_log();
    let states = load_waitpoint_states(&log, &waitpoint_ids)
        .await
        .map_err(WaitpointWaitFailure::Vm)?;
    if states
        .iter()
        .all(|record| record.status == WaitpointStatus::Completed)
    {
        return Ok(states);
    }
    if let Some(cancelled) = states
        .iter()
        .find(|record| record.status == WaitpointStatus::Cancelled)
    {
        return Err(WaitpointWaitFailure::Cancelled {
            wait_id: String::new(),
            waitpoint_ids: vec![cancelled.id.clone()],
            reason: cancelled.reason.clone(),
        });
    }

    let Some(context) = current_dispatch_context() else {
        return wait_live_outside_dispatch(&log, waitpoint_ids, options)
            .await
            .map_err(WaitpointWaitFailure::Vm);
    };
    let wait_id = next_wait_id(Some(&context));
    if let Some(existing) = read_waiter_record(&log, &wait_id)
        .await
        .map_err(WaitpointWaitFailure::Vm)?
    {
        return resolve_existing_waiter(&log, existing)
            .await
            .map_err(|failure| match failure {
                WaitpointWaitFailure::Cancelled {
                    wait_id: _,
                    waitpoint_ids,
                    reason,
                } => WaitpointWaitFailure::Cancelled {
                    wait_id,
                    waitpoint_ids,
                    reason,
                },
                WaitpointWaitFailure::Timeout { wait_id: _ } => {
                    WaitpointWaitFailure::Timeout { wait_id }
                }
                WaitpointWaitFailure::Vm(error) => WaitpointWaitFailure::Vm(error),
            });
    }

    if current_dispatch_is_replay() {
        return Err(WaitpointWaitFailure::Vm(VmError::Runtime(format!(
            "replay is missing a recorded waitpoint resolution for '{wait_id}'"
        ))));
    }

    let waiter = WaiterRecord {
        wait_id: wait_id.clone(),
        waitpoint_ids,
        binding_id: context.binding_id.clone(),
        binding_version: context.binding_version,
        original_event_id: context
            .replay_of_event_id
            .clone()
            .unwrap_or_else(|| context.trigger_event.id.0.clone()),
        created_at: now_rfc3339(),
        timeout_at: options.timeout.map(|timeout| {
            format_timestamp(
                OffsetDateTime::now_utc() + time::Duration::try_from(timeout).unwrap_or_default(),
            )
        }),
        resolved_at: None,
        status: WaiterStatus::Pending,
        completed_ids: Vec::new(),
        cancelled_ids: Vec::new(),
        cancel_reason: None,
        event: context.trigger_event.clone(),
    };
    append_waiter_record(&log, &waiter)
        .await
        .map_err(WaitpointWaitFailure::Vm)?;
    Err(WaitpointWaitFailure::Vm(waitpoint_suspend_error(&wait_id)))
}

pub(crate) async fn complete_waitpoint(
    id: &str,
    value: Option<JsonValue>,
    actor: Option<String>,
    reason: Option<String>,
    metadata: Option<JsonValue>,
) -> Result<WaitpointRecord, VmError> {
    let log = ensure_waitpoint_event_log();
    complete_waitpoint_on(&log, id, value, actor, reason, metadata).await
}

pub(crate) async fn complete_waitpoint_on(
    log: &Arc<AnyEventLog>,
    id: &str,
    value: Option<JsonValue>,
    actor: Option<String>,
    reason: Option<String>,
    metadata: Option<JsonValue>,
) -> Result<WaitpointRecord, VmError> {
    let mut record = read_waitpoint_record(&log, id)
        .await?
        .ok_or_else(|| VmError::Runtime(format!("waitpoint.complete: unknown waitpoint '{id}'")))?;
    if record.status == WaitpointStatus::Completed {
        return Ok(record);
    }
    if record.status == WaitpointStatus::Cancelled {
        return Err(VmError::Runtime(format!(
            "waitpoint.complete: waitpoint '{id}' is already cancelled"
        )));
    }
    record.status = WaitpointStatus::Completed;
    record.completed_at = Some(now_rfc3339());
    record.completed_by = actor;
    record.value = value;
    record.reason = reason;
    if metadata.is_some() {
        record.metadata = metadata;
    }
    append_waitpoint_record(&log, &record).await?;
    trigger_waitpoint_service(&log, Some(id.to_string())).await?;
    Ok(record)
}

pub(crate) async fn cancel_waitpoint(
    id: &str,
    actor: Option<String>,
    reason: Option<String>,
    metadata: Option<JsonValue>,
) -> Result<WaitpointRecord, VmError> {
    let log = ensure_waitpoint_event_log();
    cancel_waitpoint_on(&log, id, actor, reason, metadata).await
}

pub(crate) async fn cancel_waitpoint_on(
    log: &Arc<AnyEventLog>,
    id: &str,
    actor: Option<String>,
    reason: Option<String>,
    metadata: Option<JsonValue>,
) -> Result<WaitpointRecord, VmError> {
    let mut record = read_waitpoint_record(&log, id)
        .await?
        .ok_or_else(|| VmError::Runtime(format!("waitpoint.cancel: unknown waitpoint '{id}'")))?;
    if record.status == WaitpointStatus::Cancelled {
        return Ok(record);
    }
    if record.status == WaitpointStatus::Completed {
        return Err(VmError::Runtime(format!(
            "waitpoint.cancel: waitpoint '{id}' is already completed"
        )));
    }
    record.status = WaitpointStatus::Cancelled;
    record.cancelled_at = Some(now_rfc3339());
    record.cancelled_by = actor;
    record.reason = reason;
    if metadata.is_some() {
        record.metadata = metadata;
    }
    append_waitpoint_record(&log, &record).await?;
    trigger_waitpoint_service(&log, Some(id.to_string())).await?;
    Ok(record)
}

pub(crate) async fn inspect_waitpoint_on(
    log: &Arc<AnyEventLog>,
    id: &str,
) -> Result<Option<WaitpointRecord>, VmError> {
    read_waitpoint_record(log, id).await
}

pub async fn service_waitpoints_once(
    dispatcher: &crate::Dispatcher,
    waitpoint_filter: Option<&BTreeSet<String>>,
) -> Result<usize, String> {
    let log = dispatcher.event_log_handle();
    let pending = list_pending_waiters(&log)
        .await
        .map_err(|error| error.to_string())?;
    let mut processed = 0usize;
    let mut state_cache = BTreeMap::<String, WaitpointRecord>::new();

    for waiter in pending {
        if let Some(filter) = waitpoint_filter {
            if !waiter
                .waitpoint_ids
                .iter()
                .any(|waitpoint_id| filter.contains(waitpoint_id))
            {
                continue;
            }
        }
        let resolution = evaluate_waiter(&log, &waiter, &mut state_cache)
            .await
            .map_err(|error| error.to_string())?;
        let Some(updated) = terminal_waiter_record(&waiter, resolution.clone()) else {
            continue;
        };
        append_waiter_record(&log, &updated)
            .await
            .map_err(|error| error.to_string())?;
        let binding = resolve_waiter_binding(&waiter).map_err(|error| error.to_string())?;
        append_replay_record(&log, &binding, &waiter.event, &waiter.original_event_id)
            .await
            .map_err(|error| error.to_string())?;
        dispatcher
            .dispatch_replay(
                &binding,
                waiter.event.clone(),
                waiter.original_event_id.clone(),
            )
            .await
            .map_err(|error| format!("waitpoint replay failed: {error}"))?;
        processed += 1;
    }

    Ok(processed)
}

pub fn is_waitpoint_suspension(error: &VmError) -> Option<String> {
    let VmError::Thrown(VmValue::Dict(dict)) = error else {
        return None;
    };
    let name = dict.get("name").and_then(vm_string)?;
    if name != "WaitpointSuspend" {
        return None;
    }
    dict.get("wait_id")
        .and_then(vm_string)
        .map(ToString::to_string)
}

async fn waitpoint_create_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    let options = args.first();
    let explicit_id = match options {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::String(text)) => Some(text.to_string()),
        Some(VmValue::Dict(dict)) => optional_string(dict, "id"),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "waitpoint.create: expected string id or dict, got {}",
                other.type_name()
            )))
        }
    };
    let metadata = options
        .and_then(|value| value.as_dict())
        .and_then(|dict| dict.get("metadata"))
        .map(crate::llm::vm_value_to_json);
    let record = create_waitpoint(explicit_id, metadata).await?;
    Ok(waitpoint_value(&record))
}

async fn waitpoint_wait_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    let Some(raw_waitpoints) = args.first() else {
        return Err(VmError::Runtime(
            "waitpoint.wait: missing waitpoint handle".to_string(),
        ));
    };
    let (waitpoint_ids, singular) = parse_waitpoint_handles(raw_waitpoints)?;
    let options = parse_wait_options(args.get(1))?;
    match wait_on_waitpoints(waitpoint_ids, options).await {
        Ok(records) => {
            if singular {
                return Ok(waitpoint_value(records.first().expect("single waitpoint")));
            }
            Ok(VmValue::List(Rc::new(
                records
                    .into_iter()
                    .map(|record| waitpoint_value(&record))
                    .collect(),
            )))
        }
        Err(WaitpointWaitFailure::Timeout { wait_id }) => Err(waitpoint_timeout_error(&wait_id)),
        Err(WaitpointWaitFailure::Cancelled {
            wait_id,
            waitpoint_ids,
            reason,
        }) => Err(waitpoint_cancelled_error(
            &wait_id,
            &waitpoint_ids,
            reason.as_deref(),
        )),
        Err(WaitpointWaitFailure::Vm(error)) => Err(error),
    }
}

async fn waitpoint_complete_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    let (id, _) = parse_single_waitpoint_handle(args.first(), "waitpoint.complete")?;
    let value = args.get(1).map(crate::llm::vm_value_to_json);
    let (actor, reason, metadata) = parse_terminal_options(args.get(2), "waitpoint.complete")?;
    let record = complete_waitpoint(&id, value, actor, reason, metadata).await?;
    Ok(waitpoint_value(&record))
}

async fn waitpoint_cancel_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    let (id, _) = parse_single_waitpoint_handle(args.first(), "waitpoint.cancel")?;
    let (actor, reason, metadata) = parse_terminal_options(args.get(1), "waitpoint.cancel")?;
    let record = cancel_waitpoint(&id, actor, reason, metadata).await?;
    Ok(waitpoint_value(&record))
}

async fn trigger_waitpoint_service(
    log: &Arc<AnyEventLog>,
    waitpoint_id: Option<String>,
) -> Result<(), VmError> {
    if let Some(base_vm) = clone_async_builtin_child_vm() {
        let dispatcher = crate::Dispatcher::with_event_log(base_vm, log.clone());
        let filter = waitpoint_id.map(|id| BTreeSet::from([id]));
        service_waitpoints_once(&dispatcher, filter.as_ref())
            .await
            .map_err(VmError::Runtime)?;
        return Ok(());
    }
    if let Some(waitpoint_id) = waitpoint_id {
        append_resume_request(log, &waitpoint_id, "waitpoint_changed").await?;
    }
    Ok(())
}

async fn append_resume_request(
    log: &Arc<AnyEventLog>,
    waitpoint_id: &str,
    reason: &str,
) -> Result<(), VmError> {
    let topic = Topic::new(WAITPOINT_RESUME_TOPIC)
        .map_err(|error| VmError::Runtime(format!("waitpoint: {error}")))?;
    let request = WaitpointResumeRequest {
        waitpoint_id: waitpoint_id.to_string(),
        requested_at: now_rfc3339(),
        reason: reason.to_string(),
    };
    log.append(
        &topic,
        LogEvent::new(
            "waitpoint.resume",
            serde_json::to_value(request).unwrap_or_default(),
        ),
    )
    .await
    .map(|_| ())
    .map_err(|error| VmError::Runtime(format!("waitpoint: {error}")))
}

async fn wait_live_outside_dispatch(
    log: &Arc<AnyEventLog>,
    waitpoint_ids: Vec<String>,
    options: WaitpointWaitOptions,
) -> Result<Vec<WaitpointRecord>, VmError> {
    let deadline = options.timeout.map(|timeout| InstantLike::now() + timeout);
    loop {
        let states = load_waitpoint_states(log, &waitpoint_ids).await?;
        if states
            .iter()
            .all(|record| record.status == WaitpointStatus::Completed)
        {
            return Ok(states);
        }
        if let Some(cancelled) = states
            .iter()
            .find(|record| record.status == WaitpointStatus::Cancelled)
        {
            return Err(waitpoint_cancelled_error(
                "",
                &[cancelled.id.clone()],
                cancelled.reason.as_deref(),
            ));
        }
        if deadline.is_some_and(|deadline| InstantLike::now() >= deadline) {
            return Err(waitpoint_timeout_error(""));
        }
        tokio::time::sleep(StdDuration::from_millis(100)).await;
    }
}

async fn resolve_existing_waiter(
    log: &Arc<AnyEventLog>,
    existing: WaiterRecord,
) -> Result<Vec<WaitpointRecord>, WaitpointWaitFailure> {
    match existing.status {
        WaiterStatus::Completed => load_waitpoint_states(log, &existing.waitpoint_ids)
            .await
            .map_err(WaitpointWaitFailure::Vm),
        WaiterStatus::TimedOut => Err(WaitpointWaitFailure::Timeout {
            wait_id: existing.wait_id,
        }),
        WaiterStatus::Cancelled => Err(WaitpointWaitFailure::Cancelled {
            wait_id: existing.wait_id,
            waitpoint_ids: if existing.cancelled_ids.is_empty() {
                existing.waitpoint_ids
            } else {
                existing.cancelled_ids
            },
            reason: existing.cancel_reason,
        }),
        WaiterStatus::Pending => {
            if current_dispatch_is_replay() {
                return Err(WaitpointWaitFailure::Vm(VmError::Runtime(format!(
                    "replay is missing a recorded waitpoint resolution for '{}'",
                    existing.wait_id
                ))));
            }
            Err(WaitpointWaitFailure::Vm(waitpoint_suspend_error(
                &existing.wait_id,
            )))
        }
    }
}

async fn evaluate_waiter(
    log: &Arc<AnyEventLog>,
    waiter: &WaiterRecord,
    state_cache: &mut BTreeMap<String, WaitpointRecord>,
) -> Result<WaiterResolution, VmError> {
    if dispatch_cancel_requested_for_waiter(log, waiter).await? {
        return Ok(WaiterResolution::Cancelled {
            cancelled_ids: waiter.waitpoint_ids.clone(),
            reason: Some("upstream_cancelled".to_string()),
        });
    }
    if let Some(timeout_at) = waiter
        .timeout_at
        .as_deref()
        .and_then(parse_timestamp)
        .filter(|deadline| OffsetDateTime::now_utc() >= *deadline)
    {
        let _ = timeout_at;
        return Ok(WaiterResolution::TimedOut);
    }

    let mut completed = Vec::new();
    let mut cancelled = Vec::new();
    for waitpoint_id in &waiter.waitpoint_ids {
        let record = if let Some(record) = state_cache.get(waitpoint_id) {
            record.clone()
        } else {
            let record = read_waitpoint_record(log, waitpoint_id)
                .await?
                .ok_or_else(|| {
                    VmError::Runtime(format!(
                        "waitpoint.wait: unknown waitpoint '{}'",
                        waitpoint_id
                    ))
                })?;
            state_cache.insert(waitpoint_id.clone(), record.clone());
            record
        };
        match record.status {
            WaitpointStatus::Completed => completed.push(waitpoint_id.clone()),
            WaitpointStatus::Cancelled => cancelled.push(waitpoint_id.clone()),
            WaitpointStatus::Open => {}
        }
    }
    if !cancelled.is_empty() {
        let reason = cancelled
            .first()
            .and_then(|waitpoint_id| state_cache.get(waitpoint_id))
            .and_then(|record| record.reason.clone());
        return Ok(WaiterResolution::Cancelled {
            cancelled_ids: cancelled,
            reason,
        });
    }
    if completed.len() == waiter.waitpoint_ids.len() {
        return Ok(WaiterResolution::Completed {
            completed_ids: completed,
        });
    }
    Ok(WaiterResolution::NotReady)
}

fn terminal_waiter_record(
    waiter: &WaiterRecord,
    resolution: WaiterResolution,
) -> Option<WaiterRecord> {
    let mut updated = waiter.clone();
    updated.resolved_at = Some(now_rfc3339());
    match resolution {
        WaiterResolution::NotReady => None,
        WaiterResolution::Completed { completed_ids } => {
            updated.status = WaiterStatus::Completed;
            updated.completed_ids = completed_ids;
            Some(updated)
        }
        WaiterResolution::Cancelled {
            cancelled_ids,
            reason,
        } => {
            updated.status = WaiterStatus::Cancelled;
            updated.cancelled_ids = cancelled_ids;
            updated.cancel_reason = reason;
            Some(updated)
        }
        WaiterResolution::TimedOut => {
            updated.status = WaiterStatus::TimedOut;
            Some(updated)
        }
    }
}

async fn dispatch_cancel_requested_for_waiter(
    log: &Arc<AnyEventLog>,
    waiter: &WaiterRecord,
) -> Result<bool, VmError> {
    let topic = Topic::new(crate::TRIGGER_CANCEL_REQUESTS_TOPIC)
        .map_err(|error| VmError::Runtime(format!("waitpoint.wait: {error}")))?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| VmError::Runtime(format!("waitpoint.wait: {error}")))?;
    Ok(events.into_iter().any(|(_, event)| {
        let Ok(request) = serde_json::from_value::<crate::DispatchCancelRequest>(event.payload)
        else {
            return false;
        };
        request.binding_key == format!("{}@v{}", waiter.binding_id, waiter.binding_version)
            && request.event_id == waiter.original_event_id
    }))
}

async fn resolve_ready_waits_for_waitpoint(
    dispatcher: &crate::Dispatcher,
    waitpoint_id: &str,
) -> Result<usize, String> {
    let filter = BTreeSet::from([waitpoint_id.to_string()]);
    service_waitpoints_once(dispatcher, Some(&filter)).await
}

pub async fn process_waitpoint_resume_event(
    dispatcher: &crate::Dispatcher,
    logged: LogEvent,
) -> Result<bool, String> {
    if logged.kind != "waitpoint.resume" {
        return Ok(false);
    }
    let request: WaitpointResumeRequest = serde_json::from_value(logged.payload)
        .map_err(|error| format!("failed to decode waitpoint resume event: {error}"))?;
    let _ = resolve_ready_waits_for_waitpoint(dispatcher, &request.waitpoint_id).await?;
    Ok(true)
}

async fn list_pending_waiters(log: &Arc<AnyEventLog>) -> Result<Vec<WaiterRecord>, VmError> {
    let topic = Topic::new(WAITPOINT_WAITS_TOPIC)
        .map_err(|error| VmError::Runtime(format!("waitpoint.wait: {error}")))?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| VmError::Runtime(format!("waitpoint.wait: {error}")))?;
    let mut latest = BTreeMap::<String, WaiterRecord>::new();
    for (_, event) in events {
        let Ok(record) = serde_json::from_value::<WaiterRecord>(event.payload) else {
            continue;
        };
        latest.insert(record.wait_id.clone(), record);
    }
    Ok(latest
        .into_values()
        .filter(|record| record.status == WaiterStatus::Pending)
        .collect())
}

async fn read_waitpoint_record(
    log: &Arc<AnyEventLog>,
    id: &str,
) -> Result<Option<WaitpointRecord>, VmError> {
    let topic = waitpoint_topic(id)?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| VmError::Runtime(format!("waitpoint: {error}")))?;
    Ok(events
        .into_iter()
        .filter_map(|(_, event)| serde_json::from_value::<WaitpointRecord>(event.payload).ok())
        .last())
}

async fn read_waiter_record(
    log: &Arc<AnyEventLog>,
    wait_id: &str,
) -> Result<Option<WaiterRecord>, VmError> {
    let topic = Topic::new(WAITPOINT_WAITS_TOPIC)
        .map_err(|error| VmError::Runtime(format!("waitpoint.wait: {error}")))?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| VmError::Runtime(format!("waitpoint.wait: {error}")))?;
    Ok(events
        .into_iter()
        .filter_map(|(_, event)| serde_json::from_value::<WaiterRecord>(event.payload).ok())
        .filter(|record| record.wait_id == wait_id)
        .last())
}

async fn load_waitpoint_states(
    log: &Arc<AnyEventLog>,
    waitpoint_ids: &[String],
) -> Result<Vec<WaitpointRecord>, VmError> {
    let mut records = Vec::with_capacity(waitpoint_ids.len());
    for waitpoint_id in waitpoint_ids {
        let record = read_waitpoint_record(log, waitpoint_id)
            .await?
            .ok_or_else(|| {
                VmError::Runtime(format!(
                    "waitpoint.wait: unknown waitpoint '{}'",
                    waitpoint_id
                ))
            })?;
        records.push(record);
    }
    Ok(records)
}

async fn append_waitpoint_record(
    log: &Arc<AnyEventLog>,
    record: &WaitpointRecord,
) -> Result<(), VmError> {
    let topic = waitpoint_topic(&record.id)?;
    log.append(
        &topic,
        LogEvent::new(
            "waitpoint.state",
            serde_json::to_value(record).unwrap_or_default(),
        )
        .with_headers(BTreeMap::from([(
            "waitpoint_id".to_string(),
            record.id.clone(),
        )])),
    )
    .await
    .map(|_| ())
    .map_err(|error| VmError::Runtime(format!("waitpoint: {error}")))
}

async fn append_waiter_record(
    log: &Arc<AnyEventLog>,
    record: &WaiterRecord,
) -> Result<(), VmError> {
    let topic = Topic::new(WAITPOINT_WAITS_TOPIC)
        .map_err(|error| VmError::Runtime(format!("waitpoint.wait: {error}")))?;
    let mut headers = BTreeMap::new();
    headers.insert("wait_id".to_string(), record.wait_id.clone());
    headers.insert("event_id".to_string(), record.event.id.0.clone());
    headers.insert("binding_id".to_string(), record.binding_id.clone());
    log.append(
        &topic,
        LogEvent::new(
            "waitpoint.wait",
            serde_json::to_value(record).unwrap_or_default(),
        )
        .with_headers(headers),
    )
    .await
    .map(|_| ())
    .map_err(|error| VmError::Runtime(format!("waitpoint.wait: {error}")))
}

fn resolve_waiter_binding(
    waiter: &WaiterRecord,
) -> Result<Arc<crate::triggers::registry::TriggerBinding>, crate::TriggerRegistryError> {
    resolve_live_or_as_of(
        &waiter.binding_id,
        RecordedTriggerBinding {
            version: waiter.binding_version,
            received_at: waiter.event.received_at,
        },
    )
}

async fn append_replay_record(
    log: &Arc<AnyEventLog>,
    binding: &crate::triggers::registry::TriggerBinding,
    event: &crate::TriggerEvent,
    replay_of_event_id: &str,
) -> Result<(), VmError> {
    let topic = Topic::new(TRIGGER_EVENTS_TOPIC)
        .map_err(|error| VmError::Runtime(format!("waitpoint.wait: {error}")))?;
    let payload = json!({
        "binding_id": binding.id.as_str(),
        "binding_version": binding.version,
        "replay_of_event_id": replay_of_event_id,
        "event": event,
    });
    log.append(&topic, LogEvent::new("trigger_event", payload))
        .await
        .map(|_| ())
        .map_err(|error| VmError::Runtime(format!("waitpoint.wait: {error}")))
}

fn ensure_waitpoint_event_log() -> Arc<AnyEventLog> {
    active_event_log()
        .unwrap_or_else(|| install_memory_for_current_thread(WAITPOINT_EVENT_LOG_QUEUE_DEPTH))
}

fn waitpoint_topic(id: &str) -> Result<Topic, VmError> {
    Topic::new(format!("waitpoint.state.{}", sanitize_topic_component(id)))
        .map_err(|error| VmError::Runtime(format!("waitpoint: {error}")))
}

fn normalize_waitpoint_ids(waitpoint_ids: Vec<String>) -> Result<Vec<String>, VmError> {
    let mut normalized = waitpoint_ids
        .into_iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    if normalized.is_empty() {
        return Err(VmError::Runtime(
            "waitpoint.wait: expected at least one waitpoint".to_string(),
        ));
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn parse_waitpoint_handles(value: &VmValue) -> Result<(Vec<String>, bool), VmError> {
    match value {
        VmValue::List(list) => Ok((
            list.iter()
                .map(|item| parse_single_waitpoint_handle(Some(item), "waitpoint.wait"))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|(id, _)| id)
                .collect(),
            false,
        )),
        _ => Ok((
            vec![parse_single_waitpoint_handle(Some(value), "waitpoint.wait")?.0],
            true,
        )),
    }
}

fn parse_single_waitpoint_handle(
    value: Option<&VmValue>,
    builtin: &str,
) -> Result<(String, bool), VmError> {
    let Some(value) = value else {
        return Err(VmError::Runtime(format!(
            "{builtin}: missing waitpoint handle"
        )));
    };
    match value {
        VmValue::String(text) => Ok((text.to_string(), true)),
        VmValue::Dict(dict) => Ok((required_string(dict, "id", builtin)?, false)),
        other => Err(VmError::Runtime(format!(
            "{builtin}: expected waitpoint id string or dict, got {}",
            other.type_name()
        ))),
    }
}

fn parse_wait_options(value: Option<&VmValue>) -> Result<WaitpointWaitOptions, VmError> {
    let Some(value) = value else {
        return Ok(WaitpointWaitOptions::default());
    };
    let dict = value
        .as_dict()
        .ok_or_else(|| VmError::Runtime("waitpoint.wait: options must be a dict".to_string()))?;
    Ok(WaitpointWaitOptions {
        timeout: dict.get("timeout").map(parse_duration_value).transpose()?,
    })
}

fn parse_terminal_options(
    value: Option<&VmValue>,
    builtin: &str,
) -> Result<(Option<String>, Option<String>, Option<JsonValue>), VmError> {
    let Some(value) = value else {
        return Ok((None, None, None));
    };
    let dict = value
        .as_dict()
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: options must be a dict")))?;
    Ok((
        optional_string(dict, "by"),
        optional_string(dict, "reason"),
        dict.get("metadata").map(crate::llm::vm_value_to_json),
    ))
}

fn waitpoint_value(record: &WaitpointRecord) -> VmValue {
    crate::stdlib::json_to_vm_value(&json!({
        "id": record.id,
        "status": record.status.as_str(),
        "created_at": record.created_at,
        "completed_at": record.completed_at,
        "cancelled_at": record.cancelled_at,
        "completed_by": record.completed_by,
        "cancelled_by": record.cancelled_by,
        "value": record.value,
        "reason": record.reason,
        "metadata": record.metadata,
    }))
}

fn next_waitpoint_id() -> String {
    if let Some(keys) = current_dispatch_keys() {
        return format!(
            "waitpoint_{}_{}",
            keys.stable_base,
            next_sequence(&CREATE_SEQUENCE, &keys)
        );
    }
    format!("waitpoint_{}", Uuid::now_v7())
}

fn next_wait_id(context: Option<&DispatchContext>) -> String {
    if let Some(context) = context {
        let keys = dispatch_keys(context);
        return format!(
            "wait_{}_{}",
            keys.stable_base,
            next_sequence(&WAIT_SEQUENCE, &keys)
        );
    }
    format!("wait_{}", Uuid::now_v7())
}

fn current_dispatch_keys() -> Option<DispatchKeys> {
    current_dispatch_context().map(|context| dispatch_keys(&context))
}

fn dispatch_keys(context: &DispatchContext) -> DispatchKeys {
    let stable_base = context
        .replay_of_event_id
        .clone()
        .unwrap_or_else(|| context.trigger_event.id.0.clone());
    let instance_key = format!(
        "{}::{}",
        context.trigger_event.id.0,
        context.replay_of_event_id.as_deref().unwrap_or("live")
    );
    DispatchKeys {
        instance_key,
        stable_base,
    }
}

fn next_sequence(
    slot: &'static std::thread::LocalKey<RefCell<SequenceState>>,
    keys: &DispatchKeys,
) -> u64 {
    slot.with(|slot| {
        let mut state = slot.borrow_mut();
        if state.instance_key != keys.instance_key {
            state.instance_key = keys.instance_key.clone();
            state.next_seq = 0;
        }
        state.next_seq += 1;
        state.next_seq
    })
}

fn required_string(
    dict: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<String, VmError> {
    dict.get(key)
        .and_then(|value| match value {
            VmValue::String(text) if !text.is_empty() => Some(text.to_string()),
            _ => None,
        })
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing string field `{key}`")))
}

fn optional_string(dict: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    dict.get(key).and_then(|value| match value {
        VmValue::String(text) if !text.is_empty() => Some(text.to_string()),
        _ => None,
    })
}

fn parse_duration_value(value: &VmValue) -> Result<StdDuration, VmError> {
    match value {
        VmValue::Duration(ms) => Ok(StdDuration::from_millis(*ms)),
        VmValue::Int(ms) if *ms >= 0 => Ok(StdDuration::from_millis(*ms as u64)),
        VmValue::Float(ms) if *ms >= 0.0 => Ok(StdDuration::from_millis(*ms as u64)),
        _ => Err(VmError::Runtime(
            "waitpoint.wait: expected a duration or millisecond count".to_string(),
        )),
    }
}

fn waitpoint_suspend_error(wait_id: &str) -> VmError {
    VmError::Thrown(crate::stdlib::json_to_vm_value(&json!({
        "name": "WaitpointSuspend",
        "category": "generic",
        "message": "waitpoint suspended dispatch for later resume",
        "wait_id": wait_id,
    })))
}

fn waitpoint_timeout_error(wait_id: &str) -> VmError {
    let _ = categorized_error("waitpoint timed out", ErrorCategory::Timeout);
    VmError::Thrown(crate::stdlib::json_to_vm_value(&json!({
        "name": "WaitpointTimeoutError",
        "category": ErrorCategory::Timeout.as_str(),
        "message": "waitpoint timed out",
        "wait_id": wait_id,
    })))
}

fn waitpoint_cancelled_error(
    wait_id: &str,
    waitpoint_ids: &[String],
    reason: Option<&str>,
) -> VmError {
    let _ = categorized_error("waitpoint cancelled", ErrorCategory::Cancelled);
    VmError::Thrown(crate::stdlib::json_to_vm_value(&json!({
        "name": "WaitpointCancelledError",
        "category": ErrorCategory::Cancelled.as_str(),
        "message": reason.unwrap_or("waitpoint was cancelled"),
        "wait_id": wait_id,
        "waitpoint_ids": waitpoint_ids,
        "reason": reason,
    })))
}

fn parse_timestamp(raw: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(raw, &Rfc3339).ok()
}

fn format_timestamp(value: OffsetDateTime) -> String {
    value.format(&Rfc3339).unwrap_or_else(|_| value.to_string())
}

fn now_rfc3339() -> String {
    format_timestamp(OffsetDateTime::now_utc())
}

fn vm_string(value: &VmValue) -> Option<&str> {
    match value {
        VmValue::String(text) => Some(text.as_ref()),
        _ => None,
    }
}

struct InstantLike;

impl InstantLike {
    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{compile_source, register_vm_stdlib, reset_thread_local_state, Vm};

    async fn execute(source: &str) -> Result<String, VmError> {
        reset_thread_local_state();
        let chunk = compile_source(source).expect("compile source");
        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        vm.execute(&chunk).await?;
        Ok(vm.output().trim_end().to_string())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn waitpoint_module_round_trip_outside_dispatch() {
        let output = execute(
            r#"
import { create, complete, wait } from "std/waitpoint"

pipeline test(task) {
  let wp = create("outside-dispatch")
  complete(wp, 9)
  let resolved = wait(wp)
  println(resolved.value)
}
"#,
        )
        .await
        .expect("script succeeds");
        assert_eq!(output, "9");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn waiter_reads_latest_terminal_record() {
        reset_thread_local_state();
        let dir = tempfile::tempdir().expect("tempdir");
        let log =
            crate::event_log::install_default_for_base_dir(dir.path()).expect("install event log");

        let pending = WaiterRecord {
            wait_id: "wait_test".to_string(),
            waitpoint_ids: vec!["shared".to_string()],
            binding_id: "binding".to_string(),
            binding_version: 1,
            original_event_id: "event-original".to_string(),
            created_at: now_rfc3339(),
            timeout_at: None,
            resolved_at: None,
            status: WaiterStatus::Pending,
            completed_ids: Vec::new(),
            cancelled_ids: Vec::new(),
            cancel_reason: None,
            event: crate::TriggerEvent::new(
                crate::triggers::ProviderId::from("github"),
                "issues.opened",
                None,
                "waitpoint-test",
                None,
                BTreeMap::new(),
                crate::triggers::ProviderPayload::Known(
                    crate::triggers::event::KnownProviderPayload::Webhook(
                        crate::triggers::event::GenericWebhookPayload {
                            source: Some("test".to_string()),
                            content_type: Some("application/json".to_string()),
                            raw: JsonValue::Null,
                        },
                    ),
                ),
                crate::triggers::SignatureStatus::Verified,
            ),
        };
        append_waiter_record(&log, &pending)
            .await
            .expect("append pending waiter");

        let mut completed = pending.clone();
        completed.status = WaiterStatus::Completed;
        completed.resolved_at = Some(now_rfc3339());
        completed.completed_ids = vec!["shared".to_string()];
        append_waiter_record(&log, &completed)
            .await
            .expect("append completed waiter");

        let latest = read_waiter_record(&log, "wait_test")
            .await
            .expect("read waiter")
            .expect("latest waiter record exists");
        assert_eq!(latest.status, WaiterStatus::Completed);
        assert_eq!(latest.completed_ids, vec!["shared".to_string()]);
    }
}
