use super::*;
use harn_session_store::{
    AppendEvent, CreateSession, MemorySessionStore, SessionEventKind, SessionStore,
    SqliteSessionStore,
};
use serde_json::json;

fn custom(kind: &str) -> SessionEventKind {
    SessionEventKind::Custom {
        custom_type: kind.to_string(),
    }
}

fn identified(kind: SessionEventKind, payload: serde_json::Value) -> AppendEvent {
    let mut event = AppendEvent::new(kind, payload);
    event
        .headers
        .insert("run_id".to_string(), "run-1".to_string());
    event
        .headers
        .insert("turn_id".to_string(), "turn-1".to_string());
    event
}

fn transcript(
    kind: &str,
    role: &str,
    visibility: &str,
    text: &str,
    metadata: serde_json::Value,
) -> serde_json::Value {
    json!({
        "transcript_event": {
            "kind": kind,
            "role": role,
            "visibility": visibility,
            "text": text,
            "metadata": metadata,
        }
    })
}

fn user(text: &str) -> AppendEvent {
    identified(
        SessionEventKind::Message,
        transcript("message", "user", "public", text, json!({})),
    )
}

fn assistant(text: &str, visibility: &str) -> AppendEvent {
    identified(
        SessionEventKind::Message,
        transcript("message", "assistant", visibility, text, json!({})),
    )
}

fn checkpoint(iteration: i64, kind: &str) -> AppendEvent {
    identified(
        custom("loop_checkpoint"),
        transcript(
            "loop_checkpoint",
            "assistant",
            "internal",
            "",
            json!({"iteration": iteration, "kind": kind}),
        ),
    )
}

fn tool_call(id: &str) -> AppendEvent {
    let mut event = identified(
        SessionEventKind::ToolCall,
        transcript(
            "tool_call",
            "assistant",
            "internal",
            "",
            json!({
                "tool_call_id": id,
                "tool_name": "write_file",
                "input": {"path": "src/lib.rs", "authorization": "Bearer hidden-token"},
            }),
        ),
    );
    event
        .headers
        .insert("tool_call_id".to_string(), id.to_string());
    event
}

fn tool_result(id: &str) -> AppendEvent {
    let mut event = identified(
        SessionEventKind::ToolResult,
        json!({
            "transcript_event": {
                "kind": "tool_result",
                "role": "tool",
                "visibility": "internal",
                "text": "updated src/lib.rs",
                "metadata": {"tool_call_id": id, "is_error": false},
            },
            "raw_message": {
                "role": "tool_result",
                "content": "updated src/lib.rs",
                "is_error": false,
                "_harn": {
                    "kind": "tool_result",
                    "tool_call_id": id,
                    "tool_name": "write_file",
                    "outcome": "ok",
                    "data": {
                        "verification": {
                            "schema": "harn.agent_tool_postcondition.v1",
                            "status": "passed",
                            "verified_paths": ["src/lib.rs"],
                        }
                    }
                }
            }
        }),
    );
    event
        .headers
        .insert("tool_call_id".to_string(), id.to_string());
    event
}

fn plan() -> AppendEvent {
    let plan_event = crate::llm::plan::create_plan_document_event(
        json!({
            "_type": "plan_artifact",
            "schema_version": "harn.plan.v1",
            "id": "plan_recap",
            "tool": "update_plan",
            "title": "Ship recap",
            "summary": "Project durable facts",
            "steps": [{
                "id": "step_recap",
                "content": "Add deterministic projection",
                "status": "in_progress",
                "priority": null,
            }],
            "assumptions": [],
            "open_questions": [],
            "verification_commands": ["cargo test -p harn-vm session_recap"],
            "approval": {"state": "unrequested"},
        }),
        "agent",
        "test",
        "2026-08-26T00:00:00Z",
        "plan_event_recap",
    )
    .expect("create canonical plan event");
    let document = plan_event.document().clone();
    identified(
        SessionEventKind::Plan,
        transcript(
            "plan_document",
            "tool",
            "public",
            "",
            json!({
                "plan_document": document,
                "plan_document_event": plan_event,
            }),
        ),
    )
}

fn progress() -> AppendEvent {
    identified(
        custom("progress_reported"),
        transcript(
            "progress_reported",
            "assistant",
            "internal",
            "",
            json!({
                "message": "Implemented the projector with Bearer recap-progress-token",
                "entries": [{"content": "Add deterministic tests", "status": "in_progress"}],
                "replace": true,
                "metadata": {"private_reasoning": "must never surface"},
            }),
        ),
    )
}

