use super::*;
use crate::value::VmDictExt;
use harn_session_store::SessionStore;

fn options(root: &std::path::Path) -> DictMap {
    let mut options = DictMap::new();
    options.put_str("root", root.to_string_lossy().as_ref());
    options
}

fn transcript_event(id: &str, kind: &str, role: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "kind": kind,
        "role": role,
        "visibility": "internal",
        "text": "",
    })
}

async fn prepare_test_journal(
    session_id: &str,
    options: &DictMap,
    run_id: &str,
    turn_id: &str,
) -> (HydratedTranscript, Option<JournalState>) {
    let prepared = prepare(session_id, options, run_id.to_string(), turn_id.to_string())
        .await
        .expect("prepare journal");
    crate::agent_sessions::open_or_create_for_test(Some(session_id.to_string()));
    (prepared.transcript, Some(prepared.state))
}

fn install_test_journal(session_id: &str, journal: &mut Option<JournalState>) {
    crate::agent_sessions::install_journal(
        session_id,
        journal.take().expect("prepared journal state"),
    )
    .expect("install journal");
}

fn mapping_config() -> JournalConfig {
    JournalConfig {
        store: crate::stdlib::canonical_store::CanonicalStore::in_memory()
            .expect("in-memory store"),
        run_id: "run-tool".to_string(),
        turn_id: "turn-tool".to_string(),
        workflow_learning_eligibility: WORKFLOW_LEARNING_EXCLUDED.to_string(),
    }
}

#[tokio::test]
async fn journal_records_subagent_lineage_at_session_creation() {
    let root = tempfile::tempdir().expect("temp root");
    let canonical_root = root.path().canonicalize().expect("canonical temp root");
    let mut options = options(root.path());
    options.put_str("parent_session_id", "parent-session");
    options.put_str("session_type", "subagent");

    let prepared = prepare(
        "child-session",
        &options,
        "child-run".to_string(),
        "child-turn".to_string(),
    )
    .await
    .expect("prepare child journal");
    let meta = prepared
        .state
        .config
        .store
        .describe("child-session")
        .await
        .expect("describe child session");

    assert_eq!(meta.parent_session_id.as_deref(), Some("parent-session"));
    assert_eq!(meta.session_type, Some(SessionType::Subagent));
    assert_eq!(
        meta.project_scope.as_deref(),
        Some(canonical_root.to_string_lossy().as_ref())
    );
}

