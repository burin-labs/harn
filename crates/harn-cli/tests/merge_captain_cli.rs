//! In-process coverage of `harn merge-captain audit`.
//!
//! Tier 1H of the de-flake epic (#1057, #1067): the CLI command in
//! `crates/harn-cli/src/commands/merge_captain.rs` is a thin wrapper
//! around `harn_vm::orchestration::{audit_transcript,
//! load_transcript_jsonl, load_merge_captain_golden}` plus pretty
//! printing. These tests call the library functions directly and
//! assert on the structured `AuditReport`, then re-derive the
//! human/JSON projections used by the CLI to keep the contract
//! pinned.

mod test_util;

use std::path::PathBuf;

use harn_vm::orchestration::{
    audit_transcript, load_merge_captain_golden, load_transcript_jsonl, AuditReport,
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

fn run_audit(scenario: &str) -> AuditReport {
    let loaded = load_transcript_jsonl(&fixture(scenario, "transcripts")).expect("load transcript");
    let golden =
        load_merge_captain_golden(&fixture(scenario, "goldens")).expect("load golden");
    let mut report = audit_transcript(&loaded.events, Some(&golden));
    report.source_path = Some(loaded.source_path.display().to_string());
    report
}

#[test]
fn green_pr_passes_audit() {
    let report = run_audit("green_pr");
    assert!(
        report.pass,
        "expected pass; findings: {:#?}",
        report.findings
    );
    assert_eq!(report.scenario.as_deref(), Some("green_pr"));
    let rendered = format!("{report}");
    assert!(rendered.contains("PASS"));
    assert!(rendered.contains("scenario=green_pr"));
}

#[test]
fn failing_ci_passes_audit_with_handoff() {
    let report = run_audit("failing_ci");
    assert!(report.pass, "findings: {:#?}", report.findings);
    let handoff_transitions: Vec<_> = report
        .state_transitions
        .iter()
        .filter(|t| t.step == "handoff" && t.triggered_by == "handoff")
        .collect();
    assert!(
        !handoff_transitions.is_empty(),
        "expected at least one handoff transition; transitions: {:#?}",
        report.state_transitions
    );
}

#[test]
fn semantic_conflict_passes_audit() {
    let report = run_audit("semantic_conflict");
    assert!(report.pass, "findings: {:#?}", report.findings);
}

#[test]
fn merge_queue_passes_audit() {
    let report = run_audit("merge_queue");
    assert!(report.pass, "findings: {:#?}", report.findings);
}

#[test]
fn new_pr_arrival_passes_audit() {
    let report = run_audit("new_pr_arrival");
    assert!(report.pass, "findings: {:#?}", report.findings);
}

#[test]
fn bad_unsafe_merge_fails_audit_with_findings() {
    let report = run_audit("bad_unsafe_merge");
    assert!(
        !report.pass,
        "expected failure; report: {:#?}",
        report.findings
    );
    let categories: Vec<&str> = report
        .findings
        .iter()
        .map(|f| f.category.as_str())
        .collect();
    for expected in [
        "repeated_read",
        "unsafe_attempted_action",
        "missing_state_step",
        "skipped_verification",
    ] {
        assert!(
            categories.contains(&expected),
            "expected `{expected}` finding category; got {categories:?}"
        );
    }
    let rendered = format!("{report}");
    assert!(rendered.contains("FAIL"));
}

#[test]
fn json_output_is_machine_readable() {
    let report = run_audit("green_pr");
    let serialized = serde_json::to_value(&report).expect("serialize");
    assert_eq!(serialized["pass"], serde_json::Value::Bool(true));
    assert_eq!(
        serialized["scenario"],
        serde_json::Value::String("green_pr".into())
    );
    assert!(!serialized["state_transitions"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn audit_without_golden_uses_defaults() {
    let loaded = load_transcript_jsonl(&fixture("green_pr", "transcripts")).expect("load");
    let mut report = audit_transcript(&loaded.events, None);
    report.source_path = Some(loaded.source_path.display().to_string());
    let rendered = format!("{report}");
    assert!(report.scenario.is_none());
    assert!(rendered.contains("scenario=<none>"));
}

#[test]
fn directory_argument_loads_rotated_logs() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("session-x");
    std::fs::create_dir_all(&session).unwrap();
    let src = std::fs::read_to_string(fixture("green_pr", "transcripts")).unwrap();
    std::fs::write(session.join("event_log.jsonl"), &src).unwrap();
    let loaded = load_transcript_jsonl(session.as_path()).expect("load directory");
    assert!(!loaded.events.is_empty());
    // The directory case is about loading mechanics, not pass/fail; calling
    // audit_transcript is enough to exercise the post-load wiring.
    let _ = audit_transcript(&loaded.events, None);
}
