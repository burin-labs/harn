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
fn tool_call_error_category_serializes_as_snake_case() {
    let pairs = [
        (ToolCallErrorCategory::SchemaValidation, "schema_validation"),
        (ToolCallErrorCategory::ToolError, "tool_error"),
        (ToolCallErrorCategory::McpServerError, "mcp_server_error"),
        (ToolCallErrorCategory::HostBridgeError, "host_bridge_error"),
        (ToolCallErrorCategory::PermissionDenied, "permission_denied"),
        (ToolCallErrorCategory::RejectedLoop, "rejected_loop"),
        (ToolCallErrorCategory::ParseAborted, "parse_aborted"),
        (ToolCallErrorCategory::Timeout, "timeout"),
        (ToolCallErrorCategory::Network, "network"),
        (ToolCallErrorCategory::Cancelled, "cancelled"),
        (ToolCallErrorCategory::Unknown, "unknown"),
    ];
    for (variant, wire) in pairs {
        let encoded = serde_json::to_string(&variant).unwrap();
        assert_eq!(encoded, format!("\"{wire}\""));
        assert_eq!(variant.as_str(), wire);
        // Round-trip via deserialize so wire stability is enforced
        // both ways.
        let decoded: ToolCallErrorCategory = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, variant);
    }
}

#[test]
fn denial_gate_serializes_as_snake_case() {
    use crate::agent_events::DenialGate;
    let pairs = [
        (DenialGate::ToolCeiling, "tool_ceiling"),
        (DenialGate::MalformedToolWrapper, "malformed_tool_wrapper"),
        (DenialGate::CapabilityCeiling, "capability_ceiling"),
        (DenialGate::SideEffectCeiling, "side_effect_ceiling"),
        (DenialGate::ArgConstraint, "arg_constraint"),
        (DenialGate::DynamicPermission, "dynamic_permission"),
        (DenialGate::ApprovalPolicy, "approval_policy"),
        (DenialGate::ApprovalUnavailable, "approval_unavailable"),
        (DenialGate::HostRejected, "host_rejected"),
        (DenialGate::HookDeny, "hook_deny"),
        (DenialGate::Unknown, "unknown"),
    ];
    // Keep ALL exhaustive so a new gate forces a wire-string decision.
    assert_eq!(pairs.len(), DenialGate::ALL.len());
    for (variant, wire) in pairs {
        let encoded = serde_json::to_string(&variant).unwrap();
        assert_eq!(encoded, format!("\"{wire}\""));
        assert_eq!(variant.as_str(), wire);
        let decoded: DenialGate = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, variant);
    }
}

#[test]
fn tool_denial_serializes_terminal_record_and_skips_empty_fields() {
    use crate::agent_events::{DenialGate, ToolDenial};
    // A capability ceiling: gate + capability present, no declared paths.
    let denial = ToolDenial::terminal(
        DenialGate::CapabilityCeiling,
        Some("workspace.write_text".to_string()),
        "tool 'edit' exceeds capability ceiling: workspace.write_text",
    );
    let json = denial.to_json();
    assert_eq!(json["gate"], "capability_ceiling");
    assert_eq!(json["capability"], "workspace.write_text");
    assert_eq!(json["retryable"], false);
    assert!(json["reason"]
        .as_str()
        .unwrap()
        .contains("capability ceiling"));
    // Empty `denied_paths` and absent `capability` are omitted from the wire.
    let bare = ToolDenial::terminal(DenialGate::ToolCeiling, None, "exceeds tool ceiling");
    let bare_json = bare.to_json();
    assert!(bare_json.get("denied_paths").is_none());
    assert!(bare_json.get("capability").is_none());
    // Round-trips back to an equal value.
    let recovered: ToolDenial = serde_json::from_value(denial.to_json()).unwrap();
    assert_eq!(recovered, denial);
}

#[test]
fn tool_executor_round_trips_with_adjacent_tag() {
    // Adjacent tagging keeps the wire shape uniform — every variant
    // is a JSON object with a `kind` discriminator. The ACP adapter
    // rewrites unit variants as bare strings; the on-disk event log
    // keeps the object shape so deserialize can recover the variant.
    for executor in [
        ToolExecutor::HarnBuiltin,
        ToolExecutor::HostBridge,
        ToolExecutor::McpServer {
            server_name: "linear".to_string(),
        },
        ToolExecutor::ProviderNative,
    ] {
        let json = serde_json::to_value(&executor).unwrap();
        let kind = json.get("kind").and_then(|v| v.as_str()).unwrap();
        match &executor {
            ToolExecutor::HarnBuiltin => assert_eq!(kind, "harn_builtin"),
            ToolExecutor::HostBridge => assert_eq!(kind, "host_bridge"),
            ToolExecutor::McpServer { server_name } => {
                assert_eq!(kind, "mcp_server");
                assert_eq!(json["server_name"], *server_name);
            }
            ToolExecutor::ProviderNative => assert_eq!(kind, "provider_native"),
        }
        let recovered: ToolExecutor = serde_json::from_value(json).unwrap();
        assert_eq!(recovered, executor);
    }
}

