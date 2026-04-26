use std::fs;
use std::process::Command;

use tempfile::TempDir;

fn write_manifest(body: &str) -> TempDir {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join(".git")).unwrap();
    fs::write(temp.path().join("harn.toml"), body).unwrap();
    temp
}

fn valid_manifest() -> &'static str {
    r#"
[[personas]]
name = "merge_captain"
description = "Owns merge readiness."
entry_workflow = "workflows/merge.harn#run"
tools = ["github"]
capabilities = ["git.get_diff", "project.test_commands"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
triggers = ["github.pr_opened"]
schedules = ["*/30 * * * *"]
handoffs = ["review_captain"]
context_packs = ["repo_policy"]
evals = ["merge_safety"]
budget = { daily_usd = 20.0, frontier_escalations = 3 }
model_policy = { default_model = "gpt-5.4-mini", escalation_model = "gpt-5.4" }

[[personas]]
name = "review_captain"
description = "Owns review quality."
entry_workflow = "workflows/review.harn#run"
tools = ["github"]
capabilities = ["git.get_diff"]
autonomy_tier = "suggest"
receipt_policy = "required"

[[personas]]
name = "oncall_captain"
description = "Owns incident intake."
entry_workflow = "workflows/oncall.harn#run"
tools = ["slack"]
capabilities = ["interaction.ask"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
"#
}

#[test]
fn persona_list_and_inspect_emit_stable_json() {
    let temp = write_manifest(valid_manifest());

    let list = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .args(["persona", "list", "--json"])
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    let personas: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(personas.as_array().unwrap().len(), 3);

    let inspect = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .args(["persona", "inspect", "merge_captain", "--json"])
        .output()
        .unwrap();
    assert!(
        inspect.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&inspect.stdout),
        String::from_utf8_lossy(&inspect.stderr)
    );
    let persona: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(persona["name"], "merge_captain");
    assert_eq!(persona["autonomy_tier"], "act_with_approval");
    assert_eq!(persona["receipt_policy"], "required");
    assert_eq!(persona["capabilities"][0], "git.get_diff");
    assert_eq!(persona["model_policy"]["default_model"], "gpt-5.4-mini");
    assert_eq!(persona["budget"]["daily_usd"], 20.0);
    assert_eq!(persona["triggers"][0], "github.pr_opened");
    assert_eq!(persona["handoffs"][0], "review_captain");
    assert_eq!(persona["context_packs"][0], "repo_policy");
    assert_eq!(persona["evals"][0], "merge_safety");
}

