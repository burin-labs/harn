use super::*;
use crate::agent_events::{
    emit_event, register_sink, reset_all_sinks, session_external_sink_count, AgentEvent,
    AgentEventSink,
};
use crate::event_log::{active_event_log, EventLog, Topic};
use futures::StreamExt as _;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

fn make_msg(role: &str, content: &str) -> VmValue {
    let mut m: BTreeMap<String, VmValue> = BTreeMap::new();
    m.insert("role".to_string(), VmValue::String(Rc::from(role)));
    m.insert("content".to_string(), VmValue::String(Rc::from(content)));
    VmValue::Dict(Rc::new(m))
}

fn message_count(id: &str) -> usize {
    SESSIONS.with(|s| {
        let map = s.borrow();
        let Some(state) = map.get(id) else { return 0 };
        let Some(dict) = state.transcript.as_dict() else {
            return 0;
        };
        match dict.get("messages") {
            Some(VmValue::List(list)) => list.len(),
            _ => 0,
        }
    })
}

fn event_count_by_kind(id: &str, expected_kind: &str) -> usize {
    snapshot(id)
        .and_then(|snapshot| snapshot.as_dict().cloned())
        .and_then(|dict| dict.get("events").cloned())
        .and_then(|events| match events {
            VmValue::List(events) => Some(
                events
                    .iter()
                    .filter(|event| {
                        event
                            .as_dict()
                            .and_then(|dict| dict.get("kind"))
                            .map(VmValue::display)
                            .as_deref()
                            == Some(expected_kind)
                    })
                    .count(),
            ),
            _ => None,
        })
        .unwrap_or(0)
}

struct CapturingSink(Arc<Mutex<Vec<AgentEvent>>>);

impl AgentEventSink for CapturingSink {
    fn handle_event(&self, event: &AgentEvent) {
        self.0
            .lock()
            .expect("capture sink poisoned")
            .push(event.clone());
    }
}

#[test]
fn records_system_prompt_as_metadata_event_without_message() {
    reset_session_store();
    let id = open_or_create(Some("system-prompt-session".into()));
    record_system_prompt(&id, "Follow the workflow.").unwrap();
    record_system_prompt(&id, "Follow the workflow.").unwrap();
    inject_message(&id, make_msg("user", "hello")).unwrap();

    let snapshot = snapshot(&id).expect("session snapshot");
    let snapshot_dict = snapshot.as_dict().expect("session snapshot dict");
    let metadata = snapshot_dict
        .get("metadata")
        .and_then(VmValue::as_dict)
        .expect("metadata");
    let system_prompt = metadata
        .get("system_prompt")
        .and_then(VmValue::as_dict)
        .expect("system prompt metadata");
    assert_eq!(
        system_prompt
            .get("content")
            .map(VmValue::display)
            .as_deref(),
        Some("Follow the workflow.")
    );
    assert!(
        matches!(snapshot_dict.get("system_prompt"), Some(VmValue::String(value)) if value.as_ref() == "Follow the workflow.")
    );
    assert!(matches!(snapshot_dict.get("length"), Some(VmValue::Int(1))));

    let transcript = transcript(&id).expect("canonical transcript");
    let transcript_dict = transcript.as_dict().expect("canonical transcript dict");
    assert!(!transcript_dict.contains_key("system_prompt"));
    assert!(transcript_dict
        .get("metadata")
        .and_then(VmValue::as_dict)
        .and_then(|metadata| metadata.get("system_prompt"))
        .is_some());
    assert_eq!(message_count(&id), 1);
    assert_eq!(event_count_by_kind(&id, "system_prompt"), 1);
}