#[tokio::test]
async fn journal_records_explicit_workflow_learning_eligibility_at_creation() {
    let root = tempfile::tempdir().expect("temp root");
    let mut eligible_options = options(root.path());
    eligible_options.put_str("workflow_learning_eligibility", "eligible");

    let prepared = prepare(
        "eligible-workflow-session",
        &eligible_options,
        "eligible-run".to_string(),
        "eligible-turn".to_string(),
    )
    .await
    .expect("prepare eligible journal");
    let meta = prepared
        .state
        .config
        .store
        .describe("eligible-workflow-session")
        .await
        .expect("describe eligible session");

    assert!(
        !meta
            .attributes
            .contains_key(WORKFLOW_LEARNING_ELIGIBILITY_HEADER),
        "workflow eligibility belongs to each run, not the long-lived session"
    );
    let event = append_event_for_payload(
        &prepared.state.config,
        SessionEventPayload::Transcript(TranscriptRecord::new(
            transcript_event("eligible-terminal", "agent_run_terminal", "assistant"),
            None,
        )),
    )
    .expect("build eligible terminal event");
    assert_eq!(
        event.headers.get(WORKFLOW_LEARNING_ELIGIBILITY_HEADER),
        Some(&WORKFLOW_LEARNING_ELIGIBLE.to_string())
    );

    let mut invalid = options(root.path());
    invalid.put_str("workflow_learning_eligibility", "maybe");
    let result = prepare(
        "invalid-workflow-session",
        &invalid,
        "invalid-run".to_string(),
        "invalid-turn".to_string(),
    )
    .await;
    let error = match result {
        Ok(_) => panic!("unknown eligibility must fail at the producer boundary"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("workflow_learning_eligibility"));
}

#[tokio::test]
async fn journal_persists_identity_hydrates_and_replays_replacements() {
    crate::agent_sessions::reset_session_store();
    let root = tempfile::tempdir().expect("temp root");
    let options = options(root.path());
    let session_id = "journal-round-trip";

    let (initial, mut journal) =
        prepare_test_journal(session_id, &options, "run-first", "turn-first").await;
    assert!(initial.messages.is_empty());

    enqueue_message(
        &mut journal,
        transcript_event("event-user", "message", "user"),
        serde_json::json!({
            "role": "user",
            "content": "persist me",
            "messageId": "message-user",
        }),
    );
    enqueue_message(
        &mut journal,
        transcript_event("event-tool", "tool_result", "tool_result"),
        serde_json::json!({
            "role": "tool_result",
            "tool_call_id": "tool-1",
            "content": "tool output",
        }),
    );
    install_test_journal(session_id, &mut journal);
    flush(session_id).await.expect("flush messages");

    let store = session_store::open_canonical_agent_session(
        &session_store::SessionStoreDir::under_root(root.path()),
        session_id,
        None,
        SessionType::User,
    )
    .await
    .expect("open canonical store");
    let events = session_store::read_all_events(&store, session_id)
        .await
        .expect("read canonical events");

    assert_eq!(events.len(), 2);
    let identity = events[0].identity().expect("identity");
    assert_eq!(identity.get(EventIdentityField::RunId), Some("run-first"));
    assert_eq!(identity.get(EventIdentityField::TurnId), Some("turn-first"));
    assert_eq!(
        identity.get(EventIdentityField::SourceEventId),
        Some("event-user")
    );
    assert_eq!(
        identity.get(EventIdentityField::MessageId),
        Some("message-user")
    );
    assert_eq!(events[1].kind, SessionEventKind::ToolResult);
    assert_eq!(
        events[1].headers.get("tool_call_id"),
        Some(&"tool-1".to_string())
    );

    crate::agent_sessions::reset_session_store();
    let (hydrated, mut journal) =
        prepare_test_journal(session_id, &options, "run-second", "turn-second").await;
    assert_eq!(hydrated.messages.len(), 2);
    assert_eq!(hydrated.messages[0]["content"], "persist me");
    assert_eq!(
        hydrated.source_event_ids,
        vec![
            Some("event-user".to_string()),
            Some("event-tool".to_string()),
        ]
    );

    enqueue_message_removed(
        &mut journal,
        "event-tool".to_string(),
        serde_json::json!({
            "role": "tool_result",
            "tool_call_id": "tool-1",
            "content": "tool output",
        }),
    );
    install_test_journal(session_id, &mut journal);
    flush(session_id).await.expect("flush durable removal");
    crate::agent_sessions::reset_session_store();

    let (removed, mut journal) =
        prepare_test_journal(session_id, &options, "run-removal", "turn-removal").await;
    assert_eq!(removed.messages.len(), 1);

    enqueue_messages_replaced(
        &mut journal,
        vec![serde_json::json!({"role": "assistant", "content": "retained turn"})],
        Some("compacted context".to_string()),
        vec![Some("compacted-assistant".to_string())],
    );
    install_test_journal(session_id, &mut journal);
    flush(session_id).await.expect("flush replacement");
    crate::agent_sessions::reset_session_store();

    let (compacted, mut journal) =
        prepare_test_journal(session_id, &options, "run-third", "turn-third").await;
    assert_eq!(compacted.messages.len(), 2);
    assert_eq!(compacted.messages[0]["content"], "compacted context");
    assert_eq!(compacted.messages[1]["content"], "retained turn");
    assert_eq!(
        compacted.source_event_ids,
        vec![None, Some("compacted-assistant".to_string())]
    );

    enqueue_message_removed(
        &mut journal,
        "new-in-memory-event".to_string(),
        serde_json::json!({"role": "assistant", "content": "retained turn"}),
    );
    install_test_journal(session_id, &mut journal);
    flush(session_id).await.expect("flush fallback removal");
    crate::agent_sessions::reset_session_store();

    let (removed_compacted, _) =
        prepare_test_journal(session_id, &options, "run-fallback", "turn-fallback").await;
    assert_eq!(removed_compacted.messages.len(), 1);
    assert_eq!(
        removed_compacted.messages[0]["content"],
        "compacted context"
    );
    crate::agent_sessions::reset_session_store();
}

/// One flush is one store transaction however many mutations it drains:
/// a per-mutation transaction costs a session-row read, a chain-tip
/// read, a row update, and a WAL commit for every event.
#[tokio::test]
async fn flush_persists_every_queued_mutation_in_one_transaction() {
    crate::agent_sessions::reset_session_store();
    let root = tempfile::tempdir().expect("temp root");
    let options = options(root.path());
    let session_id = "journal-one-transaction";
    let (_, mut journal) =
        prepare_test_journal(session_id, &options, "run-batch", "turn-batch").await;
    let store = journal.as_ref().expect("prepared journal").store();
    for index in 0..5 {
        enqueue_message(
            &mut journal,
            transcript_event(&format!("event-{index}"), "message", "user"),
            serde_json::json!({"role": "user", "content": format!("message {index}")}),
        );
    }
    enqueue_audit_event(
        &mut journal,
        transcript_event("event-audit", "turn_phase_changed", "system"),
    );
    install_test_journal(session_id, &mut journal);

    let before = store.append_transactions();
    flush(session_id).await.expect("flush queued mutations");
    assert_eq!(
        store.append_transactions() - before,
        1,
        "a flush must commit its queued mutations together"
    );
    flush(session_id).await.expect("flush an empty queue");
    assert_eq!(
        store.append_transactions() - before,
        1,
        "an empty flush must not open a transaction"
    );

    let events = session_store::read_all_events(&store, session_id)
        .await
        .expect("read canonical events");
    let sources: Vec<_> = events
        .iter()
        .map(|event| event.headers.get("source_event_id").cloned())
        .collect();
    assert_eq!(
        sources,
        [
            Some("event-0"),
            Some("event-1"),
            Some("event-2"),
            Some("event-3"),
            Some("event-4"),
            // Bookkeeping rows are not history positions, so they keep no source identity.
            None,
        ]
        .map(|id| id.map(str::to_string))
    );
    let report = store.verify(session_id).await.expect("verify chain");
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.event_count, 6);
    assert!(crate::agent_sessions::next_journal_event(session_id)
        .expect("inspect queue")
        .is_none());
    crate::agent_sessions::reset_session_store();
}

