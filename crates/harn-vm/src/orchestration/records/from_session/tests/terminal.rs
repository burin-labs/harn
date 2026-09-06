//! Terminal, liveness, and run-clock projection.
//!
//! These cases read one axis: what a session's terminal state, its writer
//! liveness, and its clock project onto the run record. They share the event
//! builders in the parent module.

use super::*;
use crate::orchestration::records::persistence as records_persistence;
use crate::orchestration::records::time as records_time;

#[tokio::test]
async fn a_loop_that_ended_in_error_projects_as_a_failed_run() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    store
        .append(&meta.id, terminal("error", "tool_failure"))
        .await
        .expect("append");

    let run = project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project");
    assert_eq!(run.status, "failed");
}

#[tokio::test]
async fn typed_terminal_projects_suspension_and_cancellation_without_reclassification() {
    for (final_status, stop_reason, expected_status, expected_kind) in [
        ("suspended", "awaiting_ci", "suspended", "suspended"),
        ("cancelled", "user_cancelled", "cancelled", "user_cancelled"),
        (
            "completion_unverified",
            "completion_unverified",
            "failed",
            "completion_unverified",
        ),
    ] {
        let store = MemorySessionStore::default();
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create session");
        store
            .append(&meta.id, terminal(final_status, stop_reason))
            .await
            .expect("append terminal");

        let run = project_run_record_from_session(&store, &meta.id)
            .await
            .expect("project");
        assert_eq!(run.status, expected_status);
        assert_eq!(run.metadata["terminal"]["kind"], expected_kind);
    }
}

#[tokio::test]
async fn typed_terminal_reason_wins_over_the_legacy_stop_reason() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    store
        .append(
            &meta.id,
            AppendEvent::new(
                custom("agent_run_terminal"),
                transcript_event(
                    "agent_run_terminal",
                    json!({
                        "final_status": "done",
                        "stop_reason": "legacy_summary",
                        "terminal": {
                            "kind": "natural",
                            "owner": "agent",
                            "reason": "model_signalled_completion",
                        },
                    }),
                ),
            ),
        )
        .await
        .expect("append terminal");

    let run = project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project");
    assert_eq!(
        run.metadata["terminal"]["reason"],
        "model_signalled_completion"
    );
    assert_eq!(run.metadata["stop_reason"], "legacy_summary");
}

#[tokio::test]
async fn journals_from_before_typed_terminals_keep_the_legacy_status_fallback() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    store
        .append(&meta.id, legacy_terminal("done", "pace_cutoff"))
        .await
        .expect("append legacy terminal");

    let run = project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project legacy journal");
    assert_eq!(run.status, "completed");
    assert!(!run.metadata.contains_key("terminal"));
}

#[tokio::test]
async fn a_still_running_session_projects_as_running_rather_than_complete() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    store
        .append(&meta.id, iteration_start(1))
        .await
        .expect("append");

    let run = project_run_record_from_session_with_writer_observation(
        &store,
        &meta.id,
        Some(RunWriterObservation::Active),
    )
    .await
    .expect("project");
    assert_eq!(run.status, "running");
    assert!(run.finished_at.is_none());
    assert!(
        run.usage.is_none(),
        "a run with no LLM calls reports no usage rather than a zeroed envelope"
    );
}

#[tokio::test]
async fn an_open_session_without_an_observable_writer_is_not_reported_as_running() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    store
        .append(&meta.id, iteration_start(1))
        .await
        .expect("append");

    let run = project_run_record_from_session_with_writer_observation(
        &store,
        &meta.id,
        Some(RunWriterObservation::NotObserved),
    )
    .await
    .expect("project");
    assert_eq!(run.status, "unknown");
    assert_eq!(
        run.metadata["projected_from"]["writer_observation"],
        "not_observed"
    );
    assert!(run.metadata["projected_from"]["last_observed_at"].is_string());
}

#[tokio::test]
async fn a_durable_writer_lease_distinguishes_a_live_writer_from_an_abandoned_run() {
    let root = tempfile::tempdir().expect("temporary lease root");
    let state_dir = crate::stdlib::session_store::SessionStoreDir::under_root(root.path());
    std::fs::create_dir_all(state_dir.as_path()).expect("create state dir");
    let store = crate::stdlib::session_store::open_canonical_store(root.path())
        .expect("open canonical store");
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    let lease_path =
        crate::agent_session_journal::run_writer_lease_path(state_dir.as_path(), &meta.id);
    std::fs::create_dir_all(lease_path.parent().expect("lease parent")).expect("create lease dir");
    let writer = std::fs::File::create(&lease_path).expect("create writer lease");
    writer.lock().expect("hold writer lease");
    store
        .append(&meta.id, run_started())
        .await
        .expect("append run start");

    let live_path = root.path().join("live-run.json");
    materialize_session_run_record(root.path(), &meta.id, Some(&live_path))
        .await
        .expect("materialize live writer");
    let live = records_persistence::load_run_record(&live_path).expect("load live record");
    assert_eq!(live.status, "running");
    assert_eq!(
        live.metadata["projected_from"]["writer_observation"],
        "active"
    );

    drop(writer);
    let abandoned_path = root.path().join("abandoned-run.json");
    materialize_session_run_record(root.path(), &meta.id, Some(&abandoned_path))
        .await
        .expect("materialize abandoned writer");
    let abandoned =
        records_persistence::load_run_record(&abandoned_path).expect("load abandoned record");
    assert_eq!(abandoned.status, "unknown");
    assert_eq!(
        abandoned.metadata["projected_from"]["writer_observation"],
        "not_observed"
    );
}

