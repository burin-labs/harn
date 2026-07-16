//! `tools/wait_command` — wait for one background command completion.

use std::time::{Duration, Instant};

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

    if timeout_ms > 0 {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);

        let remaining = deadline.saturating_duration_since(Instant::now());
        if let Some(result) = super::long_running::wait_for_result(&handle_id, remaining) {
            // The waiter publishes the normal inbox feedback before waking
            // direct waiters. Consume that matching entry so explicit wait
            // preserves the historical "result is delivered once" contract.
            let _ = drain_matching_result(&session_id, &handle_id);
            return Ok(mark_tool_result(result));
        }
        if let Some(result) = drain_matching_result(&session_id, &handle_id) {
            return Ok(result);
        }

        // The inbox is keyed per-session, but multiple concurrent background
        // handles can share one session (notably `session_id == ""` under
        // `harn test`/headless). `wait_sync` only parks on "this session has
        // *any* entry", so a sibling handle's completion can wake us; the
        // single drain below would then requeue that foreign entry and report
        // our handle as still running. Loop until either our handle's result
        // arrives or the deadline elapses, re-parking for the remaining budget
        // after each foreign wakeup.
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let _ = harn_vm::orchestration::agent_inbox::wait_sync(&session_id, remaining);
            if let Some(result) = drain_matching_result(&session_id, &handle_id) {
                return Ok(result);
            }
        }
    }

    let mut builder = ResponseBuilder::new()
        .str("handle_id", handle_id.clone())
        .str("status", "running")
        .bool("completed", false)
        .bool("timed_out", false);
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

    /// Regression test for the shared-session-inbox race.
    ///
    /// Two background handles (`H1`, `H2`) share one session inbox (the
    /// empty-session case under `harn test`/headless). A sibling's completion
    /// (`H2`) is enqueued FIRST, then our handle's completion (`H1`) arrives
    /// from another thread *during* the wait. The old single-shot code would
    /// drain once on the first wakeup, find only `H2`, requeue it, and falsely
    /// report `H1` as `running`. The deadline loop must skip past `H2`'s
    /// wakeup and return `H1`'s result.
    #[test]
    fn wait_skips_foreign_wakeup_and_returns_own_result() {
        let session = fresh_session_id();

        // A foreign sibling completion is already queued before we wait.
        agent_inbox::push(&session, "tool_result", &result_for("H2"), "test");

        // Our handle's completion lands shortly after the wait begins, on a
        // separate thread — modelling the real waiter thread that pushes the
        // `tool_result` for our handle while we are parked.
        let push_session = session.clone();
        let pusher = std::thread::spawn(move || {
            // Park briefly so `handle` has entered its first `wait_sync`. The
            // correctness of the test does not depend on this sleep being
            // long enough: even if H1 is already present, the initial drain
            // would return it; the sleep just steers the common case through
            // the loop path that the old code got wrong.
            std::thread::sleep(Duration::from_millis(50));
            agent_inbox::push(&push_session, "tool_result", &result_for("H1"), "test");
        });

        // Generous timeout: the assertion is on the RESULT, not on timing.
        let value = handle(&wait_args(&session, "H1", 5_000)).expect("handle ok");
        pusher.join().expect("pusher join");

        assert_eq!(
            status_of(&value).as_deref(),
            Some("completed"),
            "wait must return H1's completed result, not falsely report running"
        );
        assert_eq!(handle_of(&value).as_deref(), Some("H1"));

        // H2's foreign completion must remain queued (requeued, not consumed).
        let leftover = agent_inbox::drain(&session);
        assert_eq!(leftover.len(), 1, "H2's completion must still be queued");
        let parsed: serde_json::Value = serde_json::from_str(&leftover[0].content).expect("json");
        assert_eq!(parsed.get("handle_id").and_then(|v| v.as_str()), Some("H2"));
    }

    /// Fully deterministic variant with no cross-thread timing at all: both
    /// completions are already in the inbox, with the foreign one (`H2`) at
    /// the head. `drain_matching_result` must scan past `H2` and select `H1`
    /// while requeueing `H2`. This guards the drain/requeue selection logic
    /// the deadline loop relies on.
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

    /// The `timeout_ms == 0` non-blocking poll must not park: a missing handle
    /// returns `running` immediately even with a foreign entry queued.
    #[test]
    fn zero_timeout_is_nonblocking_poll() {
        let session = fresh_session_id();
        agent_inbox::push(&session, "tool_result", &result_for("H2"), "test");
        let value = handle(&wait_args(&session, "H1", 0)).expect("handle ok");
        assert_eq!(status_of(&value).as_deref(), Some("running"));
        assert_eq!(handle_of(&value).as_deref(), Some("H1"));
        // The foreign entry is untouched by a poll for a different handle.
        assert_eq!(agent_inbox::pending_count(&session), 1);
    }
}
