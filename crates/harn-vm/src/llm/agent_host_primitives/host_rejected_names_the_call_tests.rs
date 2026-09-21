//! What a host refusal leaves behind for whoever has to act on it.
//!
//! A host that refuses a tool call without saying why produces the runtime's
//! own fallback sentence. That sentence is true and useless: it names the gate
//! and nothing else. A reader downstream gets "a permission gate refused one
//! command" with no command, and because the refusal also ends the run there
//! is nowhere left to recover it from.
//!
//! The call is not missing. It is in hand at the moment the record is built,
//! and it is already in the same event beside the denial. These cases pin it
//! into the denial record itself, where a consumer reads.

use std::sync::Arc;

use super::auto_review_decider_tests::{
    asking_policy, dispatch_pip_install, rejecting_bridge_with_reason,
};

/// The command the harness dispatches, so the assertions name what they expect
/// rather than matching a fragment that a different call could also satisfy.
const DISPATCHED_COMMAND: &str = "pip install pytest";

async fn refuse_with(reason: Option<String>, label: &str) -> serde_json::Value {
    crate::orchestration::clear_execution_policy_stacks();
    crate::orchestration::clear_approval_reviewers();
    crate::orchestration::clear_all_approval_policy_repeat_counts();
    let session_id =
        crate::agent_sessions::open_or_create_for_test(Some(format!("host-rejected-{label}")));
    crate::orchestration::push_approval_policy(asking_policy());
    let seen = Arc::new(std::sync::Mutex::new(0usize));
    let previous = crate::llm::agent_runtime::swap_current_host_bridge(Some(
        rejecting_bridge_with_reason(seen.clone(), reason),
    ));

    let dispatched = dispatch_pip_install(&session_id, None).await;

    crate::llm::agent_runtime::swap_current_host_bridge(previous);
    crate::orchestration::pop_approval_policy();
    crate::orchestration::clear_approval_reviewers();
    crate::orchestration::clear_all_approval_policy_repeat_counts();

    // The negative control. If the host were never asked, an absent fact in
    // the record would prove nothing about this path.
    assert_eq!(
        *seen.lock().expect("seen count"),
        1,
        "this case only means anything if the ask reached the host: {dispatched}"
    );
    assert_eq!(
        dispatched["result"]["denial"]["gate"],
        serde_json::json!("host_rejected"),
        "the host refused, so this is the gate under test: {dispatched}"
    );
    dispatched
}

/// The reported case: the host says no and gives no reason.
#[tokio::test(flavor = "current_thread")]
async fn a_reasonless_host_refusal_still_names_the_command_it_refused() {
    let dispatched = refuse_with(None, "silent").await;
    let denial = &dispatched["result"]["denial"];
    let rendered = serde_json::to_string(denial).expect("denial serializes");
    assert!(
        rendered.contains(DISPATCHED_COMMAND),
        "a refusal a reader cannot act on is the defect: the denial must name the \
         command it refused, got {rendered}"
    );
}

/// The host DID say why. Its reason is the model-facing text and must survive
/// verbatim; naming the call is additive, not a replacement.
#[tokio::test(flavor = "current_thread")]
async fn a_host_supplied_reason_survives_beside_the_named_command() {
    let dispatched = refuse_with(Some("policy forbids installs".to_string()), "stated").await;
    let denial = &dispatched["result"]["denial"];
    let reason = denial["reason"].as_str().expect("denial reason");
    assert!(
        reason.contains("policy forbids installs"),
        "the host's own words are the model-facing text and must not be replaced: {reason}"
    );
    let rendered = serde_json::to_string(denial).expect("denial serializes");
    assert!(
        rendered.contains(DISPATCHED_COMMAND),
        "and the command is named whether or not the host explained itself: {rendered}"
    );
}
