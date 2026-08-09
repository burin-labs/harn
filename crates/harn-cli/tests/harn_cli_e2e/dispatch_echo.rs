//! Integration tests for the CLI dispatch wedge (harn#2294 / G1).
//!
//! Exercises the public entry point [`harn_cli::dispatch::run_embedded_script`]
//! to prove that argv → embedded `.harn` script → stdout round-trips
//! through the same `execute_run` pipeline that production CLI commands
//! use. Each test runs in a fresh tokio runtime via `#[tokio::test]`.

use harn_cli::dispatch::run_embedded_script;

#[tokio::test]
async fn echo_dispatch_forwards_two_args_as_json_array() {
    let outcome = run_embedded_script("echo", vec!["foo".into(), "bar".into()], false).await;
    assert_eq!(
        outcome.exit_code, 0,
        "echo failed: stderr={}",
        outcome.stderr
    );
    assert_eq!(outcome.stdout, "[\"foo\",\"bar\"]\n");
}

#[tokio::test]
async fn dispatch_to_unknown_script_returns_software_error() {
    let outcome = run_embedded_script("this/script/does/not/exist", vec![], false).await;
    assert_eq!(outcome.exit_code, 70, "expected EX_SOFTWARE on miss");
    assert!(
        outcome.stderr.contains("not embedded"),
        "stderr should explain the dispatch miss; got: {}",
        outcome.stderr
    );
}
