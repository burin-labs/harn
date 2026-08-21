use std::sync::{Arc, Mutex};

use crate::agent_events::{
    AgentEvent, AgentEventSink, DurableAgentEventProjector, EventLogSink, JsonlEventSink,
    MultiSink, ToolCallErrorCategory, ToolCallStatus, ToolMutationStatus,
};
use crate::event_log::{AnyEventLog, EventLog, MemoryEventLog, Topic};

struct CapturingSink(Mutex<Vec<AgentEvent>>);

impl AgentEventSink for CapturingSink {
    fn handle_event(&self, event: &AgentEvent) {
        self.0
            .lock()
            .expect("capture mutex poisoned")
            .push(event.clone());
    }
}

fn streaming_update(session_id: &str, raw_input_partial: &str) -> AgentEvent {
    streaming_update_for(session_id, "call-streaming", raw_input_partial)
}

fn streaming_update_for(
    session_id: &str,
    tool_call_id: &str,
    raw_input_partial: &str,
) -> AgentEvent {
    AgentEvent::ToolCallUpdate {
        session_id: session_id.into(),
        tool_call_id: tool_call_id.into(),
        tool_name: "edit".into(),
        status: ToolCallStatus::Pending,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: ToolMutationStatus::Unknown,
        changed_paths: None,
        data: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: Some(raw_input_partial.into()),
        audit: None,
    }
}

fn parsed_streaming_update(
    session_id: &str,
    tool_call_id: &str,
    raw_input: serde_json::Value,
) -> AgentEvent {
    AgentEvent::ToolCallUpdate {
        session_id: session_id.into(),
        tool_call_id: tool_call_id.into(),
        tool_name: "edit".into(),
        status: ToolCallStatus::Pending,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: ToolMutationStatus::Unknown,
        changed_paths: None,
        data: None,
        executor: None,
        parsing: None,
        raw_input: Some(raw_input),
        raw_input_partial: None,
        audit: None,
    }
}

fn completed_update(session_id: &str) -> AgentEvent {
    AgentEvent::ToolCallUpdate {
        session_id: session_id.into(),
        tool_call_id: "call-streaming".into(),
        tool_name: "edit".into(),
        status: ToolCallStatus::Completed,
        raw_output: Some(serde_json::json!({"ok": true})),
        error: None,
        duration_ms: Some(7),
        execution_duration_ms: Some(5),
        error_category: None,
        mutation_status: ToolMutationStatus::Applied,
        changed_paths: Some(vec!["src/lib.rs".into()]),
        data: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    }
}