#[tokio::test]
async fn failed_append_keeps_the_mutation_queued() {
    crate::agent_sessions::reset_session_store();
    let root = tempfile::tempdir().expect("temp root");
    let options = options(root.path());
    let session_id = "journal-failed-append";
    let prepared = prepare(
        session_id,
        &options,
        "run-failure".to_string(),
        "turn-failure".to_string(),
    )
    .await
    .expect("prepare journal");
    let store = prepared.state.config.store.clone();
    crate::agent_sessions::open_or_create_for_test(Some(session_id.to_string()));
    let mut journal = Some(prepared.state);
    enqueue_message(
        &mut journal,
        transcript_event("event-failure", "message", "user"),
        serde_json::json!({"role": "user", "content": "must remain queued"}),
    );
    install_test_journal(session_id, &mut journal);
    store
        .close(session_id)
        .await
        .expect("close canonical session");

    let error = flush(session_id)
        .await
        .expect_err("append to a closed session must fail");
    assert!(error.to_string().contains("closed"));
    assert!(
        crate::agent_sessions::next_journal_event(session_id)
            .expect("inspect queued mutation")
            .is_some(),
        "a failed append must not discard its pending mutation"
    );
    crate::agent_sessions::reset_session_store();
}

#[tokio::test]
async fn a_second_writer_cannot_prepare_the_same_session_until_the_first_exits() {
    crate::agent_sessions::reset_session_store();
    let root = tempfile::tempdir().expect("temp root");
    let options = options(root.path());
    let session_id =
        crate::agent_sessions::open_or_create_for_test(Some("journal-active-run".into()));
    let first = prepare(
        &session_id,
        &options,
        "run-first".to_string(),
        "turn-first".to_string(),
    )
    .await
    .expect("prepare first journal");
    let error = match prepare(
        &session_id,
        &options,
        "run-second".to_string(),
        "turn-second".to_string(),
    )
    .await
    {
        Ok(_) => panic!("a live writer must exclude a second process before either appends"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("agent-run-writers"));

    crate::agent_sessions::install_journal(&session_id, first.state)
        .expect("install first journal");
    crate::agent_sessions::reset_session_store();

    prepare(
        &session_id,
        &options,
        "run-second".to_string(),
        "turn-second".to_string(),
    )
    .await
    .expect("the next invocation can acquire the released session lease");
}

#[tokio::test]
async fn maintenance_open_is_contended_by_a_live_vm_writer() {
    crate::agent_sessions::reset_session_store();
    let root = tempfile::tempdir().expect("temp root");
    let options = options(root.path());
    let state_dir = session_store::canonical_store_state_dir(Some(&options))
        .expect("resolve session store directory");
    let writer = prepare(
        "live-maintenance-race",
        &options,
        "run-live".to_string(),
        "turn-live".to_string(),
    )
    .await
    .expect("prepare live VM writer");

    let database = state_dir.as_path().join("session-store.sqlite");
    let error = match harn_session_store::SqliteSessionStore::open_for_maintenance(&database) {
        Ok(_) => panic!("live VM writer must exclude maintenance open"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        harn_session_store::StoreError::Contention {
            kind: harn_session_store::StoreContention::ProjectLeaseHeld,
            ..
        }
    ));

    drop(writer);
    let maintenance = harn_session_store::SqliteSessionStore::open_for_maintenance(&database)
        .expect("maintenance enters after live VM writer drops");
    let dormant = maintenance
        .describe("live-maintenance-race")
        .await
        .expect("resumable session remains after writer exits");
    assert_eq!(dormant.status, harn_session_store::SessionStatus::Open);
    let report = maintenance
        .sweep_retention(
            &harn_session_store::RetentionPolicy {
                max_age_seconds: Some(60),
                ..Default::default()
            },
            dormant.created_at_ms + 61_000,
        )
        .await
        .expect("age dormant canonical history under maintenance ownership");
    assert_eq!(report.soft_deleted, 1);
    assert_eq!(
        maintenance
            .describe(&dormant.id)
            .await
            .expect("retained tombstone")
            .status,
        harn_session_store::SessionStatus::SoftDeleted
    );
}

#[test]
fn singular_tool_call_message_uses_tool_call_row() {
    let event = append_event_for_payload(
        &mapping_config(),
        SessionEventPayload::Transcript(TranscriptRecord::new(
            transcript_event("event-call", "message", "assistant"),
            Some(serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id": "tool-1", "name": "look", "arguments": {}}],
            })),
        )),
    )
    .expect("map tool call");

    assert_eq!(event.kind, SessionEventKind::ToolCall);
    assert_eq!(
        event.headers.get("tool_call_id"),
        Some(&"tool-1".to_string())
    );
}