#[tokio::test]
async fn a_reused_session_projects_only_its_latest_run_invocation() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession {
            title: Some("stable session title".to_string()),
            ..CreateSession::default()
        })
        .await
        .expect("create session");
    for event in [
        run_started(),
        user_message("old task"),
        iteration_start(7),
        llm_call(100, 50, 0.25),
        tool_call("old-call", "old-tool"),
        terminal("done", "natural"),
    ] {
        store.append(&meta.id, event).await.expect("append event");
    }
    let current_start = store
        .append(&meta.id, run_started())
        .await
        .expect("append current start");
    for event in [
        user_message("current task"),
        iteration_start(1),
        llm_call(3, 2, 0.01),
        tool_call("current-call", "current-tool"),
    ] {
        store.append(&meta.id, event).await.expect("append event");
    }
    let current_terminal = store
        .append(&meta.id, terminal("done", "natural"))
        .await
        .expect("append current terminal");

    let run = project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project latest invocation");
    assert_eq!(run.task, "current task");
    assert_eq!(run.started_at, current_start.ts);
    assert_eq!(
        run.finished_at.as_deref(),
        Some(current_terminal.ts.as_str())
    );
    assert_eq!(run.metadata["iterations"], 1);
    assert_eq!(run.usage.as_ref().expect("usage").call_count, 1);
    assert_eq!(run.usage.as_ref().expect("usage").input_tokens, 3);
    assert_eq!(run.tool_recordings.len(), 1);
    assert_eq!(run.tool_recordings[0].tool_name, "current-tool");
    assert_eq!(run.evidence.trace_spans.len(), 1);
    assert!(run.evidence.execution_id.is_none());
    assert_eq!(run.evidence.gaps[0].component, "execution_identity");
    assert_eq!(run.evidence.gaps[0].code, "session_projection_unavailable");
    assert_eq!(
        crate::orchestration::validate_execution_evidence(&run.evidence),
        Err(crate::orchestration::ExecutionEvidenceValidationError::MissingExecutionId),
        "legacy sessions remain visibly incomplete instead of bypassing the validator",
    );
}

#[tokio::test]
async fn terminal_run_clock_does_not_move_with_later_session_metadata() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    let start = store
        .append(&meta.id, run_started())
        .await
        .expect("append start");
    let finish = store
        .append(&meta.id, terminal("done", "natural"))
        .await
        .expect("append terminal");
    let events = drain_events(&store, &meta.id).await.expect("read events");
    let mut mutated_meta = store.describe(&meta.id).await.expect("describe");
    mutated_meta.updated_at_ms = finish.ts_ms.saturating_add(600_000);
    mutated_meta.updated_at = "2026-12-31T23:59:59Z".to_string();

    let run = assemble(
        mutated_meta,
        SessionFold::from_events(&events),
        Vec::new(),
        meta.id.clone(),
        Some(RunWriterObservation::NotObserved),
    )
    .expect("assemble");
    assert_eq!(run.started_at, start.ts);
    assert_eq!(run.finished_at.as_deref(), Some(finish.ts.as_str()));
    assert_eq!(
        run.metadata["wall_clock_ms"].as_u64(),
        records_time::timestamp_delta_ms(&start.ts, &finish.ts)
    );
    assert_eq!(
        run.metadata["run_clock"],
        json!({
            "started_at_source": "agent_run_started",
            "finished_at_source": "agent_run_terminal",
        })
    );
}

#[tokio::test]
async fn legacy_session_timestamps_do_not_masquerade_as_a_measured_run() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    store
        .append(&meta.id, terminal("done", "natural"))
        .await
        .expect("append terminal");

    let run = project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project");
    assert!(run.metadata["wall_clock_ms"].is_null());
    assert_eq!(
        run.metadata["run_clock"],
        json!({
            "started_at_source": "session_created",
            "finished_at_source": "agent_run_terminal",
        })
    );
}
