use std::sync::Arc;

use crate::agent_events::{
    clear_session_sinks, emit_event, flush_session_sinks, register_sink, register_wildcard_sink,
    unregister_wildcard_sink, AgentEvent, AgentEventSink, EventLogSink, ToolCallStatus,
    ToolMutationStatus,
};
use crate::event_log::{AnyEventLog, EventLog, MemoryEventLog, Topic};

#[test]
fn redacts_tool_payloads_before_append() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(8)));
    let sink = EventLogSink::new(log.clone(), "s");
    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "s".into(),
        tool_call_id: "tc-1".into(),
        tool_name: "http".into(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({
            "authorization": "Bearer raw-bearer-value",
            "url": "https://user:password@example.com/items?sig=raw-signature&ok=1"
        }),
        parsing: None,
        audit: None,
    });

    let topic = Topic::new("observability.agent_events.s").unwrap();
    let events = futures::executor::block_on(log.read_range(&topic, None, 8)).unwrap();
    assert_eq!(events.len(), 1);
    let persisted = serde_json::to_string(&events[0].1).unwrap();
    assert!(persisted.contains("[redacted]") || persisted.contains("%5Bredacted%5D"));
    for secret in ["raw-bearer-value", "user:password", "raw-signature"] {
        assert!(
            !persisted.contains(secret),
            "event-log sink appended secret {secret}: {persisted}"
        );
    }
}

#[test]
fn skips_text_parsing_candidates() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(8)));
    let sink = EventLogSink::new(log.clone(), "s");
    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "s".into(),
        tool_call_id: "text-cand-0".into(),
        tool_name: "read".into(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({}),
        parsing: Some(true),
        audit: None,
    });
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "s".into(),
        tool_call_id: "call-real".into(),
        tool_name: "read".into(),
        status: ToolCallStatus::Completed,
        raw_output: Some(serde_json::json!({"text": "ok"})),
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: ToolMutationStatus::Unknown,
        changed_paths: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });

    let topic = Topic::new("observability.agent_events.s").unwrap();
    let events = futures::executor::block_on(log.read_range(&topic, None, 8)).unwrap();
    assert_eq!(events.len(), 1);
    let persisted = serde_json::to_string(&events[0].1).unwrap();
    assert!(persisted.contains("call-real"));
    assert!(!persisted.contains("text-cand-0"));
}

#[tokio::test(flavor = "current_thread")]
async fn flush_waits_for_queued_appends_without_polling() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(8)));
    let sink = EventLogSink::new(log.clone(), "flush-session");
    sink.handle_event(&AgentEvent::AgentMessageChunk {
        session_id: "flush-session".into(),
        content: "persist before replay".into(),
    });

    sink.flush().await.expect("flush queued event-log append");

    let topic = Topic::new("observability.agent_events.flush-session").unwrap();
    let events = log.read_range(&topic, None, 8).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].1.payload["event"]["content"],
        "persist before replay"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn session_flush_is_a_causal_registry_barrier() {
    let session_id = "registry-flush-session";
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(8)));
    register_sink(session_id, EventLogSink::new(log.clone(), session_id));
    emit_event(&AgentEvent::AgentMessageChunk {
        session_id: session_id.into(),
        content: "causally durable".into(),
    });

    flush_session_sinks(session_id)
        .await
        .expect("flush registered sinks");

    let topic = Topic::new("observability.agent_events.registry-flush-session").unwrap();
    let events = log.read_range(&topic, None, 8).await.unwrap();
    assert_eq!(events.len(), 1);
    clear_session_sinks(session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn flush_drains_concurrent_producers_without_scheduler_polling() {
    const PRODUCERS: usize = 8;
    const EVENTS_PER_PRODUCER: usize = 32;

    let session_id = "concurrent-flush-session";
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(
        PRODUCERS * EVENTS_PER_PRODUCER,
    )));
    let sink = EventLogSink::new(log.clone(), session_id);
    let mut producers = Vec::with_capacity(PRODUCERS);
    for producer in 0..PRODUCERS {
        let sink = sink.clone();
        producers.push(std::thread::spawn(move || {
            for event in 0..EVENTS_PER_PRODUCER {
                sink.handle_event(&AgentEvent::AgentMessageChunk {
                    session_id: session_id.into(),
                    content: format!("producer-{producer}-event-{event}"),
                });
            }
        }));
    }
    for producer in producers {
        producer.join().expect("event producer");
    }

    sink.flush().await.expect("flush all accepted events");

    let topic = Topic::new("observability.agent_events.concurrent-flush-session").unwrap();
    let events = log
        .read_range(&topic, None, PRODUCERS * EVENTS_PER_PRODUCER)
        .await
        .unwrap();
    assert_eq!(events.len(), PRODUCERS * EVENTS_PER_PRODUCER);
}

#[tokio::test(flavor = "current_thread")]
async fn session_flush_includes_wildcard_sinks() {
    let session_id = "wildcard-flush-session";
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(8)));
    let handle = register_wildcard_sink(EventLogSink::new(log.clone(), session_id));
    emit_event(&AgentEvent::AgentMessageChunk {
        session_id: session_id.into(),
        content: "persisted by wildcard".into(),
    });

    flush_session_sinks(session_id)
        .await
        .expect("flush wildcard event sink");

    let topic = Topic::new("observability.agent_events.wildcard-flush-session").unwrap();
    let events = log.read_range(&topic, None, 8).await.unwrap();
    assert_eq!(events.len(), 1);
    unregister_wildcard_sink(handle);
}