fn settled_tool_call(session_id: &str) -> AgentEvent {
    AgentEvent::ToolCall {
        session_id: session_id.into(),
        tool_call_id: "call-streaming".into(),
        tool_name: "edit".into(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({"body": "abcde"}),
        parsing: None,
        audit: None,
    }
}

fn parse_aborted_update(session_id: &str, tool_call_id: &str) -> AgentEvent {
    AgentEvent::ToolCallUpdate {
        session_id: session_id.into(),
        tool_call_id: tool_call_id.into(),
        tool_name: "edit".into(),
        status: ToolCallStatus::Failed,
        raw_output: None,
        error: Some("provider stream closed before arguments settled".into()),
        duration_ms: None,
        execution_duration_ms: None,
        error_category: Some(ToolCallErrorCategory::ParseAborted),
        mutation_status: ToolMutationStatus::Unknown,
        changed_paths: None,
        data: None,
        executor: None,
        parsing: Some(false),
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    }
}

#[test]
fn live_sink_keeps_full_stream_while_both_durable_sinks_checkpoint_prefix_growth() {
    let session_id = "durable-projection-parity";
    let temp = tempfile::tempdir().expect("event-log tempdir");
    let jsonl_path = temp.path().join("event_log.jsonl");
    let jsonl_sink = JsonlEventSink::open(&jsonl_path).expect("open JSONL sink");
    let event_log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));
    let event_log_sink = EventLogSink::new(event_log.clone(), session_id);
    let live_sink = Arc::new(CapturingSink(Mutex::new(Vec::new())));
    let sinks = MultiSink::new();
    sinks.push(live_sink.clone());
    sinks.push(jsonl_sink.clone());
    sinks.push(event_log_sink);

    let events = [
        streaming_update(session_id, "a"),
        streaming_update(session_id, "ab"),
        streaming_update(session_id, "abc"),
        streaming_update(session_id, "abcd"),
        streaming_update(session_id, "abcde"),
        settled_tool_call(session_id),
        completed_update(session_id),
    ];
    for event in &events {
        sinks.handle_event(event);
    }

    assert_eq!(
        live_sink.0.lock().expect("capture mutex poisoned").len(),
        events.len(),
        "live observers must receive every streaming update"
    );

    jsonl_sink.flush().expect("flush JSONL sink");
    let jsonl_events = std::fs::read_to_string(&jsonl_path)
        .expect("read JSONL sink")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("valid JSONL event"))
        .collect::<Vec<_>>();
    assert_eq!(jsonl_events.len(), 5);
    assert_eq!(
        jsonl_events
            .iter()
            .filter_map(|event| event["raw_input_partial"].as_str())
            .collect::<Vec<_>>(),
        ["a", "ab", "abcd"]
    );
    assert_eq!(
        jsonl_events[3]["raw_input"],
        serde_json::json!({"body": "abcde"})
    );
    assert_eq!(jsonl_events[4]["status"], "completed");
    assert_eq!(
        jsonl_events[4]["raw_output"],
        serde_json::json!({"ok": true})
    );

    let topic = Topic::new("observability.agent_events.durable-projection-parity")
        .expect("valid agent-events topic");
    let event_log_events = futures::executor::block_on(event_log.read_range(&topic, None, 16))
        .expect("read event log");
    assert_eq!(event_log_events.len(), 5);
    assert_eq!(
        event_log_events
            .iter()
            .filter_map(|(_, record)| record.payload["event"]["raw_input_partial"].as_str())
            .collect::<Vec<_>>(),
        ["a", "ab", "abcd"]
    );
    assert_eq!(
        event_log_events[3].1.payload["event"]["raw_input"],
        serde_json::json!({"body": "abcde"})
    );
    assert_eq!(
        event_log_events[4].1.payload["event"]["status"],
        "completed"
    );
    assert_eq!(
        event_log_events[4].1.payload["event"]["raw_output"],
        serde_json::json!({"ok": true})
    );
}

#[test]
fn projector_fails_open_and_rearms_on_adversarial_stream_changes() {
    let mut projector = DurableAgentEventProjector::new();

    assert!(projector.should_persist(&streaming_update_for("s", "a", "a")));
    assert!(projector.should_persist(&streaming_update_for("s", "a", "ab")));
    assert!(!projector.should_persist(&streaming_update_for("s", "a", "abc")));
    assert!(!projector.should_persist(&streaming_update_for("s", "a", "abc")));

    assert!(
        projector.should_persist(&streaming_update_for("s", "a", "abx")),
        "same-length mutation must fail open"
    );
    assert!(
        projector.should_persist(&streaming_update_for("s", "a", "zzzz")),
        "non-prefix growth must fail open"
    );
    assert!(!projector.should_persist(&streaming_update_for("s", "a", "zzzzz")));
    assert!(
        projector.should_persist(&streaming_update_for("s", "a", "z")),
        "shrink must fail open"
    );
    let parsed = parsed_streaming_update("s", "a", serde_json::json!({"body": "z"}));
    assert!(
        projector.should_persist(&parsed),
        "representation change must fail open"
    );
    assert!(
        !projector.should_persist(&parsed),
        "identical parsed JSON serialization must be recognized as a duplicate"
    );

    let mut ambiguous = streaming_update_for("s", "a", "ambiguous");
    if let AgentEvent::ToolCallUpdate { raw_input, .. } = &mut ambiguous {
        *raw_input = Some(serde_json::json!({"body": "ambiguous"}));
    }
    assert!(
        projector.should_persist(&ambiguous),
        "mutually exclusive argument representations must fail open"
    );
    assert!(
        projector.should_persist(&streaming_update_for("s", "a", "ambiguous")),
        "ambiguous representation must evict and re-arm the call"
    );

    let mut parsing_settled = streaming_update_for("s", "a", "settled");
    if let AgentEvent::ToolCallUpdate { parsing, .. } = &mut parsing_settled {
        *parsing = Some(false);
    }
    assert!(projector.should_persist(&parsing_settled));
    assert!(
        projector.should_persist(&streaming_update_for("s", "a", "settled")),
        "settled transition must evict and re-arm the call"
    );

    assert!(projector.should_persist(&streaming_update_for("s", "progress", "x")));
    let mut in_progress = streaming_update_for("s", "progress", "x");
    if let AgentEvent::ToolCallUpdate { status, .. } = &mut in_progress {
        *status = ToolCallStatus::InProgress;
    }
    assert!(projector.should_persist(&in_progress));
    assert!(
        projector.should_persist(&streaming_update_for("s", "progress", "x")),
        "in-progress transition must persist and re-arm the call"
    );

    let mut non_streaming = streaming_update_for("s", "progress", "x");
    if let AgentEvent::ToolCallUpdate {
        raw_input_partial, ..
    } = &mut non_streaming
    {
        *raw_input_partial = None;
    }
    assert!(projector.should_persist(&non_streaming));
    assert!(
        projector.should_persist(&streaming_update_for("s", "progress", "x")),
        "non-streaming pending transition must persist and re-arm the call"
    );
}

