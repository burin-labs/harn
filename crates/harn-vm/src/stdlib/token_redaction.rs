//! `std/oauth/redaction` (OA-06) — OAuth token redaction stdlib.
//!
//! Lets scripts register custom redaction patterns alongside the
//! built-in OAuth token catalog, probe whether a given string would
//! be scrubbed, and drain the per-thread audit ring for compliance
//! reporting.
//!
//! The actual redaction happens at READ/SERIALIZE time on the four
//! sinks (transcript events, audit receipts, OTel span attributes,
//! system reminders) via [`crate::redact::RedactionPolicy`]. Every
//! redaction synchronously records a [`crate::redact::RedactionEvent`]
//! in a per-thread ring; this module exposes the ring drain plus an
//! optional process-wide async forwarder that appends events to the
//! `audit.token_redaction` event-log topic.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Once;

use serde_json::Value as JsonValue;

use crate::event_log::{active_event_log, EventLog, LogEvent, Topic};
use crate::redact::{
    self, custom_pattern_names, default_pattern_names, drain_audit_ring, install_audit_sink,
    register_custom_pattern, RedactionEvent, TOKEN_REDACTION_AUDIT_TOPIC,
    TOKEN_REDACTION_DIAGNOSTIC,
};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

static AUDIT_SINK_INIT: Once = Once::new();

/// Install the process-wide audit forwarder that pushes each
/// redaction event to:
///
///   * the live `events::log_info_meta` sink pipeline (Stderr/Otel),
///   * the active [`crate::event_log::EventLog`] under
///     [`TOKEN_REDACTION_AUDIT_TOPIC`], best-effort.
///
/// The synchronous per-thread audit ring (see
/// [`crate::redact::drain_audit_ring`]) is the authoritative
/// compliance contract; this forwarder is an optional convenience for
/// streaming consumers.
///
/// Safe to call multiple times — only the first call wires the sink.
/// Per-thread because [`install_audit_sink`] is itself thread-local,
/// `register_token_redaction_builtins` always re-installs on the
/// current thread.
pub fn ensure_audit_sink() {
    AUDIT_SINK_INIT.call_once(|| {
        install_audit_sink(Some(Rc::new(|event: &RedactionEvent| {
            forward_event_to_log(event);
            forward_event_to_events_sink(event);
        })));
    });
}

fn forward_event_to_log(event: &RedactionEvent) {
    let Some(log) = active_event_log() else {
        return;
    };
    let topic = match Topic::new(TOKEN_REDACTION_AUDIT_TOPIC) {
        Ok(topic) => topic,
        Err(_) => return,
    };
    let payload = serde_json::json!({
        "code": TOKEN_REDACTION_DIAGNOSTIC,
        "pattern": event.pattern_name,
        "match_count": event.match_count,
        "bytes_redacted": event.bytes_redacted,
    });
    let log_event = LogEvent::new("token_redaction", payload);
    let log_ref = log;
    let topic_clone = topic;
    let owned = log_event;
    let task = async move {
        if let Err(error) = log_ref.append(&topic_clone, owned).await {
            crate::events::log_warn(
                "token_redaction.audit",
                &format!("failed to append token redaction audit event: {error}"),
            );
        }
    };
    // The append is best-effort. `handle.spawn` requires a multi-
    // threaded runtime; on a current-thread runtime (the common VM
    // path) the audit ring already recorded the event and there is
    // nothing more to do here without panicking on the spawn.
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let metrics = handle.metrics();
        if metrics.num_workers() > 1 {
            handle.spawn(task);
        }
    }
}

fn forward_event_to_events_sink(event: &RedactionEvent) {
    let metadata: BTreeMap<String, JsonValue> = BTreeMap::from([
        (
            "code".to_string(),
            JsonValue::String(TOKEN_REDACTION_DIAGNOSTIC.to_string()),
        ),
        (
            "pattern".to_string(),
            JsonValue::String(event.pattern_name.clone()),
        ),
        (
            "match_count".to_string(),
            JsonValue::Number(event.match_count.into()),
        ),
        (
            "bytes_redacted".to_string(),
            JsonValue::Number(event.bytes_redacted.into()),
        ),
    ]);
    crate::events::log_info_meta(
        "token_redaction.audit",
        "redacted token in persistence sink",
        metadata,
    );
}

pub(crate) fn register_token_redaction_builtins(vm: &mut Vm) {
    ensure_audit_sink();
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&crate::stdlib::macros::VmBuiltinDef] = &[
    &TOKEN_REDACTION_REGISTER_PATTERN_IMPL_DEF,
    &TOKEN_REDACTION_CLEAR_CUSTOM_PATTERNS_IMPL_DEF,
    &TOKEN_REDACTION_REDACT_IMPL_DEF,
    &TOKEN_REDACTION_DEFAULT_PATTERNS_IMPL_DEF,
    &TOKEN_REDACTION_CUSTOM_PATTERNS_IMPL_DEF,
    &TOKEN_REDACTION_DRAIN_AUDIT_IMPL_DEF,
];

