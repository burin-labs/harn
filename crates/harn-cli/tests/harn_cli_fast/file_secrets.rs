//! Real CLI processes must share the explicit durable provider, including revoke.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::test_util::process::harn_e2e_command;

fn run(directory: &Path, file: Option<&Path>, program: &str) -> std::process::Output {
    let mut command = harn_e2e_command();
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", directory)
        .env("HARN_SECRET_PROVIDERS", "file")
        .env("HARN_LLM_CALLS_DISABLED", "1")
        .current_dir(directory)
        .args(["run", "--environment-policy", "isolated", "-e", program]);
    if let Some(file) = file {
        command.env("HARN_SECRET_FILE_PATH", file);
    }
    command.output().expect("run native Harn")
}

fn private_directory() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

#[test]
fn native_file_grant_read_and_revoke_cross_process_boundaries() {
    let directory = private_directory();
    let file = directory.path().join("secrets.json");
    for program in [
        r#"harness.secrets.write("application/token", "synthetic-only")"#,
        r#"assert(harness.secrets.read("application/token") == "synthetic-only")"#,
        r#"harness.secrets.delete("application/token")"#,
    ] {
        let output = run(directory.path(), Some(&file), program);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let deleted = run(
        directory.path(),
        Some(&file),
        r#"harness.secrets.read("application/token")"#,
    );
    assert!(
        !deleted.status.success(),
        "deleted credential must be absent to a new VM"
    );
    let stored: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(stored, serde_json::json!({}));
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn native_file_configuration_and_corruption_fail_without_exposing_values() {
    let directory = private_directory();
    let program = r#"harness.secrets.write("application/token", "synthetic-only")"#;
    let absent = run(directory.path(), None, program);
    assert!(!absent.status.success());
    assert!(String::from_utf8_lossy(&absent.stderr).contains("HARN_SECRET_FILE_PATH"));
    let file = directory.path().join("secrets.json");
    let corrupt = br#"{"synthetic-private-canary": not-json}"#;
    fs::write(&file, corrupt).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let failed = run(directory.path(), Some(&file), program);
    assert!(!failed.status.success());
    assert!(!String::from_utf8_lossy(&failed.stderr).contains("synthetic-private-canary"));
    assert_eq!(fs::read(&file).unwrap(), corrupt);
}