#[test]
fn pinned_model_round_trips_through_session_state_and_snapshot() {
    reset_session_store();
    let id = open_or_create(Some("pinned-model-session".into()));

    // Default: no pin.
    assert!(pinned_model(&id).is_none());
    let initial_snapshot = snapshot(&id).expect("session snapshot");
    assert!(matches!(
        initial_snapshot
            .as_dict()
            .and_then(|d| d.get("pinned_model")),
        Some(VmValue::Nil)
    ));

    // First set returns changed=true; snapshot reflects the pin.
    assert!(set_pinned_model(&id, Some("custom-model".into())).unwrap());
    assert_eq!(pinned_model(&id).as_deref(), Some("custom-model"));
    let pinned_snapshot = snapshot(&id).expect("session snapshot");
    let pinned_value = pinned_snapshot
        .as_dict()
        .and_then(|d| d.get("pinned_model"))
        .map(|v| v.display())
        .unwrap_or_default();
    assert_eq!(pinned_value, "custom-model");

    // Re-setting to the same selector returns changed=false; no churn.
    assert!(!set_pinned_model(&id, Some("custom-model".into())).unwrap());

    // Whitespace-only input is normalized to None (clears the pin).
    assert!(set_pinned_model(&id, Some("   ".into())).unwrap());
    assert!(pinned_model(&id).is_none());

    // Setting on an unknown session surfaces a descriptive error.
    let error = set_pinned_model("ghost-session", Some("x".into())).unwrap_err();
    assert!(
        error.contains("ghost-session"),
        "unknown-session error must name the session: {error}"
    );
}

#[test]
fn fork_inherits_parent_pinned_model_so_branch_starts_on_same_route() {
    reset_session_store();
    let parent_id = open_or_create(Some("fork-pin-parent".into()));
    set_pinned_model(&parent_id, Some("claude-sonnet-4-6".into())).unwrap();
    let child_id = fork(&parent_id, Some("fork-pin-child".into())).expect("fork");

    assert_eq!(
        pinned_model(&child_id).as_deref(),
        Some("claude-sonnet-4-6"),
        "fork should mirror tool_format/system_prompt by carrying the parent's model pin",
    );

    // Independent state: re-pinning on the child must not affect
    // the parent.
    set_pinned_model(&child_id, Some("gpt-4o-mini".into())).unwrap();
    assert_eq!(
        pinned_model(&parent_id).as_deref(),
        Some("claude-sonnet-4-6")
    );
    assert_eq!(pinned_model(&child_id).as_deref(), Some("gpt-4o-mini"));
}

#[test]
fn pinned_reasoning_policy_round_trips_through_session_state_and_snapshot() {
    reset_session_store();
    let id = open_or_create(Some("pinned-reasoning-session".into()));

    assert!(pinned_reasoning_policy(&id).is_none());
    let initial_snapshot = snapshot(&id).expect("session snapshot");
    assert!(matches!(
        initial_snapshot
            .as_dict()
            .and_then(|d| d.get("pinned_reasoning_policy")),
        Some(VmValue::Nil)
    ));

    assert!(set_pinned_reasoning_policy(&id, Some("HIGH".into())).unwrap());
    assert_eq!(pinned_reasoning_policy(&id).as_deref(), Some("high"));
    let pinned_snapshot = snapshot(&id).expect("session snapshot");
    let pinned_value = pinned_snapshot
        .as_dict()
        .and_then(|d| d.get("pinned_reasoning_policy"))
        .map(|v| v.display())
        .unwrap_or_default();
    assert_eq!(pinned_value, "high");

    assert!(!set_pinned_reasoning_policy(&id, Some("high".into())).unwrap());
    assert!(set_pinned_reasoning_policy(&id, Some("@inherit".into())).unwrap());
    assert!(pinned_reasoning_policy(&id).is_none());

    let error = set_pinned_reasoning_policy(&id, Some("slow".into())).unwrap_err();
    assert!(
        error.contains("expected auto, off, minimal, low, medium, high, or xhigh"),
        "invalid policy should explain accepted values: {error}",
    );
}

#[test]
fn fork_inherits_parent_pinned_reasoning_policy_but_child_changes_stay_local() {
    reset_session_store();
    let parent_id = open_or_create(Some("fork-reasoning-parent".into()));
    set_pinned_reasoning_policy(&parent_id, Some("high".into())).unwrap();
    let child_id = fork(&parent_id, Some("fork-reasoning-child".into())).expect("fork");

    assert_eq!(pinned_reasoning_policy(&child_id).as_deref(), Some("high"));

    set_pinned_reasoning_policy(&child_id, Some("off".into())).unwrap();
    assert_eq!(pinned_reasoning_policy(&parent_id).as_deref(), Some("high"));
    assert_eq!(pinned_reasoning_policy(&child_id).as_deref(), Some("off"));
}

