use super::*;

fn observed_session(id: &str) -> Arc<Mutex<Vec<AgentEvent>>> {
    open_or_create(Some(id.into()));
    let captured = Arc::new(Mutex::new(Vec::new()));
    register_sink(id, Arc::new(CapturingSink(captured.clone())));
    assert_eq!(session_external_sink_count(id), 1);
    captured
}

fn emit_child_request(child: &str) {
    emit_event(&AgentEvent::IterationStart {
        session_id: child.into(),
        iteration: 0,
        provider: "fixture".into(),
        model: "fixture-model".into(),
    });
}

#[test]
fn linked_child_observation_reaches_explicit_parent_without_ambient_session() {
    reset_all_sinks();
    reset_session_store();
    let parent = observed_session("linked-observation-parent");
    assert_eq!(
        current_session_id(),
        None,
        "background workers own fresh session scope"
    );
    let child = open_child_session(
        "linked-observation-parent",
        Some("linked-observation-child".into()),
    );
    emit_child_request(&child);
    let events = parent.lock().unwrap();
    assert_eq!(
        events.len(),
        1,
        "the real child event must reach its declared parent transport"
    );
    assert_eq!(events[0].session_id(), child);
}

#[test]
fn linked_child_observation_does_not_follow_an_unrelated_ambient_parent() {
    reset_all_sinks();
    reset_session_store();
    let parent = observed_session("linked-explicit-parent");
    let unrelated = observed_session("linked-unrelated-parent");
    let _ambient = enter_current_session("linked-unrelated-parent");
    let child = open_child_session(
        "linked-explicit-parent",
        Some("linked-explicit-child".into()),
    );
    emit_child_request(&child);
    assert_eq!(
        unrelated.lock().unwrap().len(),
        0,
        "an unrelated transport must receive no child events"
    );
    assert_eq!(
        parent.lock().unwrap().len(),
        1,
        "the declared parent must still receive the event"
    );
}

#[test]
fn linking_an_existing_child_preserves_its_observer_and_deduplicates_the_parent() {
    reset_all_sinks();
    reset_session_store();
    let parent = observed_session("linked-existing-parent");
    let child = observed_session("linked-existing-child");
    super::super::link_child_session("linked-existing-parent", "linked-existing-child").unwrap();
    super::super::link_child_session("linked-existing-parent", "linked-existing-child").unwrap();
    emit_child_request("linked-existing-child");
    assert_eq!(
        child.lock().unwrap().len(),
        1,
        "the existing child observer is retained"
    );
    assert_eq!(
        parent.lock().unwrap().len(),
        1,
        "repeated linking cannot duplicate delivery"
    );
}
