use super::*;
use crate::event_log::{FileEventLog, MemoryEventLog};
use crate::orchestration::{save_run_record, RunRecord};
use futures::StreamExt;
use serde_json::json;

fn span(span_id: u64, parent_id: Option<u64>, metadata: serde_json::Value) -> RunTraceSpanRecord {
    RunTraceSpanRecord {
        trace_id: "trace-1".to_string(),
        span_id,
        parent_id,
        kind: if parent_id.is_some() {
            "tool_call".to_string()
        } else {
            "pipeline".to_string()
        },
        name: format!("span-{span_id}"),
        start_ms: span_id * 10,
        duration_ms: 5,
        metadata: serde_json::from_value(metadata).unwrap_or_default(),
        links: Vec::new(),
    }
}

#[test]
fn run_record_spans_project_parent_child_tree_and_redact_metadata() {
    let mut run = RunRecord {
        id: "run-1".to_string(),
        trace_spans: vec![
            span(
                2,
                Some(1),
                json!({"status": "ok", "api_key": "should-redact"}),
            ),
            span(1, None, json!({"session_id": "s1"})),
        ],
        ..RunRecord::default()
    };
    run.metadata
        .insert("project_id".to_string(), json!("project-1"));

    let snapshot = timeline_from_run_record(
        &run,
        SessionTimelineQuery {
            session_id: Some("s1".to_string()),
            run_id: Some("run-1".to_string()),
            project_id: Some("project-1".to_string()),
            ..SessionTimelineQuery::default()
        },
    );

    assert_eq!(snapshot.nodes.len(), 2);
    let root = snapshot
        .nodes
        .iter()
        .find(|node| node.id == "span:trace-1:1")
        .expect("root span node");
    assert_eq!(root.children, vec!["span:trace-1:2"]);
    let child = snapshot
        .nodes
        .iter()
        .find(|node| node.id == "span:trace-1:2")
        .expect("child span node");
    assert_eq!(child.parent_id.as_deref(), Some("span:trace-1:1"));
    assert_eq!(
        child.attributes["api_key"],
        json!(crate::redact::REDACTED_PLACEHOLDER)
    );
}

#[tokio::test]
async fn channel_emit_and_match_project_causal_links() {
    let log = AnyEventLog::Memory(MemoryEventLog::new(16));
    let topic = static_topic(crate::channels::CHANNEL_TRANSCRIPT_TOPIC);
    log.append(
        &topic,
        LogEvent::new(
            crate::channels::CHANNEL_EMIT_TRANSCRIPT_KIND,
            json!({
                "event_id": "evt-1",
                "name_resolved": "session:s1:updates",
                "scope": "session",
                "scope_id": "s1",
                "session_id": "s1",
                "payload_summary": {"kind": "object"},
                "span_id": 7
            }),
        ),
    )
    .await
    .unwrap();
    log.append(
        &topic,
        LogEvent::new(
            crate::channels::CHANNEL_MATCH_TRANSCRIPT_KIND,
            json!({
                "event_id": "evt-1",
                "name_resolved": "session:s1:updates",
                "scope": "session",
                "scope_id": "s1",
                "trigger_id": "trigger-1",
                "matched_in_session_id": "s1",
                "batch": {
                    "count": 2,
                    "constituent_event_ids": ["evt-a", "evt-b"]
                },
                "span_id": 8
            }),
        ),
    )
    .await
    .unwrap();

    let snapshot =
        query_session_timeline(Some(&log), None, SessionTimelineQuery::for_session("s1"))
            .await
            .unwrap();

    let emit = snapshot
        .nodes
        .iter()
        .find(|node| node.id == "channel:evt-1:emit")
        .expect("emit node");
    assert_eq!(emit.category, "channel");
    let matched = snapshot
        .nodes
        .iter()
        .find(|node| node.id == "channel:evt-1:match:trigger-1")
        .expect("match node");
    assert_eq!(
        matched.links[0].target_id.as_deref(),
        Some("channel:evt-1:emit")
    );
    assert!(matched.links.iter().any(|link| {
        link.kind == "channel_batch_member" && link.event_id.as_deref() == Some("evt-a")
    }));
    assert!(matched.links.iter().any(|link| {
        link.kind == "channel_batch_member" && link.event_id.as_deref() == Some("evt-b")
    }));
}

