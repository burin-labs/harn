use harn_session_store::{
    AppendEvent, CreateSession, SessionEventKind, SessionStore, SqliteSessionStore,
};

use super::*;

#[tokio::test]
async fn internal_turn_phases_restore_candidate_and_finality_in_order() {
    use crate::agent_events::{AgentTerminalKind, AgentTerminalOutcome, AgentTurnPhase};
    let session_id = "turn-phase-replay";
    let store = store_with_session(session_id).await;
    let phases = [
        AgentTurnPhase::Generating,
        AgentTurnPhase::Verifying {
            candidate_reply: "candidate".into(),
        },
        AgentTurnPhase::Terminal {
            reply: "accepted".into(),
            outcome: Box::new(AgentTerminalOutcome::new(
                AgentTerminalKind::Natural,
                "done",
            )),
        },
    ];
    for phase in &phases {
        let event = AgentEvent::TurnPhaseChanged {
            session_id: session_id.into(),
            phase: phase.clone(),
        };
        let payload = serde_json::json!({"transcript_event": {
            "kind": "turn_phase_changed", "role": "assistant", "visibility": "internal",
            "text": "", "metadata": serde_json::to_value(event).unwrap(),
        }});
        store
            .append(
                session_id,
                AppendEvent::new(
                    SessionEventKind::Custom {
                        custom_type: "turn_phase_changed".into(),
                    },
                    payload,
                ),
            )
            .await
            .expect("append phase");
    }
    let provisional = load_canonical_session_replay_from_store(&store, session_id)
        .await
        .unwrap()
        .expect("known session")
        .events;
    assert_eq!(
        provisional.len(),
        2,
        "a phase row alone cannot commit finality"
    );
    assert!(matches!(
        provisional[1].event,
        AgentEvent::TurnPhaseChanged {
            phase: AgentTurnPhase::Verifying { .. },
            ..
        }
    ));
    let AgentTurnPhase::Terminal { reply, outcome } = &phases[2] else {
        unreachable!("terminal fixture")
    };
    store
        .append(
            session_id,
            AppendEvent::new(
                SessionEventKind::Custom {
                    custom_type: "agent_run_terminal".into(),
                },
                serde_json::json!({"transcript_event": {
                    "kind": "agent_run_terminal", "visibility": "internal", "text": "",
                    "metadata": {"visible_reply": reply, "terminal": outcome},
                }}),
            ),
        )
        .await
        .expect("commit terminal run record");
    let restored = load_canonical_session_replay_from_store(&store, session_id)
        .await
        .unwrap()
        .expect("known session")
        .events;
    assert_eq!(
        restored.len(),
        3,
        "internal phases must not disappear on reconnect"
    );
    for (event, expected) in restored.iter().zip(phases) {
        match &event.event {
            AgentEvent::TurnPhaseChanged { phase, .. } => assert_eq!(phase, &expected),
            other => panic!("expected phase, got {other:?}"),
        }
    }
}

fn transcript_row(kind: &str, role: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "transcript_event": {
            "id": format!("{role}-{text}"),
            "kind": kind,
            "role": role,
            "visibility": "public",
            "text": text,
        }
    })
}

async fn store_with_session(session_id: &str) -> SqliteSessionStore {
    let store = SqliteSessionStore::open_in_memory().expect("in-memory canonical store");
    store
        .create(CreateSession {
            id: Some(session_id.to_string()),
            ..CreateSession::default()
        })
        .await
        .expect("create session");
    store
}

#[tokio::test]
async fn captured_checkpoint_rejects_equal_tip_and_count_with_replaced_history() {
    let session_id = "replaced-restore-prefix";
    let store = store_with_session(session_id).await;
    for text in ["original first", "original second"] {
        store
            .append(
                session_id,
                AppendEvent::new(
                    SessionEventKind::Message,
                    transcript_row("message", "user", text),
                ),
            )
            .await
            .unwrap();
    }
    let captured = store.describe(session_id).await.unwrap();
    store.truncate(session_id, 1).await.unwrap();
    store
        .append(
            session_id,
            AppendEvent::new(
                SessionEventKind::Message,
                transcript_row("message", "user", "replacement second"),
            ),
        )
        .await
        .unwrap();
    let replaced = store.describe(session_id).await.unwrap();
    assert_eq!(captured.event_count, replaced.event_count);
    assert_eq!(captured.last_event_id, replaced.last_event_id);
    assert_ne!(captured.chain_root_hash, replaced.chain_root_hash);
    assert!(read_canonical_session_prefix(&store, session_id, captured)
        .await
        .is_err());
}

