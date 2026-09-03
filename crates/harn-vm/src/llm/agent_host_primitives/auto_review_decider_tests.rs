//! Who the run says decided an `ask` the automated reviewer answered.
//!
//! The behaviour that a granting reviewer lets a refused call through is
//! covered by the conformance loop case. This covers the RECORD, which is a
//! different claim and was the one that was false: every reviewer grant was
//! filed under `runtime_policy`, the layer that could not decide the call, so
//! nothing a run emitted could answer "was a reviewer consulted here". A run
//! with a working reviewer and a run with none were indistinguishable in the
//! permission record, which is the shape that let the resolver look wired while
//! it was not.
//!
//! The pair is the proof. Same policy, same tool, same session shape: with a
//! reviewer the decider is `auto_reviewer`, without one it is
//! `host_unavailable`. Asserting only the first would pass on a build that
//! stamped `auto_reviewer` on everything.

use std::sync::Arc;

use super::host_agent_dispatch_tool_call;
use crate::value::{VmClosure, VmEnv, VmValue};

/// Compile one Harn function into a callable closure.
fn compiled_closure(name: &str, source: &str) -> Arc<VmClosure> {
    let program = harn_parser::check_source_strict(source).expect("reviewer source parses");
    let chunk = crate::compiler::Compiler::new()
        .compile(&program)
        .expect("reviewer source compiles");
    let function = chunk
        .functions
        .iter()
        .find(|function| function.name.as_str() == name)
        .expect("compiled reviewer function")
        .clone();
    Arc::new(VmClosure {
        func: function,
        env: VmEnv::new(),
        source_dir: None,
        module_functions: None,
        module_state: None,
        retained_module_scope: None,
    })
}

/// A reviewer that grants, in the decision-record shape the seam requires.
fn granting_reviewer() -> Arc<VmClosure> {
    compiled_closure(
        "reviewer",
        "fn reviewer(request: dict) { return {approved: true, reviewer_answered: true, rationale: \"the stated task authorizes this install\"} }",
    )
}

fn asking_policy() -> crate::orchestration::ToolApprovalPolicy {
    serde_json::from_value(serde_json::json!({
        "rules": [{
            "ask": {"tool": "exec", "command_identity": "pip"},
            "reason": "package installs require approval"
        }]
    }))
    .expect("approval policy")
}

/// The single tool-permission activity this session recorded.
///
/// Reads the transcript rather than a counter: a count cannot say WHO decided,
/// and the decider is the whole claim. Panics when the run recorded none, so a
/// build that stops emitting the record fails loudly instead of reading as an
/// empty, satisfied search.
fn permission_activity(session_id: &str) -> serde_json::Value {
    let transcript = crate::agent_sessions::transcript(session_id)
        .expect("the test session exists and has a transcript");
    let json = crate::llm::helpers::vm_value_to_json(&transcript);
    let mut found = Vec::new();
    collect_activities(&json, &mut found);
    assert_eq!(
        found.len(),
        1,
        "expected exactly one tool-permission activity, got {}: {json}",
        found.len()
    );
    found.remove(0)
}

fn collect_activities(value: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("schema").and_then(serde_json::Value::as_str)
                == Some("harn.tool_permission_activity.v1")
            {
                out.push(value.clone());
                return;
            }
            for nested in map.values() {
                collect_activities(nested, out);
            }
        }
        serde_json::Value::Array(items) => {
            for nested in items {
                collect_activities(nested, out);
            }
        }
        _ => {}
    }
}

async fn dispatch_pip_install(
    session_id: &str,
    reviewer: Option<Arc<VmClosure>>,
) -> serde_json::Value {
    let mut options = crate::value::DictMap::new();
    options.insert(
        crate::value::intern_key("session_id"),
        crate::stdlib::json_to_vm_value(&serde_json::json!(session_id)),
    );
    if let Some(reviewer) = reviewer {
        options.insert(
            crate::value::intern_key("approval_reviewer"),
            VmValue::Closure(reviewer),
        );
    }
    let call = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "id": "exec-1",
        "name": "exec",
        "arguments": {"command": "pip install pytest"},
    }));
    let result = host_agent_dispatch_tool_call(
        crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
        call,
        None,
        &options,
    )
    .await
    .expect("dispatch returns a value in both arms");
    crate::llm::helpers::vm_value_to_json(&result)
}

#[tokio::test]
async fn an_installed_reviewer_is_recorded_as_the_decider() {
    crate::orchestration::clear_execution_policy_stacks();
    crate::orchestration::clear_approval_reviewers();
    crate::orchestration::clear_all_approval_policy_repeat_counts();
    let session_id = crate::agent_sessions::open_or_create_for_test(Some(
        "auto-review-decider-positive".to_string(),
    ));
    crate::orchestration::push_approval_policy(asking_policy());

    let dispatched = dispatch_pip_install(&session_id, Some(granting_reviewer())).await;

    crate::orchestration::pop_approval_policy();
    crate::orchestration::clear_approval_reviewers();
    crate::orchestration::clear_all_approval_policy_repeat_counts();

    assert_ne!(
        dispatched["result"]["error"],
        serde_json::json!("permission_denied"),
        "a granted call must not still read as a permission denial: {dispatched}"
    );
    let activity = permission_activity(&session_id);
    assert_eq!(
        activity["decider"],
        serde_json::json!("auto_reviewer"),
        "the reviewer answered, so the record must say so: {activity}"
    );
    assert_eq!(activity["outcome"], serde_json::json!("approved"));
    assert_eq!(
        activity["policy_evaluations"][0]["outcome"],
        serde_json::json!("approval_required"),
        "the policy layer still required approval; only the reviewer resolved it: {activity}"
    );
    crate::agent_sessions::close(&session_id);
}

#[tokio::test]
async fn no_reviewer_still_records_the_host_as_unavailable() {
    // The control. Without it the case above passes on a build that stamps
    // `auto_reviewer` on every ask, reviewer or not.
    crate::orchestration::clear_execution_policy_stacks();
    crate::orchestration::clear_approval_reviewers();
    crate::orchestration::clear_all_approval_policy_repeat_counts();
    let session_id = crate::agent_sessions::open_or_create_for_test(Some(
        "auto-review-decider-control".to_string(),
    ));
    crate::orchestration::push_approval_policy(asking_policy());

    let dispatched = dispatch_pip_install(&session_id, None).await;

    crate::orchestration::pop_approval_policy();
    crate::orchestration::clear_all_approval_policy_repeat_counts();

    assert_eq!(
        dispatched["result"]["denial"]["gate"],
        serde_json::json!("approval_unavailable"),
        "with nobody to ask, the call is still refused: {dispatched}"
    );
    let activity = permission_activity(&session_id);
    assert_eq!(
        activity["decider"],
        serde_json::json!("host_unavailable"),
        "no reviewer was consulted, so the record must not claim one: {activity}"
    );
    assert_eq!(activity["outcome"], serde_json::json!("denied"));
    crate::agent_sessions::close(&session_id);
}
