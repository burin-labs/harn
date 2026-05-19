use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use harn_parser::diagnostic_codes::Code;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::event_log::{
    active_event_log, install_memory_for_current_thread, EventLog, LogEvent, Topic,
};
use crate::triggers::dispatcher::current_dispatch_context;
use crate::triggers::dispatcher::DEFAULT_MAX_ATTEMPTS;
use crate::triggers::registry::{AgentScope, TargetExpr};
use crate::triggers::test_util::{clock, run_trigger_harness_fixture};
use crate::triggers::{
    deregister_webhook_intake, dynamic_deregister, dynamic_register, feed_webhook_intake,
    recent_webhook_deliveries, register_webhook_intake, registered_provider_metadata,
    resolve_live_or_as_of, resolve_live_trigger_binding, snapshot_trigger_bindings,
    snapshot_webhook_intakes, HmacAlgorithm, RecordedTriggerBinding, RetryPolicy,
    SignatureEncoding, TriggerBindingSnapshot, TriggerBindingSource, TriggerBindingSpec,
    TriggerEvent, TriggerEventId, TriggerHandlerSpec, TriggerPredicateSpec, TriggerRegistryError,
    TriggerRetryConfig, WebhookIntakeConfig, WebhookIntakeError, WebhookIntakeRequest,
    TRIGGERS_LIFECYCLE_TOPIC, TRIGGER_DLQ_TOPIC,
};
use crate::trust_graph::{
    group_trust_records_by_trace, policy_for_agent, query_trust_graph_records, query_trust_records,
    trust_score_for, verify_trust_chain, AutonomyTier, TrustOutcome, TrustQueryFilters,
    TrustRecord,
};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;
use crate::TriggerPredicateBudget;

const ACTION_GRAPH_TOPIC: &str = "observability.action_graph";
const TRIGGER_EVENTS_TOPIC: &str = "triggers.events";
const TRIGGER_EVENT_LOG_QUEUE_DEPTH: usize = 128;

