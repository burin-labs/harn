//! End-to-end CLI contract for complete public API type linting.

use std::path::Path;

mod test_util;

use test_util::process::harn_e2e_command;

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    harn_e2e_command()
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn harn lint")
}

fn stdout_json(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("stdout is not JSON: {error}\nstdout:\n{stdout}");
    })
}

fn write_untyped_api(root: &Path) {
    std::fs::write(
        root.join("api.harn"),
        "pub fn run(value) { return value }\npub pipeline deploy(task) { return task }\n",
    )
    .unwrap();
}

fn owned_diagnostics(value: &serde_json::Value) -> Vec<&serde_json::Value> {
    value["data"]["files"][0]["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "HARN-LNT-067")
        .collect()
}

#[test]
fn cli_override_emits_structured_public_api_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    write_untyped_api(temp.path());

    let output = run(
        temp.path(),
        &["lint", "--require-public-api-types", "--json", "api.harn"],
    );
    assert!(
        output.status.success(),
        "warnings remain advisory: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed = stdout_json(&output);
    let diagnostics = owned_diagnostics(&parsed);
    assert_eq!(diagnostics.len(), 4, "envelope: {parsed}");
    assert!(diagnostics
        .iter()
        .all(|diagnostic| diagnostic["source"] == "lint"));
    assert!(diagnostics
        .iter()
        .all(|diagnostic| diagnostic["span"]["start"].is_number()));
}

#[test]
fn project_policy_and_severity_override_fail_lint_without_cli_flag() {
    let temp = tempfile::tempdir().unwrap();
    write_untyped_api(temp.path());
    std::fs::write(
        temp.path().join("harn.toml"),
        r#"
[lint]
require-public-api-types = true

[lint.severity]
missing-public-api-type = "error"
"#,
    )
    .unwrap();

    let output = run(temp.path(), &["lint", "--json", "api.harn"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "configured error should fail lint: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed = stdout_json(&output);
    let diagnostics = owned_diagnostics(&parsed);
    assert_eq!(diagnostics.len(), 4, "envelope: {parsed}");
    assert!(diagnostics
        .iter()
        .all(|diagnostic| diagnostic["severity"] == "error"));
}
