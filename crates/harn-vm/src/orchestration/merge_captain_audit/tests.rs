use super::*;
use crate::agent_events::{AgentEvent, PersistedAgentEvent, ToolCallStatus};
use serde_json::json;

#[path = "tests/structured_output_tests.rs"]
mod structured_output;
fn env(index: u64, event: AgentEvent) -> PersistedAgentEvent {
    PersistedAgentEvent {
        index,
        emitted_at_ms: 0,
        frame_depth: None,
        execution_id: None,
        event,
    }
}

fn iteration_start(index: u64, session: &str, iter: usize) -> PersistedAgentEvent {
    env(
        index,
        AgentEvent::IterationStart {
            session_id: session.into(),
            iteration: iter,
            provider: String::new(),
            model: String::new(),
        },
    )
}

fn iteration_end(index: u64, session: &str, iter: usize) -> PersistedAgentEvent {
    env(
        index,
        AgentEvent::IterationEnd {
            session_id: session.into(),
            iteration: iter,
            iteration_info: serde_json::Value::Null,
        },
    )
}

fn tool_call(
    index: u64,
    session: &str,
    tool: &str,
    args: serde_json::Value,
) -> PersistedAgentEvent {
    env(
        index,
        AgentEvent::ToolCall {
            session_id: session.into(),
            tool_call_id: format!("call_{index}"),
            tool_name: tool.into(),
            kind: None,
            status: ToolCallStatus::Pending,
            raw_input: args,
            parsing: None,
            audit: None,
            intent: None,
        },
    )
}

fn plan(index: u64, session: &str, plan: serde_json::Value) -> PersistedAgentEvent {
    env(
        index,
        AgentEvent::OrchestrationDecision {
            session_id: session.into(),
            decision: plan,
        },
    )
}

fn handoff(index: u64, session: &str) -> PersistedAgentEvent {
    env(
        index,
        AgentEvent::Handoff {
            session_id: session.into(),
            artifact_id: format!("artifact_{index}"),
            handoff: Box::new(crate::orchestration::HandoffArtifact::default()),
        },
    )
}

fn loop_stuck_signal(index: u64, session: &str, terminal: bool) -> PersistedAgentEvent {
    env(
        index,
        AgentEvent::LoopStuckSignal {
            session_id: session.into(),
            payload: json!({"terminal": terminal}),
        },
    )
}

#[test]
fn pass_minimal_green_pr_default_rules() {
    let events = vec![
        iteration_start(1, "s", 1),
        tool_call(2, "s", "fetch_pull_request", json!({"number": 1})),
        tool_call(3, "s", "list_checks", json!({"pr": 1})),
        plan(
            4,
            "s",
            json!({
                "review_risk": "low",
                "approval_required": false,
                "pr_number": 1,
            }),
        ),
        iteration_end(5, "s", 1),
    ];
    let report = audit_transcript(&events, None);
    assert!(report.pass, "report: {report}");
    assert_eq!(report.tool_call_count, 2);
    assert_eq!(report.model_call_count, 1);
    assert!(
        report.findings.is_empty(),
        "findings: {:?}",
        report.findings
    );
}

#[test]
fn flags_repeated_reads_with_default_threshold() {
    let events = vec![
        iteration_start(1, "s", 1),
        tool_call(2, "s", "list_checks", json!({"pr": 1})),
        tool_call(3, "s", "list_checks", json!({"pr": 1})),
        tool_call(4, "s", "list_checks", json!({"pr": 1})),
        iteration_end(5, "s", 1),
    ];
    let report = audit_transcript(&events, None);
    assert!(!report.pass);
    assert!(report
        .findings
        .iter()
        .any(|f| f.category == FindingCategory::RepeatedRead));
}

#[test]
fn flags_unsafe_action_without_approval() {
    let events = vec![
        iteration_start(1, "s", 1),
        tool_call(2, "s", "merge_pull_request", json!({"number": 1})),
        iteration_end(3, "s", 1),
    ];
    let report = audit_transcript(&events, None);
    assert!(!report.pass);
    assert!(report
        .findings
        .iter()
        .any(|f| f.category == FindingCategory::UnsafeAttemptedAction));
}

#[test]
fn approval_required_false_does_not_open_approval_gate() {
    let events = vec![
        iteration_start(1, "s", 1),
        plan(
            2,
            "s",
            json!({"approval_required": false, "review_risk": "low"}),
        ),
        tool_call(3, "s", "merge_pull_request", json!({"number": 1})),
        iteration_end(4, "s", 1),
    ];
    let report = audit_transcript(&events, None);
    assert!(!report.pass);
    assert!(report
        .findings
        .iter()
        .any(|f| f.category == FindingCategory::UnsafeAttemptedAction));
}