#[test]
fn tool_call_error_category_from_internal_collapses_transient_family() {
    use crate::value::ErrorCategory as Internal;
    assert_eq!(
        ToolCallErrorCategory::from_internal(&Internal::Timeout),
        ToolCallErrorCategory::Timeout
    );
    for net in [
        Internal::RateLimit,
        Internal::Overloaded,
        Internal::ServerError,
        Internal::TransientNetwork,
    ] {
        assert_eq!(
            ToolCallErrorCategory::from_internal(&net),
            ToolCallErrorCategory::Network,
            "{net:?} should map to Network",
        );
    }
    assert_eq!(
        ToolCallErrorCategory::from_internal(&Internal::SchemaValidation),
        ToolCallErrorCategory::SchemaValidation
    );
    assert_eq!(
        ToolCallErrorCategory::from_internal(&Internal::ToolError),
        ToolCallErrorCategory::ToolError
    );
    assert_eq!(
        ToolCallErrorCategory::from_internal(&Internal::ToolRejected),
        ToolCallErrorCategory::PermissionDenied
    );
    assert_eq!(
        ToolCallErrorCategory::from_internal(&Internal::Cancelled),
        ToolCallErrorCategory::Cancelled
    );
    for bridge in [
        Internal::Auth,
        Internal::EgressBlocked,
        Internal::NotFound,
        Internal::CircuitOpen,
        Internal::Generic,
    ] {
        assert_eq!(
            ToolCallErrorCategory::from_internal(&bridge),
            ToolCallErrorCategory::HostBridgeError,
            "{bridge:?} should map to HostBridgeError",
        );
    }
}

#[test]
fn only_schema_validation_is_recoverable() {
    // Recoverable == the model can fix the call itself and retry (bad/missing
    // args, malformed tool name). Everything else — permission denials,
    // tool/transport errors, timeouts — is NOT something a retry-with-correction
    // fixes, so it must not get retry-positive feedback.
    assert!(ToolCallErrorCategory::SchemaValidation.is_recoverable());
    for category in ToolCallErrorCategory::ALL {
        if matches!(category, ToolCallErrorCategory::SchemaValidation) {
            continue;
        }
        assert!(
            !category.is_recoverable(),
            "{category:?} must not be treated as recoverable"
        );
    }
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

// --- from_host_payload: typed host-emit deserialization -------------------
//
// These pin the accept/reject boundary and the per-field defaults that the
// retired hand-written `build_agent_event` match applied, so the serde path
// stays behavior-identical.

use serde_json::json;

#[test]
fn from_host_deserializes_generic_variant() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "iteration_start",
        &json!({ "iteration": 3, "provider": "openai", "model": "gpt" }),
    )
    .expect("iteration_start");
    match event {
        AgentEvent::IterationStart {
            session_id,
            iteration,
            provider,
            model,
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(iteration, 3);
            assert_eq!(provider, "openai");
            assert_eq!(model, "gpt");
        }
        other => panic!("expected IterationStart, got {other:?}"),
    }
}

#[test]
fn from_host_tool_call_defaults_status_and_audit() {
    // No `status` in the payload -> Pending; audit comes from the (absent)
    // ambient mutation session -> None, never from the payload.
    let event = AgentEvent::from_host_payload(
        "s1",
        "tool_call",
        &json!({ "tool_call_id": "t1", "tool_name": "read_file", "audit": {"bogus": true} }),
    )
    .expect("tool_call");
    match event {
        AgentEvent::ToolCall {
            status,
            raw_input,
            audit,
            ..
        } => {
            assert_eq!(status, ToolCallStatus::Pending);
            assert_eq!(raw_input, serde_json::Value::Null);
            assert!(audit.is_none());
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

#[test]
fn from_host_tool_call_update_defaults_status_and_maps_executor_alias() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "tool_call_update",
        &json!({ "tool_call_id": "t1", "tool_name": "read_file", "executor": "harn" }),
    )
    .expect("tool_call_update");
    match event {
        AgentEvent::ToolCallUpdate {
            status, executor, ..
        } => {
            assert_eq!(status, ToolCallStatus::InProgress);
            assert_eq!(executor, Some(ToolExecutor::HarnBuiltin));
        }
        other => panic!("expected ToolCallUpdate, got {other:?}"),
    }
}

#[test]
fn from_host_tool_call_update_preserves_structured_executor() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "tool_call_update",
        &json!({
            "tool_call_id": "t1",
            "tool_name": "linear_create",
            "status": "completed",
            "executor": { "kind": "mcp_server", "server_name": "linear" },
        }),
    )
    .expect("tool_call_update");
    match event {
        AgentEvent::ToolCallUpdate { executor, .. } => {
            assert_eq!(
                executor,
                Some(ToolExecutor::McpServer {
                    server_name: "linear".to_string()
                })
            );
        }
        other => panic!("expected ToolCallUpdate, got {other:?}"),
    }
}

