use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::llm::receipts::ToolCallReceipt;
use crate::orchestration::MutationSessionRecord;

struct CountingSink(Arc<AtomicUsize>);
impl AgentEventSink for CountingSink {
    fn handle_event(&self, _event: &AgentEvent) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn multi_sink_fans_out_in_order() {
    let multi = MultiSink::new();
    let a = Arc::new(AtomicUsize::new(0));
    let b = Arc::new(AtomicUsize::new(0));
    multi.push(Arc::new(CountingSink(a.clone())));
    multi.push(Arc::new(CountingSink(b.clone())));
    let event = AgentEvent::IterationStart {
        session_id: "s1".into(),
        iteration: 1,
        provider: String::new(),
        model: String::new(),
    };
    multi.handle_event(&event);
    assert_eq!(a.load(Ordering::SeqCst), 1);
    assert_eq!(b.load(Ordering::SeqCst), 1);
}

#[test]
fn session_scoped_sink_routing() {
    reset_all_sinks();
    let a = Arc::new(AtomicUsize::new(0));
    let b = Arc::new(AtomicUsize::new(0));
    register_sink("session-a", Arc::new(CountingSink(a.clone())));
    register_sink("session-b", Arc::new(CountingSink(b.clone())));
    emit_event(&AgentEvent::IterationStart {
        session_id: "session-a".into(),
        iteration: 0,
        provider: String::new(),
        model: String::new(),
    });
    assert_eq!(a.load(Ordering::SeqCst), 1);
    assert_eq!(b.load(Ordering::SeqCst), 0);
    emit_event(&AgentEvent::IterationEnd {
        session_id: "session-b".into(),
        iteration: 0,
        iteration_info: serde_json::json!({}),
    });
    assert_eq!(a.load(Ordering::SeqCst), 1);
    assert_eq!(b.load(Ordering::SeqCst), 1);
    clear_session_sinks("session-a");
    assert_eq!(session_external_sink_count("session-a"), 0);
    assert_eq!(session_external_sink_count("session-b"), 1);
    reset_all_sinks();
}

#[test]
fn session_scoped_sink_routing_crosses_worker_threads() {
    reset_all_sinks();
    let delivered = Arc::new(AtomicUsize::new(0));
    let session_id = format!("session-cross-thread-{}", uuid::Uuid::now_v7());
    register_sink(&session_id, Arc::new(CountingSink(delivered.clone())));

    let emit_session_id = session_id.clone();
    std::thread::spawn(move || {
        emit_event(&AgentEvent::IterationStart {
            session_id: emit_session_id,
            iteration: 0,
            provider: String::new(),
            model: String::new(),
        });
    })
    .join()
    .expect("worker thread");

    assert_eq!(delivered.load(Ordering::SeqCst), 1);
    clear_session_sinks(&session_id);
    assert_eq!(session_external_sink_count(&session_id), 0);
}

#[test]
fn newly_opened_child_session_inherits_current_external_sinks() {
    reset_all_sinks();
    let delivered = Arc::new(AtomicUsize::new(0));
    register_sink("outer-session", Arc::new(CountingSink(delivered.clone())));
    {
        let _guard = crate::agent_sessions::enter_current_session("outer-session");
        let inner = crate::agent_sessions::open_or_create(None);
        assert_ne!(inner, "outer-session");
        emit_event(&AgentEvent::IterationStart {
            session_id: inner,
            iteration: 0,
            provider: String::new(),
            model: String::new(),
        });
    }
    assert_eq!(delivered.load(Ordering::SeqCst), 1);
    reset_all_sinks();
}

#[test]
fn jsonl_sink_writes_monotonic_indices_and_timestamps() {
    use std::io::{BufRead, BufReader};
    let dir = std::env::temp_dir().join(format!("harn-event-log-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("event_log.jsonl");
    let sink = JsonlEventSink::open(&path).unwrap();
    for i in 0..5 {
        sink.handle_event(&AgentEvent::IterationStart {
            session_id: "s".into(),
            iteration: i,
            provider: String::new(),
            model: String::new(),
        });
    }
    assert_eq!(sink.event_count(), 5);
    sink.flush().unwrap();

    // Read back + assert monotonic indices + non-decreasing timestamps.
    let file = std::fs::File::open(&path).unwrap();
    let mut last_idx: i64 = -1;
    let mut last_ts: i64 = 0;
    for line in BufReader::new(file).lines() {
        let line = line.unwrap();
        let val: serde_json::Value = serde_json::from_str(&line).unwrap();
        let idx = val["index"].as_i64().unwrap();
        let ts = val["emitted_at_ms"].as_i64().unwrap();
        assert_eq!(idx, last_idx + 1, "indices must be contiguous");
        assert!(ts >= last_ts, "timestamps must be non-decreasing");
        last_idx = idx;
        last_ts = ts;
        // Event payload flattened — type tag must survive.
        assert_eq!(val["type"], "iteration_start");
    }
    assert_eq!(last_idx, 4);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn judge_decision_round_trips_through_jsonl_sink() {
    use std::io::{BufRead, BufReader};
    let dir = std::env::temp_dir().join(format!("harn-judge-event-log-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("event_log.jsonl");
    let sink = JsonlEventSink::open(&path).unwrap();
    sink.handle_event(&AgentEvent::JudgeDecision {
        session_id: "s".into(),
        iteration: 2,
        verdict: "continue".into(),
        reasoning: "needs a concrete next step".into(),
        next_step: Some("run the verifier".into()),
        judge_duration_ms: 17,
        source: Some("llm".into()),
        trigger: Some("stalled".into()),
        reason: Some("missing_verification".into()),
        confirm: Some(false),
        converted_from: None,
        specific_gaps: vec!["rerun the verifier".into(), "cite the changed file".into()],
        accepted_evidence: Vec::new(),
    });
    sink.flush().unwrap();

    let file = std::fs::File::open(&path).unwrap();
    let line = BufReader::new(file).lines().next().unwrap().unwrap();
    let recovered: PersistedAgentEvent = serde_json::from_str(&line).unwrap();
    match recovered.event {
        AgentEvent::JudgeDecision {
            session_id,
            iteration,
            verdict,
            reasoning,
            next_step,
            judge_duration_ms,
            source,
            trigger,
            reason,
            confirm,
            converted_from,
            specific_gaps,
            accepted_evidence,
        } => {
            assert_eq!(session_id, "s");
            assert_eq!(iteration, 2);
            assert_eq!(verdict, "continue");
            assert_eq!(reasoning, "needs a concrete next step");
            assert_eq!(next_step.as_deref(), Some("run the verifier"));
            assert_eq!(judge_duration_ms, 17);
            assert_eq!(source.as_deref(), Some("llm"));
            assert_eq!(trigger.as_deref(), Some("stalled"));
            assert_eq!(reason.as_deref(), Some("missing_verification"));
            assert_eq!(confirm, Some(false));
            assert_eq!(converted_from, None);
            assert_eq!(
                specific_gaps,
                vec![
                    "rerun the verifier".to_string(),
                    "cite the changed file".to_string()
                ]
            );
            assert!(accepted_evidence.is_empty());
        }
        other => panic!("expected JudgeDecision, got {other:?}"),
    }
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["type"], "judge_decision");
    assert_eq!(value["source"], "llm");
    assert_eq!(
        value["specific_gaps"],
        serde_json::json!(["rerun the verifier", "cite the changed file"])
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn jsonl_sink_lines_are_durable_without_drop_or_explicit_flush() {
    let dir = std::env::temp_dir().join(format!("harn-durable-event-log-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("event_log.jsonl");
    let sink = JsonlEventSink::open(&path).unwrap();

    sink.handle_event(&AgentEvent::IterationEnd {
        session_id: "s".into(),
        iteration: 7,
        iteration_info: serde_json::json!({"reason": "verified"}),
    });
    sink.handle_event(&AgentEvent::TypedCheckpoint {
        session_id: "s".into(),
        checkpoint: serde_json::json!({"schema": "burin.completion.v1"}),
    });
    sink.handle_event(&AgentEvent::JudgeDecision {
        session_id: "s".into(),
        iteration: 7,
        verdict: "done".into(),
        reasoning: "verification passed".into(),
        next_step: None,
        judge_duration_ms: 11,
        source: Some("deterministic".into()),
        trigger: Some("completion_check".into()),
        reason: Some("verified_after_write".into()),
        confirm: Some(true),
        converted_from: None,
        specific_gaps: Vec::new(),
        accepted_evidence: vec!["targeted verifier passed".into()],
    });

    let text = std::fs::read_to_string(&path).unwrap();
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        3,
        "tail events must be readable before sink drop"
    );
    assert!(
        text.contains("\"type\":\"iteration_end\""),
        "iteration_end tail must be line-durable"
    );
    assert!(
        text.contains("\"type\":\"typed_checkpoint\""),
        "typed_checkpoint tail must be line-durable"
    );
    assert!(
        text.contains("\"type\":\"judge_decision\""),
        "judge_decision tail must be line-durable"
    );

    drop(sink);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn jsonl_sink_redacts_tool_payloads_before_write() {
    let dir =
        std::env::temp_dir().join(format!("harn-redacted-event-log-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("event_log.jsonl");
    let sink = JsonlEventSink::open(&path).unwrap();

    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "s".into(),
        tool_call_id: "tc-1".into(),
        tool_name: "http".into(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({
            "api_key": "raw-api-key-value",
            "url": "https://user:password@example.com/items?client_secret=raw-client-secret&ok=1"
        }),
        parsing: None,
        audit: None,
    });
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "s".into(),
        tool_call_id: "tc-1".into(),
        tool_name: "http".into(),
        status: ToolCallStatus::Completed,
        raw_output: Some(serde_json::json!({
            "callback": "https://api.example.com/cb?access_token=raw-access-token&ok=1"
        })),
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: crate::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });
    sink.flush().unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[redacted]") || text.contains("%5Bredacted%5D"));
    for secret in [
        "raw-api-key-value",
        "user:password",
        "raw-client-secret",
        "raw-access-token",
    ] {
        assert!(
            !text.contains(secret),
            "JSONL event sink persisted secret {secret}: {text}"
        );
    }

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn jsonl_sink_skips_text_parsing_candidates() {
    use std::io::{BufRead, BufReader};

    let dir = std::env::temp_dir().join(format!(
        "harn-candidate-filter-event-log-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("event_log.jsonl");
    let sink = JsonlEventSink::open(&path).unwrap();

    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "s".into(),
        tool_call_id: "text-cand-0".into(),
        tool_name: "read".into(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({}),
        parsing: Some(true),
        audit: None,
    });
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "s".into(),
        tool_call_id: "text-cand-0".into(),
        tool_name: "read".into(),
        status: ToolCallStatus::Failed,
        raw_output: None,
        error: Some("candidate aborted".into()),
        duration_ms: None,
        execution_duration_ms: None,
        error_category: Some(ToolCallErrorCategory::ParseAborted),
        mutation_status: crate::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        executor: None,
        parsing: Some(false),
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });
    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "s".into(),
        tool_call_id: "call-real".into(),
        tool_name: "read".into(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({"path": "README.md"}),
        parsing: None,
        audit: None,
    });
    sink.flush().unwrap();

    assert_eq!(
        sink.event_count(),
        1,
        "skipped candidates must not consume indices"
    );
    let file = std::fs::File::open(&path).unwrap();
    let lines = BufReader::new(file)
        .lines()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(lines.len(), 1);
    let value: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(value["index"], serde_json::json!(0));
    assert_eq!(value["tool_call_id"], "call-real");
    assert!(!lines[0].contains("text-cand-0"));

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn event_log_sink_redacts_tool_payloads_before_append() {
    use crate::event_log::{AnyEventLog, EventLog, MemoryEventLog, Topic};

    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(8)));
    let sink = EventLogSink::new(log.clone(), "s");
    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "s".into(),
        tool_call_id: "tc-1".into(),
        tool_name: "http".into(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({
            "authorization": "Bearer raw-bearer-value",
            "url": "https://user:password@example.com/items?sig=raw-signature&ok=1"
        }),
        parsing: None,
        audit: None,
    });

    let topic = Topic::new("observability.agent_events.s").unwrap();
    let events = futures::executor::block_on(log.read_range(&topic, None, 8)).unwrap();
    assert_eq!(events.len(), 1);
    let persisted = serde_json::to_string(&events[0].1).unwrap();
    assert!(persisted.contains("[redacted]") || persisted.contains("%5Bredacted%5D"));
    for secret in ["raw-bearer-value", "user:password", "raw-signature"] {
        assert!(
            !persisted.contains(secret),
            "event-log sink appended secret {secret}: {persisted}"
        );
    }
}

#[test]
fn event_log_sink_skips_text_parsing_candidates() {
    use crate::event_log::{AnyEventLog, EventLog, MemoryEventLog, Topic};

    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(8)));
    let sink = EventLogSink::new(log.clone(), "s");
    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "s".into(),
        tool_call_id: "text-cand-0".into(),
        tool_name: "read".into(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({}),
        parsing: Some(true),
        audit: None,
    });
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "s".into(),
        tool_call_id: "call-real".into(),
        tool_name: "read".into(),
        status: ToolCallStatus::Completed,
        raw_output: Some(serde_json::json!({"text": "ok"})),
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: crate::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });

