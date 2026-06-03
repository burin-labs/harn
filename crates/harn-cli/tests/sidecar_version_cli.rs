//! Regression test for #2975.
//!
//! `harn-lsp` / `harn-dap` are argv[0]-dispatched into the single multi-call
//! `harn` binary. They must answer `--version`/`-V`/`--help` instead of
//! silently starting the stdio server (which a version-probe would otherwise
//! do, hanging on stdin). We invoke the built `harn` binary with a spoofed
//! `argv[0]` via `CommandExt::arg0`, which exercises the real dispatch path
//! without needing an on-disk symlink. Unix-only because `arg0` is a Unix
//! extension; the shipped Windows aliases are copies and hit the same code.

#![cfg(unix)]

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

fn run_as(argv0: &str, args: &[&str]) -> (String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harn"));
    cmd.arg0(argv0);
    cmd.args(args);
    // Null stdin: if dispatch were broken and the server started, it would get
    // EOF and exit rather than hang the test forever.
    cmd.stdin(Stdio::null());
    let output = cmd.output().expect("spawn harn");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

#[test]
fn lsp_prints_version() {
    for flag in ["--version", "-V"] {
        let (stdout, code) = run_as("harn-lsp", &[flag]);
        assert_eq!(code, 0, "harn-lsp {flag} exit code; stdout={stdout:?}");
        assert!(
            stdout.starts_with("harn-lsp "),
            "expected 'harn-lsp <version>' for {flag}, got {stdout:?}"
        );
        assert!(
            stdout.trim().ends_with(env!("CARGO_PKG_VERSION")),
            "version string missing for {flag}: {stdout:?}"
        );
    }
}

#[test]
fn dap_prints_version() {
    for flag in ["--version", "-V"] {
        let (stdout, code) = run_as("harn-dap", &[flag]);
        assert_eq!(code, 0, "harn-dap {flag} exit code; stdout={stdout:?}");
        assert!(
            stdout.starts_with("harn-dap "),
            "expected 'harn-dap <version>' for {flag}, got {stdout:?}"
        );
        assert!(
            stdout.trim().ends_with(env!("CARGO_PKG_VERSION")),
            "version string missing for {flag}: {stdout:?}"
        );
    }
}

#[test]
fn lsp_prints_help_and_exits() {
    let (stdout, code) = run_as("harn-lsp", &["--help"]);
    assert_eq!(code, 0, "stdout={stdout:?}");
    assert!(stdout.contains("harn-lsp"), "help banner: {stdout:?}");
    assert!(
        stdout.to_lowercase().contains("language server"),
        "help should describe the LSP: {stdout:?}"
    );
}

#[test]
fn dap_help_describes_debug_adapter() {
    let (stdout, code) = run_as("harn-dap", &["-h"]);
    assert_eq!(code, 0, "stdout={stdout:?}");
    assert!(
        stdout.to_lowercase().contains("debug adapter"),
        "help should describe the DAP: {stdout:?}"
    );
}

#[test]
fn plain_harn_version_unaffected() {
    // argv[0] == harn → the normal CLI version path, untouched by the shim.
    let (stdout, code) = run_as("harn", &["--version"]);
    assert_eq!(code, 0, "stdout={stdout:?}");
    assert!(stdout.contains("harn"), "plain harn --version: {stdout:?}");
}