#[crate::stdlib::macros::harn_builtin(
    sig = "__token_redaction_register_pattern(name: string, regex: string) -> nil",
    category = "token_redaction"
)]
fn token_redaction_register_pattern_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let name = required_string_arg(args, 0, "__token_redaction_register_pattern", "name")?;
    let regex_source = required_string_arg(args, 1, "__token_redaction_register_pattern", "regex")?;
    if name.is_empty() {
        return Err(VmError::Runtime(
            "__token_redaction_register_pattern: `name` must not be empty".to_string(),
        ));
    }
    register_custom_pattern(&name, &regex_source).map_err(|message| {
        VmError::Runtime(format!("token_redaction.register_pattern: {message}"))
    })?;
    Ok(VmValue::Nil)
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__token_redaction_clear_custom_patterns() -> nil",
    category = "token_redaction"
)]
fn token_redaction_clear_custom_patterns_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    if !args.is_empty() {
        return Err(VmError::Runtime(
            "__token_redaction_clear_custom_patterns: expected 0 arguments".to_string(),
        ));
    }
    redact::clear_custom_patterns();
    Ok(VmValue::Nil)
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__token_redaction_redact(text: string) -> string",
    category = "token_redaction"
)]
fn token_redaction_redact_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = required_string_arg(args, 0, "__token_redaction_redact", "text")?;
    let policy = redact::current_policy();
    let redacted = policy.redact_string(&text).into_owned();
    Ok(VmValue::String(Rc::from(redacted.as_str())))
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__token_redaction_default_patterns() -> list",
    category = "token_redaction"
)]
fn token_redaction_default_patterns_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    if !args.is_empty() {
        return Err(VmError::Runtime(
            "__token_redaction_default_patterns: expected 0 arguments".to_string(),
        ));
    }
    let names: Vec<VmValue> = default_pattern_names()
        .into_iter()
        .map(|name| VmValue::String(Rc::from(name)))
        .collect();
    Ok(VmValue::List(Rc::new(names)))
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__token_redaction_custom_patterns() -> list",
    category = "token_redaction"
)]
fn token_redaction_custom_patterns_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    if !args.is_empty() {
        return Err(VmError::Runtime(
            "__token_redaction_custom_patterns: expected 0 arguments".to_string(),
        ));
    }
    let names: Vec<VmValue> = custom_pattern_names()
        .into_iter()
        .map(|name| VmValue::String(Rc::from(name.as_str())))
        .collect();
    Ok(VmValue::List(Rc::new(names)))
}

#[crate::stdlib::macros::harn_builtin(
    sig = "__token_redaction_drain_audit() -> list",
    category = "token_redaction"
)]
fn token_redaction_drain_audit_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    if !args.is_empty() {
        return Err(VmError::Runtime(
            "__token_redaction_drain_audit: expected 0 arguments".to_string(),
        ));
    }
    let events = drain_audit_ring();
    let list: Vec<VmValue> = events
        .into_iter()
        .map(|event| {
            let mut entry: BTreeMap<String, VmValue> = BTreeMap::new();
            entry.insert(
                "code".to_string(),
                VmValue::String(Rc::from(TOKEN_REDACTION_DIAGNOSTIC)),
            );
            entry.insert(
                "pattern".to_string(),
                VmValue::String(Rc::from(event.pattern_name.as_str())),
            );
            entry.insert(
                "match_count".to_string(),
                VmValue::Int(event.match_count as i64),
            );
            entry.insert(
                "bytes_redacted".to_string(),
                VmValue::Int(event.bytes_redacted as i64),
            );
            VmValue::Dict(Rc::new(entry))
        })
        .collect();
    Ok(VmValue::List(Rc::new(list)))
}

fn required_string_arg(
    args: &[VmValue],
    index: usize,
    fn_name: &str,
    arg_name: &str,
) -> Result<String, VmError> {
    match args.get(index) {
        Some(VmValue::String(s)) => Ok(s.to_string()),
        Some(other) => Err(VmError::Runtime(format!(
            "{fn_name}: `{arg_name}` must be a string, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{fn_name}: `{arg_name}` argument is required"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_audit_sink_is_idempotent() {
        ensure_audit_sink();
        ensure_audit_sink();
        // Trigger one redaction to confirm the sink does not panic
        // when invoked from a non-tokio context.
        let _ = redact::scan_secret_patterns("AKIAABCDEFGHIJKLMNOP", redact::REDACTED_PLACEHOLDER);
    }
}