#[test]
fn tool_lifecycle_event_uses_tool_call_row_and_metadata_identity() {
    let event = append_event_for_payload(
        &mapping_config(),
        SessionEventPayload::Transcript(TranscriptRecord::new(
            serde_json::json!({
                "id": "event-call",
                "kind": "tool_call",
                "role": "assistant",
                "metadata": {"tool_call_id": "tool-1", "tool_name": "look"},
            }),
            None,
        )),
    )
    .expect("map lifecycle tool call");

    assert_eq!(event.kind, SessionEventKind::ToolCall);
    assert_eq!(
        event.headers.get("tool_call_id"),
        Some(&"tool-1".to_string())
    );
}

#[test]
fn plan_document_event_uses_the_canonical_plan_kind() {
    let event = append_event_for_payload(
        &mapping_config(),
        SessionEventPayload::Transcript(TranscriptRecord::new(
            serde_json::json!({
                "id": "plan-event",
                "kind": "plan_document",
                "role": "tool",
                "visibility": "public",
                "text": "",
                "metadata": {
                    "plan_document": {
                        "_type": "plan_document",
                        "schema_version": "harn.plan_document.v1"
                    }
                }
            }),
            None,
        )),
    )
    .expect("map plan document");

    assert_eq!(event.kind, SessionEventKind::Plan);
}

