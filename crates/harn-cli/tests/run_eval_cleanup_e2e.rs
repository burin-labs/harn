//! End-to-end coverage for invocation-owned `harn run -e` source cleanup.

mod test_util;

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Output;

use tempfile::TempDir;
use test_util::process::harn_e2e_command;

fn source_files(path: &Path) -> Vec<OsString> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .expect("read test workspace")
        .filter_map(|entry| {
            let entry = entry.expect("read directory entry");
            let path = entry.path();
            (path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "harn"))
            .then(|| entry.file_name())
        })
        .collect();
    entries.sort();
    entries
}

fn run_inline(workspace: &Path, source: &str) -> Output {
    harn_e2e_command()
        .current_dir(workspace)
        .args(["run", "-e", source])
        .output()
        .expect("spawn harn run -e")
}

fn assert_workspace_unchanged(workspace: &Path, before: &[OsString]) {
    assert_eq!(source_files(workspace), before);
    assert_eq!(
        fs::read_to_string(workspace.join(".harn-eval-user-authored.harn")).unwrap(),
        "pipeline preserved() {}\n"
    );
}

fn workspace_with_user_source() -> TempDir {
    let workspace = TempDir::new().expect("test workspace");
    fs::write(
        workspace.path().join(".harn-eval-user-authored.harn"),
        "pipeline preserved() {}\n",
    )
    .expect("write user source");
    workspace
}

#[test]
fn successful_inline_run_leaves_workspace_unchanged() {
    let workspace = workspace_with_user_source();
    let before = source_files(workspace.path());

    let output = run_inline(workspace.path(), "return 0");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_workspace_unchanged(workspace.path(), &before);
}

#[test]
fn failed_inline_run_leaves_workspace_unchanged() {
    let workspace = workspace_with_user_source();
    let before = source_files(workspace.path());

    let output = run_inline(workspace.path(), "throw \"expected failure\"");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("expected failure"),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_workspace_unchanged(workspace.path(), &before);
}

#[test]
fn invalid_inline_source_leaves_workspace_unchanged() {
    let workspace = workspace_with_user_source();
    let before = source_files(workspace.path());

    let output = run_inline(workspace.path(), "return missing_name");

    assert_eq!(output.status.code(), Some(1));
    assert_workspace_unchanged(workspace.path(), &before);
}
