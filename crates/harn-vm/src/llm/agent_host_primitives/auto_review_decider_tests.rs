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

/// A host bridge that refuses every permission request and counts what it saw.
///
/// The count is the load-bearing half. A reviewer that answers first means the
/// host is never asked at all, and "the call was allowed" alone cannot tell
/// that apart from a host that happened to say yes.
fn rejecting_bridge(seen: Arc<std::sync::Mutex<usize>>) -> Arc<crate::bridge::HostBridge> {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::Mutex as TokioMutex;

    let pending: Arc<TokioMutex<HashMap<u64, tokio::sync::oneshot::Sender<serde_json::Value>>>> =
        Arc::new(TokioMutex::new(HashMap::new()));
    let response_pending = pending.clone();
    let writer = Arc::new(move |line: &str| {
        let request: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("invalid bridge request: {error}"))?;
        let id = request["id"]
            .as_u64()
            .ok_or_else(|| "bridge request missing numeric id".to_string())?;
        // Count ONLY the approval question. This bridge also serves the tool
        // dispatch itself, and counting that made an earlier draft of this test
        // fail for the wrong reason.
        let is_permission = request["method"].as_str()
            == Some(crate::llm::acp_permission::METHOD_REQUEST_PERMISSION);
        let result = if is_permission {
            *seen.lock().map_err(|_| "seen mutex poisoned".to_string())? += 1;
            crate::llm::acp_permission::reject_response(Some("no human here".to_string()))
        } else {
            serde_json::json!({"ok": true})
        };
        let sender = response_pending
            .try_lock()
            .map_err(|_| "bridge pending map unexpectedly locked".to_string())?
            .remove(&id)
            .ok_or_else(|| "bridge request was not pending".to_string())?;
        sender
            .send(serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}))
            .map_err(|_| "bridge caller dropped before response".to_string())
    });
    Arc::new(crate::bridge::HostBridge::from_parts_with_writer(
        pending,
        Arc::new(AtomicBool::new(false)),
        writer,
        1,
    ))
}

#[tokio::test(flavor = "current_thread")]
async fn an_installed_reviewer_answers_before_a_refusing_host_is_asked() {
    // The embedded shape, which neither sibling covers: a host bridge IS
    // present and refuses. Every product loop runs this way, and a bridge that
    // answers "no" is a decision, so a seam that defers to it never reconsiders
    // anything. The two cases above both run bridge-less, so a reviewer that is
    // skipped whenever a host can be asked passes them both.
    crate::orchestration::clear_execution_policy_stacks();
    crate::orchestration::clear_approval_reviewers();
    crate::orchestration::clear_all_approval_policy_repeat_counts();
    let session_id = crate::agent_sessions::open_or_create_for_test(Some(
        "auto-review-decider-refusing-host".to_string(),
    ));
    crate::orchestration::push_approval_policy(asking_policy());
    let seen = Arc::new(std::sync::Mutex::new(0usize));
    let previous =
        crate::llm::agent_runtime::swap_current_host_bridge(Some(rejecting_bridge(seen.clone())));

    let dispatched = dispatch_pip_install(&session_id, Some(granting_reviewer())).await;

    crate::llm::agent_runtime::swap_current_host_bridge(previous);
    crate::orchestration::pop_approval_policy();
    crate::orchestration::clear_approval_reviewers();
    crate::orchestration::clear_all_approval_policy_repeat_counts();

    assert_ne!(
        dispatched["result"]["denial"]["gate"],
        serde_json::json!("host_rejected"),
        "the reviewer granted this call, so a refusing host must never decide it: {dispatched}"
    );
    let activity = permission_activity(&session_id);
    assert_eq!(
        *seen.lock().expect("seen count"),
        0,
        "the reviewer answered, so the host must not have been asked at all; dispatched={dispatched} activity={activity}"
    );
    assert_eq!(
        activity["decider"],
        serde_json::json!("auto_reviewer"),
        "the reviewer answered, so the record must say so: {activity}"
    );
    crate::agent_sessions::close(&session_id);
}

/// Every `auto_review` annotation the run recorded anywhere in its transcript.
///
/// Searches the whole document rather than one known path: the claim is that
/// the run says why no reviewer answered, not that a particular field exists.
fn collect_auto_review(value: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(found) = map.get("auto_review") {
                out.push(found.clone());
            }
            for nested in map.values() {
                collect_auto_review(nested, out);
            }
        }
        serde_json::Value::Array(items) => {
            for nested in items {
                collect_auto_review(nested, out);
            }
        }
        _ => {}
    }
}

fn auto_review_annotations(session_id: &str) -> Vec<serde_json::Value> {
    let transcript = crate::agent_sessions::transcript(session_id)
        .expect("the test session exists and has a transcript");
    let json = crate::llm::helpers::vm_value_to_json(&transcript);
    let mut found = Vec::new();
    collect_auto_review(&json, &mut found);
    found
}