#[tokio::test]
async fn captured_checkpoint_accepts_later_append_and_keeps_nonvisible_tip() {
    let session_id = "bounded-restore-prefix";
    let store = store_with_session(session_id).await;
    store
        .append(
            session_id,
            AppendEvent::new(
                SessionEventKind::Message,
                transcript_row("message", "user", "captured message"),
            ),
        )
        .await
        .unwrap();
    store
        .append(
            session_id,
            AppendEvent::new(
                SessionEventKind::Receipt,
                serde_json::json!({"audit": true}),
            ),
        )
        .await
        .unwrap();
    let captured = store.describe(session_id).await.unwrap();
    store
        .append(
            session_id,
            AppendEvent::new(
                SessionEventKind::Message,
                transcript_row("message", "user", "later append"),
            ),
        )
        .await
        .unwrap();
    let replay = read_canonical_session_prefix(&store, session_id, captured)
        .await
        .unwrap();
    assert_eq!(replay.last_event_id, Some(2));
    assert_eq!(replay.events.len(), 1);
    assert_eq!(replay.events[0].event_id, 1);
}

#[tokio::test]
async fn captured_empty_checkpoint_does_not_read_later_events() {
    let session_id = "empty-captured-prefix";
    let store = store_with_session(session_id).await;
    let captured = store.describe(session_id).await.unwrap();
    store
        .append(
            session_id,
            AppendEvent::new(
                SessionEventKind::Message,
                transcript_row("message", "user", "after capture"),
            ),
        )
        .await
        .unwrap();
    let replay = read_canonical_session_prefix(&store, session_id, captured)
        .await
        .unwrap();
    assert_eq!(replay.last_event_id, None);
    assert!(replay.events.is_empty());
}

#[tokio::test]
async fn captured_checkpoint_validates_across_multiple_bounded_pages() {
    let session_id = "paged-captured-prefix";
    let store = store_with_session(session_id).await;
    for index in 0..RESTORE_PAGE + 2 {
        store
            .append(
                session_id,
                AppendEvent::new(
                    SessionEventKind::Message,
                    transcript_row("message", "user", &format!("captured {index}")),
                ),
            )
            .await
            .unwrap();
    }
    let captured = store.describe(session_id).await.unwrap();
    let replay = read_canonical_session_prefix(&store, session_id, captured)
        .await
        .unwrap();
    assert_eq!(replay.events.len(), RESTORE_PAGE + 2);
    assert_eq!(replay.last_event_id, Some((RESTORE_PAGE + 2) as u64));
    assert_eq!(replay.events.first().unwrap().event_id, 1);
    assert_eq!(
        replay.events.last().unwrap().event_id,
        (RESTORE_PAGE + 2) as u64
    );
}

#[tokio::test]
async fn captured_checkpoint_refuses_truncated_prefix() {
    let session_id = "truncated-captured-prefix";
    let store = store_with_session(session_id).await;
    for text in ["first", "second"] {
        store
            .append(
                session_id,
                AppendEvent::new(
                    SessionEventKind::Message,
                    transcript_row("message", "user", text),
                ),
            )
            .await
            .unwrap();
    }
    let captured = store.describe(session_id).await.unwrap();
    store.truncate(session_id, 1).await.unwrap();
    assert!(read_canonical_session_prefix(&store, session_id, captured)
        .await
        .is_err());
}

#[tokio::test]
async fn internal_progress_replays_through_the_typed_host_event_owner() {
    let session_id = "durable-progress-replay";
    let store = store_with_session(session_id).await;
    store
        .append(
            session_id,
            AppendEvent::new(
                SessionEventKind::Message,
                serde_json::json!({"transcript_event": {
                    "kind": "progress_reported", "role": "assistant", "visibility": "internal",
                    "metadata": {"message": "Running verification", "entries": [], "replace": true},
                }}),
            ),
        )
        .await
        .unwrap();
    let replay = load_canonical_session_replay_from_store(&store, session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replay.last_event_id, Some(1));
    assert_eq!(replay.events.len(), 1);
    match &replay.events[0].event {
        AgentEvent::ProgressReported {
            message, replace, ..
        } => {
            assert_eq!(message.as_deref(), Some("Running verification"));
            assert!(*replace);
        }
        other => panic!("expected typed progress, got {other:?}"),
    }
}

