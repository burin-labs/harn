//! The builtins that register, list, fire, and replay triggers.
//!
//! `trigger_fire` and `trigger_replay` share one path: record the event, resolve
//! the binding (for a replay, the version that was live when the event was first
//! received), run it through a [`Dispatcher`], and fold the dispatch outcome into
//! a `DispatchHandle` — resolving or opening a dead-letter entry on the way.

use time::OffsetDateTime;

use crate::stdlib::macros::harn_builtin;
use crate::triggers::dispatcher::current_dispatch_context;
use crate::triggers::test_util::run_trigger_harness_fixture;
use crate::triggers::{
    dynamic_register, registered_provider_metadata, resolve_live_or_as_of,
    resolve_live_trigger_binding, snapshot_trigger_bindings, RecordedTriggerBinding,
    TriggerBindingSnapshot, TriggerEvent, TriggerRegistryError,
};
use crate::value::{VmError, VmValue};

use super::args::{require_dict_arg, required_string, trigger_registry_error, value_from_serde};
use super::binding_config::parse_trigger_config;
use super::event_input::parse_trigger_event;
use super::journal::{
    append_log, ensure_trigger_event_log, find_pending_dlq_entry_for_event, find_replayable_event,
    resolve_dlq_entry, upsert_dlq_entry, DispatchHandleRecord, DlqEntryRecord, TriggerEventRecord,
};
use super::TRIGGER_EVENTS_TOPIC;
use crate::event_log::LogEvent;

#[harn_builtin(sig = "handler_context() -> dict | nil", category = "triggers")]
fn handler_context_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(sig = "list_providers_native() -> list", category = "triggers")]
fn list_providers_native_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::List(std::sync::Arc::new(
        registered_provider_metadata()
            .into_iter()
            .map(|provider| value_from_serde(&provider))
            .collect(),
    )))
}

#[harn_builtin(sig = "trigger_list(...args: any) -> list", category = "triggers")]
fn trigger_list_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::List(std::sync::Arc::new(
        snapshot_trigger_bindings()
            .into_iter()
            .map(|binding| value_from_serde(&binding))
            .collect(),
    )))
}

#[harn_builtin(
    sig = "trigger_register(...args: any) -> TriggerHandle",
    kind = "async",
    category = "triggers"
)]
async fn trigger_register_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let config = require_dict_arg(&args, 0, "trigger_register")?;
    let spec = parse_trigger_config(config)?;
    let id = dynamic_register(spec)
        .await
        .map_err(trigger_registry_error)?;
    let binding =
        resolve_live_trigger_binding(id.as_str(), None).map_err(trigger_registry_error)?;
    Ok(value_from_serde(&binding.snapshot()))
}

#[harn_builtin(
    sig = "trigger_fire(...args: any) -> DispatchHandle",
    kind = "async",
    category = "triggers"
)]
async fn trigger_fire_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let (binding_id, binding_version) = trigger_handle_from_args(&args, "trigger_fire")?;
    let raw_event = args
        .get(1)
        .ok_or_else(|| VmError::Runtime("trigger_fire: missing trigger event".to_string()))?;
    let event = parse_trigger_event(raw_event)?;
    dispatch_trigger_event(Some(&ctx), binding_id, binding_version, event, None, None).await
}

#[harn_builtin(
    sig = "trigger_replay(...args: any) -> DispatchHandle",
    kind = "async",
    category = "triggers"
)]
async fn trigger_replay_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let event_id = args
        .first()
        .and_then(|value| match value {
            VmValue::String(text) => Some(text.to_string()),
            _ => None,
        })
        .ok_or_else(|| VmError::Runtime("trigger_replay: expected event id string".to_string()))?;
    replay_trigger_event(Some(&ctx), &event_id).await
}

#[harn_builtin(
    sig = "trigger_test_harness(...args: any) -> dict",
    kind = "async",
    category = "triggers"
)]
async fn trigger_test_harness_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
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
}

async fn dispatch_trigger_event(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
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
        dispatch_binding_via_dispatcher(ctx, &binding, &event, replay_of_event_id.clone()).await?;
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

async fn replay_trigger_event(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    event_id: &str,
) -> Result<VmValue, VmError> {
    let record = find_replayable_event(event_id).await?;
    let received_at = record.event.received_at;
    dispatch_trigger_event(
        ctx,
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
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    binding: &crate::triggers::registry::TriggerBinding,
    event: &TriggerEvent,
    replay_of_event_id: Option<String>,
) -> Result<crate::triggers::DispatchOutcome, VmError> {
    let base_vm = ctx
        .map(crate::vm::AsyncBuiltinCtx::child_vm)
        .ok_or_else(|| {
            VmError::Runtime(
                "trigger stdlib builtins require an async builtin VM context".to_string(),
            )
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