#[tokio::test(flavor = "current_thread")]
async fn a_refusal_no_reviewer_answered_records_why_not() {
    // The reported shape, and the one the record could not describe. An `ask`
    // reaches a host that refuses, and the run files a bare `host_rejected`:
    // true about the host, silent about the reviewer that was supposed to
    // answer first. A run whose reviewer was never installed and a run that
    // never wanted one produced the same bytes, so "was a reviewer consulted
    // here" had no answer and a resolver could look wired while it was not.
    //
    // The falsifier: under a refusal the seam declined, the run must record a
    // verdict or `reviewer_answered: false` with a reason. Never nothing.
    crate::orchestration::clear_execution_policy_stacks();
    crate::orchestration::clear_approval_reviewers();
    crate::orchestration::clear_all_approval_policy_repeat_counts();
    let session_id = crate::agent_sessions::open_or_create_for_test(Some(
        "auto-review-silent-decline".to_string(),
    ));
    crate::orchestration::push_approval_policy(asking_policy());
    let seen = Arc::new(std::sync::Mutex::new(0usize));
    let previous =
        crate::llm::agent_runtime::swap_current_host_bridge(Some(rejecting_bridge(seen.clone())));

    let dispatched = dispatch_pip_install(&session_id, None).await;

    crate::llm::agent_runtime::swap_current_host_bridge(previous);
    crate::orchestration::pop_approval_policy();
    crate::orchestration::clear_approval_reviewers();
    crate::orchestration::clear_all_approval_policy_repeat_counts();

    // The negative control for the assertion below: if the host were never
    // asked, a missing annotation would prove nothing about this path.
    assert_eq!(
        *seen.lock().expect("seen count"),
        1,
        "this case only means anything if the ask actually reached the host: {dispatched}"
    );
    assert_eq!(
        dispatched["result"]["denial"]["gate"],
        serde_json::json!("host_rejected"),
        "the host refused, so the gate is still its refusal: {dispatched}"
    );

    let annotations = auto_review_annotations(&session_id);
    assert_eq!(
        annotations.len(),
        1,
        "the refusal must carry exactly one auto-review annotation saying why no \
         reviewer answered, so a bare host_rejected is never the whole record; got {annotations:?}"
    );
    let annotation = &annotations[0];
    assert_eq!(
        annotation["reviewer_answered"],
        serde_json::json!(false),
        "no reviewer answered this refusal: {annotation}"
    );
    assert_eq!(
        annotation["unavailable_reason"],
        serde_json::json!("no_reviewer_installed"),
        "the record must name which decline path fired: {annotation}"
    );
    crate::agent_sessions::close(&session_id);
}

/// A reviewer that answers and refuses, in the decision-record shape the seam
/// requires.
fn refusing_reviewer() -> Arc<VmClosure> {
    compiled_closure(
        "reviewer",
        "fn reviewer(request: dict) { return {approved: false, reviewer_answered: true, rationale: \"this install is unrelated to the stated task\"} }",
    )
}

#[tokio::test(flavor = "current_thread")]
async fn a_reviewer_that_refuses_is_the_decider_not_a_person() {
    // The record used to contradict itself. A reviewer answered and said no,
    // the refusal then reached the host as a formality, and the host returned
    // no decision metadata -- which defaults to `person`. So a run with no
    // person present filed the refusal under a human who was never asked,
    // while the same event carried the reviewer's own verdict and rationale.
    //
    // The falsifier: when a reviewer answered, the decider is the reviewer.
    crate::orchestration::clear_execution_policy_stacks();
    crate::orchestration::clear_approval_reviewers();
    crate::orchestration::clear_all_approval_policy_repeat_counts();
    let session_id = crate::agent_sessions::open_or_create_for_test(Some(
        "auto-review-refusal-attribution".to_string(),
    ));
    crate::orchestration::push_approval_policy(asking_policy());
    let seen = Arc::new(std::sync::Mutex::new(0usize));
    let previous =
        crate::llm::agent_runtime::swap_current_host_bridge(Some(rejecting_bridge(seen.clone())));

    let dispatched = dispatch_pip_install(&session_id, Some(refusing_reviewer())).await;

    crate::llm::agent_runtime::swap_current_host_bridge(previous);
    crate::orchestration::pop_approval_policy();
    crate::orchestration::clear_approval_reviewers();
    crate::orchestration::clear_all_approval_policy_repeat_counts();

    // Negative control: the attribution claim only means something if the host
    // was asked and returned the metadata-less rejection that defaults to
    // `person`. Without this the assertion could pass on a build that never
    // reached the host at all.
    assert_eq!(
        *seen.lock().expect("seen count"),
        1,
        "the refusal must still have reached the host: {dispatched}"
    );
    let activity = permission_activity(&session_id);
    assert_eq!(
        activity["decider"],
        serde_json::json!("auto_reviewer"),
        "a reviewer answered this refusal, so no person may be credited with it: {activity}"
    );
    assert_eq!(activity["outcome"], serde_json::json!("denied"));

    let annotations = auto_review_annotations(&session_id);
    assert_eq!(
        annotations.len(),
        1,
        "expected one annotation: {annotations:?}"
    );
    assert_eq!(
        annotations[0]["reviewer_answered"],
        serde_json::json!(true),
        "the reviewer answered, and the record must say so: {}",
        annotations[0]
    );
    crate::agent_sessions::close(&session_id);
}