/// The defect this module exists to close (burin#6267): a session the canonical
/// store holds must be restorable, whether or not the observability event log
/// ever observed it.
#[tokio::test]
async fn a_stored_session_restores_its_public_transcript() {
    let session_id = "01a003d0-1513-7271-90aa-4542d6059498";
    let store = store_with_session(session_id).await;
    for (kind, role, text) in [
        ("message", "user", "add a multiply function"),
        ("message", "assistant", "done, tests pass"),
    ] {
        store
            .append(
                session_id,
                AppendEvent::new(SessionEventKind::Message, transcript_row(kind, role, text))
                    .with_actor(role),
            )
            .await
            .expect("append transcript row");
    }

    let restored = load_canonical_session_replay_from_store(&store, session_id)
        .await
        .expect("restore should not error")
        .expect("the store knows this session")
        .events;

    let rendered: Vec<String> = restored
        .iter()
        .map(|entry| match &entry.event {
            AgentEvent::UserMessage { content, .. } => format!("user: {content:?}"),
            AgentEvent::AgentMessageChunk { content, .. } => format!("assistant: {content}"),
            other => format!("other: {other:?}"),
        })
        .collect();
    assert_eq!(restored.len(), 2, "both rows replay: {rendered:?}");
    assert!(
        rendered[0].contains("add a multiply function"),
        "the user turn restores in order: {rendered:?}"
    );
    assert!(
        rendered[1].starts_with("assistant: done, tests pass"),
        "the assistant turn restores in order: {rendered:?}"
    );
    assert!(
        restored[0].event_id < restored[1].event_id,
        "replay keeps stored order: {rendered:?}"
    );
}

/// An id no store holds is the one case that should still fail loudly, so a
/// typo or a stale id stays distinguishable from a real restore.
#[tokio::test]
async fn an_unknown_id_reports_no_session_rather_than_an_empty_one() {
    let store = store_with_session("known-session").await;
    let restored = load_canonical_session_replay_from_store(&store, "never-existed")
        .await
        .expect("a missing session is not an error");
    assert!(
        restored.is_none(),
        "an id the store does not hold must be reported absent, not empty"
    );
}

/// A freshly created session with no turns yet is a real session. Reporting it
/// unknown is what made every zero-event launch row unopenable.
#[tokio::test]
async fn a_session_with_no_transcript_yet_is_still_restorable() {
    let store = store_with_session("brand-new").await;
    let restored = load_canonical_session_replay_from_store(&store, "brand-new")
        .await
        .expect("restore should not error");
    assert_eq!(
        restored.map(|replay| replay.events.len()),
        Some(0),
        "an empty session restores as an empty transcript, not as absent"
    );
}

/// The journal persists a tool lifecycle event with its identity under
/// `metadata`, and stamps the call id into a header. Replay must restore the
/// tool's real name from that shape, not a generic placeholder.
#[tokio::test]
async fn a_replayed_tool_call_keeps_its_metadata_tool_name() {
    let session_id = "with-tool-call";
    let store = store_with_session(session_id).await;
    let mut event = AppendEvent::new(
        SessionEventKind::ToolCall,
        serde_json::json!({
            "transcript_event": {
                "id": "event-call",
                "kind": "tool_call",
                "role": "assistant",
                "visibility": "internal",
                "metadata": {"tool_call_id": "tool-1", "tool_name": "look"},
            }
        }),
    );
    event
        .headers
        .insert("tool_call_id".to_string(), "tool-1".to_string());
    store
        .append(session_id, event)
        .await
        .expect("append tool call row");

    let restored = load_canonical_session_replay_from_store(&store, session_id)
        .await
        .expect("restore should not error")
        .expect("the store knows this session")
        .events;
    assert_eq!(
        restored.len(),
        2,
        "the tool call and its close replay: {restored:?}"
    );
    match &restored[0].event {
        AgentEvent::ToolCall {
            tool_call_id,
            tool_name,
            ..
        } => {
            assert_eq!(tool_call_id, "tool-1");
            assert_eq!(tool_name, "look");
        }
        other => panic!("expected a replayed tool call, got {other:?}"),
    }
}