#[test]
fn close_with_status_emits_terminal_event_and_clears_sinks() {
    reset_all_sinks();
    let id = open_or_create(Some("close-reason-session".into()));
    inject_message(&id, make_msg("user", "hello")).unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    register_sink(&id, Arc::new(CapturingSink(captured.clone())));
    assert_eq!(session_external_sink_count(&id), 1);

    assert!(close_with_status(
        &id,
        "timeout",
        "timeout",
        serde_json::json!({"idle_ms": 5000}),
    ));

    assert!(!exists(&id));
    assert_eq!(session_external_sink_count(&id), 0);
    let events = captured.lock().expect("capture sink poisoned");
    assert_eq!(events.len(), 1);
    match &events[0] {
        AgentEvent::SessionClosed {
            session_id,
            reason,
            status,
            metadata,
        } => {
            assert_eq!(session_id, "close-reason-session");
            assert_eq!(reason, "timeout");
            assert_eq!(status, "timeout");
            assert_eq!(metadata["idle_ms"], 5000);
        }
        other => panic!("expected SessionClosed, got {other:?}"),
    }
    reset_all_sinks();
}

#[test]
fn inject_identified_user_message_emits_replayable_user_event() {
    reset_all_sinks();
    reset_session_store();
    let id = open_or_create(Some("identified-user-message-session".into()));
    let captured = Arc::new(Mutex::new(Vec::new()));
    register_sink(&id, Arc::new(CapturingSink(captured.clone())));

    let mut message = BTreeMap::new();
    message.insert("role".to_string(), VmValue::String(Rc::from("user")));
    message.insert(
        "content".to_string(),
        VmValue::String(Rc::from("queued follow-up")),
    );
    message.insert(
        "messageId".to_string(),
        VmValue::String(Rc::from("msg_inj_test")),
    );
    inject_message(&id, VmValue::Dict(Rc::new(message))).unwrap();

    let events = captured.lock().expect("capture sink poisoned");
    assert_eq!(events.len(), 1);
    match &events[0] {
        AgentEvent::UserMessage {
            session_id,
            message_id,
            content,
        } => {
            assert_eq!(session_id, &id);
            assert_eq!(message_id, "msg_inj_test");
            assert_eq!(
                content,
                &vec![serde_json::json!({
                    "type": "text",
                    "text": "queued follow-up",
                })]
            );
        }
        event => panic!("expected UserMessage event, got {event:?}"),
    }
    reset_all_sinks();
}

#[test]
fn close_drops_pending_inbox_entries_for_reused_session_ids() {
    // Regression: before close() cleared the inbox, a pending
    // notification could survive past close() and get delivered to a
    // future session that happened to reuse the same id.
    reset_all_sinks();
    reset_session_store();
    crate::orchestration::agent_inbox::reset();
    let id = open_or_create(Some("reused-id".into()));
    crate::orchestration::agent_inbox::push(&id, "test", "stale", "regression");
    assert_eq!(crate::orchestration::agent_inbox::pending_count(&id), 1);
    close(&id);
    assert_eq!(
        crate::orchestration::agent_inbox::pending_count(&id),
        0,
        "close() must drain per-session inbox state"
    );
}

#[test]
fn fork_at_truncates_destination_to_keep_first() {
    reset_session_store();
    let src = open_or_create(Some("src-fork-at".into()));
    inject_message(&src, make_msg("user", "a")).unwrap();
    inject_message(&src, make_msg("assistant", "b")).unwrap();
    inject_message(&src, make_msg("user", "c")).unwrap();
    inject_message(&src, make_msg("assistant", "d")).unwrap();
    assert_eq!(message_count(&src), 4);

    let dst = fork_at(&src, 2, Some("dst-fork-at".into())).expect("fork_at");
    assert_ne!(dst, src);
    assert_eq!(message_count(&dst), 2, "branched at message index 2");
    assert_eq!(
        snapshot(&dst)
            .and_then(|value| value.as_dict().cloned())
            .and_then(|dict| dict
                .get("branched_at_event_index")
                .and_then(VmValue::as_int)),
        Some(2)
    );
    // Source untouched.
    assert_eq!(message_count(&src), 4);
    // Subscribers not carried — forks start with a clean fanout list.
    assert_eq!(subscriber_count(&dst), 0);
    reset_session_store();
}

