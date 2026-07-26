use super::*;

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
        reason: Some("repeated_verification_failures".into()),
        confirm: Some(false),
        converted_from: None,
        escalation_recommended: Some(true),
        escalation_target: Some("frontier".into()),
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
            escalation_recommended,
            escalation_target,
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
            assert_eq!(reason.as_deref(), Some("repeated_verification_failures"));
            assert_eq!(confirm, Some(false));
            assert_eq!(converted_from, None);
            assert_eq!(escalation_recommended, Some(true));
            assert_eq!(escalation_target.as_deref(), Some("frontier"));
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
        escalation_recommended: None,
        escalation_target: None,
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
