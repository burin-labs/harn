#![allow(dead_code)]

//! Small fixture and subprocess helpers shared by the harn-cli integration
//! test binaries. Keep per-test environment differences at the call site
//! (extra `.env(..)` calls or a thin local wrapper) rather than forking these.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

pub use harn_cli::tests::common::async_runtime::block_on_cli_stack_multi_thread as run_in_harn_runtime;

/// Path to the warmed `harn` binary under test.
pub fn binary_path() -> PathBuf {
    super::process::harn_e2e_binary().to_path_buf()
}

/// Run `harn <args>` from the test process's working directory.
pub fn run_harn(args: &[&str]) -> Output {
    Command::new(binary_path())
        .args(args)
        .output()
        .expect("spawn harn")
}

/// Parse a command's stdout as one JSON document, panicking with both
/// streams when it is not JSON.
pub fn stdout_json(output: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON: {error}\nstdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    })
}

/// Parse captured `label` stdout as JSON.
pub fn parse_json(text: &str, label: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("{label} stdout is not valid JSON: {error}\n--- payload ---\n{text}")
    })
}

/// Write `contents` to `dir/relative`, creating parent directories.
pub fn write_file(dir: &Path, relative: &str, contents: &str) -> PathBuf {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, contents).unwrap();
    path
}

/// Run `git <args>` in `repo` without signing or inherited repository
/// overrides, asserting success and returning trimmed stdout.
pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The Harn repository root (two levels above this crate).
pub fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives under crates/harn-cli")
}

/// A per-process scratch directory under the system temp dir, emptied first.
pub fn fresh_temp_dir(prefix: &str, name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{prefix}-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir root");
    dir
}