#[test]
fn projector_bounds_cumulative_payload_and_is_sequence_deterministic() {
    const FINAL_SIZE: usize = 4 * 1_024;
    let mut first = DurableAgentEventProjector::new();
    let mut decisions = Vec::with_capacity(FINAL_SIZE);
    let mut retained_bytes = 0;
    for size in 1..=FINAL_SIZE {
        let event = streaming_update_for("s", "large", &"x".repeat(size));
        let retained = first.should_persist(&event);
        decisions.push(retained);
        if retained {
            retained_bytes += size;
        }
    }
    assert!(
        retained_bytes < FINAL_SIZE * 2,
        "geometric snapshots must remain below twice the final cumulative payload: {retained_bytes}"
    );

    let mut second = DurableAgentEventProjector::new();
    for (size, expected) in (1..=FINAL_SIZE).zip(decisions) {
        let event = streaming_update_for("s", "large", &"x".repeat(size));
        assert_eq!(
            second.should_persist(&event),
            expected,
            "projection must depend only on source event order at byte {size}"
        );
    }
}

#[test]
fn projector_isolates_interleaved_calls_and_retains_parse_abort() {
    let mut projector = DurableAgentEventProjector::new();
    assert!(projector.should_persist(&streaming_update_for("s", "a", "a")));
    assert!(projector.should_persist(&streaming_update_for("s", "b", "a")));
    assert!(projector.should_persist(&streaming_update_for("s", "a", "ab")));
    assert!(projector.should_persist(&streaming_update_for("s", "b", "ab")));
    assert!(!projector.should_persist(&streaming_update_for("s", "a", "abc")));
    assert!(!projector.should_persist(&streaming_update_for("s", "b", "abc")));
    assert!(projector.should_persist(&streaming_update_for("s", "a", "abcd")));
    assert!(projector.should_persist(&parse_aborted_update("s", "b")));
    assert!(
        projector.should_persist(&streaming_update_for("s", "b", "abc")),
        "parse-aborted transition must evict only its exact call"
    );
    assert!(
        !projector.should_persist(&streaming_update_for("s", "a", "abcde")),
        "the interleaved call must keep its own next checkpoint"
    );
}

