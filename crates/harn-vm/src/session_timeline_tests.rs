use super::*;
use crate::event_log::{FileEventLog, MemoryEventLog};
use crate::orchestration::{save_run_record, RunRecord};
use futures::StreamExt;
use harn_session_store::{
    AppendEvent, CreateSession, ImportSession, SessionEventKind, SessionImporter, SessionStore,
    SqliteSessionStore,
};
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
        ttft_ms: None,
        metadata: serde_json::from_value(metadata).unwrap_or_default(),
        links: Vec::new(),
        cost_usd: None,
    }
}

#[tokio::test]
async fn persisted_transcript_projects_stable_tool_revision_and_identity_links() {
    let temp = tempfile::tempdir().expect("project root");
    std::fs::create_dir(temp.path().join(".harn")).expect("state dir");
    let store = SqliteSessionStore::open(temp.path().join(".harn/session-store.sqlite"))
        .expect("canonical store");
    store
        .create(CreateSession {
            id: Some("session-1".to_string()),
            project_scope: Some(temp.path().to_string_lossy().into_owned()),
            ..CreateSession::default()
        })
        .await
        .expect("create session");
    let mut call = AppendEvent::new(
        SessionEventKind::ToolCall,
        json!({
            "transcript_event": {
                "text": "Read source",
                "metadata": {
                    "tool_name": "read_file",
                    "input": {"path": "src/lib.rs"}
                }
            }
        }),
    );
    call.headers = BTreeMap::from([
        ("run_id".to_string(), "run-1".to_string()),
        ("turn_id".to_string(), "turn-1".to_string()),
        ("source_event_id".to_string(), "source-1".to_string()),
        ("tool_call_id".to_string(), "tool-1".to_string()),
    ]);
    store.append("session-1", call).await.expect("append call");
    let mut result = AppendEvent::new(
        SessionEventKind::ToolResult,
        json!({
            "transcript_event": {
                "text": "source contents",
                "metadata": {
                    "is_error": false,
                    "output": {"bytes": 42}
                }
            }
        }),
    );
    result.headers = BTreeMap::from([
        ("run_id".to_string(), "run-1".to_string()),
        ("turn_id".to_string(), "turn-1".to_string()),
        ("source_event_id".to_string(), "source-2".to_string()),
        ("tool_call_id".to_string(), "tool-1".to_string()),
    ]);
    store
        .append("session-1", result)
        .await
        .expect("append result");

    let snapshot = query_persisted_session_timeline(
        temp.path(),
        SessionTimelineQuery::for_session("session-1"),
    )
    .await
    .expect("query timeline")
    .expect("persisted session");

    assert_eq!(snapshot.nodes.len(), 1);
    assert_eq!(
        snapshot.nodes[0].id,
        "session:session-1:run:run-1:turn:turn-1:tool:tool-1"
    );
    assert_eq!(snapshot.nodes[0].status, "completed");
    assert_eq!(snapshot.nodes[0].name, "read_file");
    assert_eq!(
        snapshot.nodes[0].attributes["input"],
        json!({"path": "src/lib.rs"})
    );
    assert_eq!(snapshot.nodes[0].attributes["output"], json!({"bytes": 42}));
    assert_eq!(snapshot.nodes[0].attributes["isError"], json!(false));
    assert!(snapshot.nodes[0].start_ms.is_some());
    assert!(snapshot.nodes[0].duration_ms.is_some());
    assert_eq!(snapshot.nodes[0].attributes["revision"], json!(2));
    assert!(snapshot.nodes[0]
        .links
        .iter()
        .any(|link| link.kind == "turn" && link.target_id.as_deref() == Some("turn-1")));
    assert_eq!(
        snapshot
            .cursor
            .topics
            .get("session-store:session-1")
            .copied(),
        Some(2)
    );
}

#[tokio::test]
async fn persisted_timeline_distinguishes_an_empty_session_from_a_missing_one() {
    let store = SqliteSessionStore::open_in_memory().expect("canonical store");
    let missing =
        query_session_store_timeline(&store, SessionTimelineQuery::for_session("missing-session"))
            .await
            .expect("query missing session");
    assert!(missing.is_none());

    store
        .create(CreateSession {
            id: Some("empty-session".to_string()),
            ..CreateSession::default()
        })
        .await
        .expect("create empty session");
    let empty =
        query_session_store_timeline(&store, SessionTimelineQuery::for_session("empty-session"))
            .await
            .expect("query empty session")
            .expect("empty session exists");
    assert!(empty.nodes.is_empty());
}

#[tokio::test]
async fn persisted_10k_event_tool_timeline_opens_under_500ms() {
    let temp = tempfile::tempdir().expect("project root");
    let store = SqliteSessionStore::open(temp.path().join("session-store.sqlite"))
        .expect("canonical store");
    let mut events = Vec::with_capacity(10_000);
    for index in 0..5_000 {
        let tool_call_id = format!("tool-{index}");
        let mut call = AppendEvent::new(
            SessionEventKind::ToolCall,
            json!({"transcript_event": {"metadata": {"tool_name": "read_file"}}}),
        );
        call.headers
            .insert("tool_call_id".to_string(), tool_call_id.clone());
        call.headers
            .insert("run_id".to_string(), "perf-run".to_string());
        call.headers
            .insert("turn_id".to_string(), "perf-turn".to_string());
        let mut result = AppendEvent::new(
            SessionEventKind::ToolResult,
            json!({"transcript_event": {"metadata": {"is_error": false}}}),
        );
        result
            .headers
            .insert("tool_call_id".to_string(), tool_call_id);
        result
            .headers
            .insert("run_id".to_string(), "perf-run".to_string());
        result
            .headers
            .insert("turn_id".to_string(), "perf-turn".to_string());
        events.extend([call, result]);
    }
    store
        .import(ImportSession {
            source_id: "timeline-perf-corpus".to_string(),
            source_digest: "sha256:timeline-perf-corpus".to_string(),
            session: CreateSession {
                id: Some("timeline-perf-session".to_string()),
                project_scope: Some(temp.path().to_string_lossy().into_owned()),
                ..CreateSession::default()
            },
            events,
        })
        .await
        .expect("import timeline corpus");

    let started = std::time::Instant::now();
    let snapshot = query_session_store_timeline(
        &store,
        SessionTimelineQuery {
            limit: Some(10_000),
            ..SessionTimelineQuery::for_session("timeline-perf-session")
        },
    )
    .await
    .expect("project timeline")
    .expect("session exists");
    let elapsed = started.elapsed();
    eprintln!("10k-event tool timeline elapsed: {elapsed:?}");

    assert_eq!(snapshot.nodes.len(), 5_000);
    assert!(snapshot.nodes[0].id.ends_with(":tool:tool-0"));
    assert!(snapshot.nodes[4_999].id.ends_with(":tool:tool-4999"));
    let timing_covered = snapshot
        .nodes
        .iter()
        .filter(|node| node.start_ms.is_some() && node.duration_ms.is_some())
        .count();
    assert!(
        timing_covered * 100 >= snapshot.nodes.len() * 99,
        "completed-tool timing coverage was {timing_covered}/{}",
        snapshot.nodes.len()
    );
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "10k-event timeline took {elapsed:?}"
    );
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
