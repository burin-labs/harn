use crate::value::{VmDictExt, VmValue};

fn insert_missing(
    object: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: serde_json::Value,
) {
    object.entry(key.to_string()).or_insert(value);
}

fn object_field<'a>(
    object: &'a mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> &'a mut serde_json::Map<String, serde_json::Value> {
    let value = object
        .entry(key.to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !value.is_object() {
        *value = serde_json::json!({});
    }
    value
        .as_object_mut()
        .expect("object field was normalized immediately above")
}

/// Project every admission-time terminal through the same public
/// `AgentResult` contract as a loop that reaches session finalization.
///
/// Hook vetoes, nested-budget denials, and autonomy approval requests all
/// finish before a live host session exists, so they cannot call the regular
/// finalize path. Their policy-specific fields remain lossless; this boundary
/// supplies the common terminal, usage, transcript, and identity projections
/// exactly once.
fn canonical_init_result(session_id: &str, run_id: &str, task: &str, result: VmValue) -> VmValue {
    let mut json = super::vm_to_json(&result);
    let Some(object) = json.as_object_mut() else {
        return result;
    };

    let status = object
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("blocked")
        .to_string();
    let final_status = object
        .get("final_status")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| match status.as_str() {
            "approval_required" => "budget_exhausted".to_string(),
            _ => status.clone(),
        });
    let stop_reason = object
        .get("stop_reason")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| match status.as_str() {
            "approval_required" => "autonomy_budget_denied".to_string(),
            _ => final_status.clone(),
        });
    let terminal_error = object.get("error").filter(|value| !value.is_null());
    let terminal_class = object
        .get("terminal_class")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            super::agent_terminal_class(&final_status, &stop_reason, terminal_error)
                .map(|class| class.as_str().to_string())
        });
    let terminal = crate::agent_events::AgentTerminalOutcome::new(
        crate::agent_events::classify_agent_terminal(
            &final_status,
            &stop_reason,
            terminal_error.is_some(),
            terminal_class.as_deref(),
        ),
        &stop_reason,
    )
    .to_json();
    let transcript = crate::agent_sessions::transcript(session_id)
        .as_ref()
        .map(super::vm_to_json)
        .unwrap_or(serde_json::Value::Null);

    object.insert("run_id".to_string(), serde_json::json!(run_id));
    object.insert("session_id".to_string(), serde_json::json!(session_id));
    object.insert("task".to_string(), serde_json::json!(task));
    insert_missing(object, "status", serde_json::json!(status));
    insert_missing(object, "final_status", serde_json::json!(final_status));
    insert_missing(object, "stop_reason", serde_json::json!(stop_reason));
    insert_missing(
        object,
        "acp_stop_reason",
        serde_json::json!(super::canonical_acp_stop_reason(&final_status, 0, 0, None,)),
    );
    insert_missing(
        object,
        "terminal_class",
        terminal_class
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );
    insert_missing(object, "terminal", terminal);
    insert_missing(object, "error", serde_json::Value::Null);
    insert_missing(object, "text", serde_json::json!(""));
    insert_missing(object, "visible_text", serde_json::json!(""));
    insert_missing(object, "private_reasoning", serde_json::Value::Null);
    insert_missing(object, "thinking_summary", serde_json::Value::Null);
    insert_missing(object, "transcript", transcript);
    insert_missing(object, "trace", serde_json::Value::Null);
    insert_missing(object, "tokens_used", serde_json::json!(0));
    insert_missing(object, "cost_usd", serde_json::json!(0.0));
    insert_missing(object, "known_cost_usd", serde_json::json!(0.0));
    insert_missing(object, "unpriced_calls", serde_json::json!(0));
    insert_missing(object, "usage_unknown_calls", serde_json::json!(0));
    insert_missing(object, "started_at", serde_json::json!(super::now_id()));
    insert_missing(object, "daemon_state", serde_json::Value::Null);
    insert_missing(object, "daemon_snapshot_path", serde_json::Value::Null);

    let llm = object_field(object, "llm");
    insert_missing(
        llm,
        "token_scope",
        serde_json::json!("accepted_turn_results"),
    );
    insert_missing(llm, "iterations", serde_json::json!(0));
    insert_missing(llm, "duration_ms", serde_json::json!(0));
    insert_missing(llm, "input_tokens", serde_json::json!(0));
    insert_missing(llm, "output_tokens", serde_json::json!(0));
    insert_missing(llm, "cache_read_tokens", serde_json::json!(0));
    insert_missing(llm, "cache_write_tokens", serde_json::json!(0));
    insert_missing(llm, "accounting_status", serde_json::json!("reported"));
    insert_missing(llm, "known_cost_usd", serde_json::json!(0.0));
    insert_missing(llm, "unpriced_calls", serde_json::json!(0));
    insert_missing(llm, "usage_unknown_calls", serde_json::json!(0));

    let tools = object_field(object, "tools");
    insert_missing(tools, "calls", serde_json::json!([]));
    insert_missing(tools, "successful", serde_json::json!([]));
    insert_missing(tools, "rejected", serde_json::json!([]));
    insert_missing(tools, "mode", serde_json::json!(""));

    crate::stdlib::json_to_vm_value(&json)
}