#[test]
fn flags_missing_approval_after_required_plan() {
    let events = vec![
        iteration_start(1, "s", 1),
        plan(
            2,
            "s",
            json!({"approval_required": true, "review_risk": "high"}),
        ),
        iteration_end(3, "s", 1),
    ];
    let report = audit_transcript(&events, None);
    assert!(!report.pass);
    assert!(report
        .findings
        .iter()
        .any(|f| f.category == FindingCategory::MissingApproval));
}

#[test]
fn handoff_satisfies_pending_approval() {
    let events = vec![
        iteration_start(1, "s", 1),
        plan(
            2,
            "s",
            json!({"approval_required": true, "review_risk": "high"}),
        ),
        handoff(3, "s"),
    ];
    let report = audit_transcript(&events, None);
    assert!(
        !report
            .findings
            .iter()
            .any(|f| f.category == FindingCategory::MissingApproval),
        "findings: {:?}",
        report.findings
    );
}

#[test]
fn non_terminal_loop_stuck_signal_does_not_complete_transcript() {
    let events = vec![iteration_start(1, "s", 1), loop_stuck_signal(2, "s", false)];
    let report = audit_transcript(&events, None);
    assert!(report
        .findings
        .iter()
        .any(|f| f.category == FindingCategory::IncompleteTranscript));
}

#[test]
fn terminal_loop_stuck_signal_completes_transcript() {
    let events = vec![iteration_start(1, "s", 1), loop_stuck_signal(2, "s", true)];
    let report = audit_transcript(&events, None);
    assert!(
        !report
            .findings
            .iter()
            .any(|f| f.category == FindingCategory::IncompleteTranscript),
        "findings: {:?}",
        report.findings
    );
}

