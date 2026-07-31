//! Canonical provider-visible projection of durable session history plus active directives.

use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

/// Return exactly the message view passed to model callers. Directive bodies
/// remain lifecycle events in durable history and occupy one fixed trailing
/// user slot only in this projection.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_visible_messages(session_id: string, messages?: list|nil) -> list",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_visible_messages_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(VmValue::display).unwrap_or_default();
    let messages = args
        .get(1)
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(super::vm_to_json)
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_else(|| {
            crate::agent_sessions::transcript(&session_id)
                .as_ref()
                .and_then(|value| super::dict_get(value, "messages"))
                .map(super::vm_to_json)
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default()
        });
    let reminders = crate::llm::helpers::pending_reminders_from_session(Some(&session_id));
    let capabilities = crate::llm::capabilities::Capabilities::default();
    let rendered = crate::llm::helpers::render_pending_reminders(&capabilities, &reminders);
    Ok(super::json_to_vm(&serde_json::Value::Array(
        crate::llm::helpers::apply_rendered_reminder_messages(messages, &rendered),
    )))
}

const VISIBLE_MESSAGE_BUILTINS: &[&VmBuiltinDef] =
    &[&HOST_AGENT_SESSION_VISIBLE_MESSAGES_BUILTIN_DEF];

pub(super) fn register_visible_message_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, VISIBLE_MESSAGE_BUILTINS);
}
