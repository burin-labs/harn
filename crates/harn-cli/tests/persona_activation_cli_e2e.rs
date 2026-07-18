use std::fs;
use std::path::{Path, PathBuf};
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

fn reviewed_apply_project() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let manifest = temp.path().join("harn.toml");
    fs::write(
        &manifest,
        "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let receipt = temp.path().join("reviewed-compile-receipt.json");
    fs::write(
        &receipt,
        include_str!("fixtures/persona/reviewed-compile-receipt.json"),
    )
    .unwrap();
    (temp, manifest, receipt)
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

#[test]
fn reviewed_receipt_apply_is_discoverable_activated_and_triggerable_through_cli() {
    let (temp, manifest, receipt) = reviewed_apply_project();
    let root = temp.path();
    let manifest = manifest.to_string_lossy();
    let receipt = receipt.to_string_lossy();
    let persona_id = "harn-accepted-prompt-watch-persona/accepted_prompt_watch";

    let applied = json(run(
        root,
        &[
            "persona",
            "--manifest",
            &manifest,
            "materialize",
            "--compile-receipt",
            &receipt,
            "--activate",
            "--json",
        ],
    ));
    assert_eq!(applied["ok"], true);
    assert_eq!(applied["stage"], "complete");
    assert_eq!(applied["verification"]["persona_id"], persona_id);

    let listed = json(run(
        root,
        &["persona", "--manifest", &manifest, "list", "--json"],
    ));
    assert!(listed
        .as_array()
        .unwrap()
        .iter()
        .any(|persona| persona["id"] == persona_id));

    let inspected = json(run(
        root,
        &[
            "persona",
            "--manifest",
            &manifest,
            "inspect",
            persona_id,
            "--json",
        ],
    ));
    assert_eq!(inspected["id"], persona_id);
    assert_eq!(inspected["source"]["kind"], "installed_package");
    assert_eq!(inspected["source"]["integrity"], "observed");

    let activations = json(run(
        root,
        &["persona", "--manifest", &manifest, "activations", "--json"],
    ));
    assert_eq!(activations.as_array().unwrap().len(), 1);
    assert_eq!(activations[0]["persona_id"], persona_id);

    let tick = json(run(
        root,
        &[
            "persona",
            "--manifest",
            &manifest,
            "tick",
            persona_id,
            "--at",
            "2099-07-18T12:00:00Z",
            "--json",
        ],
    ));
    assert_eq!(tick["persona"], persona_id);
    assert_eq!(tick["status"], "completed");
}

#[test]
fn activation_failure_rolls_back_install_without_ledger_mutation() {
    let (temp, manifest, receipt) = reviewed_apply_project();
    let root = temp.path();
    let ledger = root.join(".harn/personas/activations.json");
    fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    let invalid_ledger = b"{\"schema_version\":99,\"activations\":{}}\n";
    fs::write(&ledger, invalid_ledger).unwrap();
    let manifest = manifest.to_string_lossy();
    let receipt = receipt.to_string_lossy();
    let persona_id = "harn-accepted-prompt-watch-persona/accepted_prompt_watch";

    let output = run(
        root,
        &[
            "persona",
            "--manifest",
            &manifest,
            "materialize",
            "--compile-receipt",
            &receipt,
            "--activate",
            "--json",
        ],
    );
    assert!(!output.status.success());
    let failure: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(failure["stage"], "activate");
    assert_eq!(failure["error"]["code"], "activation_failed");
    assert_eq!(failure["error"]["installed_inert"], false);
    assert_eq!(failure["error"]["activation_present"], false);
    assert_eq!(fs::read(&ledger).unwrap(), invalid_ledger);

    let listed = json(run(
        root,
        &["persona", "--manifest", &manifest, "list", "--json"],
    ));
    assert!(!listed
        .as_array()
        .unwrap()
        .iter()
        .any(|persona| persona["id"] == persona_id));
    assert_eq!(
        fs::read_to_string(root.join("harn.toml")).unwrap(),
        "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n"
    );
    assert!(!root.join("harn.lock").exists());
    assert!(!root.join(".harn/package-current.toml").exists());
}