#[test]
fn from_host_tool_call_update_rejects_unknown_executor() {
    let error = AgentEvent::from_host_payload(
        "s1",
        "tool_call_update",
        &json!({ "tool_call_id": "t1", "tool_name": "x", "executor": "nope" }),
    )
    .expect_err("unknown executor rejected");
    assert!(format!("{error:?}").contains("invalid tool executor"));
}

#[test]
fn from_host_progress_reported_defaults_replace_true() {
    let event = AgentEvent::from_host_payload("s1", "progress_reported", &json!({}))
        .expect("progress_reported");
    match event {
        AgentEvent::ProgressReported {
            replace,
            entries,
            metadata,
            message,
            ..
        } => {
            assert!(replace, "replace defaults to true");
            assert_eq!(entries, json!([]));
            assert_eq!(metadata, json!({}));
            assert!(message.is_none());
        }
        other => panic!("expected ProgressReported, got {other:?}"),
    }
}

#[test]
fn from_host_verdict_deserializes_complete_payload() {
    // The loop always emits a normalized (complete) verdict payload; the
    // typed path deserializes it 1:1.
    let event = AgentEvent::from_host_payload(
        "s1",
        "scope_classifier_verdict",
        &json!({
            "iteration": 1,
            "label": "escalate",
            "original_label": "trivial",
            "confidence": 0.4,
            "confidence_threshold": 0.65,
            "evidence": "e",
            "skip_main_turn": false,
        }),
    )
    .expect("scope_classifier_verdict");
    match event {
        AgentEvent::ScopeClassifierVerdict {
            label,
            confidence,
            confidence_threshold,
            skip_main_turn,
            ..
        } => {
            assert_eq!(label, "escalate");
            assert_eq!(confidence, 0.4);
            assert_eq!(confidence_threshold, 0.65);
            assert!(!skip_main_turn);
        }
        other => panic!("expected ScopeClassifierVerdict, got {other:?}"),
    }
}

#[test]
fn from_host_input_guardrail_deserializes_complete_payload() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "input_guardrail_verdict",
        &json!({
            "iteration": 1,
            "tripwire": true,
            "reason": "policy denied",
            "label": "prompt_injection",
            "confidence": 0.97,
            "confidence_threshold": 0.8,
            "classifier_kind": "custom",
        }),
    )
    .expect("input_guardrail_verdict");
    match event {
        AgentEvent::InputGuardrailVerdict {
            tripwire,
            reason,
            label,
            confidence,
            classifier_kind,
            ..
        } => {
            assert!(tripwire);
            assert_eq!(reason, "policy denied");
            assert_eq!(label, "prompt_injection");
            assert_eq!(confidence, 0.97);
            assert_eq!(classifier_kind.as_deref(), Some("custom"));
        }
        other => panic!("expected InputGuardrailVerdict, got {other:?}"),
    }
}

#[test]
fn from_host_nudge_maps_to_feedback_injected() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "llm_auto_continue",
        &json!({
            "previous_max_tokens": 100,
            "raised_max_tokens": 400,
            "attempt": 1,
            "max_continuations": 3,
        }),
    )
    .expect("llm_auto_continue");
    match event {
        AgentEvent::FeedbackInjected { kind, content, .. } => {
            assert_eq!(kind, "llm_auto_continue");
            assert_eq!(content, "100->400 (attempt 1/3)");
        }
        other => panic!("expected FeedbackInjected, got {other:?}"),
    }
}

