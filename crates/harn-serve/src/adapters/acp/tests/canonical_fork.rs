//! Fork a cold canonical parent before its first prompt, then reload the child.

use super::*;
use harn_session_store::{AppendEvent, CreateSession, ReadRange, SessionEventKind, SessionStore};

#[tokio::test(flavor = "current_thread")]
async fn acknowledged_tool_history_survives_compaction_and_child_restart() {
    harn_vm::reset_thread_local_state();
    let root = tempfile::tempdir().expect("root");
    let store = harn_vm::open_canonical_store(root.path()).expect("store");
    let parent = "tool-history-parent";
    store
        .create(CreateSession {
            id: Some(parent.into()),
            ..CreateSession::default()
        })
        .await
        .expect("parent");
    let messages = serde_json::json!([
        {"role":"user", "content":"first"},
        {"role":"assistant", "content":"", "tool_calls":[{"id":"call-1", "type":"function", "function":{"name":"read_file", "arguments":"{}"}}]},
        {"role":"tool", "content":"file contents", "tool_call_id":"call-1"},
        {"role":"assistant", "content":"selected answer"},
        {"role":"user", "content":"later question"}
    ]);
    for (index, message) in messages.as_array().expect("messages").iter().enumerate() {
        let identity = format!("tool-message-{index}");
        let kind = match index {
            1 => SessionEventKind::ToolCall,
            2 => SessionEventKind::ToolResult,
            _ => SessionEventKind::Message,
        };
        let mut event = AppendEvent::new(kind, serde_json::json!({"raw_message":message}));
        event.headers.insert("source_event_id".into(), identity);
        store
            .append(parent, event)
            .await
            .expect("canonical message");
    }
    let acknowledged =
        harn_vm::agent_sessions::canonical_history_boundaries(&store, root.path(), parent)
            .await
            .expect("acknowledged positions");
    assert_eq!(acknowledged.positions.len(), 5);
    let selected = acknowledged.positions[3].boundary.clone();
    store
        .append(
            parent,
            AppendEvent::new(
                SessionEventKind::Compaction,
                serde_json::json!({
                    "messages":[messages[0],messages[4]], "summary":"compacted tool exchange",
                    "source_event_ids":["tool-message-0", "tool-message-4"]
                }),
            ),
        )
        .await
        .expect("compaction");
    let archived = store.read_all(parent).await.expect("parent archive");
    let mut server = AcpServer::new(AcpServerConfig::new(None));
    server
        .handle_session_load(
            &serde_json::json!(1),
            &serde_json::json!({"sessionId":parent,"cwd":root.path()}),
        )
        .await;
    server.handle_session_fork(&serde_json::json!(2),
        &serde_json::json!({"sessionId":parent,"id":"tool-history-child","canonicalBoundary":selected})).await;
    assert!(
        server.sessions.contains_key("tool-history-child"),
        "fork action must fire"
    );
    let live = harn_vm::llm::vm_value_to_json(
        &harn_vm::agent_sessions::transcript("tool-history-child").expect("child"),
    );
    let expected = serde_json::Value::Array(messages.as_array().unwrap()[..4].to_vec());
    assert_eq!(
        live["messages"], expected,
        "all tool rows and selected answer, no later question"
    );
    assert!(
        live["summary"].is_null(),
        "a later summary cannot leak into historical context"
    );
    server
        .handle_session_fork(
            &serde_json::json!("current-tip"),
            &serde_json::json!({"sessionId":parent,"id":"current-tip-child"}),
        )
        .await;
    assert!(server.sessions.contains_key("current-tip-child"));
    let current = harn_vm::llm::vm_value_to_json(
        &harn_vm::agent_sessions::transcript("current-tip-child").expect("current context"),
    );
    assert_eq!(
        current["messages"],
        serde_json::json!([messages[0], messages[4]])
    );
    assert_eq!(current["summary"], "compacted tool exchange");
    let after = store
        .read_all(parent)
        .await
        .expect("parent archive after fork");
    assert_eq!(after.len(), archived.len());
    assert_eq!(
        after.last().unwrap().record_hash,
        archived.last().unwrap().record_hash
    );
    drop(server);
    harn_vm::reset_thread_local_state();
    let mut restarted = AcpServer::new(AcpServerConfig::new(None));
    restarted
        .handle_session_load(
            &serde_json::json!(3),
            &serde_json::json!({"sessionId":"tool-history-child","cwd":root.path()}),
        )
        .await;
    restarted
        .handle_session_fork(
            &serde_json::json!(4),
            &serde_json::json!({"sessionId":"tool-history-child","id":"tool-history-grandchild"}),
        )
        .await;
    assert!(restarted.sessions.contains_key("tool-history-grandchild"));
    let restored = harn_vm::llm::vm_value_to_json(
        &harn_vm::agent_sessions::transcript("tool-history-grandchild").expect("grandchild"),
    );
    assert_eq!(restored["messages"], expected);
    assert_eq!(
        store
            .describe("tool-history-grandchild")
            .await
            .expect("grandchild")
            .parent_session_id
            .as_deref(),
        Some("tool-history-child")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cold_parent_forks_persist_selected_context_and_lineage_before_prompt() {
    harn_vm::reset_thread_local_state();
    let root = tempfile::tempdir().expect("root");
    let store = harn_vm::open_canonical_store(root.path()).expect("store");
    let parent = "canonical-fork-parent";
    store
        .create(CreateSession {
            id: Some(parent.into()),
            cwd: Some(root.path().to_string_lossy().into_owned()),
            project_scope: Some(root.path().to_string_lossy().into_owned()),
            ..CreateSession::default()
        })
        .await
        .expect("parent");
    for (index, text) in ["first", "second", "third"].iter().enumerate() {
        let identity = format!("canonical-message-{index}");
        let mut event = AppendEvent::new(
            SessionEventKind::Message,
            serde_json::json!({
                "raw_message": {"role": "user", "content": text},
                "transcript_event": {"id": identity, "kind": "message", "role": "user", "text": text},
            }),
        );
        event.headers.insert("source_event_id".into(), identity);
        store.append(parent, event).await.expect("parent message");
    }
    let mut server = AcpServer::new(AcpServerConfig::new(None));
    server
        .handle_session_load(
            &serde_json::json!(1),
            &serde_json::json!({
                "sessionId": parent, "cwd": root.path(),
            }),
        )
        .await;
    assert!(
        server.sessions.contains_key(parent),
        "cold parent must actually load"
    );
    let boundaries =
        harn_vm::agent_sessions::canonical_history_boundaries(&store, root.path(), parent)
            .await
            .expect("acknowledged canonical boundaries");
    let first_boundary = boundaries.positions[0].boundary.clone();
    for (count, child, boundary) in [
        (2, "prefix-child", boundaries.positions[1].boundary.clone()),
        (
            0,
            "empty-child",
            harn_vm::agent_sessions::CanonicalSessionBoundary::empty(parent),
        ),
    ] {
        server
            .handle_session_fork(
                &serde_json::json!(2),
                &serde_json::json!({
                    "sessionId": parent, "id": child, "canonicalBoundary": boundary,
                    "environmentPolicy": {"kind": "isolated", "grants": []},
                }),
            )
            .await;
        assert!(server.sessions.contains_key(child), "fork must be admitted");
        let live = harn_vm::llm::vm_value_to_json(
            &harn_vm::agent_sessions::transcript(child).expect("live child"),
        );
        assert_eq!(live["messages"].as_array().expect("messages").len(), count);
        if count > 0 {
            assert_eq!(live["messages"][1]["content"], "second");
        }
        let metadata = store
            .describe(child)
            .await
            .expect("durable child before prompt");
        assert_eq!(metadata.parent_session_id.as_deref(), Some(parent));
        let persisted = store
            .read(child, ReadRange::default())
            .await
            .expect("persisted context");
        assert_eq!(persisted.events.len(), count);
        if count > 0 {
            assert_eq!(
                persisted.events[1].payload["raw_message"]["content"],
                "second"
            );
        }
    }
    drop(server);
    harn_vm::reset_thread_local_state();
    let mut restarted = AcpServer::new(AcpServerConfig::new(None));
    restarted
        .handle_session_load(
            &serde_json::json!(3),
            &serde_json::json!({
                "sessionId": "prefix-child", "cwd": root.path(),
            }),
        )
        .await;
    assert!(
        restarted.sessions.contains_key("prefix-child"),
        "durable child must reload after restart"
    );
    let child_boundaries =
        harn_vm::agent_sessions::canonical_history_boundaries(&store, root.path(), "prefix-child")
            .await
            .expect("child acknowledgments");
    restarted
        .handle_session_fork(
            &serde_json::json!(4),
            &serde_json::json!({
                "sessionId": "prefix-child", "id": "grandchild", "canonicalBoundary": child_boundaries.positions[0].boundary,
            }),
        )
        .await;
    assert!(
        restarted.sessions.contains_key("grandchild"),
        "reloaded child must retain forkable context"
    );
    let restored = store
        .read("grandchild", ReadRange::default())
        .await
        .expect("grandchild context");
    assert_eq!(restored.events.len(), 1);
    assert_eq!(
        restored.events[0].payload["raw_message"]["content"],
        "first"
    );
    assert_eq!(
        store
            .describe("grandchild")
            .await
            .expect("grandchild")
            .parent_session_id
            .as_deref(),
        Some("prefix-child")
    );

    // A later replacement must not redefine the acknowledged historical prefix.
    store
        .append(
            parent,
            AppendEvent::new(
                SessionEventKind::Compaction,
                serde_json::json!({
                    "messages": [
                        {"role": "user", "content": "changed first"},
                        {"role": "user", "content": "second"},
                    ],
                    "summary": null,
                    "source_event_ids": ["canonical-message-0", "canonical-message-1"],
                }),
            ),
        )
        .await
        .expect("replacement");
    restarted
        .handle_session_load(
            &serde_json::json!(5),
            &serde_json::json!({"sessionId": parent, "cwd": root.path()}),
        )
        .await;
    restarted
        .handle_session_fork(
            &serde_json::json!(6),
            &serde_json::json!({"sessionId": parent, "id": "historical-prefix", "canonicalBoundary": first_boundary}),
        )
        .await;
    assert!(restarted.sessions.contains_key("historical-prefix"));
    let historical = harn_vm::llm::vm_value_to_json(
        &harn_vm::agent_sessions::transcript("historical-prefix").expect("historical context"),
    );
    assert_eq!(
        historical["messages"],
        serde_json::json!([{"role":"user", "content":"first"}])
    );
    let stored = store
        .read("historical-prefix", ReadRange::default())
        .await
        .expect("durable historical prefix");
    assert_eq!(stored.events.len(), 1);
    assert_eq!(stored.events[0].payload["raw_message"]["content"], "first");

    // Numeric collisions are not provenance: a foreign ID, wrong hash, or
    // observability domain must never create a live or durable child.
    restarted
        .handle_session_fork(
            &serde_json::json!("count-refused"),
            &serde_json::json!({"sessionId":parent, "id":"count-based-child", "keep_first":1}),
        )
        .await;
    assert!(!restarted.sessions.contains_key("count-based-child"));
    assert!(!harn_vm::agent_sessions::exists("count-based-child"));
    assert!(matches!(
        store.describe("count-based-child").await,
        Err(harn_session_store::StoreError::NotFound(_))
    ));
    for (name, invalid) in [
        (
            "foreign-boundary",
            serde_json::json!({"schema":first_boundary.schema, "session_id":"another-session", "event_id":first_boundary.event_id, "record_hash":first_boundary.record_hash}),
        ),
        (
            "stale-boundary",
            serde_json::json!({"schema":first_boundary.schema, "session_id":parent, "event_id":1, "record_hash":"sha256:not-the-acknowledged-row"}),
        ),
        (
            "topic-collision",
            serde_json::json!({"schema":"observability.agent_events", "session_id":parent, "event_id":1, "record_hash":first_boundary.record_hash}),
        ),
    ] {
        restarted
            .handle_session_fork(
                &serde_json::json!(7),
                &serde_json::json!({
                    "sessionId":parent, "id":name, "canonicalBoundary":invalid,
                }),
            )
            .await;
        assert!(
            !restarted.sessions.contains_key(name),
            "{name} must be refused"
        );
        assert!(!harn_vm::agent_sessions::exists(name));
        assert!(matches!(
            store.describe(name).await,
            Err(harn_session_store::StoreError::NotFound(_))
        ));
    }
}
