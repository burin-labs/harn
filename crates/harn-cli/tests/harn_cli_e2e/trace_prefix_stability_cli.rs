use crate::test_util::process::harn_e2e_command;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/prefix_stability/stable-agent-loop")
}

fn run_check(path: &Path) -> std::process::Output {
    harn_e2e_command()
        .arg("trace")
        .arg("prefix-stability")
        .arg(path)
        .arg("--json")
        .output()
        .expect("run prefix stability check")
}

#[test]
fn captured_agent_loop_fixture_is_append_only() {
    let output = run_check(&fixture_dir());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON report");

    assert_eq!(report["schema_version"], "harn.trace.prefix_stability.v1");
    assert_eq!(report["stable"], true);
    assert_eq!(report["request_count"], 3);
    assert_eq!(report["pairs"][0]["leading_identical_messages"], 2);
    assert_eq!(report["pairs"][1]["leading_identical_messages"], 4);
}

#[test]
fn changed_early_message_fails_with_an_actionable_report() {
    let fixture = fixture_dir();
    let temp = tempfile::tempdir().expect("transcript tempdir");
    std::fs::create_dir(temp.path().join("raw-provider")).expect("raw provider directory");
    std::fs::copy(
        fixture.join("llm_transcript.jsonl"),
        temp.path().join("llm_transcript.jsonl"),
    )
    .expect("copy transcript");
    for name in [
        "turn-0-request.json",
        "turn-1-request.json",
        "turn-2-request.json",
    ] {
        std::fs::copy(
            fixture.join("raw-provider").join(name),
            temp.path().join("raw-provider").join(name),
        )
        .expect("copy request");
    }
    let changed_path = temp.path().join("raw-provider/turn-1-request.json");
    let mut changed: Value =
        serde_json::from_slice(&std::fs::read(&changed_path).expect("read copied request"))
            .expect("request JSON");
    changed["body"]["messages"][0]["content"] = Value::String("You help. Turn 1.".into());
    std::fs::write(
        &changed_path,
        serde_json::to_vec_pretty(&changed).expect("encode changed request"),
    )
    .expect("write changed request");

    let output = run_check(temp.path());
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON report");

    assert_eq!(report["stable"], false);
    assert_eq!(report["pairs"][0]["modified_message"]["index"], 0);
    assert!(report["pairs"][0]["modified_message"]["first_differing_byte"].is_number());
    assert_eq!(report["pairs"][0]["modified_message"]["role"], "system");
    assert_eq!(
        report["pairs"][0]["modified_message"]["before"]["content"],
        "You help."
    );
    assert_eq!(
        report["pairs"][0]["modified_message"]["after"]["content"],
        "You help. Turn 1."
    );
}
