use std::path::PathBuf;
use std::process::Command;

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

#[test]
fn green_pr_passes_audit() {
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .args([
            "merge-captain",
            "audit",
            fixture("green_pr", "transcripts").to_str().unwrap(),
            "--golden",
            fixture("green_pr", "goldens").to_str().unwrap(),
        ])
        .output()
        .expect("run harn merge-captain audit");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected pass; stdout={}\nstderr={}",
        stdout,
        stderr
    );
    assert!(stdout.contains("PASS"));
    assert!(stdout.contains("scenario=green_pr"));
}

#[test]
fn failing_ci_passes_audit_with_handoff() {
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .args([
            "merge-captain",
            "audit",
            fixture("failing_ci", "transcripts").to_str().unwrap(),
            "--golden",
            fixture("failing_ci", "goldens").to_str().unwrap(),
        ])
        .output()
        .expect("run audit");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout={}", stdout);
    assert!(stdout.contains("handoff <- handoff"));
}

#[test]
fn semantic_conflict_passes_audit() {
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .args([
            "merge-captain",
            "audit",
            fixture("semantic_conflict", "transcripts")
                .to_str()
                .unwrap(),
            "--golden",
            fixture("semantic_conflict", "goldens").to_str().unwrap(),
        ])
        .output()
        .expect("run audit");
    assert!(output.status.success());
}

#[test]
fn merge_queue_passes_audit() {
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .args([
            "merge-captain",
            "audit",
            fixture("merge_queue", "transcripts").to_str().unwrap(),
            "--golden",
            fixture("merge_queue", "goldens").to_str().unwrap(),
        ])
        .output()
        .expect("run audit");
    assert!(output.status.success());
}

#[test]
fn new_pr_arrival_passes_audit() {
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .args([
            "merge-captain",
            "audit",
            fixture("new_pr_arrival", "transcripts").to_str().unwrap(),
            "--golden",
            fixture("new_pr_arrival", "goldens").to_str().unwrap(),
        ])
        .output()
        .expect("run audit");
    assert!(output.status.success());
}

#[test]
fn bad_unsafe_merge_fails_audit_with_findings() {
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .args([
            "merge-captain",
            "audit",
            fixture("bad_unsafe_merge", "transcripts").to_str().unwrap(),
            "--golden",
            fixture("bad_unsafe_merge", "goldens").to_str().unwrap(),
        ])
        .output()
        .expect("run audit");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "expected non-zero exit; stdout={}",
        stdout
    );
    assert!(stdout.contains("FAIL"));
    assert!(stdout.contains("repeated_read"));
    assert!(stdout.contains("unsafe_attempted_action"));
    assert!(stdout.contains("missing_state_step"));
    assert!(stdout.contains("skipped_verification"));
}

#[test]
fn json_output_is_machine_readable() {
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .args([
            "merge-captain",
            "audit",
            fixture("green_pr", "transcripts").to_str().unwrap(),
            "--golden",
            fixture("green_pr", "goldens").to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .expect("run audit");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("parse json");
    assert_eq!(parsed["pass"], serde_json::Value::Bool(true));
    assert_eq!(
        parsed["scenario"],
        serde_json::Value::String("green_pr".into())
    );
    assert!(!parsed["state_transitions"].as_array().unwrap().is_empty());
}

#[test]
fn audit_without_golden_uses_defaults() {
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .args([
            "merge-captain",
            "audit",
            fixture("green_pr", "transcripts").to_str().unwrap(),
        ])
        .output()
        .expect("run audit");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("scenario=<none>"));
}

#[test]
fn directory_argument_loads_rotated_logs() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("session-x");
    std::fs::create_dir_all(&session).unwrap();
    let src = std::fs::read_to_string(fixture("green_pr", "transcripts")).unwrap();
    std::fs::write(session.join("event_log.jsonl"), &src).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .args(["merge-captain", "audit", session.to_str().unwrap()])
        .output()
        .expect("run audit");
    assert!(output.status.success());
}