thread_local! {
    static AUTO_RESUME_TIMEOUTS: RefCell<BTreeMap<String, tokio::task::JoinHandle<()>>> =
        const { RefCell::new(BTreeMap::new()) };
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AutoResumeTriggerHandle {
    pub(crate) id: String,
    pub(crate) version: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TriggerEventRecord {
    binding_id: String,
    binding_version: u32,
    replay_of_event_id: Option<String>,
    event: TriggerEvent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DispatchHandleRecord {
    event_id: String,
    binding_id: String,
    binding_version: u32,
    status: String,
    replay_of_event_id: Option<String>,
    dlq_entry_id: Option<String>,
    error: Option<String>,
    result: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DlqAttemptRecord {
    attempt: u32,
    at: String,
    status: String,
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DlqEntryRecord {
    id: String,
    event_id: String,
    binding_id: String,
    binding_version: u32,
    provider: String,
    kind: String,
    state: String,
    error: String,
    #[serde(default = "default_dlq_error_class")]
    error_class: String,
    event: TriggerEvent,
    retry_history: Vec<DlqAttemptRecord>,
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

pub(crate) fn register_trigger_builtins(vm: &mut Vm) {
    register_trust_namespace(vm);
    register_corrections_namespace(vm);

    vm.register_builtin("handler_context", |_args, _out| {
        let Some(context) = current_dispatch_context() else {
            return Ok(VmValue::Nil);
        };
        Ok(value_from_serde(&serde_json::json!({
            "agent": context.agent_id,
            "action": context.action,
            "trace_id": context.trigger_event.trace_id.0,
            "replay_of_event_id": context.replay_of_event_id,
            "autonomy_tier": context.autonomy_tier,
            "trigger_event": context.trigger_event,
        })))
    });

    vm.register_builtin("list_providers_native", |_args, _out| {
        Ok(VmValue::List(Rc::new(
            registered_provider_metadata()
                .into_iter()
                .map(|provider| value_from_serde(&provider))
                .collect(),
        )))
    });

    vm.register_builtin("trigger_list", |_args, _out| {
        Ok(VmValue::List(Rc::new(
            snapshot_trigger_bindings()
                .into_iter()
                .map(|binding| value_from_serde(&binding))
                .collect(),
        )))
    });

    vm.register_async_builtin("trigger_register", |args| async move {
        let config = require_dict_arg(&args, 0, "trigger_register")?;
        let spec = parse_trigger_config(config)?;
        let id = dynamic_register(spec)
            .await
            .map_err(trigger_registry_error)?;
        let binding =
            resolve_live_trigger_binding(id.as_str(), None).map_err(trigger_registry_error)?;
        Ok(value_from_serde(&binding.snapshot()))
    });

    vm.register_async_builtin("trigger_fire", |args| async move {
        let (binding_id, binding_version) = trigger_handle_from_args(&args, "trigger_fire")?;
        let raw_event = args
            .get(1)
            .ok_or_else(|| VmError::Runtime("trigger_fire: missing trigger event".to_string()))?;
        let event = parse_trigger_event(raw_event)?;
        dispatch_trigger_event(binding_id, binding_version, event, None, None).await
    });

    vm.register_async_builtin("trigger_replay", |args| async move {
        let event_id = args
            .first()
            .and_then(|value| match value {
                VmValue::String(text) => Some(text.to_string()),
                _ => None,
            })
            .ok_or_else(|| {
                VmError::Runtime("trigger_replay: expected event id string".to_string())
            })?;
        replay_trigger_event(&event_id).await
    });

    vm.register_async_builtin("trigger_inspect_dlq", |_args| async move {
        let entries = inspect_dlq_entries().await?;
        Ok(VmValue::List(Rc::new(
            entries
                .into_iter()
                .map(|entry| value_from_serde(&entry))
                .collect(),
        )))
    });

    vm.register_async_builtin("trigger_inspect_lifecycle", |args| async move {
        let kind = args.first().and_then(|value| match value {
            VmValue::String(text) => Some(text.to_string()),
            VmValue::Nil => None,
            _ => None,
        });
        let entries = inspect_lifecycle_events(kind.as_deref()).await?;
        Ok(VmValue::List(Rc::new(
            entries
                .into_iter()
                .map(|entry| value_from_serde(&entry))
                .collect(),
        )))
    });

    vm.register_async_builtin("trigger_inspect_action_graph", |args| async move {
        let trace_id = args.first().and_then(|value| match value {
            VmValue::String(text) => Some(text.to_string()),
            VmValue::Nil => None,
            _ => None,
        });
        let entries = inspect_action_graph_events(trace_id.as_deref()).await?;
        Ok(VmValue::List(Rc::new(
            entries
                .into_iter()
                .map(|entry| value_from_serde(&entry))
                .collect(),
        )))
    });

    vm.register_async_builtin("trust_record", |args| async move {
        append_trust_record_from_parts("trust_record", &args)
            .await
            .map(|record| value_from_serde(&record))
    });

    vm.register_async_builtin("trust_graph_record", |args| async move {
        let decision = args.first().ok_or_else(|| {
            VmError::Runtime("trust_graph_record: expected decision dict".to_string())
        })?;
        let record = append_trust_record_from_decision_for("trust_graph_record", decision).await?;
        Ok(VmValue::String(Rc::from(record.record_id)))
    });

    vm.register_async_builtin("trust_query", |args| async move {
        let filters = args
            .first()
            .map(parse_trust_query_filters)
            .transpose()?
            .unwrap_or_default();
        let log = ensure_trigger_event_log();
        let records = query_trust_records(&log, &filters)
            .await
            .map_err(|error| VmError::Runtime(format!("trust_query: {error}")))?;
        if filters.grouped_by_trace {
            return Ok(value_from_serde(&group_trust_records_by_trace(&records)));
        }
        Ok(VmValue::List(Rc::new(
            records
                .into_iter()
                .map(|record| value_from_serde(&record))
                .collect(),
        )))
    });

    vm.register_async_builtin("trust_graph_query", |args| async move {
        let agent = required_string_arg(&args, 0, "trust_graph_query", "agent")?;
        let action = optional_string_arg(&args, 1, "trust_graph_query", "action")?;
        let log = ensure_trigger_event_log();
        let score = trust_score_for(&log, &agent, action.as_deref())
            .await
            .map_err(|error| VmError::Runtime(format!("trust_graph_query: {error}")))?;
        Ok(value_from_serde(&score))
    });

    vm.register_async_builtin("trust_graph_policy_for", |args| async move {
        let agent = required_string_arg(&args, 0, "trust_graph_policy_for", "agent")?;
        let log = ensure_trigger_event_log();
        let policy = policy_for_agent(&log, &agent)
            .await
            .map_err(|error| VmError::Runtime(format!("trust_graph_policy_for: {error}")))?;
        Ok(value_from_serde(&policy))
    });

    vm.register_async_builtin("trust_graph_verify_chain", |_args| async move {
        let log = ensure_trigger_event_log();
        let report = verify_trust_chain(&log)
            .await
            .map_err(|error| VmError::Runtime(format!("trust_graph_verify_chain: {error}")))?;
        Ok(value_from_serde(&report))
    });

    vm.register_async_builtin("trust.query", |args| async move {
        let filters = args
            .first()
            .map(parse_trust_query_filters)
            .transpose()?
            .unwrap_or_default();
        let log = ensure_trigger_event_log();
        let records = query_trust_graph_records(&log, &filters)
            .await
            .map_err(|error| VmError::Runtime(format!("trust.query: {error}")))?;
        Ok(VmValue::List(Rc::new(
            records
                .into_iter()
                .map(|record| value_from_serde(&record))
                .collect(),
        )))
    });

    vm.register_async_builtin("trust.record", |args| async move {
        let decision = args
            .first()
            .ok_or_else(|| VmError::Runtime("trust.record: expected decision dict".to_string()))?;
        let record = append_trust_record_from_decision_for("trust.record", decision).await?;
        Ok(VmValue::String(Rc::from(record.record_id)))
    });

    vm.register_async_builtin("trust.score", |args| async move {
        let agent = required_string_arg(&args, 0, "trust.score", "actor_id")?;
        let action = optional_string_arg(&args, 1, "trust.score", "action")?;
        let log = ensure_trigger_event_log();
        let score = trust_score_for(&log, &agent, action.as_deref())
            .await
            .map_err(|error| VmError::Runtime(format!("trust.score: {error}")))?;
        Ok(value_from_serde(&score))
    });

    vm.register_async_builtin("trust.policy_for", |args| async move {
        let agent = required_string_arg(&args, 0, "trust.policy_for", "actor_id")?;
        let log = ensure_trigger_event_log();
        let policy = policy_for_agent(&log, &agent)
            .await
            .map_err(|error| VmError::Runtime(format!("trust.policy_for: {error}")))?;
        Ok(value_from_serde(&policy))
    });

    vm.register_async_builtin("trust.verify_chain", |_args| async move {
        let log = ensure_trigger_event_log();
        let report = verify_trust_chain(&log)
            .await
            .map_err(|error| VmError::Runtime(format!("trust.verify_chain: {error}")))?;
        Ok(value_from_serde(&report))
    });

    vm.register_async_builtin("correction_record", |args| async move {
        let value = args.first().ok_or_else(|| {
            VmError::Runtime("correction_record: expected correction dict".to_string())
        })?;
        let record = correction_record_from_value("correction_record", value)?;
        let log = ensure_trigger_event_log();
        let record = crate::append_correction_record(&log, &record)
            .await
            .map_err(|error| VmError::Runtime(format!("correction_record: {error}")))?;
        Ok(value_from_serde(&record))
    });

    vm.register_async_builtin("correction_query", |args| async move {
        let filters = args
            .first()
            .map(|value| correction_query_filters_from_value("correction_query", value))
            .transpose()?
            .unwrap_or_default();
        let log = ensure_trigger_event_log();
        let records = crate::query_correction_records(&log, &filters)
            .await
            .map_err(|error| VmError::Runtime(format!("correction_query: {error}")))?;
        Ok(VmValue::List(Rc::new(
            records
                .into_iter()
                .map(|record| value_from_serde(&record))
                .collect(),
        )))
    });

    vm.register_async_builtin("corrections.record", |args| async move {
        let value = args.first().ok_or_else(|| {
            VmError::Runtime("corrections.record: expected correction dict".to_string())
        })?;
        let record = correction_record_from_value("corrections.record", value)?;
        let log = ensure_trigger_event_log();
        let record = crate::append_correction_record(&log, &record)
            .await
            .map_err(|error| VmError::Runtime(format!("corrections.record: {error}")))?;
        Ok(VmValue::String(Rc::from(record.correction_id)))
    });

    vm.register_async_builtin("corrections.query", |args| async move {
        let filters = args
            .first()
            .map(|value| correction_query_filters_from_value("corrections.query", value))
            .transpose()?
            .unwrap_or_default();
        let log = ensure_trigger_event_log();
        let records = crate::query_correction_records(&log, &filters)
            .await
            .map_err(|error| VmError::Runtime(format!("corrections.query: {error}")))?;
        Ok(VmValue::List(Rc::new(
            records
                .into_iter()
                .map(|record| value_from_serde(&record))
                .collect(),
        )))
    });

    vm.register_async_builtin("webhook_intake_register", |args| async move {
        let config = require_dict_arg(&args, 0, "webhook_intake_register")?;
        let parsed = parse_webhook_intake_config(config)?;
        let snapshot = register_webhook_intake(parsed).map_err(webhook_intake_error)?;
        Ok(value_from_serde(&snapshot))
    });

    vm.register_async_builtin("webhook_intake_feed", |args| async move {
        let intake_id = required_string_arg(&args, 0, "webhook_intake_feed", "intake_id")?;
        let request_dict = require_dict_arg(&args, 1, "webhook_intake_feed")?;
        let request = parse_webhook_intake_request(request_dict)?;
        let outcome = feed_webhook_intake(&intake_id, request)
            .await
            .map_err(webhook_intake_error)?;
        Ok(value_from_serde(&outcome))
    });

    vm.register_async_builtin("webhook_intake_deregister", |args| async move {
        let intake_id = required_string_arg(&args, 0, "webhook_intake_deregister", "intake_id")?;
        Ok(VmValue::Bool(deregister_webhook_intake(&intake_id)))
    });

    vm.register_builtin("webhook_intake_list", |_args, _out| {
        Ok(VmValue::List(Rc::new(
            snapshot_webhook_intakes()
                .into_iter()
                .map(|snapshot| value_from_serde(&snapshot))
                .collect(),
        )))
    });

    vm.register_async_builtin("webhook_intake_recent", |args| async move {
        let intake_id = required_string_arg(&args, 0, "webhook_intake_recent", "intake_id")?;
        let limit = match args.get(1) {
            Some(VmValue::Int(value)) if *value >= 0 => *value as usize,
            Some(VmValue::Nil) | None => 32,
            Some(other) => {
                return Err(VmError::Runtime(format!(
                    "webhook_intake_recent: limit must be a non-negative int, got {}",
                    other.type_name()
                )))
            }
        };
        let entries = recent_webhook_deliveries(&intake_id, limit)
            .await
            .map_err(webhook_intake_error)?;
        Ok(VmValue::List(Rc::new(
            entries
                .iter()
                .map(crate::stdlib::json_to_vm_value)
                .collect(),
        )))
    });

    vm.register_async_builtin("trigger_test_harness", |args| async move {
        let fixture = match args.first() {
            Some(VmValue::String(text)) => text.to_string(),
            Some(VmValue::Dict(map)) => required_string(map, "fixture", "trigger_test_harness")?,
            Some(other) => {
                return Err(VmError::Runtime(format!(
                    "trigger_test_harness: expected fixture string or dict, got {}",
                    other.type_name()
                )))
            }
            None => {
                return Err(VmError::Runtime(
                    "trigger_test_harness: missing fixture name".to_string(),
                ))
            }
        };
        let result = run_trigger_harness_fixture(&fixture)
            .await
            .map_err(|error| VmError::Runtime(format!("trigger_test_harness: {error}")))?;
        Ok(value_from_serde(&result))
    });
}

fn register_trust_namespace(vm: &mut Vm) {
    let names = ["query", "record", "score", "policy_for", "verify_chain"];
    vm.set_global(
        "trust",
        VmValue::Dict(Rc::new(
            std::iter::once(("_namespace".to_string(), VmValue::String(Rc::from("trust"))))
                .chain(names.into_iter().map(|name| {
                    (
                        name.to_string(),
                        VmValue::BuiltinRef(Rc::from(format!("trust.{name}"))),
                    )
                }))
                .collect::<BTreeMap<_, _>>(),
        )),
    );
}

fn register_corrections_namespace(vm: &mut Vm) {
    let names = ["query", "record"];
    vm.set_global(
        "corrections",
        VmValue::Dict(Rc::new(
            std::iter::once((
                "_namespace".to_string(),
                VmValue::String(Rc::from("corrections")),
            ))
            .chain(names.into_iter().map(|name| {
                (
                    name.to_string(),
                    VmValue::BuiltinRef(Rc::from(format!("corrections.{name}"))),
                )
            }))
            .collect::<BTreeMap<_, _>>(),
        )),
    );
}

async fn dispatch_trigger_event(
    binding_id: String,
    binding_version: Option<u32>,
    event: TriggerEvent,
    replay_of_event_id: Option<String>,
    replay_received_at: Option<OffsetDateTime>,
) -> Result<VmValue, VmError> {
    let log = ensure_trigger_event_log();
    let binding = resolve_dispatch_binding(&binding_id, binding_version, replay_received_at)
        .map_err(trigger_registry_error)?;
    let version = binding.version;
    let event_id = event.id.0.clone();

    append_log(
        &log,
        TRIGGER_EVENTS_TOPIC,
        LogEvent::new(
            "trigger_event",
            serde_json::to_value(TriggerEventRecord {
                binding_id: binding.id.as_str().to_string(),
                binding_version: version,
                replay_of_event_id: replay_of_event_id.clone(),
                event: event.clone(),
            })
            .unwrap_or_default(),
        ),
    )
    .await?;
    let existing_dlq_entry = find_pending_dlq_entry_for_event(&event_id).await?;
    let dispatch_outcome =
        dispatch_binding_via_dispatcher(&binding, &event, replay_of_event_id.clone()).await?;
    let handle = dispatch_handle_from_outcome(
        &binding.snapshot(),
        &event_id,
        dispatch_outcome,
        existing_dlq_entry,
        &log,
        &event,
        replay_of_event_id,
    )
    .await?;

    Ok(value_from_serde(&handle))
}

async fn replay_trigger_event(event_id: &str) -> Result<VmValue, VmError> {
    let record = find_replayable_event(event_id).await?;
    let received_at = record.event.received_at;
    dispatch_trigger_event(
        record.binding_id,
        Some(record.binding_version),
        record.event,
        Some(event_id.to_string()),
        Some(received_at),
    )
    .await
}

fn resolve_dispatch_binding(
    binding_id: &str,
    binding_version: Option<u32>,
    replay_received_at: Option<OffsetDateTime>,
) -> Result<std::sync::Arc<crate::triggers::registry::TriggerBinding>, TriggerRegistryError> {
    match (binding_version, replay_received_at) {
        (Some(version), Some(received_at)) => resolve_live_or_as_of(
            binding_id,
            RecordedTriggerBinding {
                version,
                received_at,
            },
        ),
        _ => resolve_live_trigger_binding(binding_id, binding_version),
    }
}

async fn dispatch_binding_via_dispatcher(
    binding: &crate::triggers::registry::TriggerBinding,
    event: &TriggerEvent,
    replay_of_event_id: Option<String>,
) -> Result<crate::triggers::DispatchOutcome, VmError> {
    let base_vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime("trigger stdlib builtins require an async builtin VM context".to_string())
    })?;
    let dispatcher =
        crate::triggers::Dispatcher::with_event_log(base_vm, ensure_trigger_event_log());
    let dispatch_result = if let Some(replay_of_event_id) = replay_of_event_id {
        dispatcher
            .dispatch_replay(binding, event.clone(), replay_of_event_id)
            .await
    } else {
        dispatcher.dispatch(binding, event.clone()).await
    };
    dispatch_result.map_err(|error| VmError::Runtime(format!("trigger stdlib: {error}")))
}

async fn dispatch_handle_from_outcome(
    binding: &TriggerBindingSnapshot,
    event_id: &str,
    outcome: crate::triggers::DispatchOutcome,
    existing_dlq_entry: Option<DlqEntryRecord>,
    log: &std::sync::Arc<crate::event_log::AnyEventLog>,
    event: &TriggerEvent,
    replay_of_event_id: Option<String>,
) -> Result<DispatchHandleRecord, VmError> {
    let prior_dlq_entry_id = existing_dlq_entry.as_ref().map(|entry| entry.id.clone());
    let prior_retry_history = existing_dlq_entry
        .as_ref()
        .map(|entry| entry.retry_history.clone())
        .unwrap_or_default();
    match outcome.status {
        crate::triggers::DispatchStatus::Succeeded | crate::triggers::DispatchStatus::Skipped => {
            if let Some(existing) = existing_dlq_entry {
                resolve_dlq_entry(log, existing, replay_of_event_id.clone()).await?;
            }
            Ok(DispatchHandleRecord {
                event_id: event_id.to_string(),
                binding_id: binding.id.clone(),
                binding_version: binding.version,
                status: "dispatched".to_string(),
                replay_of_event_id,
                dlq_entry_id: None,
                error: None,
                result: outcome.result,
            })
        }
        crate::triggers::DispatchStatus::Dlq => {
            let dlq_entry = upsert_dlq_entry(
                log,
                binding,
                event,
                outcome
                    .error
                    .as_deref()
                    .unwrap_or("trigger dispatch failed"),
                replay_of_event_id.clone(),
                prior_dlq_entry_id,
                prior_retry_history,
            )
            .await?;
            Ok(DispatchHandleRecord {
                event_id: event_id.to_string(),
                binding_id: binding.id.clone(),
                binding_version: binding.version,
                status: "dlq".to_string(),
                replay_of_event_id,
                dlq_entry_id: Some(dlq_entry.id),
                error: outcome.error,
                result: None,
            })
        }
        crate::triggers::DispatchStatus::Failed => Ok(DispatchHandleRecord {
            event_id: event_id.to_string(),
            binding_id: binding.id.clone(),
            binding_version: binding.version,
            status: "failed".to_string(),
            replay_of_event_id,
            dlq_entry_id: None,
            error: outcome.error,
            result: None,
        }),
        crate::triggers::DispatchStatus::Cancelled => Ok(DispatchHandleRecord {
            event_id: event_id.to_string(),
            binding_id: binding.id.clone(),
            binding_version: binding.version,
            status: "cancelled".to_string(),
            replay_of_event_id,
            dlq_entry_id: None,
            error: outcome.error,
            result: None,
        }),
        crate::triggers::DispatchStatus::Waiting => Ok(DispatchHandleRecord {
            event_id: event_id.to_string(),
            binding_id: binding.id.clone(),
            binding_version: binding.version,
            status: "waiting".to_string(),
            replay_of_event_id,
            dlq_entry_id: None,
            error: None,
            result: outcome.result,
        }),
    }
}

async fn find_replayable_event(event_id: &str) -> Result<TriggerEventRecord, VmError> {
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

async fn find_pending_dlq_entry_for_event(
    event_id: &str,
) -> Result<Option<DlqEntryRecord>, VmError> {
    Ok(inspect_dlq_entries()
        .await?
        .into_iter()
        .find(|entry| entry.event_id == event_id))
}

async fn upsert_dlq_entry(
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

async fn resolve_dlq_entry(
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

fn ensure_trigger_event_log() -> std::sync::Arc<crate::event_log::AnyEventLog> {
    active_event_log()
        .unwrap_or_else(|| install_memory_for_current_thread(TRIGGER_EVENT_LOG_QUEUE_DEPTH))
}

async fn append_log(
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

fn trigger_handle_from_args(
    args: &[VmValue],
    builtin: &str,
) -> Result<(String, Option<u32>), VmError> {
    let handle = args
        .first()
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing trigger handle")))?;
    match handle {
        VmValue::String(text) => Ok((text.to_string(), None)),
        VmValue::Dict(map) => {
            let id = map
                .get("id")
                .and_then(|value| match value {
                    VmValue::String(text) => Some(text.to_string()),
                    _ => None,
                })
                .ok_or_else(|| {
                    VmError::Runtime(format!(
                        "{builtin}: trigger handle is missing string field `id`"
                    ))
                })?;
            let version = map
                .get("version")
                .and_then(VmValue::as_int)
                .map(|value| value as u32);
            Ok((id, version))
        }
        other => Err(VmError::Runtime(format!(
            "{builtin}: expected trigger handle dict or id string, got {}",
            other.type_name()
        ))),
    }
}

fn parse_trigger_config(config: &BTreeMap<String, VmValue>) -> Result<TriggerBindingSpec, VmError> {
    let id = optional_string(config, "id").unwrap_or_default();
    let kind = required_string(config, "kind", "trigger_register")?;
    let provider =
        crate::ProviderId::from(required_string(config, "provider", "trigger_register")?);
    let allow_cleartext =
        optional_bool(config, "allow_cleartext", "trigger_register")?.unwrap_or(false);
    let (handler, handler_descriptor) = parse_handler_value(
        config
            .get("handler")
            .ok_or_else(|| VmError::Runtime("trigger_register: missing `handler`".to_string()))?,
        "trigger_register",
        "handler",
        allow_cleartext,
    )?;
    let when = match config.get("when") {
        Some(VmValue::Nil) | None => None,
        Some(value) => {
            let (handler, _) = parse_handler_value(value, "trigger_register", "when", false)?;
            match handler {
                TriggerHandlerSpec::Local { raw, closure } => {
                    Some(TriggerPredicateSpec { raw, closure })
                }
                _ => {
                    return Err(VmError::Runtime(
                        "trigger_register: `when` must be a closure".to_string(),
                    ))
                }
            }
        }
    };
    let match_events = config
        .get("match")
        .and_then(|value| match value {
            VmValue::Dict(map) => map.get("events"),
            _ => None,
        })
        .or_else(|| config.get("events"))
        .map(parse_string_list)
        .transpose()?
        .unwrap_or_default();
    let autonomy_tier = match config.get("autonomy_tier").or_else(|| config.get("tier")) {
        Some(VmValue::Nil) | None => AutonomyTier::default(),
        Some(value) => parse_autonomy_tier(value)?,
    };
    let budget = config.get("budget").and_then(|value| match value {
        VmValue::Dict(map) => Some(map),
        _ => None,
    });
    let when_budget = config.get("when_budget").and_then(|value| match value {
        VmValue::Dict(map) => Some(map),
        VmValue::Nil => None,
        _ => None,
    });
    let retry = config.get("retry").and_then(|value| match value {
        VmValue::Dict(map) => Some(map),
        VmValue::Nil => None,
        _ => None,
    });
    let dedupe_key = optional_string(config, "dedupe_key");
    let filter = optional_string(config, "filter");
    let daily_cost_usd = budget
        .and_then(|map| map.get("daily_cost_usd"))
        .and_then(number_value);
    let hourly_cost_usd = budget
        .and_then(|map| map.get("hourly_cost_usd"))
        .and_then(number_value);
    let max_autonomous_decisions_per_hour = budget
        .and_then(|map| map.get("max_autonomous_decisions_per_hour"))
        .and_then(VmValue::as_int)
        .map(|value| value.max(0) as u64);
    let max_autonomous_decisions_per_day = budget
        .and_then(|map| map.get("max_autonomous_decisions_per_day"))
        .and_then(VmValue::as_int)
        .map(|value| value.max(0) as u64);
    let on_budget_exhausted = match budget.and_then(|map| map.get("on_budget_exhausted")) {
        Some(VmValue::String(text)) => match text.as_ref() {
            "false" => crate::TriggerBudgetExhaustionStrategy::False,
            "retry_later" => crate::TriggerBudgetExhaustionStrategy::RetryLater,
            "fail" => crate::TriggerBudgetExhaustionStrategy::Fail,
            "warn" => crate::TriggerBudgetExhaustionStrategy::Warn,
            raw => {
                return Err(VmError::Runtime(format!(
                    "trigger_register: unsupported budget.on_budget_exhausted '{raw}'"
                )))
            }
        },
        Some(_) => {
            return Err(VmError::Runtime(
                "trigger_register: budget.on_budget_exhausted must be a string".to_string(),
            ))
        }
        None => crate::TriggerBudgetExhaustionStrategy::False,
    };
    let max_concurrent = budget
        .and_then(|map| map.get("max_concurrent"))
        .and_then(VmValue::as_int)
        .map(|value| value as u32);
    if max_autonomous_decisions_per_hour == Some(0) {
        return Err(VmError::Runtime(
            "trigger_register: budget.max_autonomous_decisions_per_hour must be greater than or equal to 1"
                .to_string(),
        ));
    }
    if max_autonomous_decisions_per_day == Some(0) {
        return Err(VmError::Runtime(
            "trigger_register: budget.max_autonomous_decisions_per_day must be greater than or equal to 1"
                .to_string(),
        ));
    }
    let when_budget = when_budget
        .map(|map| {
            Ok::<TriggerPredicateBudget, VmError>(TriggerPredicateBudget {
                max_cost_usd: map.get("max_cost_usd").and_then(number_value),
                tokens_max: map
                    .get("tokens_max")
                    .and_then(VmValue::as_int)
                    .map(|value| value.max(0) as u64),
                timeout_ms: map
                    .get("timeout")
                    .and_then(|value| match value {
                        VmValue::String(text) => Some(text.to_string()),
                        _ => None,
                    })
                    .map(|text| parse_duration_millis(&text))
                    .transpose()?,
            })
        })
        .transpose()?;
    let when_budget = {
        let mut merged = when_budget;
        if let Some(map) = budget {
            let max_cost_usd = map.get("max_cost_usd").and_then(number_value);
            let max_tokens = map
                .get("max_tokens")
                .or_else(|| map.get("tokens_max"))
                .and_then(VmValue::as_int)
                .map(|value| value.max(0) as u64);
            if max_cost_usd.is_some() || max_tokens.is_some() {
                let budget = merged.get_or_insert_with(TriggerPredicateBudget::default);
                if budget.max_cost_usd.is_none() {
                    budget.max_cost_usd = max_cost_usd;
                }
                if budget.tokens_max.is_none() {
                    budget.tokens_max = max_tokens;
                }
            }
        }
        merged
    };
    if when_budget.is_some() && when.is_none() {
        return Err(VmError::Runtime(
            "trigger_register: when_budget requires a when predicate".to_string(),
        ));
    }
    let manifest_path = optional_string(config, "manifest_path").map(std::path::PathBuf::from);
    let package_name = optional_string(config, "package_name");
    let retry = parse_retry_config(retry.map(|value| &**value), "trigger_register")?;

    // CH-04 (#1875): parse the optional `batch` aggregation field.
    let aggregation = match config.get("batch") {
        Some(value) => crate::triggers::aggregation::parse_aggregation_config(value)?,
        None => None,
    };
    let aggregation_fingerprint = aggregation.as_ref().map(|cfg| {
        serde_json::json!({
            "count": cfg.count,
            "window_ms": cfg.window.as_millis() as u64,
            "key": cfg.key_path,
            "expire_action": cfg.expire_action.as_str(),
        })
    });

    let fingerprint = serde_json::to_string(&serde_json::json!({
        "id": id,
        "kind": kind,
        "provider": provider.as_str(),
        "autonomy_tier": autonomy_tier,
        "handler": handler_descriptor,
        "when": when.as_ref().map(|predicate| predicate.raw.clone()),
        "when_budget": when_budget,
        "retry": {
            "max": retry.max_attempts(),
            "policy": format!("{:?}", retry.policy),
        },
        "match_events": match_events,
        "dedupe_key": dedupe_key,
        "filter": filter,
        "allow_cleartext": allow_cleartext,
        "daily_cost_usd": daily_cost_usd,
        "hourly_cost_usd": hourly_cost_usd,
        "max_autonomous_decisions_per_hour": max_autonomous_decisions_per_hour,
        "max_autonomous_decisions_per_day": max_autonomous_decisions_per_day,
        "on_budget_exhausted": on_budget_exhausted.as_str(),
        "max_concurrent": max_concurrent,
        "aggregation": aggregation_fingerprint,
        "manifest_path": manifest_path.as_ref().map(|path| path.display().to_string()),
        "package_name": package_name,
    }))
    .unwrap_or_else(|_| format!("{}:{}:{}", id, kind, provider.as_str()));

    // CH-02 (#1872): channel-source triggers parse their selector strings
    // at registration so malformed selectors fail loudly before any emit.
    if provider.as_str() == "channel" {
        if match_events.is_empty() {
            return Err(VmError::Runtime(
                "trigger_register: provider=\"channel\" requires `match.events: [\"channel:<scope>:<name>\"]`"
                    .to_string(),
            ));
        }
        for selector in &match_events {
            crate::channels::ChannelSelector::parse(selector).map_err(|error| {
                VmError::Runtime(format!(
                    "trigger_register: invalid channel selector: {error}"
                ))
            })?;
        }
    }

    Ok(TriggerBindingSpec {
        id,
        source: TriggerBindingSource::Dynamic,
        kind,
        provider,
        autonomy_tier,
        handler,
        dispatch_priority: crate::WorkerQueuePriority::Normal,
        when,
        when_budget,
        retry,
        match_events,
        dedupe_key,
        dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
        filter,
        daily_cost_usd,
        hourly_cost_usd,
        max_autonomous_decisions_per_hour,
        max_autonomous_decisions_per_day,
        on_budget_exhausted,
        max_concurrent,
        flow_control: crate::triggers::TriggerFlowControlConfig::default(),
        aggregation,
        manifest_path,
        package_name,
        definition_fingerprint: fingerprint,
    })
}

pub(crate) fn validate_resume_trigger_spec(
    config: &BTreeMap<String, VmValue>,
) -> Result<(), VmError> {
    let mut normalized = config.clone();
    normalized
        .entry("handler".to_string())
        .or_insert_with(|| VmValue::String(Rc::from("worker://__resume_auto_resume__")));
    parse_trigger_config(&normalized).map(|_| ())
}

fn suspend_diagnostic_error(code: Code, message: impl Into<String>) -> VmError {
    VmError::Runtime(format!("{}: {}", code.as_str(), message.into()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AutoResumeTimeoutSpec {
    duration_minutes: u64,
    on_timeout: String,
}

pub(crate) async fn register_auto_resume_trigger(
    worker_id: &str,
    conditions: Option<&VmValue>,
) -> Result<Option<AutoResumeTriggerHandle>, VmError> {
    let Some(VmValue::Dict(conditions)) = conditions else {
        return Ok(None);
    };
    let Some(trigger) = conditions.get("trigger") else {
        return Ok(None);
    };
    let VmValue::Dict(trigger_config) = trigger else {
        if matches!(trigger, VmValue::Nil) {
            return Ok(None);
        }
        return Err(suspend_diagnostic_error(
            Code::ResumeConditionsInvalid,
            format!(
                "invalid ResumeConditions.trigger: expected dict or nil, got {}",
                trigger.type_name()
            ),
        ));
    };

    let mut normalized = trigger_config.as_ref().clone();
    normalized.insert(
        "handler".to_string(),
        VmValue::String(Rc::from("worker://__resume_auto_resume__")),
    );
    let mut spec = parse_trigger_config(&normalized).map_err(|error| {
        suspend_diagnostic_error(
            Code::ResumeTriggerRegistrationFailed,
            format!("auto_resume: failed to parse conditions.trigger for registration: {error}"),
        )
    })?;
    let source_kind = spec.kind.clone();
    if spec.match_events.is_empty() {
        spec.match_events.push(source_kind);
    }
    spec.id = format!(
        "auto_resume_{}_{}",
        sanitize_auto_resume_id(worker_id),
        Uuid::now_v7()
    );
    spec.kind = "auto_resume".to_string();
    spec.handler = TriggerHandlerSpec::AutoResume {
        worker_id: worker_id.to_string(),
    };
    spec.definition_fingerprint = serde_json::to_string(&serde_json::json!({
        "kind": "auto_resume",
        "worker_id": worker_id,
        "trigger": crate::llm::vm_value_to_json(trigger),
        "match_events": spec.match_events,
    }))
    .unwrap_or_else(|_| format!("auto_resume:{worker_id}"));

    let timeout = auto_resume_timeout_spec(conditions)?;
    let id = dynamic_register(spec).await.map_err(|error| {
        suspend_diagnostic_error(
            Code::ResumeTriggerRegistrationFailed,
            format!("auto_resume: failed to register trigger: {error}"),
        )
    })?;
    let binding = resolve_live_trigger_binding(id.as_str(), None).map_err(|error| {
        suspend_diagnostic_error(
            Code::ResumeTriggerRegistrationFailed,
            format!("auto_resume: failed to resolve registered trigger: {error}"),
        )
    })?;
    let handle = AutoResumeTriggerHandle {
        id: id.as_str().to_string(),
        version: binding.version,
    };
    if let Some(timeout) = timeout {
        schedule_auto_resume_timeout(handle.clone(), worker_id.to_string(), timeout);
    }
    Ok(Some(handle))
}

pub(crate) async fn unregister_auto_resume_trigger(
    handle: &AutoResumeTriggerHandle,
) -> Result<(), VmError> {
    cancel_auto_resume_timeout(handle.id.as_str());
    match dynamic_deregister(handle.id.as_str()).await {
        Ok(()) | Err(TriggerRegistryError::UnknownId(_)) => Ok(()),
        Err(error) => Err(trigger_registry_error(error)),
    }
}

pub(crate) fn reset_auto_resume_timeouts() {
    AUTO_RESUME_TIMEOUTS.with(|slot| {
        let mut tasks = slot.borrow_mut();
        for (_, task) in std::mem::take(&mut *tasks) {
            task.abort();
        }
    });
}

fn sanitize_auto_resume_id(worker_id: &str) -> String {
    worker_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn auto_resume_timeout_spec(
    conditions: &BTreeMap<String, VmValue>,
) -> Result<Option<AutoResumeTimeoutSpec>, VmError> {
    let Some(timeout) = conditions.get("timeout") else {
        return Ok(None);
    };
    let VmValue::Dict(timeout) = timeout else {
        if matches!(timeout, VmValue::Nil) {
            return Ok(None);
        }
        return Err(suspend_diagnostic_error(
            Code::ResumeConditionsInvalid,
            format!(
                "invalid ResumeConditions.timeout: expected dict or nil, got {}",
                timeout.type_name()
            ),
        ));
    };
    let duration_minutes = timeout
        .get("duration_minutes")
        .and_then(VmValue::as_int)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            suspend_diagnostic_error(
                Code::ResumeConditionsInvalid,
                "invalid ResumeConditions.timeout.duration_minutes: must be a positive int",
            )
        })? as u64;
    let on_timeout = match timeout.get("on_timeout") {
        Some(VmValue::String(value))
            if matches!(
                value.as_ref(),
                "resume_with_summary" | "resume_with_input" | "fail"
            ) =>
        {
            value.to_string()
        }
        Some(VmValue::Nil) | None => "resume_with_summary".to_string(),
        Some(VmValue::String(value)) => {
            return Err(suspend_diagnostic_error(
                Code::ResumeTimeoutUnsupported,
                format!(
                    "auto_resume: unsupported conditions.timeout.on_timeout `{}`; expected resume_with_summary, resume_with_input, or fail",
                    value.as_ref()
                ),
            ))
        }
        Some(other) => {
            return Err(suspend_diagnostic_error(
                Code::ResumeTimeoutUnsupported,
                format!(
                    "auto_resume: conditions.timeout.on_timeout must be resume_with_summary, resume_with_input, or fail; got {}",
                    other.type_name()
                ),
            ))
        }
    };
    Ok(Some(AutoResumeTimeoutSpec {
        duration_minutes,
        on_timeout,
    }))
}

fn cancel_auto_resume_timeout(trigger_id: &str) {
    AUTO_RESUME_TIMEOUTS.with(|slot| {
        if let Some(task) = slot.borrow_mut().remove(trigger_id) {
            task.abort();
        }
    });
}

fn schedule_auto_resume_timeout(
    handle: AutoResumeTriggerHandle,
    worker_id: String,
    timeout: AutoResumeTimeoutSpec,
) {
    cancel_auto_resume_timeout(handle.id.as_str());
    let event_log = ensure_trigger_event_log();
    let base_vm = crate::vm::clone_async_builtin_child_vm().unwrap_or_default();
    let event = auto_resume_timeout_event(&handle, &worker_id, &timeout);
    let trigger_id = handle.id.clone();
    let version = handle.version;
    let task_trigger_id = trigger_id.clone();
    let task = tokio::task::spawn_local(async move {
        clock::sleep(Duration::from_secs(
            timeout.duration_minutes.saturating_mul(60),
        ))
        .await;
        AUTO_RESUME_TIMEOUTS.with(|slot| {
            slot.borrow_mut().remove(task_trigger_id.as_str());
        });
        let Ok(binding) = resolve_live_trigger_binding(trigger_id.as_str(), Some(version)) else {
            return;
        };
        let dispatcher = crate::triggers::Dispatcher::with_event_log(base_vm, event_log);
        let _ = dispatcher.dispatch(&binding, event).await;
    });
    AUTO_RESUME_TIMEOUTS.with(|slot| {
        slot.borrow_mut().insert(handle.id.clone(), task);
    });
}

fn auto_resume_timeout_event(
    handle: &AutoResumeTriggerHandle,
    worker_id: &str,
    timeout: &AutoResumeTimeoutSpec,
) -> TriggerEvent {
    let raw = serde_json::json!({
        "type": "auto_resume.timeout",
        "worker_id": worker_id,
        "trigger_id": handle.id,
        "on_timeout": timeout.on_timeout,
    });
    TriggerEvent::new(
        crate::triggers::ProviderId::from("harn"),
        "auto_resume.timeout",
        None,
        format!("auto_resume_timeout:{}:{}", handle.id, Uuid::now_v7()),
        None,
        BTreeMap::new(),
        crate::triggers::ProviderPayload::Extension(crate::triggers::ExtensionProviderPayload {
            provider: "harn".to_string(),
            schema_name: "AutoResumeTimeout".to_string(),
            raw,
        }),
        crate::triggers::SignatureStatus::Unsigned,
    )
}

fn parse_duration_millis(raw: &str) -> Result<u64, VmError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(VmError::Runtime(
            "trigger_register: when_budget.timeout cannot be empty".to_string(),
        ));
    }
    let (value, unit) = trimmed
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(index, _)| (&trimmed[..index], &trimmed[index..]))
        .unwrap_or((trimmed, "ms"));
    let amount = value.parse::<u64>().map_err(|_| {
        VmError::Runtime(format!(
            "trigger_register: invalid when_budget.timeout '{raw}'"
        ))
    })?;
    let multiplier = match unit.trim() {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => {
            return Err(VmError::Runtime(format!(
                "trigger_register: unsupported when_budget.timeout unit in '{raw}'"
            )))
        }
    };
    Ok(amount.saturating_mul(multiplier))
}

fn parse_autonomy_tier(value: &VmValue) -> Result<AutonomyTier, VmError> {
    let raw = match value {
        VmValue::String(text) => text.as_ref(),
        other => {
            return Err(VmError::Runtime(format!(
                "trigger_register: `autonomy_tier` must be a string, got {}",
                other.type_name()
            )))
        }
    };
    match raw {
        "shadow" => Ok(AutonomyTier::Shadow),
        "suggest" => Ok(AutonomyTier::Suggest),
        "act_with_approval" => Ok(AutonomyTier::ActWithApproval),
        "act_auto" => Ok(AutonomyTier::ActAuto),
        other => Err(VmError::Runtime(format!(
            "trigger_register: unsupported autonomy_tier '{other}', expected shadow|suggest|act_with_approval|act_auto"
        ))),
    }
}

fn parse_trust_outcome(value: &VmValue) -> Result<TrustOutcome, VmError> {
    let raw = match value {
        VmValue::String(text) => text.as_ref(),
        other => {
            return Err(VmError::Runtime(format!(
                "trust_record: outcome must be a string, got {}",
                other.type_name()
            )))
        }
    };
    match raw {
        "success" => Ok(TrustOutcome::Success),
        "failure" => Ok(TrustOutcome::Failure),
        "denied" => Ok(TrustOutcome::Denied),
        "timeout" => Ok(TrustOutcome::Timeout),
        other => Err(VmError::Runtime(format!(
            "trust_record: unsupported outcome '{other}', expected success|failure|denied|timeout"
        ))),
    }
}

async fn append_trust_record_from_parts(
    builtin: &str,
    args: &[VmValue],
) -> Result<TrustRecord, VmError> {
    let agent = required_string_arg(args, 0, builtin, "agent")?;
    let action = required_string_arg(args, 1, builtin, "action")?;
    let approver = args.get(2).and_then(|value| match value {
        VmValue::String(text) if !text.is_empty() => Some(text.to_string()),
        VmValue::Nil => None,
        _ => None,
    });
    let outcome = args
        .get(3)
        .map(parse_trust_outcome)
        .transpose()?
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: expected outcome")))?;
    let tier = args
        .get(4)
        .map(parse_autonomy_tier)
        .transpose()?
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: expected autonomy tier")))?;
    let trace_id = current_dispatch_context()
        .map(|context| context.trigger_event.trace_id.0)
        .unwrap_or_else(|| format!("trace-{}", uuid::Uuid::now_v7()));
    append_trust_record_value(
        builtin,
        TrustRecord::new(agent, action, approver, outcome, trace_id, tier),
    )
    .await
}

async fn append_trust_record_from_decision_for(
    builtin: &str,
    value: &VmValue,
) -> Result<TrustRecord, VmError> {
    let VmValue::Dict(map) = value else {
        return Err(VmError::Runtime(format!(
            "{builtin}: expected decision dict"
        )));
    };
    let agent = optional_string(map, "agent")
        .or_else(|| optional_string(map, "actor"))
        .or_else(|| optional_string(map, "actor_id"))
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{builtin}: missing string field `actor_id` (or `agent`)"
            ))
        })?;
    let action = required_string(map, "action", builtin)?;
    let approver = optional_string(map, "approver");
    let outcome = map
        .get("outcome")
        .map(parse_trust_outcome)
        .transpose()?
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing outcome")))?;
    let tier = map
        .get("autonomy_tier")
        .or_else(|| map.get("autonomy_tier_at_time"))
        .or_else(|| map.get("tier"))
        .map(parse_autonomy_tier)
        .transpose()?
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing autonomy_tier")))?;
    let trace_id = optional_string(map, "trace_id")
        .or_else(|| current_dispatch_context().map(|context| context.trigger_event.trace_id.0))
        .unwrap_or_else(|| format!("trace-{}", uuid::Uuid::now_v7()));
    let mut record = TrustRecord::new(agent, action, approver, outcome, trace_id, tier);
    if let Some(cost_usd) = map.get("cost_usd").and_then(vm_number_as_f64) {
        record.cost_usd = Some(cost_usd);
    }
    if let Some(evidence_refs) = map.get("evidence_refs") {
        let VmValue::List(items) = evidence_refs else {
            return Err(VmError::Runtime(format!(
                "{builtin}: evidence_refs must be a list"
            )));
        };
        record.metadata.insert(
            "evidence_refs".to_string(),
            serde_json::Value::Array(
                items
                    .iter()
                    .map(crate::llm::vm_value_to_json)
                    .collect::<Vec<_>>(),
            ),
        );
    }
    if let Some(metadata) = map.get("metadata") {
        let metadata_json = crate::llm::vm_value_to_json(metadata);
        let serde_json::Value::Object(object) = metadata_json else {
            return Err(VmError::Runtime(format!(
                "{builtin}: metadata must be a dict"
            )));
        };
        record.metadata.extend(object);
    }
    append_trust_record_value(builtin, record).await
}

async fn append_trust_record_value(
    builtin: &str,
    record: TrustRecord,
) -> Result<TrustRecord, VmError> {
    let log = ensure_trigger_event_log();
    crate::append_trust_record(&log, &record)
        .await
        .map_err(|error| VmError::Runtime(format!("{builtin}: {error}")))
}

fn correction_record_from_value(
    builtin: &str,
    value: &VmValue,
) -> Result<crate::CorrectionRecord, VmError> {
    crate::correction_record_from_json(crate::llm::vm_value_to_json(value))
        .map_err(|error| VmError::Runtime(format!("{builtin}: {error}")))
}

fn correction_query_filters_from_value(
    builtin: &str,
    value: &VmValue,
) -> Result<crate::CorrectionQueryFilters, VmError> {
    crate::correction_query_filters_from_json(crate::llm::vm_value_to_json(value))
        .map_err(|error| VmError::Runtime(format!("{builtin}: {error}")))
}

fn vm_number_as_f64(value: &VmValue) -> Option<f64> {
    match value {
        VmValue::Float(value) => Some(*value),
        VmValue::Int(value) => Some(*value as f64),
        _ => None,
    }
}

fn parse_trust_query_filters(value: &VmValue) -> Result<TrustQueryFilters, VmError> {
    let VmValue::Dict(map) = value else {
        return Err(VmError::Runtime(
            "trust_query: filters must be a dict".to_string(),
        ));
    };
    Ok(TrustQueryFilters {
        agent: optional_string(map, "agent")
            .or_else(|| optional_string(map, "actor"))
            .or_else(|| optional_string(map, "actor_id")),
        action: optional_string(map, "action"),
        since: optional_string(map, "since")
            .map(|raw| parse_query_timestamp("trust_query", "since", &raw))
            .transpose()?,
        until: optional_string(map, "until")
            .map(|raw| parse_query_timestamp("trust_query", "until", &raw))
            .transpose()?,
        tier: map
            .get("tier")
            .or_else(|| map.get("autonomy_tier"))
            .or_else(|| map.get("autonomy_tier_at_time"))
            .map(parse_autonomy_tier)
            .transpose()?,
        outcome: map.get("outcome").map(parse_trust_outcome).transpose()?,
        limit: map.get("limit").map(parse_trust_query_limit).transpose()?,
        grouped_by_trace: map
            .get("grouped_by_trace")
            .map(parse_trust_query_grouped_flag)
            .transpose()?
            .unwrap_or(false),
    })
}

fn parse_trust_query_limit(value: &VmValue) -> Result<usize, VmError> {
    let limit = value.as_int().ok_or_else(|| {
        VmError::Runtime(format!(
            "trust_query: limit must be an int, got {}",
            value.type_name()
        ))
    })?;
    usize::try_from(limit).map_err(|_| {
        VmError::Runtime(format!(
            "trust_query: limit must be non-negative, got {limit}"
        ))
    })
}

fn parse_trust_query_grouped_flag(value: &VmValue) -> Result<bool, VmError> {
    match value {
        VmValue::Bool(flag) => Ok(*flag),
        other => Err(VmError::Runtime(format!(
            "trust_query: grouped_by_trace must be a bool, got {}",
            other.type_name()
        ))),
    }
}

fn parse_query_timestamp(builtin: &str, field: &str, raw: &str) -> Result<OffsetDateTime, VmError> {
    if let Ok(parsed) = OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339) {
        return Ok(parsed);
    }
    if let Ok(unix) = raw.parse::<i64>() {
        let parsed = if raw.len() > 10 {
            OffsetDateTime::from_unix_timestamp_nanos(unix as i128 * 1_000_000)
        } else {
            OffsetDateTime::from_unix_timestamp(unix)
        }
        .map_err(|error| {
            VmError::Runtime(format!(
                "{builtin}: invalid `{field}` timestamp '{raw}': {error}"
            ))
        })?;
        return Ok(parsed);
    }
    Err(VmError::Runtime(format!(
        "{builtin}: invalid `{field}` timestamp '{raw}', expected RFC3339 or unix seconds/milliseconds"
    )))
}

fn parse_retry_config(
    retry: Option<&BTreeMap<String, VmValue>>,
    builtin: &str,
) -> Result<TriggerRetryConfig, VmError> {
    let Some(retry) = retry else {
        return Ok(TriggerRetryConfig::default());
    };
    let max = retry
        .get("max")
        .and_then(VmValue::as_int)
        .unwrap_or(DEFAULT_MAX_ATTEMPTS as i64)
        .max(1) as u32;
    let policy = match optional_string(retry, "backoff").as_deref() {
        None | Some("svix") => RetryPolicy::Svix,
        Some("immediate") => RetryPolicy::Linear { delay_ms: 0 },
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: unsupported retry.backoff '{other}', expected 'svix' or 'immediate'"
            )))
        }
    };
    Ok(TriggerRetryConfig::new(max, policy))
}