#[test]
fn headless_durable_sink_keeps_checkpointed_progress_and_exact_crash_evidence() {
    let session_id = "headless-crash";
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(8)));
    let sink = EventLogSink::new(log.clone(), session_id);
    for event in [
        streaming_update_for(session_id, "crash", "a"),
        streaming_update_for(session_id, "crash", "ab"),
        streaming_update_for(session_id, "crash", "abc"),
        parse_aborted_update(session_id, "crash"),
    ] {
        sink.handle_event(&event);
    }

    let topic = Topic::new("observability.agent_events.headless-crash").expect("valid topic");
    let events = futures::executor::block_on(log.read_range(&topic, None, 8))
        .expect("read headless durable events");
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].1.payload["event"]["raw_input_partial"], "a");
    assert_eq!(events[1].1.payload["event"]["raw_input_partial"], "ab");
    assert_eq!(events[2].1.payload["event"]["status"], "failed");
    assert_eq!(events[2].1.payload["event"]["parsing"], false);
    assert_eq!(
        events[2].1.payload["event"]["error_category"],
        "parse_aborted"
    );
    assert_eq!(
        events[2].1.payload["event"]["error"],
        "provider stream closed before arguments settled"
    );
}

#[test]
fn projector_contains_abandoned_calls_and_evicts_exact_lifecycles() {
    let mut projector = DurableAgentEventProjector::new();
    let limit = projector.max_tracked_streams();
    for call in 0..limit {
        assert!(projector.should_persist(&streaming_update_for(
            "abandoned",
            &format!("call-{call}"),
            "x",
        )));
    }
    assert_eq!(projector.tracked_stream_count(), limit);
    assert!(!projector.should_persist(&streaming_update_for("abandoned", "call-0", "x",)));
    assert!(projector.should_persist(&streaming_update_for(
        "abandoned",
        &format!("call-{limit}"),
        "x",
    )));
    assert!(
        projector.should_persist(&streaming_update_for("abandoned", "call-1", "x")),
        "least-recently observed abandoned call must be evicted and fail open if it returns"
    );
    assert!(
        !projector.should_persist(&streaming_update_for("abandoned", "call-0", "x")),
        "recently observed call must remain tracked"
    );

    assert!(projector.should_persist(&streaming_update_for("other", "call", "x")));
    assert!(projector.should_persist(&AgentEvent::SessionClosed {
        session_id: "abandoned".into(),
        reason: "test".into(),
        status: "completed".into(),
        metadata: serde_json::json!({}),
    }));
    assert_eq!(projector.tracked_stream_count(), 1);

    assert!(projector.should_persist(&AgentEvent::ToolCall {
        session_id: "other".into(),
        tool_call_id: "call".into(),
        tool_name: "edit".into(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({"body": "x"}),
        parsing: None,
        audit: None,
    }));
    assert_eq!(projector.tracked_stream_count(), 0);

    let oversized_id = "x".repeat(300 * 1_024);
    let oversized = streaming_update_for("oversized", &oversized_id, "x");
    assert!(projector.should_persist(&oversized));
    assert!(
        projector.should_persist(&oversized),
        "identity that exceeds the retained-state byte budget must stay fail-open"
    );
    assert_eq!(projector.tracked_stream_count(), 0);
}

#[test]
fn jsonl_rotation_preserves_projection_state_and_contiguous_durable_indices() {
    let temp = tempfile::tempdir().expect("event-log tempdir");
    let path = temp.path().join("event_log.jsonl");
    let sink = JsonlEventSink::open(&path).expect("open JSONL sink");

    sink.handle_event(&streaming_update("rotation", "a"));
    sink.force_rotation_after_next_write();
    sink.handle_event(&streaming_update("rotation", "ab"));
    sink.handle_event(&streaming_update("rotation", "abc"));
    sink.handle_event(&streaming_update("rotation", "abcd"));
    sink.flush().expect("flush rotated JSONL sink");

    let base = std::fs::read_to_string(&path).expect("read base JSONL file");
    let rotated = std::fs::read_to_string(temp.path().join("event_log-000001.jsonl"))
        .expect("read rotated JSONL file");
    assert_eq!(base.lines().count(), 2);
    assert!(base.contains("\"raw_input_partial\":\"a\""));
    assert!(base.contains("\"raw_input_partial\":\"ab\""));
    assert_eq!(rotated.lines().count(), 1);
    assert!(rotated.contains("\"raw_input_partial\":\"abcd\""));
    assert!(!rotated.contains("\"raw_input_partial\":\"abc\""));
    assert!(base.contains("\"index\":0"));
    assert!(base.contains("\"index\":1"));
    assert!(rotated.contains("\"index\":2"));
}
