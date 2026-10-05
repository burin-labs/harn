use super::*;
use crate::agent_events::clear_session_sinks;

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

#[test]
fn reparenting_preserves_direct_observers_and_removes_the_former_parent() {
    reset_all_sinks();
    reset_session_store();
    let former = observed_session("linked-former-parent");
    let current = observed_session("linked-current-parent");
    let child = observed_session("linked-reparented-child");
    super::super::link_child_session("linked-former-parent", "linked-reparented-child").unwrap();
    super::super::link_child_session("linked-current-parent", "linked-reparented-child").unwrap();
    assert_eq!(
        super::super::parent_id("linked-reparented-child").as_deref(),
        Some("linked-current-parent")
    );
    emit_child_request("linked-reparented-child");
    assert_eq!(
        former.lock().unwrap().len(),
        0,
        "the former parent must not retain child observation"
    );
    assert_eq!(current.lock().unwrap().len(), 1);
    assert_eq!(
        child.lock().unwrap().len(),
        1,
        "direct child observation must survive reparenting"
    );
}

#[test]
fn descendants_follow_reparenting_and_observer_removal() {
    reset_all_sinks();
    reset_session_store();
    let former = observed_session("linked-ancestor-old");
    let current = observed_session("linked-ancestor-new");
    let child = open_child_session("linked-ancestor-old", Some("linked-middle".into()));
    let descendant = open_child_session(&child, Some("linked-descendant".into()));
    super::super::link_child_session("linked-ancestor-new", &child).unwrap();
    emit_child_request(&descendant);
    assert_eq!(former.lock().unwrap().len(), 0);
    assert_eq!(current.lock().unwrap().len(), 1);
    clear_session_sinks("linked-ancestor-new");
    emit_child_request(&descendant);
    assert_eq!(
        current.lock().unwrap().len(),
        1,
        "cleared observers cannot survive in descendants"
    );
}

#[test]
fn forks_observe_the_explicit_source_not_the_ambient_session() {
    reset_all_sinks();
    reset_session_store();
    let source = observed_session("linked-fork-source");
    let unrelated = observed_session("linked-fork-unrelated");
    let _ambient = enter_current_session("linked-fork-unrelated");
    let child = fork("linked-fork-source", Some("linked-fork-child".into())).unwrap();
    emit_child_request(&child);
    assert_eq!(unrelated.lock().unwrap().len(), 0);
    assert_eq!(source.lock().unwrap().len(), 1);
}

#[test]
fn linked_observation_crosses_worker_threads_and_matches_effective_queries() {
    reset_all_sinks();
    reset_session_store();
    let parent = observed_session("linked-cross-worker-parent");
    let child = open_child_session(
        "linked-cross-worker-parent",
        Some("linked-cross-worker-child".into()),
    );
    crate::runtime_stack::spawn(move || {
        assert_eq!(session_external_sink_count(&child), 1);
        emit_child_request(&child);
    })
    .join()
    .unwrap();
    assert_eq!(parent.lock().unwrap().len(), 1);
}

#[test]
fn a_closed_and_reused_id_cannot_retain_its_former_observer_route() {
    reset_all_sinks();
    reset_session_store();
    let parent = observed_session("linked-reuse-parent");
    let child = open_child_session("linked-reuse-parent", Some("linked-reused-child".into()));
    assert!(super::super::close(&child));
    open_or_create(Some(child.clone()));
    emit_child_request(&child);
    assert_eq!(parent.lock().unwrap().len(), 0);
}

#[test]
fn replacing_a_request_observer_preserves_existing_session_lineage() {
    reset_all_sinks();
    reset_session_store();
    let first = observed_session("linked-request-parent");
    let child = open_child_session("linked-request-parent", Some("linked-request-child".into()));
    let descendant = open_child_session(&child, Some("linked-request-descendant".into()));
    clear_session_sinks("linked-request-parent");
    let replacement = observed_session("linked-request-parent");
    emit_child_request(&descendant);
    assert_eq!(first.lock().unwrap().len(), 0);
    assert_eq!(
        replacement.lock().unwrap().len(),
        1,
        "a new request observer must follow the unchanged session lineage"
    );
}