fn parse_handler_value(
    value: &VmValue,
    builtin: &str,
    field_name: &str,
    allow_cleartext: bool,
) -> Result<(TriggerHandlerSpec, serde_json::Value), VmError> {
    match value {
        VmValue::Closure(closure) => {
            let raw = closure.func.name.clone();
            Ok((
                TriggerHandlerSpec::Local {
                    raw: raw.clone(),
                    closure: closure.clone(),
                },
                serde_json::json!({
                    "kind": "local",
                    "raw": raw,
                }),
            ))
        }
        VmValue::String(text) => {
            if let Some(target) = text.strip_prefix("a2a://") {
                return Ok((
                    TriggerHandlerSpec::A2a {
                        target: target.to_string(),
                        allow_cleartext,
                    },
                    serde_json::json!({
                        "kind": "a2a",
                        "target": target,
                        "allow_cleartext": allow_cleartext,
                    }),
                ));
            }
            if let Some(queue) = text.strip_prefix("worker://") {
                return Ok((
                    TriggerHandlerSpec::Worker {
                        queue: queue.to_string(),
                    },
                    serde_json::json!({
                        "kind": "worker",
                        "queue": queue,
                    }),
                ));
            }
            Err(VmError::Runtime(format!(
                "{builtin}: `{field_name}` string must use `a2a://` or `worker://` URI syntax"
            )))
        }
        VmValue::Dict(map) => parse_handler_dict(map, builtin, field_name),
        other => Err(VmError::Runtime(format!(
            "{builtin}: `{field_name}` must be a closure, handler URI string, or handler-variant dict, got {}",
            other.type_name()
        ))),
    }
}