#[test]
fn persona_cli_rejects_required_invalid_manifest_cases() {
    for (body, expected) in [
        (
            r#"
[[personas]]
name = "merge_captain"
description = "Owns merge readiness."
tools = ["github"]
capabilities = ["git.get_diff"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
"#,
            "missing required entry_workflow",
        ),
        (
            r#"
[[personas]]
name = "merge_captain"
description = "Owns merge readiness."
entry_workflow = "workflows/merge.harn#run"
tools = ["github"]
capabilities = ["unknown.do_thing"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
"#,
            "unknown capability 'unknown.do_thing'",
        ),
        (
            r#"
[[personas]]
name = "merge_captain"
description = "Owns merge readiness."
entry_workflow = "workflows/merge.harn#run"
tools = ["github"]
capabilities = ["git.get_diff"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
budget = { daily_usd = 2.0, surprise = 1 }
"#,
            "unknown budget field",
        ),
        (
            r#"
[[personas]]
name = "merge_captain"
description = "Owns merge readiness."
entry_workflow = "workflows/merge.harn#run"
tools = ["github"]
capabilities = ["git.get_diff"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
handoffs = ["review_captain"]
"#,
            "unknown handoff target 'review_captain'",
        ),
    ] {
        let temp = write_manifest(body);
        let output = Command::new(env!("CARGO_BIN_EXE_harn"))
            .current_dir(temp.path())
            .args(["persona", "list"])
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "expected failure for {expected}, stdout={}, stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(expected),
            "expected {expected:?} in stderr: {stderr}"
        );
    }
}

#[test]
fn persona_manifest_flag_loads_example_personas() {
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .args([
            "persona",
            "--manifest",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../examples/personas/harn.toml"
            ),
            "inspect",
            "merge_captain",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let persona: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(persona["name"], "merge_captain");
    assert_eq!(persona["receipt_policy"], "required");
}

#[test]
fn persona_manifest_flag_loads_fixer_persona() {
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .args([
            "persona",
            "--manifest",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../personas/fixer/harn.toml"
            ),
            "inspect",
            "fixer",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let persona: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(persona["name"], "fixer");
    assert_eq!(persona["triggers"][0], "invariant.blocked_with_remediation");
    assert_eq!(persona["entry_workflow"], "manifest.harn#run");
    assert_eq!(persona["receipt_policy"], "required");
}

#[test]
fn persona_runtime_status_tick_and_budget_are_persisted() {
    let temp = write_manifest(valid_manifest());
    let state_dir = temp.path().join(".harn-personas-test");

    let status = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .args([
            "persona",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "status",
            "merge_captain",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    let status_json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status_json["state"], "idle");
    assert_eq!(status_json["queued_events"], 0);
    assert_eq!(status_json["budget"]["daily_usd"], 20.0);

    let tick = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .args([
            "persona",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "tick",
            "merge_captain",
            "--at",
            "2026-04-24T12:30:00Z",
            "--cost-usd",
            "0.25",
            "--tokens",
            "12",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        tick.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&tick.stdout),
        String::from_utf8_lossy(&tick.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&tick.stdout).unwrap();
    assert_eq!(receipt["status"], "completed");
    assert!(receipt["lease"]["id"]
        .as_str()
        .unwrap()
        .starts_with("persona_lease_"));

    // Pin the status query to the same UTC day as the tick above. Without
    // --at, the budget window is computed from real wall-clock time, so the
    // assertion silently breaks the moment the test runs after the tick's
    // UTC midnight (i.e. roughly any time of day in PT/CT/ET).
    let status = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .args([
            "persona",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "status",
            "merge_captain",
            "--at",
            "2026-04-24T13:00:00Z",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(status.status.success());
    let status_json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status_json["state"], "idle");
    assert_eq!(status_json["last_run"], "2026-04-24T12:30:00Z");
    assert_eq!(status_json["budget"]["spent_today_usd"], 0.25);
    assert_eq!(status_json["budget"]["tokens_today"], 12);
}

#[test]
fn persona_pause_resume_disable_trigger_controls_are_durable() {
    let temp = write_manifest(valid_manifest());
    let state_dir = temp.path().join(".harn-personas-test");

    let pause = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .args([
            "persona",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "pause",
            "merge_captain",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(pause.status.success());

    let trigger = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .args([
            "persona",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "trigger",
            "merge_captain",
            "--provider",
            "github",
            "--kind",
            "pull_request",
            "--metadata",
            "repository=burin-labs/harn",
            "--metadata",
            "number=462",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        trigger.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&trigger.stdout),
        String::from_utf8_lossy(&trigger.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&trigger.stdout).unwrap();
    assert_eq!(receipt["status"], "queued");
    assert_eq!(receipt["work_key"], "github:burin-labs/harn:pr:462");

    let resume = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .args([
            "persona",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "resume",
            "merge_captain",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(resume.status.success());
    let status_json: serde_json::Value = serde_json::from_slice(&resume.stdout).unwrap();
    assert_eq!(status_json["state"], "idle");
    assert_eq!(status_json["queued_events"], 0);

    let disable = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .args([
            "persona",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "disable",
            "merge_captain",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(disable.status.success());

    let trigger = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .args([
            "persona",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "trigger",
            "merge_captain",
            "--provider",
            "slack",
            "--kind",
            "message",
            "--metadata",
            "channel=C123",
            "--metadata",
            "ts=1713988800.000100",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(trigger.status.success());
    let receipt: serde_json::Value = serde_json::from_slice(&trigger.stdout).unwrap();
    assert_eq!(receipt["status"], "dead_lettered");
}

#[test]
fn persona_runtime_blocks_budget_exhaustion() {
    let temp = write_manifest(
        r#"
[[personas]]
name = "merge_captain"
description = "Owns merge readiness."
entry_workflow = "workflows/merge.harn#run"
tools = ["github"]
capabilities = ["git.get_diff"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
triggers = ["github.pr_opened"]
budget = { daily_usd = 0.01, run_usd = 0.01, max_tokens = 10 }
"#,
    );
    let state_dir = temp.path().join(".harn-personas-test");
    let trigger = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .args([
            "persona",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "trigger",
            "merge_captain",
            "--provider",
            "github",
            "--kind",
            "check_run",
            "--metadata",
            "repository=burin-labs/harn",
            "--metadata",
            "check_name=ci",
            "--cost-usd",
            "0.02",
            "--tokens",
            "1",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(trigger.status.success());
    let receipt: serde_json::Value = serde_json::from_slice(&trigger.stdout).unwrap();
    assert_eq!(receipt["status"], "budget_exhausted");
    assert!(receipt["error"].as_str().unwrap().contains("run_usd"));

    let status = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .args([
            "persona",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "status",
            "merge_captain",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(status.status.success());
    let status_json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert!(status_json["last_error"]
        .as_str()
        .unwrap()
        .contains("run_usd"));
}
