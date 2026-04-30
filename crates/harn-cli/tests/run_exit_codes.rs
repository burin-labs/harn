//! End-to-end exit-code semantics for `harn run`.
//!
//! A pipeline body's last value flows through `vm.execute()` to the CLI.
//! These tests pin the rule:
//! - `int n`            → process exits with n (clamped 0..=255)
//! - `Result::Ok(_)`    → exits 0
//! - `Result::Err(msg)` → writes msg to stderr, exits 1
//! - implicit           → exits 0
//! - `exit(code)`       → exits code (existing builtin, still honored)

mod test_util;

use std::fs;

use tempfile::TempDir;
use test_util::process::harn_command;

fn run_script(body: &str) -> std::process::Output {
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(&script, body).unwrap();
    harn_command()
        .current_dir(temp.path())
        .args(["run", script.to_str().unwrap()])
        .output()
        .unwrap()
}

#[test]
fn pipeline_main_returning_int_sets_exit_code() {
    let out = run_script("pipeline main() {\n  return 42\n}\n");
    assert_eq!(
        out.status.code(),
        Some(42),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn pipeline_main_returning_err_writes_stderr_and_exits_one() {
    let out = run_script("pipeline main() {\n  return Err(\"boom\")\n}\n");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("boom"),
        "expected 'boom' in stderr, got: {stderr}"
    );
}

#[test]
fn pipeline_main_returning_ok_exits_zero() {
    let out = run_script("pipeline main() {\n  return Ok(\"done\")\n}\n");
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn pipeline_main_implicit_return_exits_zero() {
    let out = run_script("pipeline main() {\n  println(\"hi\")\n}\n");
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("hi"));
}

#[test]
fn pipeline_main_int_clamped_to_byte_range() {
    let out = run_script("pipeline main() {\n  return 999\n}\n");
    // Linux limits child exit codes to a byte; we clamp to 0..=255 explicitly.
    assert_eq!(out.status.code(), Some(255));
}

#[test]
fn explicit_exit_builtin_still_works() {
    let out = run_script("pipeline main() {\n  exit(3)\n}\n");
    assert_eq!(out.status.code(), Some(3));
}
