//! Read back the typed control words a session accepted.
//!
//! `crate::agent_sessions::control_events` is the reader for the rows
//! `record_control_event` writes at acceptance. This exposes it to the
//! stdlib so an exit authority can see a stop.
//!
//! A stop is why this exists. A steer arrives as a delivered user message and
//! is therefore recoverable from transcript history; a stop unwinds the loop
//! and delivers nothing, so without these rows an authority reading the
//! transcript alone reports a stopped run as still owing its whole task.

use super::*;

/// Return every typed control event recorded for an agent session, oldest
/// first. An unknown session and a session nobody used a control word on both
/// return an empty list, so a caller must not read emptiness as proof that no
/// control happened.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_control_events(session_id: string) -> list",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_control_events_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let events = crate::agent_sessions::control_events(&session_id)
        .iter()
        .map(|event| crate::stdlib::json_to_vm_value(&event.to_payload()))
        .collect::<Vec<_>>();
    Ok(VmValue::List(std::sync::Arc::new(events)))
}

const CONTROL_HISTORY_BUILTINS: &[&VmBuiltinDef] =
    &[&HOST_AGENT_SESSION_CONTROL_EVENTS_BUILTIN_DEF];

pub(super) fn register_control_history_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, CONTROL_HISTORY_BUILTINS);
}