#[test]
fn worker_placeholders_and_cleanup_preserve_other_workers_parent_lineage() {
    reset_all_sinks();
    reset_session_store();
    let root = observed_session("linked-worker-root");
    open_child_session("linked-worker-root", Some("linked-worker-parent".into()));
    let (ready_a_tx, ready_a_rx) = std::sync::mpsc::channel();
    let (reset_a_tx, reset_a_rx) = std::sync::mpsc::channel();
    let worker_a = crate::runtime_stack::spawn(move || {
        open_child_session("linked-worker-parent", Some("linked-worker-a".into()));
        ready_a_tx.send(()).unwrap();
        reset_a_rx.recv().unwrap();
        reset_session_store();
    });
    ready_a_rx.recv().unwrap();
    let (ready_b_tx, ready_b_rx) = std::sync::mpsc::channel();
    let (resume_b_tx, resume_b_rx) = std::sync::mpsc::channel();
    let worker_b = crate::runtime_stack::spawn(move || {
        let child = open_child_session("linked-worker-parent", Some("linked-worker-b".into()));
        emit_child_request(&child);
        ready_b_tx.send(()).unwrap();
        resume_b_rx.recv().unwrap();
        emit_child_request(&child);
        reset_session_store();
    });
    ready_b_rx.recv().unwrap();
    assert_eq!(
        root.lock().unwrap().len(),
        1,
        "creating worker placeholders must preserve the parent's root observer"
    );
    reset_a_tx.send(()).unwrap();
    worker_a.join().unwrap();
    resume_b_tx.send(()).unwrap();
    worker_b.join().unwrap();
    assert_eq!(
        root.lock().unwrap().len(),
        2,
        "one worker's cleanup must not disconnect another worker"
    );
}

fn assert_observation_survives_unowned_close(parent_reference: bool) {
    reset_all_sinks();
    reset_session_store();
    let parent = observed_session("linked-close-owner-parent");
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let worker_b = crate::runtime_stack::spawn(move || {
        let child = open_child_session(
            "linked-close-owner-parent",
            Some("linked-close-owner-child".into()),
        );
        ready_tx.send(()).unwrap();
        resume_rx.recv().unwrap();
        emit_child_request(&child);
        reset_session_store();
    });
    ready_rx.recv().unwrap();
    let worker_a = crate::runtime_stack::spawn(move || {
        open_child_session(
            "linked-close-owner-parent",
            Some("linked-close-other-child".into()),
        );
        let id = if parent_reference {
            "linked-close-owner-parent"
        } else {
            "linked-close-owner-child"
        };
        let status_closed =
            super::super::close_with_status(id, "fixture", "completed", serde_json::json!({}))
                .unwrap();
        let closed = super::super::close(id);
        reset_session_store();
        (closed, status_closed)
    });
    let (closed, status_closed) = worker_a.join().unwrap();
    resume_tx.send(()).unwrap();
    worker_b.join().unwrap();
    assert!(
        !closed,
        "an absent session or a parent reference must not claim to close an admitted identity"
    );
    assert!(
        !status_closed,
        "status closure must preserve the same admission boundary"
    );
    assert_eq!(
        parent.lock().unwrap().len(),
        1,
        "another worker's false or placeholder close must preserve delivery"
    );
}

#[test]
fn unowned_close_of_an_absent_child_preserves_the_owning_workers_observer() {
    assert_observation_survives_unowned_close(false);
}

#[test]
fn unowned_close_of_a_parent_reference_preserves_the_admitted_parent() {
    assert_observation_survives_unowned_close(true);
}

#[test]
fn a_parent_reference_becomes_closable_only_after_successful_explicit_admission() {
    reset_all_sinks();
    reset_session_store();
    open_child_session(
        "linked-reference-parent",
        Some("linked-reference-child".into()),
    );
    assert!(!super::super::close("linked-reference-parent"));
    set_session_cap(1);
    let refused =
        super::super::link_child_session("linked-reference-grandparent", "linked-reference-parent");
    set_session_cap(DEFAULT_SESSION_CAP);
    assert!(
        matches!(refused, Err(SessionOpenError::CapacityExhausted { .. })),
        "failed atomic admission cannot promote the reference"
    );
    assert!(!super::super::close("linked-reference-parent"));
    super::super::link_child_session("linked-reference-grandparent", "linked-reference-parent")
        .unwrap();
    assert!(
        super::super::close("linked-reference-parent"),
        "successful child admission promotes the reference"
    );
    open_child_session("linked-open-reference", Some("linked-open-child".into()));
    open_or_create(Some("linked-open-reference".into()));
    assert!(
        super::super::close("linked-open-reference"),
        "public explicit open is admission, unlike an internal lineage placeholder"
    );
}
