//! Projection tests driven by session events shaped exactly as a real headless
//! run persisted them.
//!
//! The payloads below are transcribed from session
//! `019fc7e6-3103-7610-81ed-91599858fa1a` (issue #6120) rather than invented,
//! so a change to the emitter's envelope breaks these tests instead of quietly
//! producing an empty projection.

use harn_session_store::{
    AppendEvent, CreateSession, MemorySessionStore, SessionEventKind, SessionStore,
};
use serde_json::json;
use std::collections::BTreeMap;

use super::*;

fn custom(kind: &str) -> SessionEventKind {
    SessionEventKind::Custom {
        custom_type: kind.to_string(),
    }
}

fn transcript_event(kind: &str, metadata: serde_json::Value) -> serde_json::Value {
    json!({
        "transcript_event": {
            "id": format!("event-{kind}"),
            "kind": kind,
            "role": "assistant",
            "text": "",
            "metadata": metadata,
        }
    })
}

fn llm_call(input: i64, output: i64, cost: f64) -> AppendEvent {
    AppendEvent::new(
        custom("llm_call"),
        transcript_event(
            "llm_call",
            json!({
                "cache_read_tokens": 0,
                "cache_write_tokens": 10948,
                "cost_usd": cost,
                "input_tokens": input,
                "model": "gpt-5.6-luna",
                "output_tokens": output,
                "provider": "openai",
            }),
        ),
    )
}

fn tool_call(id: &str, name: &str) -> AppendEvent {
    AppendEvent::new(
        SessionEventKind::ToolCall,
        transcript_event(
            "tool_call",
            json!({
                "raw_input": {"file": "test/unit/cart_test.rb", "intent": "read"},
                "status": "pending",
                "tool_call_id": id,
                "tool_name": name,
            }),
        ),
    )
}

fn tool_update(id: &str, name: &str, status: &str, duration_ms: i64) -> AppendEvent {
    AppendEvent::new(
        custom("tool_call_update"),
        transcript_event(
            "tool_call_update",
            json!({
                "duration_ms": duration_ms,
                "status": status,
                "tool_call_id": id,
                "tool_name": name,
            }),
        ),
    )
}

fn tool_result(id: &str, text: &str) -> AppendEvent {
    AppendEvent::new(
        SessionEventKind::ToolResult,
        json!({
            "transcript_event": {
                "id": format!("result-{id}"),
                "kind": "tool_result",
                "role": "tool",
                "text": text,
                "metadata": {"tool_call_id": id},
            }
        }),
    )
}

fn iteration_start(iteration: i64) -> AppendEvent {
    AppendEvent::new(
        custom("loop_checkpoint"),
        transcript_event(
            "loop_checkpoint",
            json!({"iteration": iteration, "kind": "iteration_start"}),
        ),
    )
}

fn terminal(final_status: &str, stop_reason: &str) -> AppendEvent {
    let kind = crate::agent_events::classify_agent_terminal(
        final_status,
        stop_reason,
        matches!(final_status, "error" | "failed" | "provider_error"),
        None,
    );
    AppendEvent::new(
        custom("agent_run_terminal"),
        transcript_event(
            "agent_run_terminal",
            json!({
                "error": null,
                "final_status": final_status,
                "stop_reason": stop_reason,
                "terminal_class": null,
                "terminal": crate::agent_events::AgentTerminalOutcome::new(kind, stop_reason),
            }),
        ),
    )
}

fn legacy_terminal(final_status: &str, stop_reason: &str) -> AppendEvent {
    AppendEvent::new(
        custom("agent_run_terminal"),
        transcript_event(
            "agent_run_terminal",
            json!({
                "error": null,
                "final_status": final_status,
                "stop_reason": stop_reason,
                "terminal_class": null,
            }),
        ),
    )
}

fn user_message(text: &str) -> AppendEvent {
    AppendEvent::new(
        SessionEventKind::Message,
        json!({
            "transcript_event": {"kind": "message", "role": "user", "text": text},
            "raw_message": {"content": text, "role": "user"},
        }),
    )
    .with_actor("user")
}

fn assistant_message(text: &str) -> AppendEvent {
    AppendEvent::new(
        SessionEventKind::Message,
        json!({
            "transcript_event": {
                "id": "assistant-visible",
                "kind": "message",
                "role": "assistant",
                "visibility": "public",
                "text": text,
            },
            "raw_message": {"content": text, "role": "assistant"},
        }),
    )
    .with_actor("assistant")
}

