//! In-process tests for the `harn merge-captain audit` library API
//! (`harn_vm::orchestration::audit_transcript`). Replaces the prior
//! subprocess-spawning version that ran the `harn` binary per case
//! (#1067).

use std::path::{Path, PathBuf};

use harn_vm::orchestration::{
    audit_transcript, load_merge_captain_golden, load_transcript_jsonl, AuditReport,
    MergeCaptainGolden,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn fixture(scenario: &str, kind: &str) -> PathBuf {
    let ext = if kind == "transcripts" {
        "jsonl"
    } else {
        "json"
    };
    repo_root()
        .join("examples/personas/merge_captain")
        .join(kind)
        .join(format!("{scenario}.{ext}"))
}

/// Mirror of `harn-cli`'s `run_audit` inner work: load transcript, load
/// optional golden, audit, attach `source_path`. Tests use the report
/// directly instead of inspecting CLI stdout/stderr.
fn audit(scenario: &str, with_golden: bool) -> AuditReport {
    let transcript = fixture(scenario, "transcripts");
    let loaded = load_transcript_jsonl(&transcript).expect("load transcript");
    let golden: Option<MergeCaptainGolden> = if with_golden {
        Some(load_merge_captain_golden(&fixture(scenario, "goldens")).expect("load golden"))
    } else {
        None
    };
    let mut report = audit_transcript(&loaded.events, golden.as_ref());
    report.source_path = Some(loaded.source_path.display().to_string());
    report
}

#[test]
fn green_pr_passes_audit() {
    let report = audit("green_pr", true);
    assert!(report.pass, "report={:#?}", report);
    assert_eq!(report.scenario.as_deref(), Some("green_pr"));
    let rendered = format!("{report}");
    assert!(rendered.contains("PASS"));
    assert!(rendered.contains("scenario=green_pr"));
}

#[test]
fn failing_ci_passes_audit_with_handoff() {
    let report = audit("failing_ci", true);
    assert!(report.pass, "report={:#?}", report);
    let rendered = format!("{report}");
    assert!(
        rendered.contains("handoff <- handoff"),
        "missing handoff transition in:\n{rendered}"
    );
}

#[test]
fn semantic_conflict_passes_audit() {
    let report = audit("semantic_conflict", true);
    assert!(report.pass, "report={:#?}", report);
}

#[test]
fn merge_queue_passes_audit() {
    let report = audit("merge_queue", true);
    assert!(report.pass, "report={:#?}", report);
}

#[test]
fn new_pr_arrival_passes_audit() {
    let report = audit("new_pr_arrival", true);
    assert!(report.pass, "report={:#?}", report);
}

#[test]
fn bad_unsafe_merge_fails_audit_with_findings() {
    let report = audit("bad_unsafe_merge", true);
    assert!(!report.pass, "report={:#?}", report);
    assert!(report.error_findings() > 0);
    let categories: Vec<_> = report
        .findings
        .iter()
        .map(|f| f.category.as_str().to_string())
        .collect();
    for expected in [
        "repeated_read",
        "unsafe_attempted_action",
        "missing_state_step",
        "skipped_verification",
    ] {
        assert!(
            categories.iter().any(|c| c == expected),
            "missing finding category {expected}; got {categories:?}"
        );
    }
}

#[test]
fn json_output_is_machine_readable() {
    let report = audit("green_pr", true);
    let json = serde_json::to_string(&report).expect("serialize");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse json");
    assert_eq!(parsed["pass"], serde_json::Value::Bool(true));
    assert_eq!(
        parsed["scenario"],
        serde_json::Value::String("green_pr".into())
    );
    assert!(!parsed["state_transitions"].as_array().unwrap().is_empty());
}

#[test]
fn audit_without_golden_uses_defaults() {
    let report = audit("green_pr", false);
    assert!(report.pass, "report={:#?}", report);
    assert!(report.scenario.is_none());
    let rendered = format!("{report}");
    assert!(
        rendered.contains("scenario=<none>"),
        "expected scenario=<none> placeholder in:\n{rendered}"
    );
}

#[test]
fn directory_argument_loads_rotated_logs() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("session-x");
    std::fs::create_dir_all(&session).unwrap();
    let src = std::fs::read_to_string(fixture("green_pr", "transcripts")).unwrap();
    std::fs::write(session.join("event_log.jsonl"), &src).unwrap();
    let loaded = load_transcript_jsonl(Path::new(&session)).expect("load directory transcript");
    let report = audit_transcript(&loaded.events, None);
    assert!(report.pass, "report={:#?}", report);
}
