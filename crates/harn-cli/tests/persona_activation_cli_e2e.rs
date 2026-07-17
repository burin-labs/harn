use std::path::Path;
use std::process::Command;

mod test_util;

use test_util::package_generation::{
    create_package_generation, package_content_hash, publish_package_generation,
};
use test_util::process::harn_e2e_command;

fn installed_persona_project() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::write(root.join("harn.toml"), "[package]\nname = \"consumer\"\n").unwrap();
    let packages = create_package_generation(root);
    let package = packages.join("agents");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("harn.toml"),
        r#"[package]
name = "agents"
version = "1.2.3"

[[personas]]
name = "reviewer"
version = "1.2.3"
description = "Reviews installed-package changes."
entry_workflow = "workflow.harn#run"
tools = ["filesystem", "shell"]
capabilities = ["workspace.read_text"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
model_policy = { default_model = "cheap", escalation_model = "frontier" }
budget = { daily_usd = 10.0, run_usd = 2.0, max_tokens = 4096 }
"#,
    )
    .unwrap();
    std::fs::write(
        package.join("workflow.harn"),
        "pub pipeline run(task) -> dict { return {ok: true} }\n",
    )
    .unwrap();
    let content_hash = package_content_hash(&package);
    let lock = format!(
        r#"version = 4
generator_version = "test"
protocol_artifact_version = "test"

[[package]]
name = "agents"
source = "path+../agents"
content_hash = "{content_hash}"
package_version = "1.2.3"
permissions = ["process:exec", "workspace:read_text"]
host_requirements = ["workspace.read_text"]

[package.exports]
personas = ["reviewer"]
"#
    );
    std::fs::write(root.join("harn.lock"), &lock).unwrap();
    publish_package_generation(root, &lock);
    temp
}

fn run(root: &Path, args: &[&str]) -> std::process::Output {
    let mut command: Command = harn_e2e_command();
    command.current_dir(root).args(args).output().unwrap()
}

fn json(output: std::process::Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn installed_persona_activation_is_explicit_attenuated_and_reversible() {
    let temp = installed_persona_project();
    let root = temp.path();

    let listed = json(run(root, &["persona", "list", "--json"]));
    assert_eq!(listed[0]["id"], "agents/reviewer");
    assert_eq!(listed[0]["source"]["integrity"], "ok");
    assert!(!root.join(".harn/personas/activations.json").exists());

    let activated = json(run(
        root,
        &[
            "persona",
            "activate",
            "agents/reviewer",
            "--autonomy-tier",
            "suggest",
            "--tool",
            "filesystem",
            "--at",
            "2026-07-16T12:00:00Z",
            "--json",
        ],
    ));
    assert_eq!(activated["action"], "activate");
    assert_eq!(activated["schema_version"], 2);
    assert_eq!(activated["changed"], true);
    assert_eq!(
        activated["activation"]["effective_policy"]["autonomy_tier"],
        "suggest"
    );
    assert_eq!(
        activated["activation"]["effective_policy"]["tools"],
        serde_json::json!(["filesystem"])
    );
    assert_eq!(
        activated["activation"]["effective_policy"],
        serde_json::json!({
            "autonomy_tier": "suggest",
            "tools": ["filesystem"],
            "capabilities": ["llm.call", "workspace.read_text"]
        })
    );
    assert!(activated["activation"].get("migration").is_none());
    let activations = json(run(root, &["persona", "activations", "--json"]));
    assert_eq!(activations.as_array().unwrap().len(), 1);
    assert_eq!(activations[0]["persona_id"], "agents/reviewer");

    std::fs::remove_dir_all(
        harn_modules::package_snapshot::PackageSnapshot::acquire(root)
            .unwrap()
            .unwrap()
            .packages_root()
            .join("agents"),
    )
    .unwrap();
    let deactivated = json(run(
        root,
        &[
            "persona",
            "deactivate",
            "agents/reviewer",
            "--at",
            "2026-07-16T12:01:00Z",
            "--json",
        ],
    ));
    assert_eq!(deactivated["action"], "deactivate");
    assert_eq!(deactivated["changed"], true);
    assert!(json(run(root, &["persona", "activations", "--json"]))
        .as_array()
        .unwrap()
        .is_empty());
}