/// A store holding one session shaped like the run in #6120: a rate-limited,
/// pace-cut agent loop with tool calls and no run record anywhere.
async fn capstone_like_store() -> (MemorySessionStore, String) {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession {
            id: Some("019fc7e6-3103-7610-81ed-91599858fa1a".to_string()),
            attributes: BTreeMap::from([
                ("source".to_string(), json!("burin-headless")),
                ("source_version".to_string(), json!("0.2.0")),
                ("source_revision".to_string(), json!("burin-sha")),
                ("harn_version".to_string(), json!("v0.10.84")),
                ("harn_revision".to_string(), json!("harn-sha")),
            ]),
            ..CreateSession::default()
        })
        .await
        .expect("create session");
    let id = meta.id.clone();
    for event in [
        user_message("Migrate the three unit test files."),
        iteration_start(1),
        llm_call(10951, 112, 0.002872),
        assistant_message("I inspected the three requested files."),
        tool_call("call_A", "look"),
        tool_update("call_A", "look", "in_progress", 0),
        tool_result("call_A", "[result of look]\n1\tclass CartTest"),
        tool_update("call_A", "look", "completed", 5),
        iteration_start(2),
        llm_call(20000, 300, 0.004),
        tool_call("call_B", "edit"),
        tool_update("call_B", "edit", "failed", 12),
        terminal("done", "pace_cutoff"),
    ] {
        store.append(&id, event).await.expect("append");
    }
    (store, id)
}

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

#[tokio::test]
async fn readable_messages_project_in_order_without_private_payload_fields() {
    let (store, id) = capstone_like_store().await;
    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");

    let transcript = run.transcript.as_ref().expect("readable transcript");
    assert_eq!(transcript["_type"], "transcript");
    assert_eq!(transcript["id"], id);
    assert_eq!(transcript["source"], PROJECTION_SOURCE);
    assert_eq!(
        transcript["messages"],
        json!([
            {"role": "user", "content": "Migrate the three unit test files."},
            {"role": "assistant", "content": "I inspected the three requested files."},
        ])
    );
    assert_eq!(transcript["events"][0]["kind"], "message");
    assert_eq!(transcript["events"][0]["visibility"], "public");
    assert_eq!(transcript["events"][1]["id"], "assistant-visible");
    assert_eq!(transcript["events"][1]["blocks"][0]["type"], "output_text");
    assert!(transcript.pointer("/events/0/metadata").is_none());
    assert!(transcript.pointer("/events/0/raw_message").is_none());

    let view = crate::orchestration::records::build_run_view(&run);
    assert_eq!(view.transcript.message_count, 2);
    assert_eq!(view.transcript.event_count, 2);
    assert_eq!(
        view.visible_text.as_deref(),
        Some("I inspected the three requested files.")
    );
}

#[tokio::test]
async fn private_and_malformed_messages_are_not_projected() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    let id = meta.id.clone();
    for event in [
        user_message("public request"),
        AppendEvent::new(
            SessionEventKind::Message,
            json!({"transcript_event": {
                "kind": "message",
                "role": "assistant",
                "visibility": "private",
                "text": "private reasoning",
            }}),
        ),
        AppendEvent::new(
            SessionEventKind::Message,
            json!({"transcript_event": {
                "kind": "message",
                "role": "future-role",
                "visibility": "public",
                "text": "unknown speaker",
            }}),
        ),
        AppendEvent::new(
            SessionEventKind::Message,
            json!({"transcript_event": {
                "kind": "message",
                "role": "assistant",
                "visibility": "public",
            }}),
        ),
    ] {
        store.append(&id, event).await.expect("append");
    }

    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");
    let transcript = run.transcript.expect("public transcript");
    assert_eq!(
        transcript["messages"],
        json!([{"role": "user", "content": "public request"}])
    );
    let encoded = transcript.to_string();
    assert!(!encoded.contains("private reasoning"));
    assert!(!encoded.contains("unknown speaker"));
}

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
        look.duration_ms, 5,
        "duration comes from the terminal update, not the in_progress one"
    );
    assert!(look.result.contains("class CartTest"));
    assert_eq!(look.iteration, 1);
    assert!(!look.args_hash.is_empty());

    let edit = &run.tool_recordings[1];
    assert_eq!(edit.tool_name, "edit");
    assert_eq!(edit.duration_ms, 12);
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
    assert_eq!(by_id["call_first"].duration_ms, 3);
    assert_eq!(by_id["call_second"].result, "second result");
    assert_eq!(by_id["call_second"].duration_ms, 900);
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
            "trace_spans[].duration_ms",
            run.trace_spans.iter().all(|span| span.duration_ms == 0),
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
async fn child_sessions_project_as_child_runs_from_the_stores_own_lineage() {
    let store = MemorySessionStore::default();
    let parent = store
        .create(CreateSession::default())
        .await
        .expect("create parent");
    for name in ["worker-a", "worker-b"] {
        store
            .create(CreateSession {
                parent_session_id: Some(parent.id.clone()),
                persona: Some(name.to_string()),
                title: Some(format!("{name} task")),
                ..CreateSession::default()
            })
            .await
            .expect("create child");
    }

    let run = project_run_record_from_session(&store, &parent.id)
        .await
        .expect("project");
    assert_eq!(run.child_runs.len(), 2);
    let names: Vec<&str> = run
        .child_runs
        .iter()
        .map(|child| child.worker_name.as_str())
        .collect();
    assert_eq!(names, vec!["worker-a", "worker-b"]);
    assert!(run
        .child_runs
        .iter()
        .all(|child| child.parent_session_id.as_deref() == Some(parent.id.as_str())));
}

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
        ("verify_capped", "verify_capped", "stopped", "policy_budget"),
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

    let run = project_run_record_from_session(&store, &meta.id)
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