#[test]
fn truncate_retains_prefix_and_reports_removed_turns() {
    reset_session_store();
    let id = open_or_create(Some("truncate-prefix".into()));
    inject_message(&id, make_msg("user", "a")).unwrap();
    inject_message(&id, make_msg("assistant", "b")).unwrap();
    inject_message(&id, make_msg("user", "c")).unwrap();
    append_event(
        &id,
        crate::llm::helpers::transcript_event(
            "tool_call_audit",
            "tool",
            "internal",
            "audit for dropped turn",
            None,
        ),
    )
    .unwrap();

    let result = truncate(&id, 2).expect("truncate result");
    assert_eq!(result.kept_turn_count, 2);
    assert_eq!(result.removed_turn_count, 1);
    assert!(
        result.new_tip_turn_id.is_some(),
        "retained tip event id should be surfaced"
    );
    assert_eq!(message_count(&id), 2);
    assert_eq!(event_count_by_kind(&id, "message"), 2);
    assert_eq!(event_count_by_kind(&id, "tool_call_audit"), 0);

    let messages = messages_json(&id);
    assert_eq!(messages[0]["content"], "a");
    assert_eq!(messages[1]["content"], "b");
    reset_session_store();
}

#[test]
fn truncate_to_zero_clears_messages_events_and_stale_summary() {
    reset_session_store();
    let id = open_or_create(Some("truncate-zero".into()));
    replace_messages_with_summary(
        &id,
        &[
            serde_json::json!({"role": "user", "content": "before"}),
            serde_json::json!({"role": "assistant", "content": "after"}),
        ],
        Some("summary that mentions removed turns"),
    );

    let result = truncate(&id, 0).expect("truncate result");
    assert_eq!(result.kept_turn_count, 0);
    assert_eq!(result.removed_turn_count, 2);
    assert_eq!(result.new_tip_turn_id, None);
    assert_eq!(message_count(&id), 0);
    assert_eq!(event_count_by_kind(&id, "message"), 0);
    let snapshot = snapshot(&id).expect("session snapshot");
    let dict = snapshot.as_dict().expect("snapshot dict");
    assert!(
        !dict.contains_key("summary"),
        "truncating away summarized turns must not leave stale prompt summary"
    );
    reset_session_store();
}

#[test]
fn truncate_unknown_session_returns_none() {
    reset_session_store();
    assert!(truncate("does-not-exist", 1).is_none());
}

#[test]
fn fork_at_on_unknown_source_returns_none() {
    reset_session_store();
    assert!(fork_at("does-not-exist", 3, None).is_none());
}

#[test]
fn child_sessions_record_parent_lineage() {
    reset_session_store();
    let parent = open_or_create(Some("parent-session".into()));
    let child = open_child_session(&parent, Some("child-session".into()));
    assert_eq!(parent_id(&child).as_deref(), Some("parent-session"));
    assert_eq!(child_ids(&parent), vec!["child-session".to_string()]);
    assert_eq!(
        ancestry(&child),
        Some(SessionAncestry {
            parent_id: Some("parent-session".to_string()),
            child_ids: Vec::new(),
            root_id: "parent-session".to_string(),
        })
    );

    let transcript = snapshot(&child).expect("child transcript");
    let transcript = transcript.as_dict().expect("child snapshot");
    let metadata = transcript
        .get("metadata")
        .and_then(|value| value.as_dict())
        .expect("child metadata");
    assert!(
        matches!(transcript.get("parent_id"), Some(VmValue::String(value)) if value.as_ref() == "parent-session")
    );
    assert!(
        matches!(transcript.get("child_ids"), Some(VmValue::List(children)) if children.is_empty())
    );
    assert!(matches!(transcript.get("length"), Some(VmValue::Int(0))));
    assert!(
        matches!(transcript.get("created_at"), Some(VmValue::String(value)) if !value.is_empty())
    );
    assert!(matches!(
        transcript.get("system_prompt"),
        Some(VmValue::Nil)
    ));
    assert!(matches!(transcript.get("tool_format"), Some(VmValue::Nil)));
    assert!(matches!(
        transcript.get("branched_at_event_index"),
        Some(VmValue::Nil)
    ));
    assert!(matches!(
        metadata.get("parent_session_id"),
        Some(VmValue::String(value)) if value.as_ref() == "parent-session"
    ));
}