    let topic = Topic::new("observability.agent_events.s").unwrap();
    let events = futures::executor::block_on(log.read_range(&topic, None, 8)).unwrap();
    assert_eq!(events.len(), 1);
    let persisted = serde_json::to_string(&events[0].1).unwrap();
    assert!(persisted.contains("call-real"));
    assert!(!persisted.contains("text-cand-0"));
}

#[test]
fn structural_validator_decision_round_trips_through_jsonl_sink() {
    use std::io::{BufRead, BufReader};
    let dir = std::env::temp_dir().join(format!(
        "harn-structural-validator-event-log-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("event_log.jsonl");
    let sink = JsonlEventSink::open(&path).unwrap();
    sink.handle_event(&AgentEvent::StructuralValidatorDecision {
        session_id: "s".into(),
        iteration: 2,
        rule: "non_empty_when_writes_expected".into(),
        diagnostic: "Assistant emitted no tool calls while writable tools were available.".into(),
        recommended_action: "Emit the concrete write or edit tool call needed for the task.".into(),
        vetoed: true,
        skipped: false,
        reason: None,
        on_failure: "regenerate_with_feedback".into(),
        attempts: 1,
        max_attempts: 3,
    });
    sink.flush().unwrap();

    let file = std::fs::File::open(&path).unwrap();
    let line = BufReader::new(file).lines().next().unwrap().unwrap();
    let recovered: PersistedAgentEvent = serde_json::from_str(&line).unwrap();
    match recovered.event {
        AgentEvent::StructuralValidatorDecision {
            session_id,
            iteration,
            rule,
            diagnostic,
            recommended_action,
            vetoed,
            skipped,
            reason,
            on_failure,
            attempts,
            max_attempts,
        } => {
            assert_eq!(session_id, "s");
            assert_eq!(iteration, 2);
            assert_eq!(rule, "non_empty_when_writes_expected");
            assert_eq!(
                diagnostic,
                "Assistant emitted no tool calls while writable tools were available."
            );
            assert_eq!(
                recommended_action,
                "Emit the concrete write or edit tool call needed for the task."
            );
            assert!(vetoed);
            assert!(!skipped);
            assert!(reason.is_none());
            assert_eq!(on_failure, "regenerate_with_feedback");
            assert_eq!(attempts, 1);
            assert_eq!(max_attempts, 3);
        }
        other => panic!("expected StructuralValidatorDecision, got {other:?}"),
    }
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["type"], "structural_validator_decision");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn scope_classifier_verdict_round_trips_through_jsonl_sink() {
    use std::io::{BufRead, BufReader};
    let dir = std::env::temp_dir().join(format!(
        "harn-scope-classifier-event-log-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("event_log.jsonl");
    let sink = JsonlEventSink::open(&path).unwrap();
    sink.handle_event(&AgentEvent::ScopeClassifierVerdict {
        session_id: "s".into(),
        iteration: 1,
        label: "out_of_scope".into(),
        original_label: "out_of_scope".into(),
        confidence: 0.91,
        confidence_threshold: 0.65,
        evidence: "mentions /workspace/other".into(),
        skip_main_turn: true,
        classifier_kind: Some("custom".into()),
        model: None,
        error: None,
    });
    sink.flush().unwrap();

    let file = std::fs::File::open(&path).unwrap();
    let line = BufReader::new(file).lines().next().unwrap().unwrap();
    let recovered: PersistedAgentEvent = serde_json::from_str(&line).unwrap();
    match recovered.event {
        AgentEvent::ScopeClassifierVerdict {
            session_id,
            iteration,
            label,
            confidence,
            evidence,
            skip_main_turn,
            classifier_kind,
            ..
        } => {
            assert_eq!(session_id, "s");
            assert_eq!(iteration, 1);
            assert_eq!(label, "out_of_scope");
            assert_eq!(confidence, 0.91);
            assert_eq!(evidence, "mentions /workspace/other");
            assert!(skip_main_turn);
            assert_eq!(classifier_kind.as_deref(), Some("custom"));
        }
        other => panic!("expected ScopeClassifierVerdict, got {other:?}"),
    }
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["type"], "scope_classifier_verdict");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn input_guardrail_verdict_round_trips_through_jsonl_sink() {
    use std::io::{BufRead, BufReader};
    let dir = std::env::temp_dir().join(format!(
        "harn-input-guardrail-event-log-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("event_log.jsonl");
    let sink = JsonlEventSink::open(&path).unwrap();
    sink.handle_event(&AgentEvent::InputGuardrailVerdict {
        session_id: "s".into(),
        iteration: 1,
        tripwire: true,
        reason: "policy denied".into(),
        label: "prompt_injection".into(),
        confidence: 0.97,
        confidence_threshold: 0.8,
        classifier_kind: Some("custom".into()),
        model: None,
        error: None,
    });
    sink.flush().unwrap();

    let file = std::fs::File::open(&path).unwrap();
    let line = BufReader::new(file).lines().next().unwrap().unwrap();
    let recovered: PersistedAgentEvent = serde_json::from_str(&line).unwrap();
    match recovered.event {
        AgentEvent::InputGuardrailVerdict {
            session_id,
            iteration,
            tripwire,
            reason,
            label,
            confidence,
            classifier_kind,
            ..
        } => {
            assert_eq!(session_id, "s");
            assert_eq!(iteration, 1);
            assert!(tripwire);
            assert_eq!(reason, "policy denied");
            assert_eq!(label, "prompt_injection");
            assert_eq!(confidence, 0.97);
            assert_eq!(classifier_kind.as_deref(), Some("custom"));
        }
        other => panic!("expected InputGuardrailVerdict, got {other:?}"),
    }
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["type"], "input_guardrail_verdict");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn missing_tool_call_verdict_round_trips_through_jsonl_sink() {
    use std::io::{BufRead, BufReader};
    let dir = std::env::temp_dir().join(format!(
        "harn-missing-tool-call-event-log-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("event_log.jsonl");
    let sink = JsonlEventSink::open(&path).unwrap();
    sink.handle_event(&AgentEvent::MissingToolCallVerdict {
        session_id: "s".into(),
        iteration: 2,
        action: "tool_call_intended".into(),
        original_action: "tool_call_intended".into(),
        tool_name: "edit".into(),
        confidence: 0.96,
        confidence_threshold: 0.65,
        evidence: "assistant described editing without a call".into(),
        language: Some("es".into()),
        classifier_kind: Some("custom".into()),
        model: None,
        error: None,
    });
    sink.flush().unwrap();

    let file = std::fs::File::open(&path).unwrap();
    let line = BufReader::new(file).lines().next().unwrap().unwrap();
    let recovered: PersistedAgentEvent = serde_json::from_str(&line).unwrap();
    match recovered.event {
        AgentEvent::MissingToolCallVerdict {
            session_id,
            iteration,
            action,
            tool_name,
            confidence,
            evidence,
            language,
            classifier_kind,
            ..
        } => {
            assert_eq!(session_id, "s");
            assert_eq!(iteration, 2);
            assert_eq!(action, "tool_call_intended");
            assert_eq!(tool_name, "edit");
            assert_eq!(confidence, 0.96);
            assert_eq!(evidence, "assistant described editing without a call");
            assert_eq!(language.as_deref(), Some("es"));
            assert_eq!(classifier_kind.as_deref(), Some("custom"));
        }
        other => panic!("expected MissingToolCallVerdict, got {other:?}"),
    }
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["type"], "missing_tool_call_verdict");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn tool_call_update_durations_serialize_when_present_and_skip_when_absent() {
    // Terminal update with both durations populated — both fields
    // appear in the JSON. Snake_case keys here because this is the
    // canonical AgentEvent shape; the ACP adapter renames to
    // camelCase separately.
    let terminal = AgentEvent::ToolCallUpdate {
        session_id: "s".into(),
        tool_call_id: "tc-1".into(),
        tool_name: "read".into(),
        status: ToolCallStatus::Completed,
        raw_output: None,
        error: None,
        duration_ms: Some(42),
        execution_duration_ms: Some(7),
        error_category: None,
        mutation_status: crate::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        executor: None,
        parsing: None,

        raw_input: None,
        raw_input_partial: None,
        audit: None,
    };
    let value = serde_json::to_value(&terminal).unwrap();
    assert_eq!(value["duration_ms"], serde_json::json!(42));
    assert_eq!(value["execution_duration_ms"], serde_json::json!(7));

    // In-progress update with `None` for both — both keys must be
    // absent (not `null`) so older ACP clients that key off
    // presence don't see a misleading zero.
    let intermediate = AgentEvent::ToolCallUpdate {
        session_id: "s".into(),
        tool_call_id: "tc-1".into(),
        tool_name: "read".into(),
        status: ToolCallStatus::InProgress,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: crate::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        executor: None,
        parsing: None,

        raw_input: None,
        raw_input_partial: None,
        audit: None,
    };
    let value = serde_json::to_value(&intermediate).unwrap();
    let object = value.as_object().expect("update serializes as object");
    assert!(
        !object.contains_key("duration_ms"),
        "duration_ms must be omitted when None: {value}"
    );
    assert!(
        !object.contains_key("execution_duration_ms"),
        "execution_duration_ms must be omitted when None: {value}"
    );
}

#[test]
fn tool_call_update_deserializes_without_duration_fields_for_back_compat() {
    // Persisted event-log entries written before the fields existed
    // must still deserialize cleanly. The missing keys map to None.
    let raw = serde_json::json!({
        "type": "tool_call_update",
        "session_id": "s",
        "tool_call_id": "tc-1",
        "tool_name": "read",
        "status": "completed",
        "mutation_status": "unknown",
        "raw_output": null,
        "error": null,
    });
    let event: AgentEvent = serde_json::from_value(raw).expect("parses without duration keys");
    match event {
        AgentEvent::ToolCallUpdate {
            duration_ms,
            execution_duration_ms,
            ..
        } => {
            assert!(duration_ms.is_none());
            assert!(execution_duration_ms.is_none());
        }
        other => panic!("expected ToolCallUpdate, got {other:?}"),
    }
}

#[test]
fn tool_call_status_serde() {
    assert_eq!(
        serde_json::to_string(&ToolCallStatus::Pending).unwrap(),
        "\"pending\""
    );
    assert_eq!(
        serde_json::to_string(&ToolCallStatus::InProgress).unwrap(),
        "\"in_progress\""
    );
    assert_eq!(
        serde_json::to_string(&ToolCallStatus::Completed).unwrap(),
        "\"completed\""
    );
    assert_eq!(
        serde_json::to_string(&ToolCallStatus::Failed).unwrap(),
        "\"failed\""
    );
}

#[test]
fn tool_mutation_status_serde_and_terminal_update_round_trip() {
    for (status, expected) in [
        (ToolMutationStatus::Applied, "applied"),
        (ToolMutationStatus::NotApplied, "not_applied"),
        (ToolMutationStatus::Unknown, "unknown"),
    ] {
        assert_eq!(status.as_str(), expected);
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            format!("\"{expected}\"")
        );

        let event = AgentEvent::ToolCallUpdate {
            session_id: "s".into(),
            tool_call_id: "tc-1".into(),
            tool_name: "edit".into(),
            status: ToolCallStatus::Completed,
            raw_output: Some(serde_json::json!({"ok": true})),
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            mutation_status: status,
            changed_paths: None,
            executor: None,
            parsing: None,
            raw_input: None,
            raw_input_partial: None,
            audit: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["mutation_status"], expected);
        let recovered: AgentEvent = serde_json::from_value(value).unwrap();
        assert!(matches!(
            recovered,
            AgentEvent::ToolCallUpdate {
                mutation_status: recovered,
                ..
            } if recovered == status
        ));
    }
}

#[test]
fn tool_call_update_rejects_missing_mutation_status() {
    let error = serde_json::from_value::<AgentEvent>(serde_json::json!({
        "type": "tool_call_update",
        "session_id": "s",
        "tool_call_id": "tc-1",
        "tool_name": "edit",
        "status": "completed",
        "raw_output": null,
        "error": null,
    }))
    .expect_err("mutation_status is required on every tool_call_update");
    assert!(error
        .to_string()
        .contains("missing field `mutation_status`"));
}

#[test]
fn tool_call_update_event_omits_error_category_when_none() {
    let event = AgentEvent::ToolCallUpdate {
        session_id: "s".into(),
        tool_call_id: "t".into(),
        tool_name: "read".into(),
        status: ToolCallStatus::Completed,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: crate::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        executor: None,
        parsing: None,

        raw_input: None,
        raw_input_partial: None,
        audit: None,
    };
    let v = serde_json::to_value(&event).unwrap();
    assert_eq!(v["type"], "tool_call_update");
    assert!(v.get("error_category").is_none());
}

#[test]
fn tool_call_update_event_serializes_error_category_when_set() {
    let event = AgentEvent::ToolCallUpdate {
        session_id: "s".into(),
        tool_call_id: "t".into(),
        tool_name: "read".into(),
        status: ToolCallStatus::Failed,
        raw_output: None,
        error: Some("missing required field".into()),
        duration_ms: None,
        execution_duration_ms: None,
        error_category: Some(ToolCallErrorCategory::SchemaValidation),
        mutation_status: crate::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        executor: None,
        parsing: None,

        raw_input: None,
        raw_input_partial: None,
        audit: None,
    };
    let v = serde_json::to_value(&event).unwrap();
    assert_eq!(v["error_category"], "schema_validation");
    assert_eq!(v["error"], "missing required field");
}

#[test]
fn tool_call_update_omits_executor_when_absent() {
    // `executor: None` must not appear in the serialized event so
    // the on-disk shape stays backward-compatible with replays
    // recorded before harn#691.
    let event = AgentEvent::ToolCallUpdate {
        session_id: "s".into(),
        tool_call_id: "tc-1".into(),
        tool_name: "read".into(),
        status: ToolCallStatus::Completed,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: crate::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        executor: None,
        parsing: None,

        raw_input: None,
        raw_input_partial: None,
        audit: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert!(json.get("executor").is_none(), "got: {json}");
}

#[test]
fn worker_event_status_strings_cover_all_variants() {
    // Wire-level status strings flow into both bridge `worker_update`
    // payloads and ACP `session/update` notifications. Pinning every
    // variant here so a future addition can't silently land without
    // a docs/wire decision.
    assert_eq!(WorkerEvent::WorkerSpawned.as_status(), "running");
    assert_eq!(WorkerEvent::WorkerProgressed.as_status(), "progressed");
    assert_eq!(
        WorkerEvent::WorkerWaitingForInput.as_status(),
        "awaiting_input"
    );
    assert_eq!(WorkerEvent::WorkerSuspended.as_status(), "suspended");
    assert_eq!(WorkerEvent::WorkerResumed.as_status(), "running");
    assert_eq!(WorkerEvent::WorkerCompleted.as_status(), "completed");
    assert_eq!(WorkerEvent::WorkerFailed.as_status(), "failed");
    assert_eq!(WorkerEvent::WorkerStopped.as_status(), "stopped");
    assert_eq!(WorkerEvent::WorkerCancelled.as_status(), "cancelled");

    for terminal in [
        WorkerEvent::WorkerCompleted,
        WorkerEvent::WorkerFailed,
        WorkerEvent::WorkerStopped,
        WorkerEvent::WorkerCancelled,
    ] {
        assert!(terminal.is_terminal(), "{terminal:?} should be terminal");
    }
    for non_terminal in [
        WorkerEvent::WorkerSpawned,
        WorkerEvent::WorkerProgressed,
        WorkerEvent::WorkerWaitingForInput,
        WorkerEvent::WorkerSuspended,
        WorkerEvent::WorkerResumed,
    ] {
        assert!(
            !non_terminal.is_terminal(),
            "{non_terminal:?} should not be terminal"
        );
    }

    // `ALL` is the iteration order downstream protocol-artifact
    // dumpers walk; keep it in lockstep with the variant list so a
    // new event doesn't slip in without a wire-status decision.
    let collected: Vec<&'static str> = WorkerEvent::ALL
        .iter()
        .map(|event| event.as_status())
        .collect();
    assert_eq!(
        collected,
        vec![
            "running",
            "progressed",
            "awaiting_input",
            "suspended",
            "running",
            "completed",
            "failed",
            "stopped",
            "cancelled",
        ]
    );
}

#[test]
fn worker_update_event_routes_through_session_keyed_sink() {
    // Worker lifecycle events ride the same session-keyed
    // `AgentEventSink` registry as message and tool events. This
    // is the canonical path ACP and A2A subscribe to — gate it
    // here so a registry-routing regression breaks loudly.
    reset_all_sinks();
    let captured: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    struct CapturingSink(Arc<Mutex<Vec<AgentEvent>>>);
    impl AgentEventSink for CapturingSink {
        fn handle_event(&self, event: &AgentEvent) {
            self.0
                .lock()
                .expect("captured sink mutex poisoned")
                .push(event.clone());
        }
    }
    register_sink(
        "worker-session-1",
        Arc::new(CapturingSink(captured.clone())),
    );
    emit_event(&AgentEvent::WorkerUpdate {
        session_id: "worker-session-1".into(),
        worker_id: "worker_42".into(),
        worker_name: "review_captain".into(),
        worker_task: "review pr".into(),
        worker_mode: "delegated_stage".into(),
        event: WorkerEvent::WorkerWaitingForInput,
        status: WorkerEvent::WorkerWaitingForInput.as_status().to_string(),
        metadata: serde_json::json!({"awaiting_started_at": "0193..."}),
        audit: None,
    });
    // Other sessions don't receive cross-talk.
    emit_event(&AgentEvent::WorkerUpdate {
        session_id: "other-session".into(),
        worker_id: "w2".into(),
        worker_name: "n2".into(),
        worker_task: "t2".into(),
        worker_mode: "delegated_stage".into(),
        event: WorkerEvent::WorkerCompleted,
        status: "completed".into(),
        metadata: serde_json::json!({}),
        audit: None,
    });
    let received = captured.lock().unwrap().clone();
    assert_eq!(received.len(), 1, "got: {received:?}");
    match &received[0] {
        AgentEvent::WorkerUpdate {
            session_id,
            worker_id,
            event,
            status,
            ..
        } => {
            assert_eq!(session_id, "worker-session-1");
            assert_eq!(worker_id, "worker_42");
            assert_eq!(*event, WorkerEvent::WorkerWaitingForInput);
            assert_eq!(status, "awaiting_input");
        }
        other => panic!("expected WorkerUpdate, got {other:?}"),
    }
    reset_all_sinks();
}

#[test]
fn worker_update_event_serializes_to_canonical_shape() {
    // Persisted event-log entries flatten the AgentEvent envelope,
    // so the WorkerUpdate variant must serialize with a `type` of
    // `worker_update` and the worker fields directly on the
    // top-level object (matching the `#[serde(tag = "type", ...)]`
    // shape the rest of AgentEvent uses).
    let event = AgentEvent::WorkerUpdate {
        session_id: "s".into(),
        worker_id: "w".into(),
        worker_name: "n".into(),
        worker_task: "t".into(),
        worker_mode: "delegated_stage".into(),
        event: WorkerEvent::WorkerProgressed,
        status: "progressed".into(),
        metadata: serde_json::json!({"started_at": "0193..."}),
        audit: Some(serde_json::json!({"run_id": "run_x"})),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "worker_update");
    assert_eq!(value["session_id"], "s");
    assert_eq!(value["worker_id"], "w");
    assert_eq!(value["status"], "progressed");
    assert_eq!(value["audit"]["run_id"], "run_x");

    // Round-trip: the persisted event log must deserialize the
    // canonical shape back into the typed variant so replay
    // tooling can re-derive lifecycle state offline.
    let recovered: AgentEvent = serde_json::from_value(value).unwrap();
    match recovered {
        AgentEvent::WorkerUpdate {
            event: recovered_event,
            ..
        } => assert_eq!(recovered_event, WorkerEvent::WorkerProgressed),
        other => panic!("expected WorkerUpdate, got {other:?}"),
    }
}

#[test]
fn tool_call_update_includes_executor_when_present() {
    let event = AgentEvent::ToolCallUpdate {
        session_id: "s".into(),
        tool_call_id: "tc-1".into(),
        tool_name: "read".into(),
        status: ToolCallStatus::Completed,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: crate::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        executor: Some(ToolExecutor::McpServer {
            server_name: "github".into(),
        }),
        parsing: None,

        raw_input: None,
        raw_input_partial: None,
        audit: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["executor"]["kind"], "mcp_server");
    assert_eq!(json["executor"]["server_name"], "github");
}

#[test]
fn tool_call_update_omits_audit_when_absent() {
    let event = AgentEvent::ToolCallUpdate {
        session_id: "s".into(),
        tool_call_id: "tc-1".into(),
        tool_name: "read".into(),
        status: ToolCallStatus::Completed,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: crate::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert!(json.get("audit").is_none(), "got: {json}");
}

#[test]
fn tool_call_update_includes_audit_when_present() {
    let audit = MutationSessionRecord {
        session_id: "session_42".into(),
        run_id: Some("run_42".into()),
        mutation_scope: "apply_workspace".into(),
        execution_kind: Some("worker".into()),
        ..Default::default()
    };
    let event = AgentEvent::ToolCallUpdate {
        session_id: "s".into(),
        tool_call_id: "tc-1".into(),
        tool_name: "edit_file".into(),
        status: ToolCallStatus::Completed,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: crate::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        executor: Some(ToolExecutor::HostBridge),
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: Some(audit),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["audit"]["session_id"], "session_42");
    assert_eq!(json["audit"]["run_id"], "run_42");
    assert_eq!(json["audit"]["mutation_scope"], "apply_workspace");
    assert_eq!(json["audit"]["execution_kind"], "worker");
}

#[test]
fn tool_call_update_deserializes_without_audit_field_for_back_compat() {
    let raw = serde_json::json!({
        "type": "tool_call_update",
        "session_id": "s",
        "tool_call_id": "tc-1",
        "tool_name": "read",
        "status": "completed",
        "mutation_status": "unknown",
        "raw_output": null,
        "error": null,
    });
    let event: AgentEvent = serde_json::from_value(raw).expect("parses without audit key");
    match event {
        AgentEvent::ToolCallUpdate { audit, .. } => {
            assert!(audit.is_none());
        }
        other => panic!("expected ToolCallUpdate, got {other:?}"),
    }
}

#[test]
fn tool_call_audit_serializes_with_free_form_audit_payload() {
    // Middleware-attached metadata is intentionally free-form JSON
    // (A2A-style `metadata` extension slot). The wire format must
    // preserve nested dicts + lists verbatim so hosts can read
    // `summary`/`consent`/`layers`/etc. without per-field schema.
    let audit = serde_json::json!({
        "summary": "Searched codebase",
        "kind": "search",
        "consent": {"decision": "approved", "decided_by": "auto"},
        "layers": [{"name": "with_required_reason", "status": "ok"}],
    });
    let event = AgentEvent::ToolCallAudit {
        session_id: "s".into(),
        tool_call_id: "tc-1".into(),
        tool_name: "search_files".into(),
        audit: audit.clone(),
        receipt: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "tool_call_audit");
    assert_eq!(json["session_id"], "s");
    assert_eq!(json["tool_call_id"], "tc-1");
    assert_eq!(json["tool_name"], "search_files");
    assert_eq!(json["audit"], audit);
}

#[test]
fn tool_call_audit_session_id_routes_correctly() {
    let event = AgentEvent::ToolCallAudit {
        session_id: "abc".into(),
        tool_call_id: "tc".into(),
        tool_name: "read".into(),
        audit: serde_json::Value::Null,
        receipt: None,
    };
    assert_eq!(event.session_id(), "abc");
}

#[test]
fn host_tool_result_event_round_trips_with_sanitization_metadata() {
    let event = AgentEvent::HostToolResult {
        session_id: "s".into(),
        injection_id: "inj-1".into(),
        tool_call_id: "hosttc-1".into(),
        tool_name: "web_fetch".into(),
        kind: Some(crate::tool_annotations::ToolKind::Fetch),
        raw_input: serde_json::json!({"url": "https://example.test"}),
        status: ToolCallStatus::Completed,
        raw_output: Some(serde_json::json!({"text": "payload"})),
        result_pointer: None,
        error: None,
        duration_ms: Some(17),
        delivery: InjectionDelivery::Immediate,
        delivered_at_seam: Some("immediate".into()),
        sequence: 2,
        provenance: HostInjectionProvenance {
            initiator: "user".into(),
            source: "user_invoked_tool".into(),
            host: Some("tui".into()),
            ts_ms: 1_782_000_000_000,
        },
        sanitization: SanitizationVerdict {
            trust: crate::security::TrustLevel::Untrusted,
            detector: None,
            action: SanitizationAction::Passed,
            original_bytes: 31,
            delivered_bytes: 7,
            summary_model: None,
            labels: vec!["web_content".into()],
        },
    };

    let json = serde_json::to_value(&event).expect("serialize");
    assert_eq!(json["type"], "host_tool_result");
    assert_eq!(json["delivery"], "immediate");
    assert_eq!(json["sanitization"]["trust"], "untrusted");
    assert_eq!(json["provenance"]["source"], "user_invoked_tool");

    let recovered: AgentEvent = serde_json::from_value(json).expect("deserialize");
    assert_eq!(recovered.session_id(), "s");
    match recovered {
        AgentEvent::HostToolResult {
            sequence,
            kind,
            sanitization,
            ..
        } => {
            assert_eq!(sequence, 2);
            assert_eq!(kind, Some(crate::tool_annotations::ToolKind::Fetch));
            assert_eq!(sanitization.trust, crate::security::TrustLevel::Untrusted);
        }
        other => panic!("expected HostToolResult, got {other:?}"),
    }
}

#[test]
fn host_attachment_event_round_trips_without_optional_description() {
    let event = AgentEvent::HostAttachment {
        session_id: "s".into(),
        injection_id: "inj-2".into(),
        media_type: "image/png".into(),
        flavor: AttachmentFlavor::Image,
        artifact_pointer: ".burin/chat-assets/a.png".into(),
        sha256: "a".repeat(64),
        size_bytes: 42,
        rendered: AttachmentRendering::PointerOnly,
        description: None,
        description_model: None,
        delivery: InjectionDelivery::Immediate,
        delivered_at_seam: Some("immediate".into()),
        sequence: 0,
        provenance: HostInjectionProvenance {
            initiator: "user".into(),
            source: "user_attachment".into(),
            host: None,
            ts_ms: 1,
        },
        sanitization: SanitizationVerdict {
            trust: crate::security::TrustLevel::Untrusted,
            detector: None,
            action: SanitizationAction::Pointerized,
            original_bytes: 42,
            delivered_bytes: 22,
            summary_model: None,
            labels: Vec::new(),
        },
    };

    let json = serde_json::to_value(&event).expect("serialize");
    assert_eq!(json["type"], "host_attachment");
    assert!(json.get("description").is_none());
    assert_eq!(json["rendered"], "pointer_only");

    let recovered: AgentEvent = serde_json::from_value(json).expect("deserialize");
    assert_eq!(recovered.session_id(), "s");
    match recovered {
        AgentEvent::HostAttachment {
            artifact_pointer,
            description,
            ..
        } => {
            assert_eq!(artifact_pointer, ".burin/chat-assets/a.png");
            assert!(description.is_none());
        }
        other => panic!("expected HostAttachment, got {other:?}"),
    }
}

#[test]
fn tool_call_audit_serializes_typed_receipt_when_present() {
    let receipt = ToolCallReceipt {
        schema_version: 1,
        session_id: "s".into(),
        run_id: None,
        tool_call_id: "tc-1".into(),
        tool_name: "search_files".into(),
        iteration: 3,
        turn_index: Some(2),
        emit_order: 0,
        reason: Some("Search for middleware".into()),
        kind: Some("search".into()),
        executor: Some("harn".into()),
        status: "ok".into(),
        error_category: None,
        duration_ms: 9,
        args_hash: "0".repeat(64),
        result_hash: Some("1".repeat(64)),
        audit: serde_json::json!({"summary": "Search for middleware"}),
        emitted_at: "2026-05-16T00:00:00Z".into(),
        model: Some("mock".into()),
        provider: Some("mock".into()),
    };
    let event = AgentEvent::ToolCallAudit {
        session_id: "s".into(),
        tool_call_id: "tc-1".into(),
        tool_name: "search_files".into(),
        audit: receipt.audit.clone(),
        receipt: Some(receipt.clone()),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["receipt"], serde_json::to_value(receipt).unwrap());
    assert_eq!(json["receipt"]["args_hash"], "0".repeat(64));
}

#[test]
fn wildcard_sink_receives_events_across_sessions() {
    // Wildcard sinks (issue #1868) observe every emit regardless
    // of session_id. The cfg(test) owner filter keeps tests on
    // other threads from polluting each other.
    reset_wildcard_sinks();
    let counter = Arc::new(AtomicUsize::new(0));
    let handle = register_wildcard_sink(Arc::new(CountingSink(counter.clone())));
    emit_event(&AgentEvent::IterationStart {
        session_id: "session-w".into(),
        iteration: 0,
        provider: String::new(),
        model: String::new(),
    });
    emit_event(&AgentEvent::IterationEnd {
        session_id: "session-w-other".into(),
        iteration: 0,
        iteration_info: serde_json::json!({}),
    });
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    unregister_wildcard_sink(handle);
    emit_event(&AgentEvent::IterationStart {
        session_id: "session-w".into(),
        iteration: 1,
        provider: String::new(),
        model: String::new(),
    });
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "unregister stops delivery"
    );
    reset_wildcard_sinks();
}

#[test]
fn wildcard_sink_unregister_unknown_handle_is_noop() {
    reset_wildcard_sinks();
    let counter = Arc::new(AtomicUsize::new(0));
    let handle = register_wildcard_sink(Arc::new(CountingSink(counter.clone())));
    unregister_wildcard_sink(handle);
    // Second unregister of the same handle is harmless.
    unregister_wildcard_sink(handle);
    // And a completely-made-up handle is also a no-op.
    let bogus = WildcardSinkHandle(u64::MAX);
    unregister_wildcard_sink(bogus);
    emit_event(&AgentEvent::IterationStart {
        session_id: "s".into(),
        iteration: 0,
        provider: String::new(),
        model: String::new(),
    });
    assert_eq!(counter.load(Ordering::SeqCst), 0);
    reset_wildcard_sinks();
}