/// `root_run_id` has to be the top of the delegation chain, not the immediate
/// parent. A grandchild reporting its parent as root would make a three-level
/// fan-out look like two unrelated two-level ones in any report that groups by
/// root.
#[tokio::test]
async fn a_grandchild_reports_the_top_of_its_chain_as_the_root_run() {
    let store = MemorySessionStore::default();
    let root = store
        .create(CreateSession::default())
        .await
        .expect("create root");
    let middle = store
        .create(CreateSession {
            parent_session_id: Some(root.id.clone()),
            ..CreateSession::default()
        })
        .await
        .expect("create middle");
    let leaf = store
        .create(CreateSession {
            parent_session_id: Some(middle.id.clone()),
            ..CreateSession::default()
        })
        .await
        .expect("create leaf");

    let projected = project_run_record_from_session(&store, &leaf.id)
        .await
        .expect("project");
    assert_eq!(projected.parent_run_id.as_deref(), Some(middle.id.as_str()));
    assert_eq!(
        projected.root_run_id.as_deref(),
        Some(root.id.as_str()),
        "root must be the chain's top, not one hop up"
    );

    // The root itself is its own root, which is what a report keying on
    // `root_run_id` needs in order to find the bundle at all.
    let root_projected = project_run_record_from_session(&store, &root.id)
        .await
        .expect("project root");
    assert_eq!(root_projected.parent_run_id, None);
    assert_eq!(
        root_projected.root_run_id.as_deref(),
        Some(root.id.as_str())
    );
}

/// A run report sources its per-call view from `trace_spans`. Leaving them
/// empty made `harn runs report` say a 96-call run made zero LLM calls — worse
/// than a missing field, because zero reads as a measurement.
#[tokio::test]
async fn every_recorded_provider_call_becomes_an_llm_call_span() {
    let (store, id) = capstone_like_store().await;
    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");

    let spans: Vec<_> = run
        .trace_spans
        .iter()
        .filter(|span| span.kind == "llm_call")
        .collect();
    assert_eq!(
        spans.len(),
        run.usage.as_ref().expect("usage").call_count as usize,
        "the per-call view and the aggregate must agree on how many calls there were"
    );

    let first = spans[0];
    assert_eq!(first.trace_id, id);
    assert_eq!(first.name, "gpt-5.6-luna");
    assert_eq!(first.cost_usd, Some(0.002872));
    assert_eq!(
        first.metadata.get("input_tokens").and_then(|v| v.as_i64()),
        Some(10951)
    );
    assert_eq!(
        first.metadata.get("model").and_then(|v| v.as_str()),
        Some("gpt-5.6-luna")
    );
    // Span ids must be distinct or the report's `trace:span` call ids collide
    // and the calls dedupe away.
    let ids: std::collections::BTreeSet<_> = spans.iter().map(|span| span.span_id).collect();
    assert_eq!(ids.len(), spans.len());
}

/// A zero duration here is "not recorded", not "instant". Saying so in the span
/// is what keeps a latency view from averaging in 96 fabricated zeroes.
#[tokio::test]
async fn a_projected_span_declares_that_its_duration_is_not_a_measurement() {
    let (store, id) = capstone_like_store().await;
    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");

    for span in run.trace_spans.iter().filter(|s| s.kind == "llm_call") {
        assert_eq!(span.duration_ms, 0);
        assert_eq!(
            span.metadata
                .get("duration_available")
                .and_then(|v| v.as_bool()),
            Some(false),
            "a zero duration must be labelled as absent evidence"
        );
        assert_eq!(span.ttft_ms, None);
    }
}

