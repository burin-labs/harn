//! `tools/wait_command` — wait for one background command completion.

use std::time::Duration;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::json::vm_value_to_json;
use crate::tools::args::to_agent_path;
use crate::tools::payload::{optional_string, optional_u64, require_dict_arg, require_string};
use crate::tools::proc;
use crate::tools::response::ResponseBuilder;

pub(crate) const NAME: &str = "hostlib_tools_wait_command";

const RUNNING_INLINE_OUTPUT_MAX_BYTES: u64 = 32 * 1024;

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
    match super::long_running::wait_for_result(&handle_id, Duration::from_millis(timeout_ms)) {
        super::long_running::BackgroundWaitOutcome::Completed(result) => {
            // The same waiter publishes inbox feedback before notifying direct
            // waiters. Consume only this handle's copy; sibling feedback stays.
            let _ = drain_matching_result(&session_id, &handle_id);
            return Ok(mark_tool_result(result));
        }
        super::long_running::BackgroundWaitOutcome::Unknown => {
            return Err(HostlibError::InvalidParameter {
                builtin: NAME,
                param: "handle_id",
                message: format!(
                    "unknown command handle {handle_id:?}; use the handle returned by run_command"
                ),
            });
        }
        super::long_running::BackgroundWaitOutcome::Running => {}
    }

    let mut builder = ResponseBuilder::new()
        .str("handle_id", handle_id.clone())
        .str("status", "running")
        .bool("completed", false)
        .bool("timed_out", false);
    if let Some(cwd) = super::long_running::cwd_for_handle(&handle_id) {
        builder = builder.str("cwd", to_agent_path(&cwd));
    }
    if let Some(snapshot_binding) = super::long_running::snapshot_binding_for_handle(&handle_id) {
        builder = builder.dict("snapshot_binding", snapshot_binding);
    }
    if let Some(artifacts) = proc::live_artifact_snapshot(None, Some(&handle_id)) {
        builder = builder
            .str("output_path", to_agent_path(&artifacts.output_path))
            .str("stdout_path", to_agent_path(&artifacts.stdout_path))
            .str("stderr_path", to_agent_path(&artifacts.stderr_path))
            .int("line_count", artifacts.line_count as i64)
            .int("byte_count", artifacts.byte_count as i64)
            .str("output_sha256", artifacts.output_sha256);
        if let Some(output) =
            proc::live_artifact_tail(None, Some(&handle_id), RUNNING_INLINE_OUTPUT_MAX_BYTES)
        {
            if !output.is_empty() {
                builder = builder
                    .str("combined", output.clone())
                    .str("inline_output", output);
            }
        }
    }
    Ok(builder.build())
}

pub(crate) fn drain_matching_result(session_id: &str, handle_id: &str) -> Option<VmValue> {
    let entries =
        super::long_running::drain_handle_feedback(session_id, handle_id, &["tool_result"]);
    let mut selected = None;

    for entry in entries {
        let parsed = serde_json::from_str::<serde_json::Value>(&entry.content).ok();
        // A handle has one immutable terminal receipt. Consume every matching
        // inbox copy in this atomic drain and project the first valid payload;
        // retaining an accidental duplicate would replay stale completion on a
        // later wait even though the receipt is already observable by handle.
        if selected.is_none() {
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
            }
        }
    }

    selected
}