/// Parse handler-variant dicts. `SpawnToPool` (#1889) and `ReminderInject`
/// (#1876) both ship as dict-shaped handlers so the trigger DSL keeps a
/// single uniform `handler:` syntax; future variants plug in here too.
fn parse_handler_dict(
    map: &BTreeMap<String, VmValue>,
    builtin: &str,
    field_name: &str,
) -> Result<(TriggerHandlerSpec, serde_json::Value), VmError> {
    let kind = map
        .get("kind")
        .or_else(|| map.get("_kind"))
        .map(VmValue::display)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{builtin}: `{field_name}` handler dict missing `kind`"
            ))
        })?;
    match kind.as_str() {
        "spawn_to_pool" => {
            let pool = map
                .get("pool")
                .map(VmValue::display)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    VmError::Runtime(format!(
                        "{builtin}: SpawnToPool handler requires `pool` (string)"
                    ))
                })?;
            let priority_from = optional_path_string(map, "priority_from", "SpawnToPool")?;
            let key_from = optional_path_string(map, "key_from", "SpawnToPool")?;
            let task_factory = map
                .get("task_factory")
                .ok_or_else(|| {
                    VmError::Runtime(format!(
                        "{builtin}: SpawnToPool handler requires `task_factory` closure"
                    ))
                })
                .and_then(|value| match value {
                    VmValue::Closure(closure) => Ok(closure.clone()),
                    other => Err(VmError::Runtime(format!(
                        "{builtin}: SpawnToPool.task_factory must be a closure, got {}",
                        other.type_name()
                    ))),
                })?;
            let descriptor = serde_json::json!({
                "kind": "spawn_to_pool",
                "pool": pool,
                "priority_from": priority_from,
                "key_from": key_from,
                "task_factory": task_factory.func.name.clone(),
            });
            Ok((
                TriggerHandlerSpec::SpawnToPool {
                    pool,
                    priority_from,
                    key_from,
                    task_factory,
                },
                descriptor,
            ))
        }
        "reminder_inject" => parse_reminder_inject_handler(map, builtin),
        "interrupt_and_suspend" => parse_interrupt_and_suspend_handler(map, builtin),
        other => Err(VmError::Runtime(format!(
            "{builtin}: unsupported handler variant '{other}'; expected 'spawn_to_pool', 'reminder_inject', or 'interrupt_and_suspend'"
        ))),
    }
}

