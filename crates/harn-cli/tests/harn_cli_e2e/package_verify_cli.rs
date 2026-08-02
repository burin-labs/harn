use std::fs;
use std::process::Command;

use crate::test_util::process::harn_e2e_binary;

fn run(command: &mut Command) -> std::process::Output {
    command
        .env("HARN_LLM_PROVIDER", "mock")
        .env("HARN_LLM_CALLS_DISABLED", "1")
        .output()
        .expect("run harn")
}

fn scaffold_and_install(kind: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().expect("tempdir");
    let package = temp.path().join(format!("example-{kind}"));
    let output = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["new", kind, package.file_name().unwrap().to_str().unwrap()]));
    assert!(
        output.status.success(),
        "scaffold failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run(Command::new(harn_e2e_binary())
        .current_dir(&package)
        .arg("install"));
    assert!(
        output.status.success(),
        "install failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (temp, package)
}

fn verify(package: &std::path::Path) -> serde_json::Value {
    verify_with_policy(package, false)
}

fn verify_with_policy(package: &std::path::Path, strict: bool) -> serde_json::Value {
    let receipt_name = if strict {
        "package-verify-strict.json"
    } else {
        "package-verify.json"
    };
    let receipt = package.join(".harn/receipts").join(receipt_name);
    let mut command = Command::new(harn_e2e_binary());
    command
        .current_dir(package)
        .args(["package", "verify", "."]);
    if strict {
        command.arg("--strict");
    }
    let output = run(command.arg("--json").arg("--receipt-out").arg(&receipt));
    assert!(
        output.status.success(),
        "verify failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON verification receipt");
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(receipt).expect("persisted receipt"))
            .expect("persisted receipt JSON");
    assert_eq!(persisted, stdout);
    stdout
}

#[test]
fn ordinary_package_receipt_marks_connector_gate_not_applicable() {
    let (_temp, package) = scaffold_and_install("package");
    let receipt = verify(&package);

    assert_eq!(receipt["schemaVersion"], 2);
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["data"]["strict_requested"], false);
    assert_eq!(
        receipt["data"]["package_kinds"],
        serde_json::json!(["package"])
    );
    let connector = receipt["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "connector contract")
        .expect("connector gate receipt");
    assert_eq!(connector["applicable"], false);
    assert_eq!(connector["reached"], false);
    assert_eq!(connector["status"], "skipped");
}

#[test]
fn strict_package_receipt_proves_both_source_gate_policies_fired() {
    let (_temp, package) = scaffold_and_install("package");
    let receipt = verify_with_policy(&package, true);

    assert_eq!(receipt["schemaVersion"], 2);
    assert_eq!(receipt["data"]["strict_requested"], true);
    let checks = receipt["data"]["checks"].as_array().unwrap();
    let command_for = |name: &str| {
        checks
            .iter()
            .find(|check| check["name"] == name)
            .and_then(|check| check["command"].as_array())
            .map(|command| {
                command
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| panic!("missing recorded command for {name}"))
    };
    let check = command_for("harn check");
    assert_eq!(&check[1..4], ["check", "--strict", "--strict-types"]);
    let lint = command_for("harn lint");
    assert_eq!(&lint[1..3], ["lint", "--strict"]);
}

#[test]
fn connector_package_receipt_proves_contract_and_fixture_gates_fired() {
    let (_temp, package) = scaffold_and_install("connector");
    let receipt = verify(&package);

    assert_eq!(
        receipt["data"]["package_kinds"],
        serde_json::json!(["package", "connector"])
    );
    assert_eq!(receipt["data"]["connector_contract"]["fixture_count"], 1);
    let connector = receipt["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "connector contract")
        .expect("connector gate receipt");
    assert_eq!(connector["applicable"], true);
    assert_eq!(connector["reached"], true);
    assert_eq!(connector["status"], "pass");
}

#[test]
fn connector_test_namespace_is_removed() {
    let output = run(Command::new(harn_e2e_binary()).args(["connector", "test"]));
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unrecognized subcommand 'test'"),
        "{stderr}"
    );
}

#[test]
fn tool_scaffold_passes_canonical_package_verification() {
    let temp = tempfile::tempdir().expect("tempdir");
    let package = temp.path().join("example-tool");
    let output = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["tool", "new", "example-tool"]));
    assert!(
        output.status.success(),
        "tool scaffold failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run(Command::new(harn_e2e_binary())
        .current_dir(&package)
        .arg("install"));
    assert!(
        output.status.success(),
        "tool install failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt = verify(&package);
    assert_eq!(
        receipt["data"]["package_kinds"],
        serde_json::json!(["package", "tool"])
    );
}