/// Money is a base-10 quantity. Adding the 96 per-call `f64` costs from the
/// real run in #6120 produced `0.6060984600000002`; a run report is read by
/// people reconciling spend against a provider invoice, so the accumulator is
/// exact and the aggregate equals the sum of the per-call spans exactly.
#[tokio::test]
async fn the_cost_aggregate_is_exact_rather_than_float_accumulated() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    // Three costs that do not sum cleanly in binary floating point.
    for cost in [0.1, 0.2, 0.3] {
        store
            .append(&meta.id, llm_call(10, 1, cost))
            .await
            .expect("append");
    }

    let run = project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project");
    let usage = run.usage.as_ref().expect("usage");
    assert_eq!(
        usage.total_cost, 0.6,
        "0.1 + 0.2 + 0.3 accumulated as f64 gives 0.6000000000000001"
    );

    // The aggregate and the per-call view must agree, or a reader reconciling
    // one against the other finds a phantom discrepancy.
    let span_total: f64 = run
        .trace_spans
        .iter()
        .filter_map(|span| span.cost_usd)
        .sum();
    assert!((span_total - usage.total_cost).abs() < 1e-9);
}

fn llm_call_with_attempts(cost: f64, total: i64, rate_limited: i64) -> AppendEvent {
    AppendEvent::new(
        custom("llm_call"),
        transcript_event(
            "llm_call",
            json!({
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
                "cost_usd": cost,
                "input_tokens": 100,
                "model": "gpt-5.6-luna",
                "output_tokens": 10,
                "provider": "openai",
                "provider_attempts": {
                    "total": total,
                    "retries": total - 1,
                    "rate_limited": rate_limited,
                    "empty_completion": 0,
                    "other": total - 1 - rate_limited,
                },
            }),
        ),
    )
}

/// #5847: a run whose provider rejected 47 of 146 requests with a retryable
/// 429 reported 96 clean calls and no contention signal at all. The call count
/// and the request count are different facts and a report has to carry both.
#[tokio::test]
async fn retried_provider_requests_are_visible_alongside_the_call_count() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    for event in [
        llm_call_with_attempts(0.01, 3, 2),
        llm_call_with_attempts(0.01, 1, 0),
        llm_call_with_attempts(0.01, 2, 0),
    ] {
        store.append(&meta.id, event).await.expect("append");
    }

    let run = project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project");
    assert_eq!(
        run.usage.as_ref().expect("usage").call_count,
        3,
        "three logical calls"
    );
    let attempts = run
        .metadata
        .get("provider_attempts")
        .expect("a run that retried must say so");
    assert_eq!(attempts.get("total").and_then(|v| v.as_i64()), Some(6));
    assert_eq!(attempts.get("retries").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(
        attempts.get("rate_limited").and_then(|v| v.as_i64()),
        Some(2),
        "rate limiting is the signal that explains a slow or truncated run"
    );
    assert_eq!(attempts.get("other").and_then(|v| v.as_i64()), Some(1));
}

/// A block of zeroes on every clean run would train a reader to skip the one
/// place the contention signal appears.
#[tokio::test]
async fn a_run_that_never_retried_reports_no_attempt_block() {
    let (store, id) = capstone_like_store().await;
    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");
    assert!(
        !run.metadata.contains_key("provider_attempts"),
        "no retries means nothing to report"
    );
}

/// Sessions recorded before provider attempts existed carry no entry. Counting
/// them as one request each keeps the total a lower bound instead of reporting
/// fewer requests than there were calls.
#[tokio::test]
async fn calls_recorded_before_attempts_existed_count_as_one_request_each() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    // One old-shape call, one new-shape call that retried twice.
    store
        .append(&meta.id, llm_call(100, 10, 0.01))
        .await
        .expect("append");
    store
        .append(&meta.id, llm_call_with_attempts(0.01, 3, 3))
        .await
        .expect("append");

    let run = project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project");
    let attempts = run.metadata.get("provider_attempts").expect("attempts");
    assert_eq!(
        attempts.get("total").and_then(|v| v.as_i64()),
        Some(4),
        "1 (unrecorded, floored to one request) + 3 (recorded)"
    );
    assert_eq!(
        attempts.get("rate_limited").and_then(|v| v.as_i64()),
        Some(3)
    );
}
