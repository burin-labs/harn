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
fn declared_package_export_reaches_its_function_through_a_colliding_directory() {
    let (temp, _package) = scaffold_and_install("package");
    let consumer = temp.path().join("consumer");
    fs::create_dir(&consumer).unwrap();
    fs::write(
        consumer.join("harn.toml"),
        "[package]\nname = \"consumer\"\n[dependencies]\nexample-package = { path = \"../example-package\" }\n",
    )
    .unwrap();
    fs::write(
        consumer.join("main.harn"),
        "import { greet } from \"example-package/lib\"\nfn main(harness: Harness) { harness.stdio.log(greet(\"consumer\")) }\n",
    )
    .unwrap();
    let install = run(Command::new(harn_e2e_binary())
        .current_dir(&consumer)
        .arg("install"));
    assert!(install.status.success(), "{install:?}");
    let check = run(Command::new(harn_e2e_binary())
        .current_dir(&consumer)
        .args(["check", "main.harn", "--json"]));
    assert!(check.status.success(), "{check:?}");
    let execute = run(Command::new(harn_e2e_binary())
        .current_dir(&consumer)
        .args(["run", "main.harn"]));
    assert!(execute.status.success(), "{execute:?}");
    assert_eq!(
        String::from_utf8(execute.stdout).unwrap().trim(),
        "[harn] Hello, consumer!"
    );
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
fn configured_package_test_roots_inventory_and_execute_the_same_nested_tests() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join("harn.toml"), "[tests]\nroots = [\"./nested/tests/\", \"scripts/tests/selected.harn\", \"nested/tests\"]\n").unwrap();
    fs::create_dir_all(temp.path().join("nested/tests")).unwrap();
    fs::create_dir_all(temp.path().join("scripts/tests")).unwrap();
    fs::write(
        temp.path().join("nested/tests/pass.harn"),
        "pipeline test_nested(task: unknown) { assert(true) }\n",
    )
    .unwrap();
    let selected = temp.path().join("scripts/tests/selected.harn");
    fs::write(
        &selected,
        "pipeline test_selected(task: unknown) { assert(true) }\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("scripts/tests/unselected.harn"),
        "pipeline test_unselected(task: unknown) { assert(false) }\n",
    )
    .unwrap();
    let inventory = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["package", "test-inventory", ".", "--json"]));
    assert!(
        inventory.status.success(),
        "{}",
        String::from_utf8_lossy(&inventory.stdout)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&inventory.stdout).unwrap();
    assert_eq!(receipt["data"]["selected_file_count"], 2);
    assert_eq!(receipt["data"]["discovered_test_count"], 2);
    let report_path = temp.path().join("run.json");
    let output = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["test", "package", "--json-out"])
        .arg(&report_path));
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("ran 2 case(s) from 2 discoverable .harn file(s) under package"));
    assert_eq!(report["summary"]["total"], 2);
    assert_eq!(report["summary"]["passed"], 2);
    let mut names = report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| case["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["test_nested", "test_selected"]);
    let verified = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["package", "verify", ".", "--json"]));
    let receipt: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    let report = if verified.status.success() {
        &receipt["data"]
    } else {
        &receipt["error"]["details"]
    };
    let check = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "package tests")
        .unwrap();
    assert_eq!(check["reached"], true, "{receipt}");
    assert_eq!(check["status"], "pass", "{receipt}");
    fs::write(
        &selected,
        "pipeline test_selected(task: unknown) { assert(false) }\n",
    )
    .unwrap();
    let inventory = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["package", "test-inventory", ".", "--json"]));
    assert!(
        inventory.status.success(),
        "inventory must not claim execution"
    );
    let failed = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["test", "package", "--json-out"])
        .arg(&report_path));
    assert!(
        !failed.status.success(),
        "a real selected failure must fail execution"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["summary"]["total"], 2);
    assert_eq!(report["summary"]["failed"], 1);
    let failed_case = report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "test_selected")
        .unwrap();
    assert_eq!(failed_case["outcome"], "failed");
    let verified = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["package", "verify", ".", "--json"]));
    assert!(!verified.status.success());
    let receipt: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    let check = receipt["error"]["details"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "package tests")
        .unwrap();
    assert_eq!(check["reached"], true, "{receipt}");
    assert_eq!(check["status"], "fail", "{receipt}");
}

