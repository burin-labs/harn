//! Fork a cold canonical parent before its first prompt, then reload the child.

use super::*;
use harn_session_store::{AppendEvent, CreateSession, ReadRange, SessionEventKind, SessionStore};

#[tokio::test(flavor = "current_thread")]
async fn cold_parent_forks_persist_selected_context_and_lineage_before_prompt() {
    use super::session_environment::{child_sees_restore_canary, RESTORE_CANARY};

    let _lock = acp_env_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _environment = EnvSnapshot::capture(&[RESTORE_CANARY]);
    std::env::set_var(RESTORE_CANARY, "synthetic-not-a-credential");
    assert!(child_sees_restore_canary(
        &harn_vm::security::SessionEnvironment::inherited()
    ));
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
                "environmentPolicy": {"kind": "isolated", "grants": []},
            }),
        )
        .await;
    assert!(
        server.sessions.contains_key(parent),
        "cold parent must actually load"
    );
    for (count, child) in [(2, "prefix-child"), (0, "empty-child")] {
        server
            .handle_session_fork(
                &serde_json::json!(2),
                &serde_json::json!({
                    "sessionId": parent, "id": child, "keep_first": count,
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
                "environmentPolicy": {"kind": "isolated", "grants": []},
            }),
        )
        .await;
    assert!(
        restarted.sessions.contains_key("prefix-child"),
        "durable child must reload after restart"
    );
    assert!(!child_sees_restore_canary(
        &restarted.sessions["prefix-child"].environment_policy
    ));
    restarted
        .handle_session_fork(
            &serde_json::json!(4),
            &serde_json::json!({
                "sessionId": "prefix-child", "id": "grandchild", "keep_first": 1,
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

    // A replacement preserves identities but changes the context those old
    // event boundaries would restore. Refuse that prefix instead of silently
    // creating a child that changes its context on the next restart.
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
            &serde_json::json!({
                "sessionId": parent, "cwd": root.path(),
                "environmentPolicy": {"kind": "isolated", "grants": []},
            }),
        )
        .await;
    restarted
        .handle_session_fork(
            &serde_json::json!(6),
            &serde_json::json!({"sessionId": parent, "id": "changed-prefix", "keep_first": 1}),
        )
        .await;
    assert!(!restarted.sessions.contains_key("changed-prefix"));
    assert!(!harn_vm::agent_sessions::exists("changed-prefix"));
    assert!(matches!(
        store.describe("changed-prefix").await,
        Err(harn_session_store::StoreError::NotFound(_))
    ));
}