/// Exact run identity owned by the active Harn-driven agent-loop invocation.
pub(crate) fn active_run_id(session_id: &str) -> Option<String> {
    super::AGENT_HOST_SESSIONS.with(|sessions| {
        sessions
            .try_borrow()
            .ok()?
            .get(session_id)
            .map(|session| session.run_id.clone())
    })
}

pub(super) fn agent_init_control_done(
    session_id: &str,
    run_id: &str,
    task: &str,
    system: Option<&str>,
    result: VmValue,
) -> VmValue {
    let result = canonical_init_result(session_id, run_id, task, result);
    agent_init_control(session_id, run_id, task, system, 0, 0, true, Some(result))
}

pub(super) fn agent_init_control(
    session_id: &str,
    run_id: &str,
    task: &str,
    system: Option<&str>,
    max_iterations: i64,
    max_verify_attempts: i64,
    done: bool,
    result: Option<VmValue>,
) -> VmValue {
    let mut control = crate::value::DictMap::new();
    control.put_str("session_id", session_id);
    control.put_str("run_id", run_id);
    control.put_str("task", task);
    control.insert(
        crate::value::intern_key("system"),
        system
            .map(|s| VmValue::String(arcstr::ArcStr::from(s.to_string())))
            .unwrap_or(VmValue::Nil),
    );
    control.insert(
        crate::value::intern_key("max_iterations"),
        VmValue::Int(max_iterations),
    );
    control.insert(
        crate::value::intern_key("max_verify_attempts"),
        VmValue::Int(max_verify_attempts),
    );
    control.insert(crate::value::intern_key("done"), VmValue::Bool(done));
    if let Some(result) = result {
        control.insert(crate::value::intern_key("result"), result);
    }
    VmValue::dict(control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_terminal_projects_the_complete_agent_result_contract() {
        let partial = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": "approval_required",
            "approval_required": true,
            "reviewers": ["oncall"],
            "llm": {"iterations": 0, "duration_ms": 0, "input_tokens": 0, "output_tokens": 0},
            "tools": {"calls": [], "successful": [], "rejected": [], "mode": ""},
        }));

        let projected = canonical_init_result("session-1", "run-1", "ship it", partial);
        let json = super::super::vm_to_json(&projected);

        assert_eq!(json["status"], "approval_required");
        assert_eq!(json["final_status"], "budget_exhausted");
        assert_eq!(json["stop_reason"], "autonomy_budget_denied");
        assert_eq!(json["acp_stop_reason"], "max_turn_requests");
        assert_eq!(json["terminal"]["kind"], "policy_budget");
        assert_eq!(json["terminal"]["owner"], "policy");
        assert_eq!(json["llm"]["token_scope"], "accepted_turn_results");
        assert_eq!(json["llm"]["cache_read_tokens"], 0);
        assert_eq!(json["llm"]["cache_write_tokens"], 0);
        assert_eq!(json["session_id"], "session-1");
        assert_eq!(json["run_id"], "run-1");
        assert_eq!(json["task"], "ship it");
        assert_eq!(json["approval_required"], true);
        assert_eq!(json["reviewers"][0], "oncall");
    }

    #[test]
    fn admission_terminal_preserves_explicit_policy_reason() {
        let partial = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": "blocked",
            "final_status": "blocked",
            "stop_reason": "user_prompt_submit_blocked",
            "error": {"category": "hook_denied"},
        }));

        let projected = canonical_init_result("session-2", "run-2", "blocked", partial);
        let json = super::super::vm_to_json(&projected);

        assert_eq!(json["stop_reason"], "user_prompt_submit_blocked");
        assert_eq!(json["terminal"]["kind"], "policy_guardrail");
        assert_eq!(json["terminal"]["reason"], "user_prompt_submit_blocked");
        assert_eq!(json["error"]["category"], "hook_denied");
    }

    #[test]
    fn admission_terminal_attributes_nested_budget_denials_to_policy() {
        let partial = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": "blocked",
            "final_status": "budget_exhausted",
            "stop_reason": "nested_execution_budget_exhausted",
            "error": {"category": "budget_exceeded"},
        }));

        let projected = canonical_init_result("session-3", "run-3", "nested", partial);
        let json = super::super::vm_to_json(&projected);

        assert_eq!(json["status"], "blocked");
        assert_eq!(json["final_status"], "budget_exhausted");
        assert_eq!(json["acp_stop_reason"], "max_turn_requests");
        assert_eq!(json["terminal"]["kind"], "policy_budget");
        assert_eq!(json["terminal"]["owner"], "policy");
    }
}
