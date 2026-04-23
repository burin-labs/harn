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