#[test]
fn package_test_roots_refuse_invalid_missing_and_empty_selections() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("empty")).unwrap();
    for roots in [
        "[]",
        "[\"../escape\"]",
        "[\"/absolute\"]",
        "[\"missing\"]",
        "[\"empty\"]",
        "[{ path = \"empty\", recursive = false, pattern = \"[\" }]",
        "[{ path = \"empty\", recursive = false, pattern = \"../*.harn\" }]",
        "[{ path = \"empty\", recursive = false, exclude = [\"[\"] }]",
    ] {
        fs::write(temp.path().join("harn.toml"), format!("[tests]\nroots = {roots}\nallow_empty = true\nreason = \"must not hide a broken root\"\n")).unwrap();
        let inventory = run(Command::new(harn_e2e_binary())
            .current_dir(temp.path())
            .args(["package", "test-inventory", ".", "--json"]));
        assert!(!inventory.status.success(), "accepted {roots}");
        let receipt: serde_json::Value = serde_json::from_slice(&inventory.stdout).unwrap();
        assert_eq!(receipt["error"]["code"], "package_test_discovery_failed");
        let execution = run(Command::new(harn_e2e_binary())
            .current_dir(temp.path())
            .args(["test", "package"]));
        assert!(!execution.status.success(), "execution accepted {roots}");
    }
}

#[test]
fn package_test_directory_patterns_exclude_helpers_and_nested_fixture_programs() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("harn.toml"), "[tests]\nroots = [{ path = \"scripts/tests\", recursive = false, pattern = \"test_*.harn\", exclude = [\"test_helpers.harn\"] }]\n").unwrap();
    fs::create_dir_all(temp.path().join("scripts/tests/fixtures")).unwrap();
    fs::write(
        temp.path().join("scripts/tests/test_selected.harn"),
        "pipeline test_selected() { assert(true) }\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("scripts/tests/helpers.harn"),
        "fn helper() {}\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("scripts/tests/test_helpers.harn"),
        "fn helper() {}\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("scripts/tests/fixtures/test_failure.harn"),
        "pipeline test_failure() { assert(false) }\n",
    )
    .unwrap();
    let inventory = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["package", "test-inventory", ".", "--json"]));
    assert!(
        inventory.status.success(),
        "{}",
        String::from_utf8_lossy(&inventory.stdout)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&inventory.stdout).unwrap();
    assert_eq!(receipt["data"]["selected_file_count"], 1);
    assert_eq!(receipt["data"]["discovered_test_count"], 1);
    let report = temp.path().join("report.json");
    let execution = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["test", "package", "--json-out"])
        .arg(&report));
    assert!(
        execution.status.success(),
        "{}",
        String::from_utf8_lossy(&execution.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(report["summary"]["total"], 1);
    assert_eq!(report["summary"]["passed"], 1);
    assert_eq!(report["cases"][0]["name"], "test_selected");
    fs::write(temp.path().join("harn.toml"), "[tests]\nallow_empty = true\nreason = \"must not hide an empty configured selection\"\nroots = [{ path = \"scripts/tests\", recursive = false, pattern = \"test_*.harn\", exclude = [\"test_*.harn\"] }]\n").unwrap();
    let empty = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["package", "test-inventory", ".", "--json"]));
    assert!(!empty.status.success());
    let receipt: serde_json::Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert_eq!(
        receipt["error"]["details"]["inventory"]["selected_file_count"],
        0
    );
}