#[tokio::test]
async fn persisted_file_log_reads_agent_events() {
    let temp = tempfile::tempdir().unwrap();
    let log = AnyEventLog::File(FileEventLog::open(temp.path().to_path_buf(), 16).unwrap());
    let topic = agent_events_topic("s1");
    log.append(
        &topic,
        LogEvent::new(
            "tool_call",
            json!({
                "session_id": "s1",
                "event": {
                    "type": "tool_call",
                    "session_id": "s1",
                    "tool_call_id": "tool-1",
                    "tool_name": "read",
                    "status": "pending",
                    "raw_input": {"token": "should-redact"}
                }
            }),
        )
        .with_headers(BTreeMap::from([(
            "session_id".to_string(),
            "s1".to_string(),
        )])),
    )
    .await
    .unwrap();
    log.flush().await.unwrap();

    let snapshot = query_session_timeline(
        Some(&log),
        None,
        SessionTimelineQuery {
            session_id: Some("s1".to_string()),
            run_id: Some("run_session_timeline_filter_00000000".to_string()),
            ..SessionTimelineQuery::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(snapshot.nodes.len(), 1);
    assert_eq!(snapshot.nodes[0].category, "agent_event");
    assert_eq!(
        snapshot.nodes[0].attributes["event"]["raw_input"]["token"],
        json!(crate::redact::REDACTED_PLACEHOLDER)
    );
}

#[tokio::test]
async fn persisted_run_record_reads_nested_spans() {
    let temp = tempfile::tempdir().unwrap();
    let run_path = temp.path().join("run-1.json");
    let mut run = RunRecord {
        id: "run-1".to_string(),
        trace_spans: vec![
            span(1, None, json!({"session_id": "s1"})),
            span(2, Some(1), json!({"session_id": "s1"})),
        ],
        ..RunRecord::default()
    };
    run.metadata
        .insert("project_id".to_string(), json!("project-1"));
    save_run_record(&run, Some(run_path.to_str().unwrap())).unwrap();

    let snapshot = query_session_timeline(
        None,
        None,
        SessionTimelineQuery {
            session_id: Some("s1".to_string()),
            run_id: Some("run-1".to_string()),
            run_path: Some(run_path.display().to_string()),
            project_id: Some("project-1".to_string()),
            ..SessionTimelineQuery::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(snapshot.nodes.len(), 2);
    assert_eq!(snapshot.nodes[0].id, "span:trace-1:1");
    assert_eq!(snapshot.nodes[0].children, vec!["span:trace-1:2"]);
    assert_eq!(
        snapshot.nodes[1].parent_id.as_deref(),
        Some("span:trace-1:1")
    );
}

#[tokio::test]
async fn subscription_streams_live_appends_without_polling() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));
    let mut stream =
        subscribe_session_timeline(log.clone(), SessionTimelineQuery::for_session("s1"))
            .await
            .unwrap();
    let topic = agent_events_topic("s1");
    log.append(
        &topic,
        LogEvent::new(
            "agent_message_chunk",
            json!({
                "session_id": "s1",
                "event": {
                    "type": "agent_message_chunk",
                    "session_id": "s1",
                    "content": "hello"
                }
            }),
        )
        .with_headers(BTreeMap::from([(
            "session_id".to_string(),
            "s1".to_string(),
        )])),
    )
    .await
    .unwrap();

    let update = stream.next().await.unwrap().unwrap();
    assert_eq!(update.node.category, "agent_event");
    assert_eq!(update.node.name, "agent_message_chunk");
    assert_eq!(update.cursor.topics.get(topic.as_str()).copied(), Some(1));
}