#[test]
fn flags_skipped_verification_when_merge_runs_without_verifier() {
    let golden = MergeCaptainGolden {
        type_name: "merge_captain_golden".into(),
        scenario: "test".into(),
        state_steps: vec![
            GoldenStateStep {
                step: "verify".into(),
                tools: vec![ToolPattern {
                    glob: Some("*list_checks*".into()),
                    ..Default::default()
                }],
                verifier: true,
                ..Default::default()
            },
            GoldenStateStep {
                step: "approve".into(),
                events: vec!["feedback_injected".into()],
                approval_gate: true,
                ..Default::default()
            },
            GoldenStateStep {
                step: "merge".into(),
                tools: vec![ToolPattern {
                    glob: Some("*merge*".into()),
                    ..Default::default()
                }],
                merge_action: true,
                required: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let events = vec![
        iteration_start(1, "s", 1),
        env(2, AgentEvent::feedback_injected("s", "approval", "ok")),
        tool_call(3, "s", "merge_pull_request", json!({"number": 1})),
        iteration_end(4, "s", 1),
    ];
    let report = audit_transcript(&events, Some(&golden));
    assert!(report
        .findings
        .iter()
        .any(|f| f.category == FindingCategory::SkippedVerification));
}

#[test]
fn verifier_scope_must_match_merge_scope() {
    let golden = MergeCaptainGolden {
        type_name: "merge_captain_golden".into(),
        scenario: "test".into(),
        state_steps: vec![
            GoldenStateStep {
                step: "verify".into(),
                tools: vec![ToolPattern {
                    glob: Some("*list_checks*".into()),
                    ..Default::default()
                }],
                verifier: true,
                ..Default::default()
            },
            GoldenStateStep {
                step: "merge".into(),
                tools: vec![ToolPattern {
                    glob: Some("*merge*".into()),
                    ..Default::default()
                }],
                merge_action: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let events = vec![
        iteration_start(1, "s", 1),
        tool_call(
            2,
            "s",
            "list_checks",
            json!({"repo": "burin-labs/harn", "pr_number": 1}),
        ),
        tool_call(
            3,
            "s",
            "merge_pull_request",
            json!({"repo": "burin-labs/harn", "pr_number": 2}),
        ),
        iteration_end(4, "s", 1),
    ];
    let report = audit_transcript(&events, Some(&golden));
    assert!(report
        .findings
        .iter()
        .any(|f| f.category == FindingCategory::SkippedVerification));
}

#[test]
fn flags_extra_model_calls_against_golden() {
    let golden = MergeCaptainGolden {
        type_name: "merge_captain_golden".into(),
        scenario: "test".into(),
        max_model_calls: Some(1),
        ..Default::default()
    };
    let events = vec![
        iteration_start(1, "s", 1),
        iteration_end(2, "s", 1),
        iteration_start(3, "s", 2),
        iteration_end(4, "s", 2),
    ];
    let report = audit_transcript(&events, Some(&golden));
    assert!(!report.pass);
    assert!(report
        .findings
        .iter()
        .any(|f| f.category == FindingCategory::ExtraModelCall));
}

#[test]
fn flags_non_minimal_tool_usage() {
    let golden = MergeCaptainGolden {
        type_name: "merge_captain_golden".into(),
        scenario: "test".into(),
        max_tool_calls: Some(1),
        ..Default::default()
    };
    let events = vec![
        iteration_start(1, "s", 1),
        tool_call(2, "s", "list_checks", json!({"a": 1})),
        tool_call(3, "s", "list_threads", json!({"a": 2})),
        iteration_end(4, "s", 1),
    ];
    let report = audit_transcript(&events, Some(&golden));
    assert!(!report.pass);
    assert!(report
        .findings
        .iter()
        .any(|f| f.category == FindingCategory::NonMinimalToolUsage));
}

#[test]
fn flags_forbidden_action() {
    let golden = MergeCaptainGolden {
        type_name: "merge_captain_golden".into(),
        scenario: "test".into(),
        forbidden_actions: vec![ToolPattern {
            glob: Some("*force_push*".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    // Approve up front so unsafe-action rule doesn't double-fire.
    let events = vec![
        iteration_start(1, "s", 1),
        env(2, AgentEvent::feedback_injected("s", "approval", "ok")),
        tool_call(3, "s", "force_push", json!({"branch": "main"})),
        iteration_end(4, "s", 1),
    ];
    let report = audit_transcript(&events, Some(&golden));
    assert!(!report.pass);
    assert!(report
        .findings
        .iter()
        .any(|f| f.category == FindingCategory::ForbiddenAction));
}

#[test]
fn missing_required_state_step() {
    let golden = MergeCaptainGolden {
        type_name: "merge_captain_golden".into(),
        scenario: "test".into(),
        state_steps: vec![GoldenStateStep {
            step: "verify".into(),
            tools: vec![ToolPattern {
                glob: Some("*list_checks*".into()),
                ..Default::default()
            }],
            required: true,
            verifier: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    let events = vec![iteration_start(1, "s", 1), iteration_end(2, "s", 1)];
    let report = audit_transcript(&events, Some(&golden));
    assert!(!report.pass);
    assert!(report
        .findings
        .iter()
        .any(|f| f.category == FindingCategory::MissingStateStep));
}

#[test]
fn glob_matching_basic_cases() {
    let p = ToolPattern {
        glob: Some("*merge*".into()),
        ..Default::default()
    };
    assert!(p.matches("gh_merge_pr"));
    assert!(p.matches("MERGE"));
    assert!(!p.matches("approve"));

    let prefix = ToolPattern {
        glob: Some("gh_*".into()),
        ..Default::default()
    };
    assert!(prefix.matches("gh_pr_list"));
    assert!(!prefix.matches("git_pr_list"));

    let suffix = ToolPattern {
        glob: Some("*_merge".into()),
        ..Default::default()
    };
    assert!(suffix.matches("force_merge"));
    assert!(!suffix.matches("merge_force"));

    let exact = ToolPattern {
        name: Some("read_file".into()),
        ..Default::default()
    };
    assert!(exact.matches("read_file"));
    assert!(!exact.matches("read_files"));
}

#[test]
fn loads_jsonl_transcript_from_file() {
    use std::io::Write;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("event_log.jsonl");
    let mut file = fs::File::create(&path).expect("create");
    for env in [iteration_start(1, "s", 1), iteration_end(2, "s", 1)] {
        let line = serde_json::to_string(&env).expect("ser");
        writeln!(file, "{line}").expect("write");
    }
    drop(file);
    let loaded = load_transcript_jsonl(&path).expect("load");
    assert_eq!(loaded.events.len(), 2);
}

#[test]
fn loads_jsonl_transcript_from_directory() {
    use std::io::Write;
    let dir = tempfile::tempdir().expect("tempdir");
    let path1 = dir.path().join("event_log.jsonl");
    let path2 = dir.path().join("event_log-000001.jsonl");
    {
        let mut file = fs::File::create(&path1).expect("create");
        writeln!(
            file,
            "{}",
            serde_json::to_string(&iteration_start(1, "s", 1)).unwrap()
        )
        .unwrap();
    }
    {
        let mut file = fs::File::create(&path2).expect("create");
        writeln!(
            file,
            "{}",
            serde_json::to_string(&iteration_end(2, "s", 1)).unwrap()
        )
        .unwrap();
    }
    let loaded = load_transcript_jsonl(dir.path()).expect("load");
    assert_eq!(loaded.events.len(), 2);
    assert_eq!(loaded.events[0].index, 1);
    assert_eq!(loaded.events[1].index, 2);
}