/// The assistant turn's own tool-call row names the call only inside its
/// provider message. A call cut off before its result has no later row to
/// rename it, so replay must read that name or the client shows "tool".
#[tokio::test]
async fn an_assistant_tool_call_row_replays_with_its_provider_name() {
    let session_id = "with-cut-off-call";
    let store = store_with_session(session_id).await;
    let mut event = AppendEvent::new(
        SessionEventKind::ToolCall,
        serde_json::json!({
            "transcript_event": {
                "id": "event-assistant",
                "kind": "message",
                "role": "assistant",
                "visibility": "public",
                "text": "",
            },
            "raw_message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    {"id": "call-other", "name": "look", "arguments": {}},
                    {"id": "call-cut", "name": "wait_command", "arguments": {"handle_id": "h-1"}},
                ],
            },
        }),
    );
    event
        .headers
        .insert("tool_call_id".to_string(), "call-cut".to_string());
    store
        .append(session_id, event)
        .await
        .expect("append assistant tool call row");

    let restored = load_canonical_session_replay_from_store(&store, session_id)
        .await
        .expect("restore should not error")
        .expect("the store knows this session")
        .events;
    match &restored[..] {
        [call, close] => {
            match &call.event {
                AgentEvent::ToolCall {
                    tool_call_id,
                    tool_name,
                    raw_input,
                    ..
                } => {
                    assert_eq!(tool_call_id, "call-cut");
                    assert_eq!(tool_name, "wait_command");
                    assert_eq!(raw_input, &serde_json::json!({"handle_id": "h-1"}));
                }
                other => panic!("expected a replayed tool call, got {other:?}"),
            }
            match &close.event {
                AgentEvent::ToolCallUpdate {
                    tool_call_id,
                    tool_name,
                    status,
                    ..
                } => {
                    assert_eq!(tool_call_id, "call-cut");
                    assert_eq!(tool_name, "wait_command");
                    assert_eq!(*status, ToolCallStatus::Failed);
                }
                other => panic!("expected the call to close as failed, got {other:?}"),
            }
        }
        other => panic!("expected the call and its close, got {other:?}"),
    }
}

/// Internal bookkeeping rows (usage checkpoints, audit annotations) are not
/// conversation, and must not surface in a restored transcript.
#[tokio::test]
async fn internal_rows_stay_out_of_the_restored_transcript() {
    let session_id = "with-internals";
    let store = store_with_session(session_id).await;
    store
        .append(
            session_id,
            AppendEvent::new(
                SessionEventKind::Message,
                serde_json::json!({
                    "transcript_event": {
                        "kind": "message",
                        "role": "assistant",
                        "visibility": "internal",
                        "text": "scratch reasoning",
                    }
                }),
            ),
        )
        .await
        .expect("append internal row");
    store
        .append(
            session_id,
            AppendEvent::new(
                SessionEventKind::Custom {
                    custom_type: "usage_checkpoint".to_string(),
                },
                serde_json::json!({"usage": {"input_tokens": 72}}),
            ),
        )
        .await
        .expect("append usage row");
    store
        .append(
            session_id,
            AppendEvent::new(
                SessionEventKind::Message,
                transcript_row("message", "assistant", "here is the answer"),
            ),
        )
        .await
        .expect("append public row");

    let restored = load_canonical_session_replay_from_store(&store, session_id)
        .await
        .expect("restore should not error")
        .expect("the store knows this session")
        .events;
    assert_eq!(
        restored.len(),
        1,
        "only the public turn replays: {restored:?}"
    );
    assert!(matches!(
        &restored[0].event,
        AgentEvent::AgentMessageChunk { content, .. } if content == "here is the answer"
    ));
}