#[tokio::test(flavor = "current_thread")]
async fn completed_session_calls_are_billed_once_for_every_purpose() {
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
    let root = tempfile::tempdir().expect("temporary journal root");
    let session = "application-auxiliary-usage";
    let (_, mut journal) =
        prepare_test_journal(session, &options(root.path()), "run-aux", "turn-aux").await;
    install_test_journal(session, &mut journal);
    crate::agent_sessions::set_transcript_budget_policy(
        session,
        crate::agent_sessions::SessionTranscriptBudgetPolicy::reject(32, 1),
    )
    .expect("set a bounded conversation buffer");
    let source = r#"
import { agent_session_inject_reminder } from "std/agent/state"
import { with_llm_script } from "std/testing"

pipeline main(harness: Harness) {
  const session = "application-auxiliary-usage"
  agent_session_inject_reminder(
    harness.agent, session,
    {body: "Primary conversation contract", authority: "contract"},
  )
  with_llm_script(harness.llm, [
    {text: "classified", input_tokens: 10, output_tokens: 2, simulated_cost_usd: 0.018376},
    {text: "judged", input_tokens: 10, output_tokens: 2, simulated_cost_usd: 0.002},
    {text: "ready", input_tokens: 10, output_tokens: 2, simulated_cost_usd: 0.003},
  ], { ->
    for role in ["application.classifier", "completion.judge", "agent.main"] {
      harness.llm.call("Reply briefly.", nil, {provider: "mock", session_id: session, call_role: role})
    }
    const calls = harness.llm.mock_calls()
    assert(len(calls) == 3)
    for index in [0, 1, 2] {
      const request = json_stringify(calls[index].messages) + to_string(calls[index]?.system ?? "")
      harness.stdio.println("directed:" + to_string(contains(request, "Primary conversation contract")))
    }
  })
}
"#;
    let chunk = crate::compile_source(source).expect("compile application auxiliary calls");
    let output = tokio::task::LocalSet::new()
        .run_until(async {
            let mut vm = crate::new_vm();
            crate::register_vm_stdlib(&mut vm);
            vm.execute(&chunk)
                .await
                .expect("execute three declared call roles");
            vm.output().to_string()
        })
        .await;
    flush(session).await.expect("flush observed usage");
    let store = session_store::open_canonical_agent_session(
        &session_store::SessionStoreDir::under_root(root.path()),
        session,
        None,
        SessionType::User,
    )
    .await
    .expect("open durable usage");
    let events = session_store::read_all_events(&store, session)
        .await
        .expect("read observed usage");
    let roles = events
        .iter()
        .filter(|event| event.payload["transcript_event"]["kind"] == "llm_call")
        .map(|event| event.payload["transcript_event"]["metadata"]["call_role"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        roles,
        vec![
            serde_json::json!("application.classifier"),
            serde_json::json!("completion.judge"),
            serde_json::json!("agent.main")
        ],
        "every completed call must be billed independently of its purpose label"
    );
    assert_eq!(
        output
            .lines()
            .filter(|line| line.starts_with("directed:"))
            .collect::<Vec<_>>(),
        vec!["directed:true", "directed:false", "directed:true"],
        "usage observation must not redefine the existing directive scopes"
    );
    let run = crate::orchestration::project_run_record_from_session(&store, session)
        .await
        .expect("project observed helper usage");
    let usage = run.usage.expect("known nonzero usage must be present");
    assert_eq!(usage.call_count, 3);
    let cost = usage.cost_usd.expect("both helper prices must be known");
    assert!((cost - 0.023376).abs() < 1e-12, "{usage:?}");
    assert_eq!(
        crate::agent_sessions::transcript_budget_policy(session)
            .expect("conversation policy retained")
            .max_events,
        1,
        "recording audit evidence must not weaken the conversation limit"
    );
    let snapshot = crate::llm::helpers::vm_value_to_json(
        &crate::agent_sessions::snapshot(session).expect("live bounded session"),
    );
    assert!(
        snapshot["events"].as_array().expect("bounded events").len() <= 1,
        "durable accounting must not overflow the bounded snapshot: {snapshot}"
    );
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
}

#[tokio::test(flavor = "current_thread")]
async fn generic_agent_loop_projects_the_current_run_after_a_long_session_history() {
    generic_agent_loop_projects_the_current_run_for_format("native").await;
}

#[tokio::test]
async fn text_channel_agent_loop_keeps_tool_results_in_the_canonical_recap() {
    generic_agent_loop_projects_the_current_run_for_format("text").await;
}

async fn generic_agent_loop_projects_the_current_run_for_format(tool_format: &str) {
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
    let root = tempfile::tempdir().expect("temp root");
    let root_literal = serde_json::to_string(
        root.path()
            .to_str()
            .expect("temporary path must be valid UTF-8"),
    )
    .expect("serialize root literal");
    let session_id = "live-journal-agent-loop";
    let prior_event_count = crate::session_recap::DEFAULT_RECAP_SOURCE_LIMIT;
    let expected_first_event_id =
        u64::try_from(prior_event_count).expect("recap source limit fits an event id") + 1;
    let store = session_store::open_canonical_agent_session(
        &session_store::SessionStoreDir::under_root(root.path()),
        session_id,
        None,
        SessionType::User,
    )
    .await
    .expect("open canonical store for prior history");
    for index in 0..prior_event_count {
        store
            .append(
                session_id,
                AppendEvent::new(
                    SessionEventKind::Custom {
                        custom_type: "prior_run_fixture".to_string(),
                    },
                    serde_json::json!({"index": index}),
                ),
            )
            .await
            .expect("append prior history");
    }
    let source = format!(
        r#"
import {{ agent_loop }} from "std/agent/loop"

pipeline main(harness: Harness, task: unknown) {{
  harness.llm.mock_enqueue({{text: "", tool_calls: [{{
    id: "live-call",
    name: "agent_progress",
    arguments: {{
      message: "Finished the durable projection.",
      entries: [{{content: "Project recap facts", status: "completed"}}],
    }},
  }}]}})
  harness.llm.mock_enqueue({{text: "", tool_calls: [{{
    id: "known-result-one", name: "noop", arguments: {{query: "first"}},
  }}]}})
  harness.llm.mock_enqueue({{text: "", tool_calls: [{{
    id: "known-result-two", name: "noop", arguments: {{query: "second"}},
  }}]}})
  harness.llm.mock_enqueue({{text: "Completed the deterministic work.\n##DONE##"}})
  let tools = tool_registry()
  tools = tool_define(
    tools,
    "noop",
    "Return a deterministic result.",
    {{
      handler: {{ args -> "known durable result: " + args.query }},
      parameters: {{query: {{type: "string"}}}},
      returns: {{type: "string"}},
      annotations: {{kind: "read"}},
    }},
  )
  const result = agent_loop(
    harness,
    "Use noop, then finish.",
    nil,
    {{
      provider: "mock",
      model: "mock",
      call_role: "application.primary",
      root: {root_literal},
      session_id: "live-journal-agent-loop",
      tools: tools,
      progress_tool: {{}},
      tool_format: "{tool_format}",
      loop_until_done: true,
      iteration_budget: {{
        mode: "adaptive", initial: 2, max: 6, extend_by: 2, expose_decisions: true,
      }},
    }},
  )
  harness.stdio.println(result.status)
  harness.stdio.println(result.recap.state)
  harness.stdio.println(result.recap.snapshot.coverage.scanned)
  harness.stdio.println(result.recap.snapshot.coverage.matched)
  harness.stdio.println(result.recap.snapshot.query.fromEventId)
  harness.stdio.println(result.recap.snapshot.projectionHash)
  assert(result.adaptive_budget.extensions_used > 0)
  assert(result.adaptive_budget.decisions[0].action == "extend")
  harness.stdio.println("budget:" + json_stringify(result.adaptive_budget))
  harness.stdio.println("recap:" + json_stringify(result.recap.snapshot))
}}
"#,
    );
    let chunk = crate::compile_source(&source).expect("compile agent loop");
    let local = tokio::task::LocalSet::new();
    let output = local
        .run_until(async {
            let mut vm = crate::new_vm();
            crate::register_vm_stdlib(&mut vm);
            vm.execute(&chunk).await.expect("run agent loop");
            vm.output().to_string()
        })
        .await;
    assert!(output.lines().any(|line| line.ends_with("done")));
    let output_lines = output.lines().collect::<Vec<_>>();
    let recap_line = output_lines
        .iter()
        .position(|line| *line == "available")
        .expect("terminal AgentResult must expose an available recap");
    let scanned = output_lines
        .get(recap_line + 1)
        .and_then(|line| line.parse::<usize>().ok())
        .expect("recap must expose a numeric scanned count");
    let matched = output_lines
        .get(recap_line + 2)
        .and_then(|line| line.parse::<usize>().ok())
        .expect("recap must expose a numeric matched count");
    let from_event_id = output_lines
        .get(recap_line + 3)
        .and_then(|line| line.parse::<u64>().ok())
        .expect("recap must expose its first source event");
    let projection_hash = output_lines
        .get(recap_line + 4)
        .expect("recap must expose its projection hash");
    assert!(
        scanned > 0 && matched > 0,
        "terminal AgentResult must expose a non-vacuous recap: {output}"
    );
    assert_eq!(
        from_event_id, expected_first_event_id,
        "terminal recap must begin at the first event in this invocation: {output}"
    );
    assert!(
        projection_hash.starts_with("sha256:"),
        "terminal AgentResult must expose the recap projection hash: {output}"
    );
    let recap: crate::session_recap::SessionRecapSnapshot = serde_json::from_str(
        output
            .lines()
            .find_map(|line| line.strip_prefix("recap:"))
            .expect("returned recap"),
    )
    .expect("typed recap snapshot");
    assert_eq!(
        recap.coverage.unassigned, 0,
        "completed {tool_format} loop left unassigned facts: {recap:?}"
    );
    assert!(!recap.turns.is_empty());
    assert!(
        recap
            .turns
            .iter()
            .all(|turn| turn.state == crate::session_recap::RecapCompletionState::Complete),
        "a completed real loop must close its recap: {recap:?}"
    );
    let tools = recap
        .turns
        .iter()
        .flat_map(|turn| &turn.iterations)
        .flat_map(|iteration| &iteration.tools)
        .collect::<Vec<_>>();
    assert_eq!(
        tools.len(),
        3,
        "every real dispatched tool must appear: {tools:?}"
    );
    assert!(
        tools
            .iter()
            .all(|tool| tool.call_observed && tool.result_observed),
        "every dispatched call must retain its actual result in {tool_format}: {tools:?}"
    );
    let known = tools
        .iter()
        .find(|tool| tool.tool_call_id == "known-result-one")
        .expect("the known result must be reached");
    let expected_output = "[result of noop]\nknown durable result: first\n[end of noop result]\n";
    assert_eq!(known.output, Some(serde_json::json!(expected_output)));

    let store = session_store::open_canonical_agent_session(
        &session_store::SessionStoreDir::under_root(root.path()),
        session_id,
        None,
        SessionType::User,
    )
    .await
    .expect("open canonical store");
    let events = session_store::read_all_events(&store, session_id)
        .await
        .expect("read canonical events");
    let calls = events
        .iter()
        .filter(|event| event.payload["transcript_event"]["kind"] == "llm_call")
        .collect::<Vec<_>>();
    assert_eq!(
        calls.len(),
        4,
        "accepting the four primary turns must not duplicate observed usage"
    );
    assert!(calls.iter().all(
        |event| event.payload["transcript_event"]["metadata"]["call_role"] == "application.primary"
    ));
    let call_ids = calls
        .iter()
        .map(|event| {
            event.payload["transcript_event"]["metadata"]["call_id"]
                .as_str()
                .expect("correlated call ID")
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        call_ids.len(),
        4,
        "each completed call must be correlated exactly once"
    );
    let publication = events
        .iter()
        .find(|event| event.payload["transcript_event"]["kind"] == "assistant_publication")
        .expect("the real loop must publish its final answer");
    assert_eq!(
        publication.payload["transcript_event"]["visibility"],
        "public"
    );
    assert_eq!(
        publication.payload["transcript_event"]["text"]
            .as_str()
            .map(str::trim),
        Some("Completed the deterministic work.")
    );
    assert!(
        recap
            .source
            .events
            .iter()
            .any(|source| source.event_id == publication.event_id),
        "the recap's source bound must include the published answer"
    );
    assert!(
        recap.turns.iter().flat_map(|turn| &turn.iterations).any(|iteration| {
            iteration.assistant_text.iter().any(|fact| {
                fact.source_event_id == publication.event_id
                    && fact.text.trim() == "Completed the deterministic work."
            })
        }),
        "the recap must project the final answer itself, not only cover its source event: {recap:?}"
    );

    let run_started = events
        .iter()
        .find(|event| {
            matches!(
                &event.kind,
                SessionEventKind::Custom { custom_type }
                    if custom_type == "agent_run_started"
            )
        })
        .expect("run start must be durable");
    assert_eq!(run_started.event_id, expected_first_event_id);

    let user_message = events
        .iter()
        .find(|event| {
            event.kind == SessionEventKind::Message
                && event.payload["raw_message"]["role"] == "user"
        })
        .expect("user message must be durable");
    assert_eq!(
        user_message.payload["raw_message"]["content"],
        "Use noop, then finish."
    );
    let run_id = user_message
        .headers
        .get("run_id")
        .expect("user message must carry a run identity");
    let turn_id = user_message
        .headers
        .get("turn_id")
        .expect("user message must carry a turn identity");
    assert!(!run_id.is_empty());
    assert!(!turn_id.is_empty());

    let assistant_tool_call = events
        .iter()
        .find(|event| {
            event.kind == SessionEventKind::ToolCall
                && event.payload["transcript_event"]["kind"] == "tool_call"
        })
        .expect("the dispatched tool call must be durable on every channel");
    assert!(
        events.iter().any(|event| {
            event.payload["raw_message"]["role"] == "assistant"
                && event.payload["raw_message"]["_harn"]["tool_calls"][0]["id"] == "live-call"
        }),
        "the provider's assistant message must retain its parsed call facts"
    );
    let tool_call_id = assistant_tool_call
        .headers
        .get("tool_call_id")
        .expect("assistant tool call must carry a correlation id");
    assert!(!tool_call_id.is_empty());
    assert!(events.iter().any(|event| {
        event.kind == SessionEventKind::ToolCall
            && event.payload["transcript_event"]["kind"] == "tool_call"
            && event.headers.get("tool_call_id") == Some(tool_call_id)
    }));
    let progress = events
        .iter()
        .find(|event| {
            matches!(
                &event.kind,
                SessionEventKind::Custom { custom_type }
                    if custom_type == "progress_reported"
            )
        })
        .expect("progress report must be durable");
    assert_eq!(
        progress.payload["transcript_event"]["metadata"]["message"],
        "Finished the durable projection."
    );
    assert_eq!(
        progress.payload["transcript_event"]["visibility"], "internal",
        "durable progress must not become provider-visible history"
    );
    assert!(events.iter().any(|event| {
        event.kind == SessionEventKind::ToolResult
            && event.headers.get("tool_call_id") == Some(tool_call_id)
    }));
    assert!(events.iter().any(|event| {
        matches!(
            &event.kind,
            SessionEventKind::Custom { custom_type } if custom_type == "tool_call_update"
        ) && event.headers.get("tool_call_id") == Some(tool_call_id)
    }));
    assert!(events.iter().any(|event| {
        matches!(
            &event.kind,
            SessionEventKind::Custom { custom_type } if custom_type == "agent_run_terminal"
        )
    }));
    let current_events = events
        .iter()
        .filter(|event| event.event_id >= expected_first_event_id)
        .collect::<Vec<_>>();
    assert!(!current_events.is_empty());
    assert!(current_events.iter().all(|event| {
        event.headers.get("run_id") == Some(run_id) && event.headers.get("turn_id") == Some(turn_id)
    }));

    let projected = crate::orchestration::project_run_record_from_session(&store, session_id)
        .await
        .expect("project live session");
    assert_eq!(projected.started_at, run_started.ts);
    assert_eq!(
        projected.metadata["run_clock"]["started_at_source"],
        "agent_run_started"
    );
    assert_eq!(
        projected.metadata["run_clock"]["finished_at_source"],
        "agent_run_terminal"
    );
    assert_eq!(projected.tool_recordings.len(), 3);
    assert!(
        projected.tool_recordings[0].duration_ms.is_some(),
        "the live terminal tool update must retain its measured duration"
    );
    let known_result = projected
        .tool_recordings
        .iter()
        .find(|record| record.tool_use_id == "known-result-one")
        .expect("the known tool call must be recorded");
    assert!(
        known_result.result.contains("known durable result: first"),
        "the live journal must preserve the tool output: {known_result:?}"
    );
    let budget = projected
        .metadata
        .get("adaptive_budget")
        .expect("the sealed run must carry the exposed budget account");
    let returned_budget: serde_json::Value = serde_json::from_str(
        output
            .lines()
            .find_map(|line| line.strip_prefix("budget:"))
            .expect("the loop must return its budget account"),
    )
    .expect("parse returned budget account");
    assert_eq!(
        budget, &returned_budget,
        "the durable account must match the returned account exactly"
    );
    assert_eq!(budget["initial"], 2);
    assert_eq!(budget["extensions_used"], 1);
    assert_eq!(budget["decisions"][0]["action"], "extend");
    assert_eq!(budget["decisions"][0]["old_limit"], 2);
    assert_eq!(budget["decisions"][0]["new_limit"], 4);
    assert!(projected.evidence.execution_id.is_some());
    assert!(projected
        .evidence
        .gaps
        .iter()
        .all(|gap| gap.component != "execution_identity"));
    assert_eq!(
        crate::orchestration::validate_execution_evidence(&projected.evidence),
        Ok(())
    );
    let encoded = serde_json::to_vec(&projected).expect("encode projected run");
    let decoded: crate::orchestration::RunRecord =
        serde_json::from_slice(&encoded).expect("decode projected run");
    assert_eq!(
        crate::orchestration::validate_execution_evidence(&decoded.evidence),
        Ok(())
    );

    let hydrated = hydrate_events(events);
    assert!(hydrated.messages.iter().all(|message| {
        message.get("content").and_then(serde_json::Value::as_str)
            != Some("Finished the durable projection.")
    }));

    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
}
