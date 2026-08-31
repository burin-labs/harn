//! Auto-resume triggers for suspended workers.
//!
//! A worker suspended with `conditions.trigger` gets a private trigger binding
//! that resumes it when the condition fires, and — when `conditions.timeout` is
//! set — a companion task that fires a synthetic `auto_resume.timeout` event
//! once the deadline passes. This module owns registering and tearing down both
//! halves, keyed by trigger id in a thread-local table so a worker that resumes
//! early cancels its own timeout.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use harn_builtin_meta::shapes::{
    is_resume_timeout_action, DEFAULT_RESUME_TIMEOUT_ACTION, RESUME_TIMEOUT_ACTIONS,
};
use harn_parser::diagnostic_codes::Code;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::triggers::{
    dynamic_deregister, dynamic_register, resolve_live_trigger_binding, TriggerEvent,
    TriggerHandlerSpec, TriggerRegistryError,
};
use crate::value::{VmDictExt, VmError, VmValue};

use super::args::trigger_registry_error;
use super::binding_config::parse_trigger_config;
use super::journal::ensure_trigger_event_log;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AutoResumeTriggerHandle {
    pub(crate) id: String,
    pub(crate) version: u32,
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
    ctx: &crate::vm::AsyncBuiltinCtx,
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
    normalized.put_str("handler", "worker://__resume_auto_resume__");
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
    spec.definition_fingerprint = crate::canonical_json::to_string(&serde_json::json!({
        "kind": "auto_resume",
        "worker_id": worker_id,
        "trigger": crate::llm::vm_value_to_json(trigger),
        "match_events": spec.match_events,
    }));

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
        schedule_auto_resume_timeout(ctx, handle.clone(), worker_id.to_string(), timeout).await;
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
    for task in crate::triggers::registry::active_trigger_registry().drain_background_tasks() {
        task.abort();
    }
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
    conditions: &crate::value::DictMap,
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
        Some(VmValue::String(value)) if is_resume_timeout_action(value) => value.to_string(),
        Some(VmValue::Nil) | None => DEFAULT_RESUME_TIMEOUT_ACTION.to_string(),
        Some(VmValue::String(value)) => {
            return Err(suspend_diagnostic_error(
                Code::ResumeTimeoutUnsupported,
                format!(
                "auto_resume: unsupported conditions.timeout.on_timeout `{}`; expected one of: {}",
                value.as_str(),
                RESUME_TIMEOUT_ACTIONS.join(", ")
            ),
            ))
        }
        Some(other) => {
            return Err(suspend_diagnostic_error(
                Code::ResumeTimeoutUnsupported,
                format!(
                    "auto_resume: conditions.timeout.on_timeout must be one of: {}; got {}",
                    RESUME_TIMEOUT_ACTIONS.join(", "),
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
    if let Some(task) =
        crate::triggers::registry::active_trigger_registry().cancel_background_task(trigger_id)
    {
        task.abort();
    }
}

async fn schedule_auto_resume_timeout(
    ctx: &crate::vm::AsyncBuiltinCtx,
    handle: AutoResumeTriggerHandle,
    worker_id: String,
    timeout: AutoResumeTimeoutSpec,
) {
    cancel_auto_resume_timeout(handle.id.as_str());
    let event_log = ensure_trigger_event_log();
    let base_vm = ctx.child_vm();
    let worker_registry = base_vm.worker_registry.clone();
    let pool_registry = base_vm.pool_registry.clone();
    let harness_inner = base_vm.harness().map(|harness| Arc::clone(harness.inner()));
    let duration = Duration::from_secs(timeout.duration_minutes.saturating_mul(60));
    let harness_inner =
        harness_inner.expect("auto-resume timeout requires Harness clock authority");
    let now_unix_ms = harn_clock::now_wall_ms(harness_inner.clock().as_ref());
    let deadline_unix_ms = now_unix_ms.saturating_add(duration.as_millis() as i64);
    let event = auto_resume_timeout_event(&handle, &worker_id, &timeout);
    let trigger_id = handle.id.clone();
    let version = handle.version;
    let task_trigger_id = trigger_id.clone();
    let (armed_tx, armed_rx) = tokio::sync::oneshot::channel();
    let (completion_tx, completion_rx) = tokio::sync::watch::channel(false);
    let timeout_task = async move {
        armed_tx
            .send(())
            .expect("auto-resume timeout scheduler dropped its registration barrier");
        harness_inner.wait_for_clock_advance(duration).await;
        crate::triggers::registry::active_trigger_registry()
            .detach_background_task(task_trigger_id.as_str());
        if let Ok(binding) = resolve_live_trigger_binding(trigger_id.as_str(), Some(version)) {
            let dispatcher = crate::triggers::Dispatcher::with_event_log(base_vm, event_log);
            let _ = dispatcher.dispatch(&binding, event).await;
        }
        let _ = completion_tx.send(true);
    };
    let task = crate::vm::subtask::spawn_inherited_child(
        pool_registry,
        crate::stdlib::agents::agents_workers::scope_worker_registry(worker_registry, timeout_task),
    );
    let replaced = crate::triggers::registry::active_trigger_registry().replace_background_task(
        handle.id.clone(),
        task,
        deadline_unix_ms,
        completion_rx,
    );
    assert!(
        replaced.is_none(),
        "auto-resume timeout was replaced without cancellation: {}",
        handle.id
    );
    armed_rx
        .await
        .expect("auto-resume timeout task exited before registration");
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
