use std::fs;

use tempfile::TempDir;

mod test_util;

use test_util::process::run_harn_e2e;

#[test]
fn host_lease_store_initialization_failure_preserves_json_contract() {
    let temp = TempDir::new().expect("create temp directory");
    let invalid_root = temp.path().join("not-a-directory");
    fs::write(&invalid_root, "file blocks lease directory creation")
        .expect("create invalid lease root");

    let invalid_root = invalid_root.to_string_lossy();
    let output = run_harn_e2e(
        &["host", "lease", "status", "--json"],
        &[(harn_hostlib::HOST_LEASE_ROOT_ENV, invalid_root.as_ref())],
    );

    assert_eq!(output.exit_code, 1);
    assert!(
        output.stderr.is_empty(),
        "JSON mode must not emit an unstructured stderr error: {}",
        output.stderr
    );
    let envelope: serde_json::Value =
        serde_json::from_str(&output.stdout).expect("failure output is a JSON envelope");
    assert_eq!(envelope["schemaVersion"], 1);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "host_lease_store");
}