fn terminal_secret() -> String {
    format!(
        "{}_{}",
        ["sk", "live"].join("_"),
        "abcdefghijklmnopqrstuvwxyz"
    )
}

fn terminal() -> AppendEvent {
    identified(
        custom("agent_run_terminal"),
        transcript(
            "agent_run_terminal",
            "assistant",
            "internal",
            "",
            json!({
                "final_status": "done",
                "stop_reason": "completed",
                "terminal": {
                    "kind": "natural",
                    "owner": "agent",
                    "reason": terminal_secret(),
                }
            }),
        ),
    )
}

async fn create_session(store: &dyn SessionStore, id: &str) {
    store
        .create(CreateSession {
            id: Some(id.to_string()),
            ..CreateSession::default()
        })
        .await
        .expect("create session");
}

async fn append_fixture(store: &dyn SessionStore, id: &str) {
    for event in [
        user("Implement durable recaps"),
        identified(
            SessionEventKind::Message,
            transcript(
                "message",
                "user",
                "private",
                "private injected user-role context",
                json!({}),
            ),
        ),
        identified(
            SessionEventKind::Message,
            transcript(
                "message",
                "user",
                "internal",
                "internal injected user-role context",
                json!({}),
            ),
        ),
        checkpoint(0, "iteration_start"),
        assistant("I implemented the projection.", "public"),
        assistant("private chain of thought", "private"),
        tool_call("tool-1"),
        tool_result("tool-1"),
        plan(),
        progress(),
        checkpoint(0, "iteration_end"),
        checkpoint(1, "iteration_start"),
        assistant("I verified the replay projection.", "public"),
        tool_call("tool-2"),
        tool_result("tool-2"),
        checkpoint(1, "iteration_end"),
        terminal(),
    ] {
        store.append(id, event).await.expect("append fixture event");
    }
}

#[tokio::test]
async fn recap_projects_non_vacuous_typed_turn_facts_without_private_content() {
    let store = MemorySessionStore::default();
    create_session(&store, "session-1").await;
    append_fixture(&store, "session-1").await;

    let recap = query_session_recap(&store, SessionRecapQuery::for_session("session-1"))
        .await
        .expect("query recap")
        .expect("session exists");

    assert_eq!(recap.coverage.scanned, 17);
    assert_eq!(recap.coverage.matched, 14);
    assert_eq!(recap.coverage.pending, 0);
    assert_eq!(recap.coverage.unassigned, 0);
    assert!(!recap.coverage.truncated);
    assert_eq!(recap.source.events.len(), 17);
    assert!(recap.content_hash.starts_with("sha256:"));
    assert!(recap.projection_hash.starts_with("sha256:"));

    let turn = &recap.turns[0];
    assert_eq!(turn.state, RecapCompletionState::Complete);
    assert_eq!(turn.prompts[0].text, "Implement durable recaps");
    assert_eq!(turn.iterations.len(), 2);
    let iteration = &turn.iterations[0];
    assert_eq!(iteration.iteration, Some(0));
    assert_eq!(iteration.state, RecapCompletionState::Complete);
    assert_eq!(iteration.assistant_text.len(), 1);
    assert_eq!(
        iteration.assistant_text[0].text,
        "I implemented the projection."
    );
    assert_eq!(iteration.tools.len(), 1);
    let tool = &iteration.tools[0];
    assert_eq!(tool.state, RecapToolState::Completed);
    assert!(tool.call_observed);
    assert!(tool.result_observed);
    assert_eq!(
        tool.verification
            .as_ref()
            .expect("typed verification")
            .status,
        "passed"
    );
    assert_eq!(iteration.plans.len(), 1);
    assert_eq!(iteration.progress.len(), 1);
    assert_eq!(iteration.progress[0].entries.len(), 1);
    let second_iteration = &turn.iterations[1];
    assert_eq!(second_iteration.iteration, Some(1));
    assert_eq!(second_iteration.state, RecapCompletionState::Complete);
    assert_eq!(second_iteration.tools.len(), 1);
    assert_eq!(second_iteration.tools[0].tool_call_id, "tool-2");
    assert_eq!(
        second_iteration.assistant_text[0].text,
        "I verified the replay projection."
    );

    let rendered = serde_json::to_string(&recap).expect("serialize recap");
    assert!(!rendered.contains("private injected user-role context"));
    assert!(!rendered.contains("internal injected user-role context"));
    assert!(!rendered.contains("private chain of thought"));
    assert!(!rendered.contains("must never surface"));
    assert!(!rendered.contains("Bearer recap-progress-token"));
    assert!(!rendered.contains(&terminal_secret()));
    assert!(!rendered.contains("Bearer hidden-token"));
    assert!(rendered.contains("[redacted]"));
}

