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
        // Generated package and connector scaffolds are the public strict-policy baseline.
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

fn recorded_command<'a>(receipt: &'a serde_json::Value, name: &str) -> Vec<&'a str> {
    receipt["data"]["checks"]
        .as_array()
        .and_then(|checks| checks.iter().find(|check| check["name"] == name))
        .and_then(|check| check["command"].as_array())
        .map(|command| {
            command
                .iter()
                .map(|argument| {
                    argument
                        .as_str()
                        .unwrap_or_else(|| panic!("non-string argument in recorded {name} command"))
                })
                .collect()
        })
        .unwrap_or_else(|| panic!("missing recorded command for {name}"))
}

fn assert_strict_source_gate_commands(receipt: &serde_json::Value) {
    let check = recorded_command(receipt, "harn check");
    assert_eq!(
        check.get(1..4),
        Some(["check", "--strict", "--strict-types"].as_slice())
    );
    let lint = recorded_command(receipt, "harn lint");
    assert_eq!(lint.get(1..3), Some(["lint", "--strict"].as_slice()));
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
    assert_strict_source_gate_commands(&receipt);
}

#[test]
fn strict_connector_package_receipt_proves_all_gates_fired() {
    let (_temp, package) = scaffold_and_install("connector");
    let receipt = verify_with_policy(&package, true);

    assert_eq!(receipt["data"]["strict_requested"], true);
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

    assert_strict_source_gate_commands(&receipt);
}

#[test]
fn connector_package_verify_rejects_inbound_credential_sources() {
    let (_temp, package) = scaffold_and_install("connector");
    let manifest_path = package.join("harn.toml");
    let manifest = fs::read_to_string(&manifest_path).expect("connector manifest");
    let unauthenticated = "auth_type = \"none\"\nflow = \"none\"\n";
    assert!(
        manifest.contains(unauthenticated),
        "connector scaffold setup contract changed"
    );
    let directed = r#"auth_type = "api-key"
flow = "api-key"
required_secrets = [
  { id = "echo/webhook-secret", direction = "inbound" },
  { id = "echo/api-token", direction = "outbound" },
]
credential_environment = [
  { secret = "echo/webhook-secret", environment_names = ["ECHO_API_TOKEN"] },
]
"#;
    fs::write(
        &manifest_path,
        manifest.replacen(unauthenticated, directed, 1),
    )
    .expect("write directed connector manifest");

    let output = run(Command::new(harn_e2e_binary())
        .current_dir(&package)
        .args(["package", "verify", ".", "--json"]));
    assert!(
        !output.status.success(),
        "inbound credential source unexpectedly passed package verification"
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON verification receipt");
    assert_eq!(receipt["ok"], false);
    let connector = receipt
        .pointer("/error/details/checks")
        .and_then(serde_json::Value::as_array)
        .and_then(|checks| {
            checks
                .iter()
                .find(|check| check["name"] == "connector contract")
        })
        .unwrap_or_else(|| panic!("connector contract gate receipt missing: {receipt}"));
    assert_eq!(connector["reached"], true);
    assert_eq!(connector["status"], "fail");
    assert!(
        connector["stderr"]
            .as_str()
            .is_some_and(|stderr| stderr.contains("must be outbound, but is declared inbound")),
        "connector receipt did not record the direction failure: {connector}"
    );
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