/// The journal writes every tool call and tool result as an internal row, with
/// the name under `metadata` on the call and on the stored provider message
/// beside the result. This is that shape, copied from a real store. Replay
/// dropped all four rows as internal bookkeeping, so a resumed session showed
/// no tool rows at all (harn#8920). Internal prose still stays hidden.
#[tokio::test]
async fn journal_shaped_tool_rows_replay_and_internal_prose_stays_hidden() {
    let session_id = "journal-shaped";
    let store = store_with_session(session_id).await;
    let call = |id: &str, file: &str| {
        let mut event = AppendEvent::new(
            SessionEventKind::ToolCall,
            serde_json::json!({
                "transcript_event": {
                    "id": format!("event-{id}"),
                    "kind": "tool_call",
                    "role": "assistant",
                    "visibility": "internal",
                    "text": "",
                    "metadata": {
                        "raw_input": {"file": file},
                        "status": "pending",
                        "tool_call_id": id,
                        "tool_name": "look",
                    },
                }
            }),
        );
        event
            .headers
            .insert("tool_call_id".to_string(), id.to_string());
        event
    };
    let result = |id: &str, text: &str, is_error: bool| {
        let mut event = AppendEvent::new(
            SessionEventKind::ToolResult,
            serde_json::json!({
                "transcript_event": {
                    "id": format!("result-{id}"),
                    "kind": "tool_result",
                    "role": "tool_result",
                    "visibility": "internal",
                    "text": text,
                },
                "raw_message": {
                    "content": text,
                    "is_error": is_error,
                    "name": "look",
                    "role": "tool_result",
                    "tool_call_id": id,
                },
            }),
        );
        event
            .headers
            .insert("tool_call_id".to_string(), id.to_string());
        event
    };
    let internal_prose = AppendEvent::new(
        SessionEventKind::Message,
        serde_json::json!({
            "transcript_event": {
                "kind": "message",
                "role": "user",
                "visibility": "internal",
                "text": "<context-directives speaker=\"harness\">",
            }
        }),
    );
    for event in [
        internal_prose,
        call("call-1", "calc.py"),
        call("call-2", "gone.py"),
        result("call-1", "calc.py (3 lines)", false),
        result("call-2", "no such file", true),
    ] {
        store.append(session_id, event).await.expect("append row");
    }

    let restored = load_canonical_session_replay_from_store(&store, session_id)
        .await
        .expect("restore should not error")
        .expect("the store knows this session")
        .events;
    let rows: Vec<String> = restored
        .iter()
        .map(|entry| match &entry.event {
            AgentEvent::ToolCall {
                tool_call_id,
                tool_name,
                raw_input,
                ..
            } => format!("call {tool_call_id} {tool_name} {raw_input}"),
            AgentEvent::ToolCallUpdate {
                tool_call_id,
                tool_name,
                status,
                ..
            } => format!("result {tool_call_id} {tool_name} {status:?}"),
            other => format!("other {other:?}"),
        })
        .collect();
    assert_eq!(
        rows,
        [
            r#"call call-1 look {"file":"calc.py"}"#,
            r#"call call-2 look {"file":"gone.py"}"#,
            "result call-1 look Completed",
            "result call-2 look Failed",
        ],
        "every stored tool row replays, named and with its input; internal prose does not"
    );
}

/// A rejected edit is not a dispatch failure: the producer returns an ordinary
/// envelope whose typed facts say nothing was written, so the provider
/// message's `is_error` is false. The live path projects those facts onto the
/// tool update; replay flattened them away, so a resumed session showed the
/// rejection with no mutation outcome and no data for a client to read.
#[tokio::test]
async fn a_replayed_not_applied_edit_keeps_its_typed_result_facts() {
    let session_id = "rejected-edit";
    let store = store_with_session(session_id).await;
    let text = "Error: content_hash is required for this text-anchored edit to calc.py.";
    let data = serde_json::json!({
        "mutation_status": "not_applied",
        "edit_outcome": {"edit_status": "rejected_not_applied", "mutation_status": "not_applied"},
    });
    let result = |id: &str, is_error: bool, outcome: &str, data: serde_json::Value| {
        let mut event = AppendEvent::new(
            SessionEventKind::ToolResult,
            serde_json::json!({
                "transcript_event": {
                    "id": format!("result-{id}"),
                    "kind": "tool_result",
                    "role": "tool_result",
                    "visibility": "internal",
                    "text": text,
                },
                "raw_message": {
                    "content": text,
                    "is_error": is_error,
                    "name": "edit",
                    "role": "tool_result",
                    "tool_call_id": id,
                    "_harn": {
                        "kind": "tool_result",
                        "tool_call_id": id,
                        "tool_name": "edit",
                        "outcome": outcome,
                        "origin": "dispatched",
                        "data": data,
                    },
                },
            }),
        );
        event
            .headers
            .insert("tool_call_id".to_string(), id.to_string());
        event
    };
    let audit_marked_error = {
        let mut event = result("edit-2", false, "ok", serde_json::Value::Null);
        event.payload["transcript_event"]["metadata"] = serde_json::json!({"is_error": true});
        event
    };
    for event in [
        result("edit-1", false, "ok", data.clone()),
        audit_marked_error,
    ] {
        store.append(session_id, event).await.expect("append row");
    }

    let restored = load_canonical_session_replay_from_store(&store, session_id)
        .await
        .expect("restore should not error")
        .expect("the store knows this session")
        .events;
    let updates: Vec<_> = restored
        .iter()
        .map(|entry| match &entry.event {
            AgentEvent::ToolCallUpdate {
                status,
                mutation_status,
                data,
                error,
                ..
            } => (*status, *mutation_status, data.clone(), error.is_some()),
            other => panic!("expected a replayed tool result, got {other:?}"),
        })
        .collect();
    assert_eq!(
        updates,
        [
            (
                ToolCallStatus::Completed,
                ToolMutationStatus::NotApplied,
                Some(data),
                false
            ),
            (
                ToolCallStatus::Failed,
                ToolMutationStatus::Unknown,
                None,
                true
            ),
        ],
        "replay restores the typed outcome the live path emitted"
    );
}
