//! `harn connect` stores credentials through the configured provider chain,
//! the same one scripts resolve through, so a credential it stores under
//! `HARN_SECRET_PROVIDERS=file` is the one a later `harn run` reads (#9184).
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use crate::test_util::process::harn_e2e_command;

const KEY_VARIABLE: &str = "CONNECT_STORE_PROBE_KEY";

fn harn(directory: &Path, chain: &str) -> Command {
    let mut command = harn_e2e_command();
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", directory)
        .env("HARN_SECRET_PROVIDERS", chain)
        .env("HARN_SECRET_FILE_PATH", directory.join("secrets.json"))
        .env("HARN_LLM_CALLS_DISABLED", "1")
        .env(KEY_VARIABLE, "synthetic-only")
        .current_dir(directory)
        .stdin(std::process::Stdio::null());
    command
}

fn private_directory() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn store_api_key(directory: &Path, chain: &str) -> Output {
    harn(directory, chain)
        .args([
            "connect",
            "api-key",
            "--connector",
            "acme",
            "--secret-id",
            "acme/api-key",
            "--from-env",
            KEY_VARIABLE,
        ])
        .output()
        .expect("run harn connect api-key")
}

#[test]
fn connect_stores_where_a_file_chain_run_reads() {
    let directory = private_directory();
    assert_success(&store_api_key(directory.path(), "env,file"));

    // A fresh process resolving through the same chain sees the credential.
    let read = harn(directory.path(), "env,file")
        .args([
            "run",
            "--environment-policy",
            "isolated",
            "-e",
            r#"assert(harness.secrets.read("acme/api-key") == "synthetic-only")"#,
        ])
        .output()
        .expect("run harn");
    assert_success(&read);

    // The connect index lives in the same store, so --list finds the entry.
    let list = harn(directory.path(), "env,file")
        .args(["connect", "--list", "--json"])
        .output()
        .expect("run harn connect --list");
    assert_success(&list);
    let index: serde_json::Value = serde_json::from_slice(&list.stdout).expect("list JSON");
    assert_eq!(index["providers"][0]["provider"], "acme", "{index}");

    let stored = fs::read_to_string(directory.path().join("secrets.json")).unwrap();
    assert!(stored.contains("acme/api-key"), "{stored}");
    assert!(
        !stored.contains("synthetic-only"),
        "values are encoded, never plaintext in the key space"
    );
}

#[test]
fn connect_refuses_a_chain_that_persists_nothing() {
    let directory = private_directory();
    let output = store_api_key(directory.path(), "env");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("persistent secret provider") && stderr.contains("HARN_SECRET_PROVIDERS"),
        "{stderr}"
    );
    assert!(!stderr.contains("synthetic-only"), "{stderr}");
}