/// CH-10 (#1910): parse the `InterruptAndSuspend` handler dict. `target_agents`
/// resolution is one of: the string `"all"` (panic-broadcast every running
/// worker in the local registry), a list of concrete worker-id strings, or a
/// closure that returns a worker-id list at dispatch time (the closure form
/// lets a single trigger registration pick targets dynamically — e.g. all
/// workers tagged with a given org / tenant). `reason` is propagated to every
/// suspended worker's `WorkerSuspension::reason` and audit entry.
fn parse_interrupt_and_suspend_handler(
    map: &BTreeMap<String, VmValue>,
    builtin: &str,
) -> Result<(TriggerHandlerSpec, serde_json::Value), VmError> {
    let target_value = map.get("target_agents").or_else(|| map.get("target"));
    let target_agents = match target_value {
        None | Some(VmValue::Nil) => AgentScope::All,
        Some(VmValue::String(s)) => match s.as_ref() {
            "all" | "all_in_scope" => AgentScope::All,
            other if !other.is_empty() => AgentScope::Concrete(vec![other.to_string()]),
            _ => {
                return Err(VmError::Runtime(format!(
                    "{builtin}: InterruptAndSuspend `target_agents` string must be 'all' or a non-empty worker id"
                )));
            }
        },
        Some(VmValue::List(items)) => {
            let mut ids = Vec::with_capacity(items.len());
            for item in items.iter() {
                let VmValue::String(text) = item else {
                    return Err(VmError::Runtime(format!(
                        "{builtin}: InterruptAndSuspend `target_agents` list entries must be strings, got {}",
                        item.type_name()
                    )));
                };
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return Err(VmError::Runtime(format!(
                        "{builtin}: InterruptAndSuspend `target_agents` list entries must be non-empty strings"
                    )));
                }
                ids.push(trimmed.to_string());
            }
            AgentScope::Concrete(ids)
        }
        Some(VmValue::Closure(closure)) => AgentScope::Closure(closure.clone()),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: InterruptAndSuspend `target_agents` must be the string 'all', a list of worker-id strings, or a closure returning a list, got {}",
                other.type_name()
            )));
        }
    };

    let reason = match map.get("reason") {
        Some(VmValue::String(s)) => s.to_string(),
        None | Some(VmValue::Nil) => "panic".to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: InterruptAndSuspend `reason` must be a string, got {}",
                other.type_name()
            )));
        }
    };

    let scope_kind = target_agents.kind();
    let concrete_count = match &target_agents {
        AgentScope::Concrete(ids) => Some(ids.len()),
        _ => None,
    };
    let descriptor = serde_json::json!({
        "kind": "interrupt_and_suspend",
        "scope_kind": scope_kind,
        "concrete_count": concrete_count,
        "reason": &reason,
    });

    Ok((
        TriggerHandlerSpec::InterruptAndSuspend {
            target_agents,
            reason,
        },
        descriptor,
    ))
}

