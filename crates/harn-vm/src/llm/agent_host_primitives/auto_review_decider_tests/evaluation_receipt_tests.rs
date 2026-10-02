use std::sync::{Arc, Mutex};

use super::{
    asking_policy, compiled_closure, dispatch_pip_install, permission_activity,
    rejecting_bridge_observing_requests,
};

#[tokio::test(flavor = "current_thread")]
async fn uncertain_review_reaches_the_human_prompt_with_candidates_and_provenance() {
    crate::orchestration::clear_execution_policy_stacks();
    crate::orchestration::clear_approval_reviewers();
    crate::orchestration::clear_all_approval_policy_repeat_counts();
    let session_id = crate::agent_sessions::open_or_create_for_test(Some(
        "decision-review-human-fallback".into(),
    ));
    crate::orchestration::push_approval_policy(asking_policy());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::new(Mutex::new(0));
    let previous = crate::llm::agent_runtime::swap_current_host_bridge(Some(
        rejecting_bridge_observing_requests(
            seen.clone(),
            Some("person declined".into()),
            Some(requests.clone()),
        ),
    ));
    let reviewer = compiled_closure("reviewer", include_str!("uncertain_reviewer.harn"));
    let dispatched = dispatch_pip_install(&session_id, Some(reviewer)).await;
    crate::llm::agent_runtime::swap_current_host_bridge(previous);
    crate::orchestration::pop_approval_policy();
    crate::orchestration::clear_approval_reviewers();
    crate::orchestration::clear_all_approval_policy_repeat_counts();

    assert_eq!(
        *seen.lock().unwrap(),
        1,
        "uncertainty must reach the actual host permission path"
    );
    let requests = requests.lock().unwrap();
    let review =
        &requests[0]["params"]["toolCall"]["_meta"]["harn"]["policyDecision"]["auto_review"];
    assert_eq!(
        review["reviewer_answered"], false,
        "a candidate is not a considered rejection"
    );
    assert_eq!(review["evaluation_review"]["disposition"], "needs_review");
    assert_eq!(
        review["evaluation_review"]["presentation"]["summary"],
        "The safety review needs your judgment."
    );
    assert_eq!(
        review["evaluation_review"]["outcome"]["kind"],
        "low_confidence"
    );
    assert_eq!(
        review["evaluation_review"]["outcome"]["candidates"]["safe"]["probability"],
        0.6
    );
    assert_eq!(
        review["evaluation_review"]["outcome"]["candidates"]["safe"]["confidence_kind"],
        "binary_probability"
    );
    assert_eq!(dispatched["result"]["denial"]["gate"], "host_rejected");
    assert_ne!(
        permission_activity(&session_id)["decider"],
        "auto_reviewer",
        "the host made the terminal decision"
    );
    crate::agent_sessions::close(&session_id);
}