#[test]
fn intentionally_testless_packages_report_zero_execution_and_the_reason() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("harn.toml"),
        "[tests]\nallow_empty = true\nreason = \"schema-only package\"\n",
    )
    .unwrap();
    let report = temp.path().join("report.json");
    let execution = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["test", "package", "--json-out"])
        .arg(&report));
    assert!(
        execution.status.success(),
        "{}",
        String::from_utf8_lossy(&execution.stderr)
    );
    assert!(String::from_utf8_lossy(&execution.stderr)
        .contains("Package intentionally has no tests: schema-only package"));
    let report: serde_json::Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(report["summary"]["total"], 0);
    assert_eq!(report["summary"]["passed"], 0);
    assert_eq!(report["cases"], serde_json::json!([]));
}

#[cfg(unix)]
#[test]
fn package_test_roots_reject_unreadable_files_and_symlink_traversal() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("tests")).unwrap();
    let file = temp.path().join("tests/test_selected.harn");
    fs::write(&file, "pipeline test_selected() { assert(true) }\n").unwrap();
    fs::write(
        temp.path().join("harn.toml"),
        "[tests]\nroots = [\"tests\"]\n",
    )
    .unwrap();
    let original = fs::metadata(&file).unwrap().permissions();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o0)).unwrap();
    let permissions_enforced = fs::read(&file).is_err();
    let unreadable = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["package", "test-inventory", ".", "--json"]));
    fs::set_permissions(&file, original).unwrap();
    if permissions_enforced {
        assert!(!unreadable.status.success());
        let receipt: serde_json::Value = serde_json::from_slice(&unreadable.stdout).unwrap();
        assert_eq!(
            receipt["error"]["details"]["inventory"]["files_with_errors"][0]["sha256"],
            "unreadable"
        );
    } else {
        eprintln!("unreadable-file control unavailable: this process bypasses mode permissions");
    }
    symlink(temp.path().join("tests"), temp.path().join("alias")).unwrap();
    fs::write(
        temp.path().join("harn.toml"),
        "[tests]\nroots = [\"alias/test_selected.harn\"]\n",
    )
    .unwrap();
    let symlinked = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["package", "test-inventory", ".", "--json"]));
    assert!(!symlinked.status.success());
    let execution = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["test", "package"]));
    assert!(!execution.status.success());
}

#[test]
fn package_test_inventory_is_parser_backed_and_read_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join("harn.toml"), "").expect("manifest");
    fs::create_dir(temp.path().join("tests")).expect("tests directory");
    fs::write(
        temp.path().join("tests/by_name.harn"),
        "pipeline test_by_name(task: unknown) { assert(true) }\n",
    )
    .expect("named test");
    fs::write(
        temp.path().join("tests/by_attribute.harn"),
        "@test\npipeline arbitrary_name(task: unknown) { assert(true) }\n",
    )
    .expect("annotated test");
    let mut before = fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    before.sort();

    let output = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["package", "test-inventory", ".", "--json"]));

    assert!(
        output.status.success(),
        "inventory failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).expect("receipt");
    assert_eq!(receipt["schemaVersion"], 1);
    assert_eq!(receipt["data"]["selected_file_count"], 2);
    assert_eq!(receipt["data"]["discovered_test_count"], 2);
    assert!(receipt["data"].get("files").is_none());
    assert_eq!(
        receipt["data"]["files_without_tests"],
        serde_json::json!([])
    );
    let mut after = fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    after.sort();
    assert_eq!(after, before, "inventory mutated the package");
}

#[test]
fn package_test_inventory_rejects_an_ordinary_pipeline_with_file_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join("harn.toml"), "").expect("manifest");
    fs::create_dir(temp.path().join("tests")).expect("tests directory");
    fs::write(
        temp.path().join("tests/noop.harn"),
        "pipeline test(task: unknown) { assert(true) }\n",
    )
    .expect("ordinary pipeline");

    let output = run(Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["package", "test-inventory", ".", "--json"]));

    assert!(!output.status.success());
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).expect("receipt");
    assert!(receipt["error"]["details"]["inventory"]
        .get("files")
        .is_none());
    let empty = &receipt["error"]["details"]["inventory"]["files_without_tests"][0];
    assert_eq!(empty["path"], "tests/noop.harn");
    assert!(empty["sha256"].as_str().unwrap().starts_with("sha256:"));
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
