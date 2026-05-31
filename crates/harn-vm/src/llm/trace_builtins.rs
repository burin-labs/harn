use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

use super::{helpers, trace};

/// Return captured agent trace events for the current process.
#[harn_builtin(sig = "agent_trace() -> list", category = "agent.trace")]
fn agent_trace_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let events = trace::peek_agent_trace();
    let list: Vec<VmValue> = events
        .iter()
        .filter_map(|e| serde_json::to_value(e).ok())
        .map(|v| json_to_vm_value(&v))
        .collect();
    Ok(VmValue::List(std::sync::Arc::new(list)))
}

/// Return a summarized view of captured agent trace events.
#[harn_builtin(sig = "agent_trace_summary() -> dict", category = "agent.trace")]
fn agent_trace_summary_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let summary = trace::agent_trace_summary();
    Ok(json_to_vm_value(&summary))
}

/// Record a typed-output checkpoint event in the current agent trace.
#[harn_builtin(
    sig = "__host_typed_checkpoint_trace(checkpoint: dict) -> nil",
    category = "agent.trace",
    runtime_only = true
)]
fn host_typed_checkpoint_trace_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let checkpoint = args.first().cloned().unwrap_or(VmValue::Nil);
    let checkpoint_json = helpers::vm_value_to_json(&checkpoint);
    let object = checkpoint_json.as_object();
    let string_field = |key: &str| -> String {
        object
            .and_then(|obj| obj.get(key))
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string()
    };
    let opt_string_field = |key: &str| -> Option<String> {
        object
            .and_then(|obj| obj.get(key))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .filter(|value| !value.is_empty())
    };
    let usize_field = |key: &str| -> usize {
        object
            .and_then(|obj| obj.get(key))
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as usize
    };
    let bool_field = |key: &str| -> bool {
        object
            .and_then(|obj| obj.get(key))
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    };
    let list_strings = |key: &str| -> Vec<String> {
        object
            .and_then(|obj| obj.get(key))
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| item.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut errors = list_strings("validation_errors");
    if errors.is_empty() {
        errors = list_strings("validator_failures");
    }
    if errors.is_empty() {
        errors = list_strings("schema_failures");
    }
    if errors.is_empty() {
        errors = list_strings("parse_failures");
    }

    trace::emit_agent_event(trace::AgentTraceEvent::TypedCheckpoint {
        name: string_field("name"),
        status: string_field("status"),
        checkpoint_attempts: usize_field("checkpoint_attempts"),
        llm_attempts: usize_field("attempts"),
        error_category: opt_string_field("error_category"),
        errors,
        repaired: bool_field("repaired"),
        final_accepted: bool_field("final_accepted"),
        raw_text: string_field("raw_text"),
    });
    Ok(VmValue::Nil)
}
