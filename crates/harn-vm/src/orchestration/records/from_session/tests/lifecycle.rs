//! Run lifecycle: terminal vocabulary, liveness, and the run clock.

use harn_session_store::{AppendEvent, CreateSession, MemorySessionStore};
use serde_json::json;

use super::super::*;
use super::support::*;
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
    let live = crate::orchestration::records::persistence::load_run_record(&live_path)
        .expect("load live record");
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
    let abandoned = crate::orchestration::records::persistence::load_run_record(&abandoned_path)
        .expect("load abandoned record");
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
        crate::orchestration::records::time::timestamp_delta_ms(&start.ts, &finish.ts)
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

/// #6741: every non-error final status collapsed to `completed`, so a run that
/// was cancelled, stopped by policy, or parked mid-loop was recorded as a
/// success. These are the spellings a loop actually reports.
#[test]
fn final_status_projects_onto_the_run_record_vocabulary() {
    for (final_status, expected) in [
        ("user_cancelled", "cancelled"),
        ("policy_stop", "stopped"),
        ("policy_budget", "stopped"),
        ("policy_no_progress", "stopped"),
        ("policy_thrash", "stopped"),
        ("policy_guardrail", "stopped"),
        ("completion_unverified", "failed"),
        ("provider_error", "failed"),
        ("runtime_error", "failed"),
        ("suspended", "suspended"),
        ("awaiting_input", "awaiting_input"),
        ("cancelled", "cancelled"),
        ("aborted", "cancelled"),
        ("timeout", "failed"),
    ] {
        assert_eq!(
            run_status_from_final_status(final_status),
            expected,
            "{final_status} must not be recorded as a success"
        );
    }
}

/// Negative control for the test above. If `run_status_from_final_status`
/// started reporting a non-success for everything, the assertions above would
/// still pass; these are the cases that must stay `completed`.
#[test]
fn genuine_and_unrecognized_completions_still_report_completed() {
    for final_status in ["natural", "done", "succeeded", "success", "ok"] {
        assert_eq!(run_status_from_final_status(final_status), "completed");
    }
    // Documented boundary, not an endorsement: a spelling neither vocabulary
    // knows and the error classifier declines is reported as a completion.
    assert_eq!(
        run_status_from_final_status("some_status_nothing_owns"),
        "completed"
    );
}

/// The class-killer. The producer-owned `AgentTerminalKind` table is the one
/// place this mapping lives; a re-derived copy in this module would drift from
/// it silently. Assert the string path agrees with the typed path for every
/// kind, so adding a kind without teaching this projection fails here.
#[test]
fn every_terminal_kind_agrees_with_its_typed_projection() {
    for kind in crate::agent_events::AgentTerminalKind::ALL {
        assert_eq!(
            run_status_from_final_status(kind.as_str()),
            kind.lifecycle_state().projection().run_record_status,
            "{} must route through AgentTerminalKind::lifecycle_state",
            kind.as_str()
        );
    }
}

/// The terminal carries its own seal time, so a reader no longer joins
/// `finished_at` through `run_clock` to learn it came from the terminal.
/// Asserting against `finished_at` rather than a literal ties the two clocks,
/// so both being wrong together cannot pass.
#[tokio::test]
async fn the_terminal_carries_the_moment_it_was_sealed() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    store
        .append(&meta.id, terminal("stuck", "no_progress"))
        .await
        .expect("append terminal");

    let run = project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project");

    let sealed_at = run.metadata["terminal"]["sealed_at"]
        .as_str()
        .expect("a terminal must say when it was sealed");
    assert!(!sealed_at.is_empty(), "an empty stamp is not a measurement");
    assert_eq!(
        Some(sealed_at),
        run.finished_at.as_deref(),
        "sealed_at must be the same stamp the run's end is dated from"
    );
    assert_eq!(
        run.metadata["run_clock"]["finished_at_source"], "agent_run_terminal",
        "the run's end must be sourced from the terminal for this pairing to mean anything"
    );

    // The control: a session with no terminal has no terminal block to carry a
    // stamp, rather than one carrying an empty or invented value.
    let bare = MemorySessionStore::default();
    let bare_meta = bare
        .create(CreateSession::default())
        .await
        .expect("create session");
    bare.append(&bare_meta.id, user_message("no terminal here"))
        .await
        .expect("append message");
    let bare_run = project_run_record_from_session(&bare, &bare_meta.id)
        .await
        .expect("project");
    assert!(
        !bare_run.metadata.contains_key("terminal"),
        "a run with no terminal must not report when one was sealed"
    );
}

/// THE FALSIFIER. A closed session that recorded no terminal and no final
/// status says nothing about how its run ended, and must not be projected as a
/// finished one.
///
/// Measured shape: an agent run ended on a `policy_no_progress` terminal, the
/// terminal was recorded against the session the loop ran in, and the record was
/// projected from a different session id that had the transcript but not the
/// terminal. Every mapping on the way was correct — a policy kind projects to
/// `stopped` — and the record still read `status: "completed", terminal: null`,
/// because the last branch answers from the session being shut rather than from
/// any evidence about the run.
#[tokio::test]
async fn a_closed_session_with_no_terminal_is_unclassified_never_completed() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    store
        .append(&meta.id, user_message("do the work"))
        .await
        .expect("append message");
    store.close(&meta.id).await.expect("close session");

    let run = project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project closed session");

    assert_eq!(
        run.status, "unknown",
        "a closed session with no terminal claimed a finish it has no evidence for"
    );
    assert!(
        !run.metadata.contains_key("terminal"),
        "no terminal was recorded, so none may be reported"
    );
}

/// THE DIRECTION CONTROLS. The change above must not be reachable by making the
/// projection uniformly pessimistic: a run that left a verdict still reports it,
/// in both directions.
///
/// `natural` is the half that matters most. If it drifted to `unknown` the
/// projection would be honest and useless, and every reader that asks "did this
/// finish" would get no for everything.
#[tokio::test]
async fn a_session_that_left_a_verdict_still_reports_it_in_both_directions() {
    for (final_status, stop_reason, expected) in [
        // `sentinel` is in the natural stop-reason set; a `done` with a stop
        // reason outside that set is a policy stop, which is the #4642 rule.
        ("done", "sentinel", "completed"),
        ("stuck", "no_progress", "stopped"),
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
        // Closed, exactly like the falsifier above. The terminal is the only
        // difference between the two cases, so it is the only thing that can be
        // deciding the answer.
        store.close(&meta.id).await.expect("close session");

        let run = project_run_record_from_session(&store, &meta.id)
            .await
            .expect("project");
        assert_eq!(
            run.status, expected,
            "a {final_status} terminal must still project {expected}"
        );
    }
}
