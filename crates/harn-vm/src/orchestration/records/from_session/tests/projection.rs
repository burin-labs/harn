//! Run-record projection identity, provenance, and the unrecoverable-field claim.

use harn_session_store::MemorySessionStore;
use serde_json::json;
use std::collections::BTreeMap;

use super::super::*;
use super::support::*;
#[tokio::test]
async fn a_headless_session_projects_the_run_record_no_host_ever_wrote() {
    let (store, id) = capstone_like_store().await;
    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");

    assert_eq!(run.id, id);
    assert_eq!(run.workflow_id, AGENT_SESSION_WORKFLOW_ID);
    assert_eq!(run.task, "Migrate the three unit test files.");
    // The session is still `open` — the host exited without closing it, exactly
    // as the run in #6120 did. The loop's own terminal verdict has to win, or a
    // finished run reports as still running forever.
    assert_eq!(run.status, "stopped");
    assert_eq!(run.metadata["terminal"]["kind"], "policy_stop");
    assert_eq!(run.metadata["terminal"]["owner"], "policy");
    assert!(
        run.finished_at.is_some(),
        "a run with a terminal event has an end time even when its session was never closed"
    );
    assert_eq!(run.root_run_id.as_deref(), Some(id.as_str()));
    assert_eq!(run.metadata["build"]["producer"], "burin-headless");
    assert_eq!(run.metadata["build"]["producer_version"], "0.2.0");
    assert_eq!(run.metadata["build"]["producer_revision"], "burin-sha");
    assert_eq!(run.metadata["build"]["harn_version"], "v0.10.84");
    assert_eq!(run.metadata["build"]["harn_revision"], "harn-sha");

    let usage = run.usage.as_ref().expect("usage");
    assert_eq!(usage.call_count, 2);
    assert_eq!(usage.input_tokens, 30951);
    assert_eq!(usage.output_tokens, 412);
    assert!((usage.total_cost - 0.006872).abs() < 1e-9);
    assert!(usage
        .cost_usd
        .is_some_and(|cost| (cost - 0.006872).abs() < 1e-9));
    assert!((usage.known_cost_usd - 0.006872).abs() < 1e-9);
    assert_eq!(usage.unpriced_calls, 0);
    assert_eq!(usage.models, vec!["gpt-5.6-luna".to_string()]);

    assert_eq!(
        run.metadata.get("stop_reason").and_then(|v| v.as_str()),
        Some("pace_cutoff"),
        "the reason a run was cut short is the first thing a reader asks for"
    );
    assert_eq!(
        run.metadata.get("iterations").and_then(|v| v.as_u64()),
        Some(2)
    );
}

#[test]
fn incomplete_or_unknown_build_attributes_do_not_claim_provenance() {
    assert!(build_from_session_attributes(&BTreeMap::from([
        ("source".to_string(), json!("burin-headless")),
        ("source_version".to_string(), json!("0.2.0")),
    ]))
    .is_none());
    assert!(build_from_session_attributes(&BTreeMap::from([
        ("source".to_string(), json!("burin-headless")),
        ("source_version".to_string(), json!("0.2.0")),
        ("harn_version".to_string(), json!("unknown")),
    ]))
    .is_none());
}

#[tokio::test]
async fn a_projection_says_it_is_one_and_names_what_it_could_not_recover() {
    let (store, id) = capstone_like_store().await;
    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");

    let projected = run
        .metadata
        .get("projected_from")
        .expect("a projected record must be identifiable as one");
    assert_eq!(
        projected.get("source").and_then(|v| v.as_str()),
        Some(PROJECTION_SOURCE)
    );
    assert_eq!(
        projected.get("session_id").and_then(|v| v.as_str()),
        Some(id.as_str())
    );
    assert_eq!(
        projected.get("session_status").and_then(|v| v.as_str()),
        Some("open")
    );
    let unrecovered: Vec<&str> = projected
        .get("not_recoverable_from_session")
        .and_then(|v| v.as_array())
        .expect("not_recoverable_from_session")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(unrecovered, UNRECOVERABLE_FIELDS.to_vec());
}

/// `UNRECOVERABLE_FIELDS` is a claim about this projector, and a hand-kept list
/// rots. This asserts the claim in both directions against a projection built
/// from a session rich enough that every *recoverable* field is populated: a
/// listed field that is now populated fails, and a field that quietly stopped
/// being populated is not silently excused because it happens to be listed.
#[tokio::test]
async fn the_unrecoverable_field_list_matches_what_the_projector_actually_leaves_empty() {
    let (store, id) = capstone_like_store().await;
    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");

    let is_empty: std::collections::BTreeMap<&str, bool> = [
        (
            "usage.total_duration_ms",
            run.usage
                .as_ref()
                .is_none_or(|usage| usage.total_duration_ms == 0),
        ),
        // Compared against the record's own default rather than a named
        // constructor, so this stays a statement about the projector leaving
        // the field untouched even if the policy default itself changes.
        (
            "evidence.trace_spans[].duration_ms",
            run.evidence
                .trace_spans
                .iter()
                .all(|span| span.duration_ms == 0),
        ),
        ("policy", run.policy == RunRecord::default().policy),
        ("replay_fixture", run.replay_fixture.is_none()),
        // Recoverable fields, asserted non-empty so this test fails if the
        // projector regresses into producing a hollow record.
        ("id", run.id.is_empty()),
        ("task", run.task.is_empty()),
        ("status", run.status.is_empty()),
        ("started_at", run.started_at.is_empty()),
        ("finished_at", run.finished_at.is_none()),
        ("transcript", run.transcript.is_none()),
        ("usage", run.usage.is_none()),
        ("tool_recordings", run.tool_recordings.is_empty()),
        (
            "tool_recordings[].duration_ms",
            run.tool_recordings
                .iter()
                .all(|record| record.duration_ms.is_none()),
        ),
        ("metadata", run.metadata.is_empty()),
    ]
    .into_iter()
    .collect();

    let actually_empty: Vec<&str> = is_empty
        .iter()
        .filter(|(_, empty)| **empty)
        .map(|(field, _)| *field)
        .collect();
    let mut declared = UNRECOVERABLE_FIELDS.to_vec();
    declared.sort_unstable();
    assert_eq!(
        actually_empty, declared,
        "UNRECOVERABLE_FIELDS must name exactly the fields this projector leaves at their default"
    );
}

#[tokio::test]
async fn an_unknown_session_names_the_session_and_how_to_find_a_real_one() {
    let store = MemorySessionStore::default();
    let error = project_run_record_from_session(&store, "019f-not-a-session")
        .await
        .expect_err("unknown session must fail");
    let message = error.to_string();
    assert!(
        message.contains("019f-not-a-session"),
        "error must name the session that was not found: {message}"
    );
    assert!(
        message.contains("harn session list"),
        "error must point at the surface that lists real sessions: {message}"
    );
}

#[tokio::test]
async fn listing_a_workspace_without_a_session_store_is_empty_rather_than_an_error() {
    // A workspace that has never run an agent is a normal state, not a
    // failure, and `harn session list` has to say so without a stack of
    // "failed to open" noise.
    let dir = tempfile::tempdir().expect("tempdir");
    let sessions = list_session_runs(dir.path(), None).await.expect("list");
    assert!(sessions.is_empty());
}
