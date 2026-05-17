//! CLI smoke tests for `harn dev --watch`.
//!
//! Spawns the binary so we exercise the real clap parser. The
//! incremental loop itself is covered by unit tests in
//! `commands::dev::tests` so this file avoids long-running subprocesses
//! and the wall-clock sleep + polling patterns banned by
//! `make lint-test-patterns`.

use std::process::Command;

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

#[test]
fn dev_help_advertises_watch_and_json_flags() {
    let output = Command::new(binary_path())
        .args(["dev", "--help"])
        .output()
        .expect("spawn harn dev --help");
    assert!(output.status.success(), "exit={:?}", output.status.code());
    let help = String::from_utf8_lossy(&output.stdout);
    for token in ["--watch", "--json", "--with-tests", "ROOT"] {
        assert!(
            help.contains(token),
            "expected `{token}` in `harn dev --help`, got:\n{help}"
        );
    }
}

#[test]
fn dev_without_watch_errors_out() {
    let output = Command::new(binary_path())
        .args(["dev"])
        .output()
        .expect("spawn harn dev");
    assert!(
        !output.status.success(),
        "harn dev without --watch should exit nonzero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--watch"),
        "stderr should mention --watch: {stderr}"
    );
}
