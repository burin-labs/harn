use std::fs;
use std::process::Command;

use tempfile::TempDir;

#[test]
fn host_lease_store_initialization_failure_preserves_json_contract() {
    let temp = TempDir::new().expect("create temp directory");
    let invalid_root = temp.path().join("not-a-directory");
    fs::write(&invalid_root, "file blocks lease directory creation")
        .expect("create invalid lease root");

    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .args(["host", "lease", "status", "--json"])
        .env(harn_hostlib::HOST_LEASE_ROOT_ENV, &invalid_root)
        .output()
        .expect("run host lease status");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stderr.is_empty(),
        "JSON mode must not emit an unstructured stderr error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("failure output is a JSON envelope");
    assert_eq!(envelope["schemaVersion"], 1);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "host_lease_store");
}