#[test]
fn branch_event_index_counts_non_message_events() {
    reset_session_store();
    let src = open_or_create(Some("branch-event-index".into()));
    let transcript = VmValue::Dict(Rc::new(BTreeMap::from([
        ("id".to_string(), VmValue::String(Rc::from(src.clone()))),
        (
            "messages".to_string(),
            VmValue::List(Rc::new(vec![
                make_msg("user", "a"),
                make_msg("assistant", "b"),
            ])),
        ),
        (
            "events".to_string(),
            VmValue::List(Rc::new(vec![
                VmValue::Dict(Rc::new(BTreeMap::from([(
                    "kind".to_string(),
                    VmValue::String(Rc::from("message")),
                )]))),
                VmValue::Dict(Rc::new(BTreeMap::from([(
                    "kind".to_string(),
                    VmValue::String(Rc::from("sub_agent_start")),
                )]))),
                VmValue::Dict(Rc::new(BTreeMap::from([(
                    "kind".to_string(),
                    VmValue::String(Rc::from("message")),
                )]))),
            ])),
        ),
    ])));
    store_transcript(&src, transcript);

    let dst = fork_at(&src, 2, Some("branch-event-index-child".into())).expect("fork_at");
    assert_eq!(
        snapshot(&dst)
            .and_then(|value| value.as_dict().cloned())
            .and_then(|dict| dict
                .get("branched_at_event_index")
                .and_then(VmValue::as_int)),
        Some(3)
    );
}

#[test]
fn child_session_records_lineage_without_reusing_parent_transcript() {
    reset_session_store();
    let parent = open_or_create(Some("parent-fork-parent".into()));
    inject_message(&parent, make_msg("user", "parent context")).unwrap();
    claim_tool_format(&parent, "native").unwrap();

    let child = open_child_session(&parent, Some("parent-fork-child".into()));
    assert_eq!(message_count(&parent), 1);
    assert_eq!(message_count(&child), 0);
    assert_eq!(tool_format(&child), None);
    assert_eq!(parent_id(&child).as_deref(), Some(parent.as_str()));
}

#[test]
fn prompt_state_prepends_summary_message_when_missing_from_messages() {
    reset_session_store();
    let session = open_or_create(Some("prompt-state-summary".into()));
    let transcript = crate::llm::helpers::new_transcript_with_events(
        Some(session.clone()),
        vec![make_msg("assistant", "latest answer")],
        Some("[auto-compacted 2 older messages]\nsummary".to_string()),
        None,
        Vec::new(),
        Vec::new(),
        Some("active"),
    );
    store_transcript(&session, transcript);

    let prompt = prompt_state_json(&session);
    assert_eq!(
        prompt.summary.as_deref(),
        Some("[auto-compacted 2 older messages]\nsummary")
    );
    assert_eq!(prompt.messages.len(), 2);
    assert_eq!(prompt.messages[0]["role"].as_str(), Some("user"));
    assert_eq!(
        prompt.messages[0]["content"].as_str(),
        Some("[auto-compacted 2 older messages]\nsummary"),
    );
    assert_eq!(prompt.messages[1]["role"].as_str(), Some("assistant"));
}

#[tokio::test(flavor = "current_thread")]
async fn current_tool_call_scope_is_task_local() {
    reset_session_store();
    let first = scope_current_tool_call("first", async {
        tokio::task::yield_now().await;
        current_tool_call_id()
    });
    let second = scope_current_tool_call("second", async { current_tool_call_id() });

    let (first_id, second_id) = tokio::join!(first, second);

    assert_eq!(first_id.as_deref(), Some("first"));
    assert_eq!(second_id.as_deref(), Some("second"));
    assert_eq!(current_tool_call_id(), None);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn open_or_create_registers_event_log_sink_when_active_log_is_installed() {
    reset_all_sinks();
    crate::event_log::reset_active_event_log();
    crate::event_log::install_memory_for_current_thread(128);

    let session = open_or_create(Some("event-log-session".into()));
    assert_eq!(session_external_sink_count(&session), 1);

    let topic = Topic::new("observability.agent_events.event-log-session").unwrap();
    let log = active_event_log().expect("active event log");
    let mut stream = log.clone().subscribe(&topic, None).await.unwrap();

    emit_event(&AgentEvent::TurnStart {
        session_id: session.clone(),
        iteration: 0,
    });

    let emitted = stream
        .next()
        .await
        .expect("event log stream should receive emitted event")
        .expect("event log stream item");
    assert_eq!(emitted.1.kind, "turn_start");

    let events = log.read_range(&topic, None, usize::MAX).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1.kind, "turn_start");

    crate::event_log::reset_active_event_log();
    reset_all_sinks();
}