fn mark_tool_result(value: VmValue) -> VmValue {
    let mut payload = vm_value_to_json(&value);
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "feedback_kind".to_string(),
            serde_json::Value::String("tool_result".to_string()),
        );
        object
            .entry("timed_out".to_string())
            .or_insert(serde_json::Value::Bool(false));
    }
    harn_vm::json_to_vm_value(&payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_vm::orchestration::agent_inbox;

    fn fresh_session_id() -> String {
        // Each test owns its own session id so the global inbox registry
        // doesn't need per-test wipes and concurrent test runs stay isolated.
        format!("wait-cmd-test-{}", uuid::Uuid::now_v7())
    }

    fn result_for(handle_id: &str) -> String {
        serde_json::json!({
            "handle_id": handle_id,
            "status": "completed",
            "exit_code": 0,
        })
        .to_string()
    }

    fn wait_args(session_id: &str, handle_id: &str, timeout_ms: u64) -> Vec<VmValue> {
        let request = serde_json::json!({
            "session_id": session_id,
            "handle_id": handle_id,
            "timeout_ms": timeout_ms,
        });
        vec![harn_vm::json_to_vm_value(&request)]
    }

    fn field(value: &VmValue, key: &str) -> Option<String> {
        let VmValue::Dict(map) = value else {
            return None;
        };
        match map.get(key) {
            Some(VmValue::String(s)) => Some(s.to_string()),
            _ => None,
        }
    }

    fn status_of(value: &VmValue) -> Option<String> {
        field(value, "status")
    }

    fn handle_of(value: &VmValue) -> Option<String> {
        field(value, "handle_id")
    }

    /// A real registered handle waits on its own notifier, even when a sibling
    /// already has feedback in the session inbox. No wall-clock sleep is needed.
    #[test]
    fn wait_skips_foreign_feedback_and_returns_registered_result() {
        use crate::process::{install_spawner, ExitStatus, MockProcessConfig, MockSpawner};
        let session = fresh_session_id();
        let spawner = std::sync::Arc::new(MockSpawner::new());
        let controller = spawner.enqueue(MockProcessConfig::running());
        let _guard = install_spawner(spawner);
        let info = super::super::long_running::spawn_long_running(
            NAME,
            "echo".into(),
            vec!["mock".into()],
            None,
            std::collections::BTreeMap::new(),
            session.clone(),
        )
        .expect("registered mock command");
        agent_inbox::push(&session, "tool_result", &result_for("H2"), "test");
        let wait_session = session.clone();
        let wait_handle = info.handle_id.clone();
        let waiter =
            std::thread::spawn(move || handle(&wait_args(&wait_session, &wait_handle, 5_000)));
        controller.complete_with(ExitStatus::from_code(0));
        let value = waiter
            .join()
            .expect("waiter join")
            .expect("completed command");
        assert_eq!(status_of(&value).as_deref(), Some("completed"));
        assert_eq!(handle_of(&value).as_deref(), Some(info.handle_id.as_str()));
        let leftover = agent_inbox::drain(&session);
        assert_eq!(leftover.len(), 1, "the sibling completion remains queued");
        let parsed: serde_json::Value = serde_json::from_str(&leftover[0].content).expect("json");
        assert_eq!(parsed.get("handle_id").and_then(|v| v.as_str()), Some("H2"));
    }

    /// Fully deterministic variant with no cross-thread timing at all: both
    /// completions are already in the inbox, with the foreign one (`H2`) at
    /// the head. `drain_matching_result` must scan past `H2` and select `H1`
    /// while requeueing `H2`. This guards the drain/requeue selection logic
    /// shared inbox delivery relies on.
    #[test]
    fn drain_selects_own_handle_past_foreign_head() {
        let session = fresh_session_id();
        agent_inbox::push(&session, "tool_result", &result_for("H2"), "test");
        agent_inbox::push(&session, "tool_result", &result_for("H1"), "test");

        let value = handle(&wait_args(&session, "H1", 5_000)).expect("handle ok");
        assert_eq!(status_of(&value).as_deref(), Some("completed"));
        assert_eq!(handle_of(&value).as_deref(), Some("H1"));

        let leftover = agent_inbox::drain(&session);
        assert_eq!(leftover.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&leftover[0].content).expect("json");
        assert_eq!(parsed.get("handle_id").and_then(|v| v.as_str()), Some("H2"));
    }

    #[test]
    fn drain_consumes_duplicate_terminal_feedback_once() {
        let session = fresh_session_id();
        agent_inbox::push(&session, "tool_result", &result_for("H1"), "test");
        agent_inbox::push(&session, "tool_result", &result_for("H1"), "test");

        let value = drain_matching_result(&session, "H1").expect("terminal result");
        assert_eq!(status_of(&value).as_deref(), Some("completed"));
        assert!(
            agent_inbox::drain(&session).is_empty(),
            "duplicate terminal feedback must not replay after one consumption",
        );
    }

    /// Neither poll nor wait can report an unregistered handle as running.
    /// A nonzero budget must not turn an invalid handle into a timed wait.
    #[test]
    fn unknown_handle_is_refused_for_poll_and_wait() {
        let session = fresh_session_id();
        agent_inbox::push(&session, "tool_result", &result_for("H2"), "test");
        for timeout_ms in [0, 120_000] {
            let error = handle(&wait_args(&session, "H1", timeout_ms)).unwrap_err();
            assert!(matches!(
                error,
                HostlibError::InvalidParameter {
                    param: "handle_id",
                    ..
                }
            ));
            assert!(error.to_string().contains("unknown command handle"));
        }
        // The foreign entry is untouched by a poll for a different handle.
        assert_eq!(agent_inbox::pending_count(&session), 1);
    }
}
