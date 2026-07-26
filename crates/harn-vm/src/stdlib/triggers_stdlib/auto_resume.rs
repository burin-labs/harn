//! Auto-resume triggers for suspended workers.
//!
//! A worker suspended with `conditions.trigger` gets a private trigger binding
//! that resumes it when the condition fires, and — when `conditions.timeout` is
//! set — a companion task that fires a synthetic `auto_resume.timeout` event
//! once the deadline passes. This module owns registering and tearing down both
//! halves, keyed by trigger id in a thread-local table so a worker that resumes
//! early cancels its own timeout.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::Duration;

use harn_parser::diagnostic_codes::Code;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::triggers::test_util::clock;
use crate::triggers::{
    dynamic_deregister, dynamic_register, resolve_live_trigger_binding, TriggerEvent,
    TriggerHandlerSpec, TriggerRegistryError,
};
use crate::value::{VmDictExt, VmError, VmValue};

use super::args::trigger_registry_error;
use super::binding_config::parse_trigger_config;
use super::journal::ensure_trigger_event_log;

thread_local! {
    static AUTO_RESUME_TIMEOUTS: RefCell<BTreeMap<String, tokio::task::JoinHandle<()>>> =
        const { RefCell::new(BTreeMap::new()) };
}

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
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
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
        schedule_auto_resume_timeout(ctx, handle.clone(), worker_id.to_string(), timeout);
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
        Some(VmValue::String(value))
            if matches!(
                value.as_str(),
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
                    value.as_str()
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
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    handle: AutoResumeTriggerHandle,
    worker_id: String,
    timeout: AutoResumeTimeoutSpec,
) {
    cancel_auto_resume_timeout(handle.id.as_str());
    let event_log = ensure_trigger_event_log();
    let base_vm = ctx
        .map(crate::vm::AsyncBuiltinCtx::child_vm)
        .unwrap_or_default();
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