/// Parse the ReminderInject (#1876) handler dict. `target` resolution is
/// one of: a string literal (`"current"`, `"parent"`, or any other value
/// interpreted as a concrete session id), or a closure that returns the
/// session id at dispatch time. Reminder metadata mirrors the
/// `transcript.inject_reminder` (#1815 R-02) shape so authors can reuse
/// the same mental model.
fn parse_reminder_inject_handler(
    map: &BTreeMap<String, VmValue>,
    builtin: &str,
) -> Result<(TriggerHandlerSpec, serde_json::Value), VmError> {
    let target_value = map.get("target").or_else(|| map.get("target_session_id"));
    let target = match target_value {
        None | Some(VmValue::Nil) => TargetExpr::Current,
        Some(VmValue::String(s)) => match s.as_ref() {
            "current" => TargetExpr::Current,
            "parent" => TargetExpr::Parent,
            other if !other.is_empty() => TargetExpr::Concrete(other.to_string()),
            _ => {
                return Err(VmError::Runtime(format!(
                    "{builtin}: ReminderInject `target` string must be 'current', 'parent', or a non-empty session id"
                )));
            }
        },
        Some(VmValue::Closure(closure)) => TargetExpr::Closure(closure.clone()),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: ReminderInject `target` must be a string ('current'/'parent'/session id) or a closure returning a session id, got {}",
                other.type_name()
            )));
        }
    };

    let body = match map.get("body") {
        Some(VmValue::String(s)) => s.to_string(),
        None | Some(VmValue::Nil) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: ReminderInject handler requires `body` (string template)"
            )));
        }
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: ReminderInject `body` must be a string, got {}",
                other.type_name()
            )));
        }
    };

    let tags = parse_reminder_tags(map, builtin)?;
    let ttl_turns = parse_reminder_ttl_turns(map, builtin)?;
    let dedupe_key = optional_path_string(map, "dedupe_key", "ReminderInject")?;
    let propagate = parse_reminder_propagate(map, builtin)?;
    let role_hint = parse_reminder_role_hint(map, builtin)?;
    let preserve_on_compact = match map.get("preserve_on_compact") {
        None | Some(VmValue::Nil) => false,
        Some(VmValue::Bool(b)) => *b,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: ReminderInject `preserve_on_compact` must be a bool, got {}",
                other.type_name()
            )));
        }
    };

    let descriptor = serde_json::json!({
        "kind": "reminder_inject",
        "target": target.kind(),
        "target_session_id": match &target {
            TargetExpr::Concrete(id) => serde_json::Value::String(id.clone()),
            _ => serde_json::Value::Null,
        },
        "body": &body,
        "tags": &tags,
        "ttl_turns": ttl_turns,
        "dedupe_key": &dedupe_key,
        "propagate": propagate.as_str(),
        "role_hint": role_hint.as_str(),
        "preserve_on_compact": preserve_on_compact,
    });

    Ok((
        TriggerHandlerSpec::ReminderInject {
            target,
            body,
            tags,
            ttl_turns,
            dedupe_key,
            propagate,
            role_hint,
            preserve_on_compact,
        },
        descriptor,
    ))
}

fn parse_reminder_tags(
    map: &BTreeMap<String, VmValue>,
    builtin: &str,
) -> Result<Vec<String>, VmError> {
    match map.get("tags") {
        None | Some(VmValue::Nil) => Ok(Vec::new()),
        Some(VmValue::List(list)) => {
            let mut tags = Vec::with_capacity(list.len());
            for item in list.iter() {
                let VmValue::String(text) = item else {
                    return Err(VmError::Runtime(format!(
                        "{builtin}: ReminderInject `tags` entries must be strings, got {}",
                        item.type_name()
                    )));
                };
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return Err(VmError::Runtime(format!(
                        "{builtin}: ReminderInject `tags` entries must be non-empty strings"
                    )));
                }
                let tag = trimmed.to_string();
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
            Ok(tags)
        }
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: ReminderInject `tags` must be a list of strings, got {}",
            other.type_name()
        ))),
    }
}

