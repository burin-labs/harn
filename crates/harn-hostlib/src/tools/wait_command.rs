//! `tools/wait_command` — wait for one background command completion.

use std::time::Duration;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::payload::{optional_string, optional_u64, require_dict_arg, require_string};
use crate::tools::response::ResponseBuilder;

pub(crate) const NAME: &str = "hostlib_tools_wait_command";

pub(crate) fn handle(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let map = require_dict_arg(NAME, args)?;
    let handle_id = require_string(NAME, &map, "handle_id")?;
    let timeout_ms = optional_u64(NAME, &map, "timeout_ms")?.unwrap_or(0);
    let session_id = optional_string(NAME, &map, "session_id")?
        .or_else(harn_vm::current_agent_session_id)
        .unwrap_or_default();

    if let Some(result) = drain_matching_result(&session_id, &handle_id) {
        return Ok(result);
    }

    if timeout_ms > 0 {
        let _ = harn_vm::orchestration::agent_inbox::wait_sync(
            &session_id,
            Duration::from_millis(timeout_ms),
        );
        if let Some(result) = drain_matching_result(&session_id, &handle_id) {
            return Ok(result);
        }
    }

    Ok(ResponseBuilder::new()
        .str("handle_id", handle_id)
        .str("status", "running")
        .bool("completed", false)
        .bool("timed_out", false)
        .build())
}

fn drain_matching_result(session_id: &str, handle_id: &str) -> Option<VmValue> {
    let entries = harn_vm::orchestration::agent_inbox::drain(session_id);
    let mut kept = Vec::new();
    let mut selected = None;

    for entry in entries {
        let parsed = serde_json::from_str::<serde_json::Value>(&entry.content).ok();
        let matches = entry.kind == "tool_result"
            && parsed
                .as_ref()
                .and_then(|value| value.get("handle_id"))
                .and_then(serde_json::Value::as_str)
                == Some(handle_id);
        if matches && selected.is_none() {
            if let Some(mut payload) = parsed {
                if let Some(object) = payload.as_object_mut() {
                    object.insert(
                        "feedback_kind".to_string(),
                        serde_json::Value::String(entry.kind.clone()),
                    );
                    object
                        .entry("timed_out".to_string())
                        .or_insert(serde_json::Value::Bool(false));
                }
                selected = Some(harn_vm::json_to_vm_value(&payload));
                continue;
            }
        }
        kept.push(entry);
    }

    for entry in kept.into_iter().rev() {
        harn_vm::orchestration::agent_inbox::requeue_front(entry);
    }

    selected
}
