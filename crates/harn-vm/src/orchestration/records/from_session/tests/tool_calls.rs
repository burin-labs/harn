//! Tool-call joining: call/update/result attribution and duration fidelity.

use harn_session_store::{AppendEvent, CreateSession, MemorySessionStore};
use serde_json::json;

use super::super::*;
use super::support::*;

#[tokio::test]
async fn tool_calls_join_their_updates_and_results_by_provider_call_id() {
    let (store, id) = capstone_like_store().await;
    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");

    assert_eq!(run.tool_recordings.len(), 2);
    let look = &run.tool_recordings[0];
    assert_eq!(look.tool_name, "look");
    assert_eq!(look.tool_use_id, "call_A");
    assert_eq!(
        look.duration_ms,
        Some(5),
        "duration comes from the terminal update, not the in_progress one"
    );
    assert!(look.result.contains("class CartTest"));
    assert_eq!(look.iteration, 1);
    assert!(!look.args_hash.is_empty());

    let edit = &run.tool_recordings[1];
    assert_eq!(edit.tool_name, "edit");
    assert_eq!(edit.duration_ms, Some(12));
    assert_eq!(
        edit.iteration, 2,
        "a call is attributed to the iteration that was open when it was made"
    );
}

/// The falsifier for the join: interleave two calls so a positional or
/// last-seen implementation attributes the result to the wrong call.
#[tokio::test]
async fn interleaved_tool_calls_do_not_cross_attribute_results() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    let id = meta.id.clone();
    for event in [
        iteration_start(1),
        tool_call("call_first", "look"),
        tool_call("call_second", "run"),
        // Second call resolves first — a real batch does this constantly.
        tool_result("call_second", "second result"),
        tool_update("call_second", "run", "completed", 900),
        tool_result("call_first", "first result"),
        tool_update("call_first", "look", "completed", 3),
    ] {
        store.append(&id, event).await.expect("append");
    }

    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");
    let by_id: std::collections::BTreeMap<_, _> = run
        .tool_recordings
        .iter()
        .map(|record| (record.tool_use_id.as_str(), record))
        .collect();
    assert_eq!(by_id["call_first"].result, "first result");
    assert_eq!(by_id["call_first"].duration_ms, Some(3));
    assert_eq!(by_id["call_second"].result, "second result");
    assert_eq!(by_id["call_second"].duration_ms, Some(900));
}

#[tokio::test]
async fn tool_duration_distinguishes_measured_zero_from_missing_timing() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    let id = meta.id.clone();
    for event in [
        tool_call("call_measured_zero", "look"),
        tool_update("call_measured_zero", "look", "completed", 0),
        tool_result("call_measured_zero", "done"),
        tool_call("call_without_timing", "look"),
        AppendEvent::new(
            custom("tool_call_update"),
            transcript_event(
                "tool_call_update",
                json!({
                    "status": "completed",
                    "tool_call_id": "call_without_timing",
                    "tool_name": "look",
                }),
            ),
        ),
        tool_result("call_without_timing", "done"),
    ] {
        store.append(&id, event).await.expect("append");
    }

    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");
    assert_eq!(run.tool_recordings[0].duration_ms, Some(0));
    let record = &run.tool_recordings[1];
    assert_eq!(record.duration_ms, None);
    assert_eq!(
        serde_json::to_value(record).expect("serialize record")["duration_ms"],
        serde_json::Value::Null,
        "an unavailable measurement must not be published as zero"
    );
}

/// A rejected call still produces a result event. Folding the result's error
/// flag into `is_rejected` would clear the rejection recorded moments earlier,
/// turning a refused tool call into an ordinary one in the report.
#[tokio::test]
async fn a_result_arriving_after_a_rejection_does_not_clear_it() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    let id = meta.id.clone();
    for event in [
        tool_call("call_R", "run"),
        tool_update("call_R", "run", "rejected", 0),
        tool_result("call_R", "[error] the user declined this command"),
    ] {
        store.append(&id, event).await.expect("append");
    }

    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");
    let record = &run.tool_recordings[0];
    assert!(
        record.is_rejected,
        "a rejected call stays rejected once its result lands"
    );
    assert!(record.result.contains("declined"));
}
