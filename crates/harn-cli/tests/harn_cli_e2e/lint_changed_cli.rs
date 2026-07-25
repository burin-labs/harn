//! End-to-end contract for Git added-line filtering in `harn lint`.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

use crate::test_util::process::harn_e2e_command;

const CLEAN_SOURCE: &str = "pipeline default(_) {\n  const value = true\n  return !value\n}\n";
const WARNING_SOURCE: &str =
    "pipeline default(_) {\n  const value = true\n  return value == false\n}\n";

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_string()
}

fn init_repo() -> TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    git(temp.path(), &["init", "-q"]);
    git(temp.path(), &["config", "user.email", "tests@example.com"]);
    git(temp.path(), &["config", "user.name", "Harn Tests"]);
    git(temp.path(), &["config", "core.hooksPath", "/dev/null"]);
    temp
}

fn commit_all(repo: &Path, message: &str) -> String {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", message]);
    git(repo, &["rev-parse", "HEAD"])
}

fn run_harn(repo: &Path, args: &[&str]) -> Output {
    harn_e2e_command()
        .current_dir(repo)
        .args(args)
        .output()
        .expect("run harn")
}

fn json_output(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON output ({error}):\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn strict_changed_lint_filters_legacy_warning_but_retains_information() {
    let repo = init_repo();
    let source = "pipeline default(task) {\n  agent_loop(task, nil, {loop_until_done: true})\n  const value = true\n  return value == false\n}\n";
    std::fs::write(repo.path().join("main.harn"), source).expect("write base");
    let base = commit_all(repo.path(), "base");
    std::fs::write(
        repo.path().join("main.harn"),
        format!("{source}// newly added comment\n"),
    )
    .expect("write head");
    let head = commit_all(repo.path(), "head");

    let output = run_harn(
        repo.path(),
        &["lint", "--strict", "--json", "--changed-from", &base],
    );
    assert!(
        output.status.success(),
        "legacy warning should not fail:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = json_output(&output);
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["changed"]["from"]["requested"], base);
    assert_eq!(json["data"]["changed"]["from"]["commit"], base);
    assert_eq!(json["data"]["changed"]["to"]["requested"], "HEAD");
    assert_eq!(json["data"]["changed"]["to"]["commit"], head);
    assert_eq!(json["data"]["changed"]["files"][0]["path"], "main.harn");
    assert_eq!(
        json["data"]["changed"]["files"][0]["added_lines"][0]["start"],
        6
    );
    let diagnostics = json["data"]["files"][0]["diagnostics"]
        .as_array()
        .expect("diagnostics");
    assert_eq!(diagnostics.len(), 1, "{json:#}");
    assert_eq!(diagnostics[0]["severity"], "info");
}

#[test]
fn strict_changed_lint_fails_only_when_warning_overlaps_added_line() {
    let repo = init_repo();
    std::fs::write(repo.path().join("main.harn"), CLEAN_SOURCE).expect("write base");
    let base = commit_all(repo.path(), "base");
    std::fs::write(repo.path().join("main.harn"), WARNING_SOURCE).expect("write head");
    commit_all(repo.path(), "head");

    let human = run_harn(repo.path(), &["lint", "--strict", "--changed-from", &base]);
    assert_eq!(human.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&human.stderr);
    assert!(
        stderr.contains("main.harn:3: warning[HARN-LNT-032]"),
        "{stderr}"
    );

    let output = run_harn(
        repo.path(),
        &[
            "lint",
            "--strict",
            "--json",
            "--changed-from",
            &base,
            "--changed-to",
            "HEAD",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let json = json_output(&output);
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "lint_failed");
    assert_eq!(json["data"]["summary"]["warnings"], 1);
    assert_eq!(
        json["data"]["files"][0]["diagnostics"][0]["severity"],
        "warning"
    );
}

#[test]
fn changed_lint_handles_rename_spaces_multiple_hunks_and_utf8() {
    let repo = init_repo();
    let base_source =
        "// α\npipeline default(_) {\n  const a = 1\n  const b = 2\n  const c = 3\n  const d = 4\n  const value = true\n  return !value\n}\n";
    std::fs::write(repo.path().join("old name.harn"), base_source).expect("write base");
    let base = commit_all(repo.path(), "base");
    git(repo.path(), &["mv", "old name.harn", "new name.harn"]);
    let head_source =
        "// α\n// first hunk\npipeline default(_) {\n  const a = 1\n  const b = 2\n  const c = 3\n  const d = 4\n  const value = true\n  return value == false\n}\n";
    std::fs::write(repo.path().join("new name.harn"), head_source).expect("write head");
    commit_all(repo.path(), "head");

    let output = run_harn(
        repo.path(),
        &["lint", "--strict", "--json", "--changed-from", &base],
    );
    assert_eq!(output.status.code(), Some(1));
    let json = json_output(&output);
    let changed = &json["data"]["changed"]["files"][0];
    assert_eq!(changed["path"], "new name.harn");
    assert_eq!(changed["previous_path"], "old name.harn");
    assert_eq!(changed["status"], "renamed");
    assert!(
        changed["added_lines"].as_array().expect("ranges").len() >= 2,
        "{json:#}"
    );
    assert_eq!(json["data"]["files"][0]["path"], "new name.harn");
    assert_eq!(json["data"]["summary"]["warnings"], 1);
}

#[test]
fn changed_lint_handles_empty_and_deleted_files_without_lint_targets() {
    let repo = init_repo();
    std::fs::write(repo.path().join("gone.harn"), CLEAN_SOURCE).expect("write base");
    let base = commit_all(repo.path(), "base");
    std::fs::remove_file(repo.path().join("gone.harn")).expect("delete source");
    std::fs::write(repo.path().join("empty.harn"), "").expect("write empty source");
    std::fs::write(
        repo.path().join("new.harn"),
        "/** A distinct new entrypoint. */\npipeline brand_new(task) {\n  return task\n}\n",
    )
    .expect("write new source");
    commit_all(repo.path(), "head");

    let output = run_harn(
        repo.path(),
        &["lint", "--strict", "--json", "--changed-from", &base],
    );
    assert!(output.status.success());
    let json = json_output(&output);
    assert_eq!(json["data"]["files"].as_array().unwrap().len(), 1);
    assert_eq!(json["data"]["files"][0]["path"], "new.harn");
    let changed = json["data"]["changed"]["files"].as_array().unwrap();
    assert_eq!(changed.len(), 3);
    assert!(changed.iter().any(|file| file["status"] == "deleted"));
    assert!(changed.iter().any(|file| file["path"] == "empty.harn"
        && file["status"] == "added"
        && file["added_lines"].as_array().unwrap().is_empty()));
    assert!(changed.iter().any(|file| file["path"] == "new.harn"
        && file["status"] == "added"
        && !file["added_lines"].as_array().unwrap().is_empty()));
}

#[test]
fn changed_lint_fails_closed_for_bad_revision_and_source_mismatch() {
    let repo = init_repo();
    std::fs::write(repo.path().join("main.harn"), CLEAN_SOURCE).expect("write base");
    let base = commit_all(repo.path(), "base");
    std::fs::write(repo.path().join("main.harn"), WARNING_SOURCE).expect("write head");
    commit_all(repo.path(), "head");

    let invalid = run_harn(
        repo.path(),
        &[
            "lint",
            "--json",
            "--changed-from",
            "definitely-not-a-revision",
        ],
    );
    assert_eq!(invalid.status.code(), Some(1));
    assert_eq!(
        json_output(&invalid)["error"]["code"],
        "changed_lint_git_failed"
    );

    std::fs::write(repo.path().join("main.harn"), CLEAN_SOURCE).expect("dirty source");
    let mismatch = run_harn(repo.path(), &["lint", "--json", "--changed-from", &base]);
    assert_eq!(mismatch.status.code(), Some(1));
    assert_eq!(
        json_output(&mismatch)["error"]["code"],
        "changed_lint_source_mismatch"
    );
}