#[test]
fn from_host_no_progress_nudge_preserves_injected_text_and_streak() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "no_progress_streak_nudge",
        &json!({
            "iteration": 4,
            "content": "No progress last turn. Emit exactly one well-formed <tool_call> now.",
            "streak": 2,
            "turns_since_progress": 2,
            "has_tools": true,
            "made_tool_calls": false,
        }),
    )
    .expect("no_progress_streak_nudge");
    match event {
        AgentEvent::FeedbackInjected {
            kind,
            content,
            streak,
            ..
        } => {
            assert_eq!(kind, "no_progress_streak_nudge");
            assert_eq!(
                content,
                "No progress last turn. Emit exactly one well-formed <tool_call> now."
            );
            assert_eq!(streak, Some(2));
        }
        other => panic!("expected FeedbackInjected, got {other:?}"),
    }
}

#[test]
fn from_host_no_progress_nudge_without_text_uses_explanatory_fallback() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "no_progress_streak_nudge",
        &json!({
            "iteration": 4,
            "streak": 2,
            "turns_since_progress": 2,
            "has_tools": true,
            "made_tool_calls": false,
        }),
    )
    .expect("no_progress_streak_nudge");
    match event {
        AgentEvent::FeedbackInjected {
            kind,
            content,
            streak,
            ..
        } => {
            assert_eq!(kind, "no_progress_streak_nudge");
            assert_ne!(content, "2");
            assert!(content.contains("No progress was detected"));
            assert_eq!(streak, Some(2));
        }
        other => panic!("expected FeedbackInjected, got {other:?}"),
    }
}

#[test]
fn from_host_judge_decision_preserves_source() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "judge_decision",
        &json!({
            "iteration": 3,
            "verdict": "continue",
            "reasoning": "need verifier output",
            "next_step": "run tests",
            "judge_duration_ms": 0,
            "source": "deterministic",
            "trigger": "verify_completion",
            "specific_gaps": [],
            "accepted_evidence": [],
        }),
    )
    .expect("judge_decision");
    match event {
        AgentEvent::JudgeDecision {
            source, trigger, ..
        } => {
            assert_eq!(source.as_deref(), Some("deterministic"));
            assert_eq!(trigger.as_deref(), Some("verify_completion"));
        }
        other => panic!("expected JudgeDecision, got {other:?}"),
    }
}

#[test]
fn from_host_stance_events_map_to_stance_transition() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "stance_write_access_granted",
        &json!({
            "escape_tool": "grant_write",
            "allowed_tools": ["read_file", "write_file"],
            "consent": "operator",
        }),
    )
    .expect("stance_write_access_granted");
    match event {
        AgentEvent::StanceTransition {
            phase,
            escape_tool,
            allowed_tools,
            consent,
            ..
        } => {
            assert_eq!(phase, "write_access_granted");
            assert_eq!(escape_tool, "grant_write");
            assert_eq!(allowed_tools, vec!["read_file", "write_file"]);
            assert_eq!(consent, "operator");
        }
        other => panic!("expected StanceTransition, got {other:?}"),
    }
}

#[test]
fn from_host_deserializes_budget_events() {
    // Budget termination is a normal host-emitted loop outcome, so both
    // variants must stay inside the explicit host allowlist.
    match AgentEvent::from_host_payload(
        "s1",
        "budget_exhausted",
        &json!({ "kind": "budget", "max_iterations": 12, "iteration": 12, "cost_usd": 0.0, "wall_clock_ms": 0 }),
    )
    .expect("budget_exhausted")
    {
        AgentEvent::BudgetExhausted {
            max_iterations,
            kind,
            ..
        } => {
            assert_eq!(max_iterations, 12);
            assert_eq!(kind.as_deref(), Some("budget"));
        }
        other => panic!("expected BudgetExhausted, got {other:?}"),
    }
    match AgentEvent::from_host_payload(
        "s1",
        "budget_circuit_breaker",
        &json!({ "kind": "consecutive_failures", "consecutive_count": 3, "paused_for_ms": 500 }),
    )
    .expect("budget_circuit_breaker")
    {
        AgentEvent::BudgetCircuitBreaker {
            consecutive_count,
            paused_for_ms,
            ..
        } => {
            assert_eq!(consecutive_count, 3);
            assert_eq!(paused_for_ms, 500);
        }
        other => panic!("expected BudgetCircuitBreaker, got {other:?}"),
    }
}

#[test]
fn from_host_rejects_non_host_and_unknown_event_types() {
    // A real `AgentEvent` variant that is never emitted through the host
    // path stays rejected (parity with the retired match's allowlist).
    AgentEvent::from_host_payload("s1", "worker_update", &json!({}))
        .expect_err("worker_update is not a host-emittable event type");
    AgentEvent::from_host_payload("s1", "totally_made_up", &json!({}))
        .expect_err("unknown event type rejected");
}
