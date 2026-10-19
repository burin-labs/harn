use serde_json::json;

use super::*;
use crate::orchestration::{ExecutionEvidenceRecord, LlmUsageRecord};

#[test]
fn historical_view_producer_defaults_remain_unknown() {
    let run: RunView = serde_json::from_value(json!({})).unwrap();
    let session: SessionView = serde_json::from_value(json!({})).unwrap();
    assert_eq!(run.producer, ViewProducer::unknown());
    assert_eq!(session.producer, ViewProducer::unknown());
    let producer: ViewProducer = serde_json::from_value(json!({"name":"historical"})).unwrap();
    assert_eq!(producer.name, "historical");
    assert!(producer.version.is_empty());
    let current = build_run_view(&RunRecord::default()).producer;
    assert_eq!(current.name, "harn");
    assert_eq!(current.version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn public_view_schema_preserves_native_presence_and_scalar_rules() {
    let schema = run_session_view_schema();
    assert!(!schema.to_string().contains(env!("CARGO_PKG_VERSION")));
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    let sparse = json!({"schema":RUN_VIEW_SCHEMA,"schema_version":RUN_VIEW_SCHEMA_VERSION});
    assert!(validator.is_valid(&sparse));
    let mut malformed = sparse;
    malformed["run"] = json!({"started_at":null});
    assert!(!validator.is_valid(&malformed));
    malformed["run"] = json!({"duration_ms":-1});
    assert!(!validator.is_valid(&malformed));
    malformed.as_object_mut().unwrap().remove("run");
    malformed["evidence"] = json!({"flight_recording":{"retained_events":1}});
    assert!(!validator.is_valid(&malformed));
    malformed["evidence"] = json!({"reasoning_receipts":[],"evaluation_receipts":null});
    assert!(validator.is_valid(&malformed));
    assert!(!validator.is_valid(&json!({"schema":RUN_VIEW_SCHEMA,"schema_version":2})));
}

fn sample_run() -> RunRecord {
    RunRecord {
        type_name: "run_record".to_string(),
        id: "run_1".to_string(),
        workflow_id: "wf".to_string(),
        workflow_name: Some("Workflow".to_string()),
        task: "do work".to_string(),
        status: "completed".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        finished_at: Some("2026-01-01T00:00:02Z".to_string()),
        stages: vec![RunStageRecord {
            id: "stage_1".to_string(),
            node_id: "plan".to_string(),
            kind: "llm".to_string(),
            status: "completed".to_string(),
            outcome: "ok".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            finished_at: Some("2026-01-01T00:00:01Z".to_string()),
            visible_text: Some("done".to_string()),
            usage: Some(LlmUsageRecord {
                input_tokens: 10,
                output_tokens: 5,
                total_duration_ms: 1000,
                call_count: 1,
                cost_usd: Some(0.01),
                known_cost_usd: 0.01,
                total_cost: 0.01,
                models: vec!["model-a".to_string()],
                ..LlmUsageRecord::default()
            }),
            metadata: BTreeMap::from([
                ("session_id".to_string(), json!("session_1")),
                ("provider".to_string(), json!("test")),
                ("model".to_string(), json!("model-a")),
            ]),
            ..RunStageRecord::default()
        }],
        evidence: ExecutionEvidenceRecord {
            trace_spans: vec![RunTraceSpanRecord {
                kind: "llm_call".to_string(),
                metadata: BTreeMap::from([
                    ("provider".to_string(), json!("test")),
                    ("model".to_string(), json!("model-a")),
                    ("input_tokens".to_string(), json!(10)),
                    ("output_tokens".to_string(), json!(5)),
                    ("cost_usd".to_string(), json!(0.01)),
                    ("api_key".to_string(), json!("must-not-project")),
                ]),
                ..RunTraceSpanRecord::default()
            }],
            ..ExecutionEvidenceRecord::default()
        },
        transcript: Some(json!({
            "source": "inline",
            "summary": "short",
            "messages": [{"role": "assistant"}],
            "events": [{"kind": "output"}]
        })),
        ..RunRecord::default()
    }
}

#[test]
fn build_run_view_projects_stable_public_fields() {
    const EXECUTION_ID: &str = "hxe-019c13e0-8080-7000-8000-000000000001";
    let mut run = sample_run();
    run.evidence.schema_version = crate::orchestration::EXECUTION_EVIDENCE_SCHEMA_VERSION;
    run.evidence.execution_id = Some(EXECUTION_ID.to_string());
    let view = build_run_view_with_path(&run, Some("runs/run_1.json"));
    assert_eq!(view.schema, RUN_VIEW_SCHEMA);
    assert_eq!(view.schema_version, RUN_VIEW_SCHEMA_VERSION);
    assert_eq!(view.run.run_id, "run_1");
    assert_eq!(view.run.session_id.as_deref(), Some("session_1"));
    assert_eq!(view.run.run_path.as_deref(), Some("runs/run_1.json"));
    assert_eq!(view.run.duration_ms, Some(2000));
    assert_eq!(view.evidence.execution_id.as_deref(), Some(EXECUTION_ID));
    assert_eq!(
        view.evidence.trace_spans[0].metadata["api_key"],
        json!(crate::redact::REDACTED_PLACEHOLDER)
    );
    assert_eq!(view.visible_text.as_deref(), Some("done"));
    assert_eq!(view.transcript.message_count, 1);
    assert_eq!(view.usage.input_tokens, 10);
    assert_eq!(view.providers.len(), 1);
    assert!(view.projection.projection_id.starts_with("run_view:run_1:"));
    assert!(view.projection.projection_hash.is_some());
}

#[test]
fn build_run_view_falls_back_to_public_assistant_transcript_blocks() {
    let secret = "sk-proj-test-abcdefghijklmnopqrstuvwxyz123456";
    let run = RunRecord {
        type_name: "run_record".to_string(),
        id: "transcript_only".to_string(),
        status: "completed".to_string(),
        transcript: Some(json!({
            "events": [
                {
                    "kind": "message",
                    "role": "user",
                    "visibility": "public",
                    "blocks": [{
                        "type": "text",
                        "text": "user prompt must not become output",
                        "visibility": "public"
                    }]
                },
                {
                    "kind": "message",
                    "role": "assistant",
                    "visibility": "public",
                    "text": "event-level text is not an authority",
                    "blocks": [
                        {"type": "output_text", "text": "visible answer ", "visibility": "public"},
                        {"type": "text", "text": format!("with {secret}"), "visibility": "public"},
                        {"type": "reasoning", "text": "public reasoning stays private", "visibility": "public"},
                        {"type": "output_text", "text": "private output", "visibility": "private"}
                    ]
                },
                {
                    "kind": "message",
                    "role": "assistant",
                    "visibility": "private",
                    "blocks": [{
                        "type": "output_text",
                        "text": "private event output",
                        "visibility": "public"
                    }]
                }
            ]
        })),
        ..RunRecord::default()
    };

    let view = build_run_view(&run);
    let visible = view.visible_text.expect("public assistant output");
    assert!(visible.starts_with("visible answer with "));
    assert!(visible.contains("<redacted:openai_key:"));
    assert!(!visible.contains(secret));
    assert!(!visible.contains("user prompt"));
    assert!(!visible.contains("event-level text"));
    assert!(!visible.contains("reasoning"));
    assert!(!visible.contains("private output"));
    assert!(!visible.contains("private event output"));
}

#[test]
fn build_run_view_tolerates_sparse_legacy_records() {
    let run = RunRecord {
        type_name: "run_record".to_string(),
        id: "legacy".to_string(),
        status: "failed".to_string(),
        ..RunRecord::default()
    };
    let view = build_run_view(&run);
    assert_eq!(view.run.run_id, "legacy");
    assert_eq!(view.run.session_id, None);
    assert!(!view.transcript.present);
    assert_eq!(
        view.failure.as_ref().map(|failure| failure.status.as_str()),
        Some("failed")
    );
}

#[test]
fn build_session_view_aggregates_runs() {
    let run = build_run_view(&sample_run());
    let view = build_session_view_from_run_views(
        vec![run],
        SessionViewOptions {
            session_id: Some("session_1".to_string()),
            last_event_id: Some(7),
            chain_root_hash: Some("sha256:abc".to_string()),
            ..SessionViewOptions::default()
        },
    );
    assert_eq!(view.schema, SESSION_VIEW_SCHEMA);
    assert_eq!(view.session.session_id.as_deref(), Some("session_1"));
    assert_eq!(view.session.last_event_id, Some(7));
    assert_eq!(view.session.chain_root_hash.as_deref(), Some("sha256:abc"));
    assert_eq!(view.history.len(), 1);
    assert_eq!(view.usage.call_count, 1);
    assert!(view
        .projection
        .projection_id
        .starts_with("session_view:session_1:"));
}

#[test]
fn build_run_view_summarizes_stage_only_transcripts() {
    let mut run = sample_run();
    run.transcript = None;
    run.stages[0].transcript = Some(json!({
        "summary": "stage transcript only",
        "messages": [{"role": "assistant"}, {"role": "tool"}],
        "events": [{"kind": "tool_result"}]
    }));

    let view = build_run_view(&run);
    assert!(view.transcript.present);
    assert_eq!(view.transcript.source.as_deref(), Some("stages"));
    assert_eq!(view.transcript.message_count, 2);
    assert_eq!(view.transcript.event_count, 1);
    assert_eq!(
        view.transcript.summary.as_deref(),
        Some("plan: stage transcript only")
    );
}

#[test]
fn build_run_view_redacts_child_tasks_and_approvals() {
    let mut run = sample_run();
    run.child_runs.push(RunChildRecord {
        worker_id: "worker_1".to_string(),
        worker_name: "worker".to_string(),
        task: "inspect AKIAABCDEFGHIJKLMNOP".to_string(),
        ..RunChildRecord::default()
    });
    run.hitl_questions.push(RunHitlQuestionRecord {
        request_id: "approval_1".to_string(),
        prompt: "approve AKIAABCDEFGHIJKLMNOP".to_string(),
        ..RunHitlQuestionRecord::default()
    });

    let view = build_run_view(&run);
    assert!(!view.run.child_runs[0].task.contains("AKIAABCDEFGHIJKLMNOP"));
    assert!(!view.pending.approvals[0]
        .prompt
        .contains("AKIAABCDEFGHIJKLMNOP"));
}