fn parse_reminder_ttl_turns(
    map: &BTreeMap<String, VmValue>,
    builtin: &str,
) -> Result<Option<i64>, VmError> {
    match map.get("ttl_turns") {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(value)) => {
            if *value <= 0 {
                Err(VmError::Runtime(format!(
                    "{builtin}: ReminderInject `ttl_turns` must be > 0"
                )))
            } else {
                Ok(Some(*value))
            }
        }
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: ReminderInject `ttl_turns` must be an int or nil, got {}",
            other.type_name()
        ))),
    }
}

fn parse_reminder_propagate(
    map: &BTreeMap<String, VmValue>,
    builtin: &str,
) -> Result<crate::llm::helpers::ReminderPropagate, VmError> {
    match map.get("propagate") {
        None | Some(VmValue::Nil) => Ok(crate::llm::helpers::ReminderPropagate::Session),
        Some(VmValue::String(s)) => match s.as_ref() {
            "all" => Ok(crate::llm::helpers::ReminderPropagate::All),
            "session" => Ok(crate::llm::helpers::ReminderPropagate::Session),
            "none" => Ok(crate::llm::helpers::ReminderPropagate::None),
            other => Err(VmError::Runtime(format!(
                "{builtin}: ReminderInject `propagate` must be 'all', 'session', or 'none', got '{other}'"
            ))),
        },
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: ReminderInject `propagate` must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn parse_reminder_role_hint(
    map: &BTreeMap<String, VmValue>,
    builtin: &str,
) -> Result<crate::llm::helpers::ReminderRoleHint, VmError> {
    match map.get("role_hint") {
        None | Some(VmValue::Nil) => Ok(crate::llm::helpers::ReminderRoleHint::System),
        Some(VmValue::String(s)) => match s.as_ref() {
            "system" => Ok(crate::llm::helpers::ReminderRoleHint::System),
            "developer" => Ok(crate::llm::helpers::ReminderRoleHint::Developer),
            "user_block" => Ok(crate::llm::helpers::ReminderRoleHint::UserBlock),
            "ephemeral_cache" => Ok(crate::llm::helpers::ReminderRoleHint::EphemeralCache),
            other => Err(VmError::Runtime(format!(
                "{builtin}: ReminderInject `role_hint` must be 'system', 'developer', 'user_block', or 'ephemeral_cache', got '{other}'"
            ))),
        },
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: ReminderInject `role_hint` must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn optional_path_string(
    map: &BTreeMap<String, VmValue>,
    field: &str,
    variant: &str,
) -> Result<Option<String>, VmError> {
    match map.get(field) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(other) => Err(VmError::Runtime(format!(
            "{variant}: `{field}` must be a string path or nil, got {}",
            other.type_name()
        ))),
    }
}

fn parse_trigger_event(value: &VmValue) -> Result<TriggerEvent, VmError> {
    let mut json = crate::llm::vm_value_to_json(value);
    let raw_event = json.clone();
    let Some(object) = json.as_object_mut() else {
        return Err(VmError::Runtime(
            "trigger_fire: trigger event must be a dict-like value".to_string(),
        ));
    };

    let provider = object
        .get("provider")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            VmError::Runtime(
                "trigger_fire: trigger event is missing string field `provider`".to_string(),
            )
        })?;
    let kind = object
        .get("kind")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            VmError::Runtime(
                "trigger_fire: trigger event is missing string field `kind`".to_string(),
            )
        })?;

    object
        .entry("id")
        .or_insert_with(|| serde_json::json!(TriggerEventId::new().0));
    object.entry("received_at").or_insert_with(|| {
        serde_json::json!(clock::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default())
    });
    object
        .entry("occurred_at")
        .or_insert(serde_json::Value::Null);
    object.entry("dedupe_key").or_insert_with(|| {
        serde_json::json!(format!("synthetic:{provider}:{kind}:{}", Uuid::now_v7()))
    });
    object
        .entry("trace_id")
        .or_insert_with(|| serde_json::json!(crate::TraceId::new().0));
    object.entry("tenant_id").or_insert(serde_json::Value::Null);
    object
        .entry("headers")
        .or_insert_with(|| serde_json::json!({}));
    object.entry("signature_status").or_insert_with(|| {
        serde_json::json!({
            "state": "unsigned",
        })
    });

    if !object.contains_key("provider_payload") {
        object.insert(
            "provider_payload".to_string(),
            default_provider_payload(provider.as_str(), kind.as_str(), raw_event),
        );
    }

    serde_json::from_value(json).map_err(|error| {
        VmError::Runtime(format!("trigger_fire: trigger event parse error: {error}"))
    })
}

fn require_dict_arg<'a>(
    args: &'a [VmValue],
    index: usize,
    builtin: &str,
) -> Result<&'a BTreeMap<String, VmValue>, VmError> {
    match args.get(index) {
        Some(VmValue::Dict(dict)) => Ok(dict),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: expected dict argument at position {}, got {}",
            index + 1,
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{builtin}: missing dict argument at position {}",
            index + 1
        ))),
    }
}

fn required_string_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    name: &str,
) -> Result<String, VmError> {
    optional_string_arg(args, index, builtin, name)?
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing string argument `{name}`")))
}

fn optional_string_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    name: &str,
) -> Result<Option<String>, VmError> {
    match args.get(index) {
        Some(VmValue::String(text)) => Ok(Some(text.to_string())),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: argument `{name}` must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn required_string(
    map: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<String, VmError> {
    optional_string(map, key)
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing string field `{key}`")))
}

fn optional_string(map: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    map.get(key).and_then(|value| match value {
        VmValue::String(text) => Some(text.to_string()),
        _ => None,
    })
}

fn optional_bool(
    map: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Option<bool>, VmError> {
    match map.get(key) {
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(VmValue::Nil) | None => Ok(None),
        Some(_) => Err(VmError::Runtime(format!(
            "{builtin}: field `{key}` must be a bool"
        ))),
    }
}

fn parse_string_list(value: &VmValue) -> Result<Vec<String>, VmError> {
    let VmValue::List(items) = value else {
        return Err(VmError::Runtime(
            "trigger_register: `events` must be a list of strings".to_string(),
        ));
    };
    items
        .iter()
        .map(|item| match item {
            VmValue::String(text) => Ok(text.to_string()),
            other => Err(VmError::Runtime(format!(
                "trigger_register: `events` entries must be strings, got {}",
                other.type_name()
            ))),
        })
        .collect()
}

fn number_value(value: &VmValue) -> Option<f64> {
    match value {
        VmValue::Float(number) => Some(*number),
        VmValue::Int(number) => Some(*number as f64),
        _ => None,
    }
}

fn trigger_registry_error(error: impl std::fmt::Display) -> VmError {
    VmError::Runtime(format!("trigger stdlib: {error}"))
}

fn webhook_intake_error(error: WebhookIntakeError) -> VmError {
    VmError::Runtime(format!("webhook_intake: {error}"))
}

fn parse_webhook_intake_config(
    config: &BTreeMap<String, VmValue>,
) -> Result<WebhookIntakeConfig, VmError> {
    let signature_header = required_string(config, "signature_header", "webhook_intake_register")?;
    let delivery_id_header =
        required_string(config, "delivery_id_header", "webhook_intake_register")?;
    let topic = required_string(config, "topic", "webhook_intake_register")?;

    let algorithm = match optional_string(config, "algorithm").as_deref() {
        Some(raw) => HmacAlgorithm::parse(raw).ok_or_else(|| {
            VmError::Runtime(format!(
                "webhook_intake_register: unsupported algorithm `{raw}`, expected sha256 or sha1"
            ))
        })?,
        None => HmacAlgorithm::default(),
    };
    let signature_encoding = match optional_string(config, "signature_encoding").as_deref() {
        Some(raw) => SignatureEncoding::parse(raw).ok_or_else(|| {
            VmError::Runtime(format!(
                "webhook_intake_register: unsupported signature_encoding `{raw}`, expected hex or base64"
            ))
        })?,
        None => SignatureEncoding::default(),
    };
    // `signature_prefix` defaults to "<algorithm>=" (matches GitHub, GitLab,
    // Bitbucket conventions). Pass an empty string explicitly to opt out.
    let signature_prefix = match config.get("signature_prefix") {
        Some(VmValue::String(text)) => Some(text.to_string()),
        Some(VmValue::Nil) | None => Some(format!("{}=", algorithm.as_str())),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "webhook_intake_register: signature_prefix must be a string or nil, got {}",
                other.type_name()
            )))
        }
    }
    .filter(|prefix| !prefix.is_empty());

    let secret = parse_intake_secret(config)?;
    let dedupe_ttl_secs = match config.get("dedupe_ttl_seconds") {
        Some(VmValue::Int(value)) if *value > 0 => *value as u64,
        Some(VmValue::Nil) | None => crate::triggers::webhook_intake::DEFAULT_DEDUPE_TTL_SECS,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "webhook_intake_register: dedupe_ttl_seconds must be a positive int, got {}",
                other.type_name()
            )))
        }
    };

    Ok(WebhookIntakeConfig {
        id: optional_string(config, "id"),
        path: optional_string(config, "path"),
        signature_header,
        signature_prefix,
        signature_encoding,
        algorithm,
        secret,
        delivery_id_header,
        topic,
        dedupe_ttl: std::time::Duration::from_secs(dedupe_ttl_secs),
    })
}

fn parse_intake_secret(config: &BTreeMap<String, VmValue>) -> Result<Vec<u8>, VmError> {
    match config.get("secret") {
        Some(VmValue::String(text)) => Ok(text.as_bytes().to_vec()),
        Some(VmValue::Bytes(bytes)) => Ok((**bytes).clone()),
        Some(other) => Err(VmError::Runtime(format!(
            "webhook_intake_register: `secret` must be a string or bytes, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(
            "webhook_intake_register: missing `secret` (string or bytes)".to_string(),
        )),
    }
}

fn parse_webhook_intake_request(
    request: &BTreeMap<String, VmValue>,
) -> Result<WebhookIntakeRequest, VmError> {
    let headers = match request.get("headers") {
        Some(VmValue::Dict(dict)) => dict
            .iter()
            .map(|(key, value)| match value {
                VmValue::String(text) => Ok((key.clone(), text.to_string())),
                VmValue::Int(value) => Ok((key.clone(), value.to_string())),
                VmValue::Float(value) => Ok((key.clone(), value.to_string())),
                VmValue::Bool(value) => Ok((key.clone(), value.to_string())),
                other => Err(VmError::Runtime(format!(
                    "webhook_intake_feed: headers.{key} must be a string, got {}",
                    other.type_name()
                ))),
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?,
        Some(VmValue::Nil) | None => BTreeMap::new(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "webhook_intake_feed: headers must be a dict, got {}",
                other.type_name()
            )))
        }
    };
    let body = match request.get("body") {
        Some(VmValue::Bytes(bytes)) => (**bytes).clone(),
        Some(VmValue::String(text)) => text.as_bytes().to_vec(),
        Some(VmValue::Nil) | None => Vec::new(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "webhook_intake_feed: body must be string or bytes, got {}",
                other.type_name()
            )))
        }
    };
    let path = optional_string(request, "path");
    let received_at = match request.get("received_at") {
        Some(VmValue::String(text)) => Some(
            OffsetDateTime::parse(
                text.as_ref(),
                &time::format_description::well_known::Rfc3339,
            )
            .map_err(|error| {
                VmError::Runtime(format!(
                    "webhook_intake_feed: received_at must be RFC3339, got `{text}`: {error}"
                ))
            })?,
        ),
        Some(VmValue::Nil) | None => None,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "webhook_intake_feed: received_at must be a string, got {}",
                other.type_name()
            )))
        }
    };
    Ok(WebhookIntakeRequest {
        headers,
        body,
        path,
        received_at,
    })
}