#[tokio::test]
async fn missing_empty_and_incomplete_states_remain_distinct() {
    let store = MemorySessionStore::default();
    assert!(
        query_session_recap(&store, SessionRecapQuery::for_session("missing"))
            .await
            .expect("query missing")
            .is_none()
    );

    create_session(&store, "empty").await;
    let empty = query_session_recap(&store, SessionRecapQuery::for_session("empty"))
        .await
        .expect("query empty")
        .expect("empty exists");
    assert_eq!(empty.coverage.scanned, 0);
    assert!(empty.turns.is_empty());

    create_session(&store, "incomplete").await;
    for event in [
        user("Run the tests"),
        checkpoint(0, "iteration_start"),
        assistant("Tests passed", "public"),
        tool_call("tool-open"),
    ] {
        store
            .append("incomplete", event)
            .await
            .expect("append incomplete event");
    }
    let incomplete = query_session_recap(&store, SessionRecapQuery::for_session("incomplete"))
        .await
        .expect("query incomplete")
        .expect("incomplete exists");
    assert_eq!(incomplete.turns[0].state, RecapCompletionState::Incomplete);
    assert_eq!(
        incomplete.turns[0].iterations[0].state,
        RecapCompletionState::Incomplete
    );
    assert_eq!(
        incomplete.turns[0].iterations[0].tools[0].state,
        RecapToolState::Incomplete
    );
    assert!(incomplete.turns[0].terminal.is_none());
    assert!(incomplete.turns[0].iterations[0].tools[0]
        .verification
        .is_none());
}

#[tokio::test]
async fn recognized_fact_without_turn_identity_is_counted_as_unassigned() {
    let store = MemorySessionStore::default();
    create_session(&store, "unassigned").await;
    store
        .append(
            "unassigned",
            AppendEvent::new(
                SessionEventKind::Message,
                transcript("message", "user", "public", "Unplaced prompt", json!({})),
            ),
        )
        .await
        .expect("append unassigned fact");

    let recap = query_session_recap(
        &store,
        SessionRecapQuery {
            run_id: Some("run-1".to_string()),
            ..SessionRecapQuery::for_session("unassigned")
        },
    )
    .await
    .expect("query unassigned recap")
    .expect("unassigned session exists");
    assert_eq!(recap.coverage.scanned, 1);
    assert_eq!(recap.coverage.matched, 0);
    assert_eq!(recap.coverage.unassigned, 1);
    assert!(recap.turns.is_empty());
}

#[tokio::test]
async fn bounded_query_reports_pending_rows_and_an_exact_next_cursor() {
    let store = MemorySessionStore::default();
    create_session(&store, "bounded").await;
    append_fixture(&store, "bounded").await;
    let recap = query_session_recap(
        &store,
        SessionRecapQuery {
            limit: Some(3),
            ..SessionRecapQuery::for_session("bounded")
        },
    )
    .await
    .expect("query bounded recap")
    .expect("bounded session exists");
    assert_eq!(recap.coverage.scanned, 3);
    assert_eq!(recap.coverage.pending, 14);
    assert!(recap.coverage.truncated);
    assert_eq!(recap.cursor.last_event_id, Some(3));
    assert_eq!(recap.cursor.next_event_id, Some(4));
}

#[tokio::test]
async fn sqlite_restart_replay_is_byte_stable_and_provider_free() {
    let temp = tempfile::tempdir().expect("session root");
    let path = temp.path().join("session-store.sqlite");
    {
        let store = SqliteSessionStore::open(&path).expect("open first store");
        create_session(&store, "restart").await;
        append_fixture(&store, "restart").await;
        let first = query_session_recap(&store, SessionRecapQuery::for_session("restart"))
            .await
            .expect("first query")
            .expect("restart session");
        let first_bytes = crate::canonical_json::of(&first).expect("canonical first recap");
        drop(store);

        let reopened = SqliteSessionStore::open(&path).expect("reopen store");
        let replay = query_session_recap(&reopened, SessionRecapQuery::for_session("restart"))
            .await
            .expect("replay query")
            .expect("restart session after reopen");
        let replay_bytes = crate::canonical_json::of(&replay).expect("canonical replay recap");
        assert_eq!(first_bytes, replay_bytes);
        assert_eq!(first.content_hash, replay.content_hash);
        assert_eq!(first.projection_hash, replay.projection_hash);
    }
}

