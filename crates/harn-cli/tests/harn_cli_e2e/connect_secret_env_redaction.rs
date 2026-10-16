//! `harn connect` options that read a credential from a named environment
//! variable must never echo its value, including a value that is not valid
//! Unicode (whose `VarError` display embeds the raw bytes).
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

use crate::test_util::process::harn_e2e_command;

const MARKER: &str = "SYNTHETIC_ONLY_7f3a";
const VARIABLE: &str = "CONNECT_SECRET_REDACTION_PROBE";

fn non_unicode_secret() -> OsString {
    let mut bytes = MARKER.as_bytes().to_vec();
    bytes.push(0xFF);
    OsString::from_vec(bytes)
}

fn run_connect(args: &[&str]) -> std::process::Output {
    let directory = tempfile::tempdir().unwrap();
    harn_e2e_command()
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", directory.path())
        .env("HARN_SECRET_PROVIDERS", "env")
        .env(VARIABLE, non_unicode_secret())
        .current_dir(directory.path())
        .arg("connect")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run harn connect")
}

fn assert_refused_without_echo(output: &std::process::Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains(VARIABLE) && stderr.contains("not valid Unicode"),
        "the error names the variable and the category:\n{stderr}"
    );
    assert!(
        !stdout.contains(MARKER) && !stderr.contains(MARKER),
        "the secret value must not be echoed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn api_key_from_env_refuses_non_unicode_without_echoing_it() {
    assert_refused_without_echo(&run_connect(&[
        "api-key",
        "--connector",
        "acme",
        "--secret-id",
        "acme/token",
        "--from-env",
        VARIABLE,
    ]));
}

#[test]
fn oauth_client_secret_from_env_refuses_non_unicode_without_echoing_it() {
    assert_refused_without_echo(&run_connect(&[
        "generic",
        "acme",
        "https://api.invalid/",
        "--client-secret-from-env",
        VARIABLE,
        "--no-open",
    ]));
}
