use std::fs;
use std::process::Command;

use crate::test_util::process::harn_e2e_binary;

#[test]
fn registry_verify_cli_persists_the_same_success_receipt_it_prints() {
    let temp = tempfile::tempdir().expect("tempdir");
    let index = temp.path().join("harn-package-index.toml");
    let receipt = temp.path().join("registry-receipt.json");
    fs::write(
        &index,
        r#"version = 2

[[package]]
name = "@acme/pkg"
repository = "https://github.com/acme/pkg"
provenance = "https://github.com/acme/pkg"

[[package.version]]
version = "1.0.0"
git = "https://github.com/acme/pkg"
tag = "v1.0.0"
rev = "0123456789abcdef0123456789abcdef01234567"
provenance = "https://github.com/acme/pkg/releases/tag/v1.0.0"
"#,
    )
    .expect("write registry fixture");

    let output = Command::new(harn_e2e_binary())
        .args([
            "package",
            "registry",
            "verify",
            index.to_str().unwrap(),
            "--json",
            "--receipt-out",
            receipt.to_str().unwrap(),
        ])
        .output()
        .expect("run harn package registry verify");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let printed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("printed JSON receipt");
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(receipt).expect("persisted receipt"))
            .expect("persisted JSON receipt");
    assert_eq!(persisted, printed);
    assert_eq!(
        printed["schema_version"],
        "harn.package_registry_verification.v1"
    );
    assert_eq!(printed["ok"], true);
    assert_eq!(printed["package_count"], 1);
    assert_eq!(printed["version_count"], 1);
}