fn value_from_serde<T: Serialize>(value: &T) -> VmValue {
    crate::stdlib::json_to_vm_value(&serde_json::to_value(value).unwrap_or_default())
}

fn default_provider_payload(
    provider: &str,
    kind: &str,
    raw_event: serde_json::Value,
) -> serde_json::Value {
    match provider {
        "github" => serde_json::json!({
            "provider": "github",
            "event": kind,
            "action": serde_json::Value::Null,
            "delivery_id": serde_json::Value::Null,
            "installation_id": serde_json::Value::Null,
            "raw": raw_event,
        }),
        "slack" => serde_json::json!({
            "provider": "slack",
            "event": kind,
            "event_id": serde_json::Value::Null,
            "api_app_id": serde_json::Value::Null,
            "team_id": serde_json::Value::Null,
            "channel_id": serde_json::Value::Null,
            "user_id": serde_json::Value::Null,
            "event_ts": serde_json::Value::Null,
            "raw": raw_event,
        }),
        "linear" => serde_json::json!({
            "provider": "linear",
            "event": kind.split('.').next().unwrap_or(kind),
            "action": kind.split('.').nth(1).unwrap_or("update"),
            "delivery_id": serde_json::Value::Null,
            "organization_id": serde_json::Value::Null,
            "webhook_id": serde_json::Value::Null,
            "url": serde_json::Value::Null,
            "created_at": serde_json::Value::Null,
            "actor": serde_json::Value::Null,
            "webhook_timestamp": serde_json::Value::Null,
            "raw": raw_event,
        }),
        "notion" => serde_json::json!({
            "provider": "notion",
            "event": kind,
            "workspace_id": serde_json::Value::Null,
            "request_id": serde_json::Value::Null,
            "subscription_id": serde_json::Value::Null,
            "integration_id": serde_json::Value::Null,
            "attempt_number": serde_json::Value::Null,
            "entity_id": serde_json::Value::Null,
            "entity_type": serde_json::Value::Null,
            "api_version": serde_json::Value::Null,
            "verification_token": serde_json::Value::Null,
            "polled": serde_json::Value::Null,
            "raw": raw_event,
        }),
        "cron" => serde_json::json!({
            "provider": "cron",
            "cron_id": serde_json::Value::Null,
            "schedule": serde_json::Value::Null,
            "tick_at": clock::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
            "raw": raw_event,
        }),
        "webhook" => serde_json::json!({
            "provider": "webhook",
            "source": "trigger_fire",
            "content_type": "application/json",
            "raw": raw_event,
        }),
        "a2a-push" => serde_json::json!({
            "provider": "a2a-push",
            "task_id": serde_json::Value::Null,
            "task_state": serde_json::Value::Null,
            "artifact": serde_json::Value::Null,
            "sender": serde_json::Value::Null,
            "raw": raw_event,
            "kind": "a2a.task.update",
        }),
        _ => serde_json::json!({
            "provider": provider,
            "schema_name": "TriggerEvent",
            "raw": raw_event,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::rc::Rc;

    use crate::event_log::{install_default_for_base_dir, EventLog};
    use crate::events::{add_event_sink, clear_event_sinks, CollectorSink, EventLevel};
    use crate::triggers::event::{CronEventPayload, KnownProviderPayload};
    use crate::{install_manifest_triggers, register_vm_stdlib, ProviderId, ProviderPayload};

    fn manifest_binding(
        id: &str,
        fingerprint: &str,
        handler_name: &str,
        closure: Rc<crate::value::VmClosure>,
    ) -> TriggerBindingSpec {
        TriggerBindingSpec {
            id: id.to_string(),
            source: TriggerBindingSource::Manifest,
            kind: "cron".to_string(),
            provider: ProviderId::from("cron"),
            autonomy_tier: crate::AutonomyTier::ActAuto,
            handler: TriggerHandlerSpec::Local {
                raw: handler_name.to_string(),
                closure,
            },
            dispatch_priority: crate::WorkerQueuePriority::Normal,
            when: None,
            when_budget: None,
            retry: TriggerRetryConfig::default(),
            match_events: vec!["cron.tick".to_string()],
            dedupe_key: None,
            dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
            filter: None,
            daily_cost_usd: None,
            hourly_cost_usd: None,
            max_autonomous_decisions_per_hour: None,
            max_autonomous_decisions_per_day: None,
            on_budget_exhausted: crate::TriggerBudgetExhaustionStrategy::False,
            max_concurrent: None,
            flow_control: crate::triggers::TriggerFlowControlConfig::default(),
            aggregation: None,
            manifest_path: None,
            package_name: Some("workspace".to_string()),
            definition_fingerprint: fingerprint.to_string(),
        }
    }

    fn recorded_cron_event(event_id: &str, received_at: OffsetDateTime) -> TriggerEvent {
        TriggerEvent {
            id: TriggerEventId(event_id.to_string()),
            provider: ProviderId::from("cron"),
            kind: "cron.tick".to_string(),
            received_at,
            occurred_at: None,
            dedupe_key: format!("delivery-{event_id}"),
            trace_id: crate::TraceId(format!("trace-{event_id}")),
            tenant_id: None,
            headers: BTreeMap::new(),
            batch: None,
            raw_body: None,
            provider_payload: ProviderPayload::Known(KnownProviderPayload::Cron(
                CronEventPayload {
                    cron_id: Some("test-cron".to_string()),
                    schedule: Some("* * * * *".to_string()),
                    tick_at: received_at,
                    raw: serde_json::json!({ "event_id": event_id }),
                },
            )),
            signature_status: crate::SignatureStatus::Verified,
            dedupe_claimed: false,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn trigger_replay_falls_back_after_binding_version_gc() {
        crate::reset_thread_local_state();
        let sink = Rc::new(CollectorSink::new());
        clear_event_sinks();
        add_event_sink(sink.clone());

        let tempdir = tempfile::tempdir().expect("tempdir");
        let event_log = install_default_for_base_dir(tempdir.path()).expect("install event log");
        let lib_path = tempdir.path().join("lib.harn");
        fs::write(
            &lib_path,
            r#"
import "std/triggers"

pub fn on_tick_v1(event: TriggerEvent) -> dict {
  return {version: "v1", kind: event.kind}
}

pub fn on_tick_v2(event: TriggerEvent) -> dict {
  return {version: "v2", kind: event.kind}
}

pub fn on_tick_v3(event: TriggerEvent) -> dict {
  return {version: "v3", kind: event.kind}
}

pub fn on_tick_v4(event: TriggerEvent) -> dict {
  return {version: "v4", kind: event.kind}
}
"#,
        )
        .expect("write lib");

        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        vm.set_project_root(tempdir.path());
        vm.set_source_dir(tempdir.path());
        let exports = vm
            .load_module_exports(&lib_path)
            .await
            .expect("load handler exports");

        install_manifest_triggers(vec![manifest_binding(
            "replay-cron",
            "v1",
            "on_tick_v1",
            exports["on_tick_v1"].clone(),
        )])
        .await
        .expect("install v1");
        install_manifest_triggers(vec![manifest_binding(
            "replay-cron",
            "v2",
            "on_tick_v2",
            exports["on_tick_v2"].clone(),
        )])
        .await
        .expect("install v2");
        install_manifest_triggers(vec![manifest_binding(
            "replay-cron",
            "v3",
            "on_tick_v3",
            exports["on_tick_v3"].clone(),
        )])
        .await
        .expect("install v3");
        let received_at = OffsetDateTime::now_utc();
        std::thread::sleep(std::time::Duration::from_millis(10));
        install_manifest_triggers(vec![manifest_binding(
            "replay-cron",
            "v4",
            "on_tick_v4",
            exports["on_tick_v4"].clone(),
        )])
        .await
        .expect("install v4");

        assert!(matches!(
            crate::resolve_live_trigger_binding("replay-cron", Some(1)),
            Err(TriggerRegistryError::UnknownBindingVersion { .. })
        ));

        event_log
            .append(
                &Topic::new(TRIGGER_EVENTS_TOPIC).expect("valid trigger events topic"),
                LogEvent::new(
                    "trigger_event",
                    serde_json::to_value(TriggerEventRecord {
                        binding_id: "replay-cron".to_string(),
                        binding_version: 1,
                        replay_of_event_id: None,
                        event: recorded_cron_event("evt-stale", received_at),
                    })
                    .expect("encode trigger event"),
                ),
            )
            .await
            .expect("append recorded event");

        let replay = vm
            .call_named_builtin(
                "trigger_replay",
                vec![VmValue::String(Rc::from("evt-stale"))],
            )
            .await
            .expect("trigger replay succeeds");
        let replay: DispatchHandleRecord =
            serde_json::from_value(crate::llm::vm_value_to_json(&replay))
                .expect("decode replay handle");
        assert_eq!(replay.status, "dispatched");
        assert_eq!(replay.binding_id, "replay-cron");
        assert_eq!(replay.binding_version, 3);
        assert_eq!(replay.replay_of_event_id.as_deref(), Some("evt-stale"));

        let warning = sink
            .logs
            .borrow()
            .iter()
            .find(|log| log.category == "replay.binding_version_gc_fallback")
            .cloned()
            .expect("gc fallback warning");
        assert_eq!(warning.level, EventLevel::Warn);
        assert_eq!(
            warning.metadata.get("trigger_id"),
            Some(&serde_json::json!("replay-cron"))
        );
        assert_eq!(
            warning.metadata.get("recorded_version"),
            Some(&serde_json::json!(1))
        );
        assert_eq!(
            warning.metadata.get("resolved_version"),
            Some(&serde_json::json!(3))
        );

        clear_event_sinks();
        crate::events::reset_event_sinks();
        crate::reset_thread_local_state();
    }
}