#[tokio::test]
async fn changing_one_source_payload_changes_both_projected_hashes() {
    let temp = tempfile::tempdir().expect("session root");
    let path = temp.path().join("session-store.sqlite");
    let store = SqliteSessionStore::open(&path).expect("open source store");
    create_session(&store, "content-bound").await;
    let stored = store
        .append("content-bound", user("Original prompt"))
        .await
        .expect("append original source event");
    let original = query_session_recap(&store, SessionRecapQuery::for_session("content-bound"))
        .await
        .expect("query original recap")
        .expect("source session exists");
    drop(store);

    let mut changed = stored;
    changed.payload = transcript("message", "user", "public", "Changed prompt", json!({}));
    changed.record_hash = harn_session_store::compute_record_hash(&changed);
    let changed_root = harn_session_store::chain_root_hash(std::slice::from_ref(&changed));
    let connection = rusqlite::Connection::open(&path).expect("open source database");
    connection
        .execute(
            "UPDATE session_events
             SET payload_json = ?1, record_hash = ?2
             WHERE session_id = ?3 AND event_id = ?4",
            rusqlite::params![
                serde_json::to_string(&changed.payload).expect("serialize changed payload"),
                changed.record_hash,
                changed.session_id,
                i64::try_from(changed.event_id).expect("fixture event id fits SQLite INTEGER"),
            ],
        )
        .expect("replace the source payload in the negative-control fixture");
    connection
        .execute(
            "UPDATE sessions SET chain_root_hash = ?1 WHERE id = ?2",
            rusqlite::params![changed_root, "content-bound"],
        )
        .expect("keep the negative-control source chain valid");
    drop(connection);

    let reopened = SqliteSessionStore::open(&path).expect("reopen changed store");
    let projected = query_session_recap(&reopened, SessionRecapQuery::for_session("content-bound"))
        .await
        .expect("query changed recap")
        .expect("changed source session exists");

    assert_eq!(original.coverage.scanned, 1);
    assert_eq!(projected.coverage.scanned, 1);
    assert_eq!(
        original.source.first_event_id,
        projected.source.first_event_id
    );
    assert_eq!(
        original.source.last_event_id,
        projected.source.last_event_id
    );
    assert_eq!(original.turns[0].prompts[0].text, "Original prompt");
    assert_eq!(projected.turns[0].prompts[0].text, "Changed prompt");
    assert_ne!(original.content_hash, projected.content_hash);
    assert_ne!(original.projection_hash, projected.projection_hash);
}

#[test]
fn terminal_availability_keeps_each_unavailable_cause_explicit() {
    let reasons = [
        SessionRecapUnavailableReason::JournalUnavailable,
        SessionRecapUnavailableReason::SessionMissing,
        SessionRecapUnavailableReason::ProjectionFailed,
        SessionRecapUnavailableReason::AdmissionTerminal,
    ];
    let rendered = reasons
        .into_iter()
        .map(|reason| {
            serde_json::to_value(SessionRecapAvailability::unavailable(reason))
                .expect("serialize unavailable recap")
        })
        .collect::<Vec<_>>();

    assert_eq!(
        rendered[0],
        json!({"state": "unavailable", "reason": "journal_unavailable"})
    );
    assert_eq!(
        rendered[1],
        json!({"state": "unavailable", "reason": "session_missing"})
    );
    assert_eq!(
        rendered[2],
        json!({"state": "unavailable", "reason": "projection_failed"})
    );
    assert_eq!(
        rendered[3],
        json!({"state": "unavailable", "reason": "admission_terminal"})
    );
}

#[test]
fn terminal_available_snapshot_round_trips_without_wire_indirection() {
    let snapshot = SessionRecapSnapshot {
        schema_version: SESSION_RECAP_SCHEMA_VERSION,
        session_id: "session-wire".to_string(),
        query: SessionRecapQuery::for_session("session-wire"),
        cursor: SessionRecapCursor::default(),
        coverage: SessionRecapCoverage::default(),
        source: SessionRecapSource::default(),
        content_hash: "sha256:content".to_string(),
        projection_hash: "sha256:projection".to_string(),
        turns: Vec::new(),
        extensions: BTreeMap::new(),
    };
    let wire = serde_json::to_value(SessionRecapAvailability::available(snapshot.clone()))
        .expect("serialize available recap");

    assert_eq!(wire["state"], json!("available"));
    assert_eq!(wire["snapshot"]["sessionId"], json!("session-wire"));
    assert!(wire.get("box").is_none());

    let decoded: SessionRecapAvailability =
        serde_json::from_value(wire).expect("deserialize available recap");
    assert_eq!(
        decoded,
        SessionRecapAvailability::Available {
            snapshot: Box::new(snapshot),
        }
    );
}
