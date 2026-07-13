use super::*;
use crate::trust_graph::{query_trust_records, TrustQueryFilters};
use std::fs;
use std::process::Command;

fn require_git() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn git(cwd: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    command.args(args).current_dir(cwd);
    // Mirror the production `exec_argv` env scrub so these tests
    // also pass when invoked from inside a git hook (which sets
    // `GIT_DIR=.git` for the hook process and its descendants —
    // without this, a child `git init` in a temp dir reuses the
    // outer repo's index and fails).
    for name in super::GIT_ENV_OVERRIDES {
        command.env_remove(name);
    }
    let output = command.output().expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    git(dir.path(), &["init", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "harn@example.test"]);
    git(dir.path(), &["config", "user.name", "Harn Test"]);
    fs::write(dir.path().join("README.md"), "initial\n").expect("write readme");
    git(dir.path(), &["add", "README.md"]);
    git(dir.path(), &["commit", "-m", "initial"]);
    dir
}

async fn run_on_local<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::task::LocalSet::new().run_until(future).await
}

#[tokio::test(flavor = "current_thread")]
async fn git_status_returns_receipt_and_trust_record() {
    if !require_git() {
        return;
    }
    crate::event_log::reset_active_event_log();
    crate::stdlib::reset_stdlib_state();
    let repo = init_repo();
    fs::write(repo.path().join("README.md"), "changed\n").expect("modify readme");

    run_on_local(async {
        let receipt = run_git_command(
            None,
            GitCommand {
                operation: "git.status",
                action: "git.status",
                cwd: repo.path().to_path_buf(),
                argv: vec![
                    "git".to_string(),
                    "status".to_string(),
                    "--porcelain=v1".to_string(),
                    "--branch".to_string(),
                ],
                mutation: GitMutation::Read,
                affected_paths: Vec::new(),
                data_parser: GitDataParser::Status,
            },
        )
        .await
        .expect("git status receipt");
        let json = crate::llm::vm_value_to_json(&receipt);
        assert_eq!(json["schema"], "harn-stdlib-git-receipt-v1");
        assert_eq!(json["operation"], "git.status");
        assert_eq!(json["success"], true);
        assert_eq!(json["data"]["dirty"], true);
        assert_eq!(json["data"]["entries"][0]["path"], "README.md");

        let log = active_event_log().expect("git receipt installed event log");
        let records = query_trust_records(
            &log,
            &TrustQueryFilters {
                action: Some("git.status".to_string()),
                ..TrustQueryFilters::default()
            },
        )
        .await
        .expect("query trust records");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, TrustOutcome::Success);
        assert_eq!(
            records[0].metadata["receipt"]["operation"],
            JsonValue::String("git.status".to_string())
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn git_subprocess_carries_noninteractive_prompt_guard() {
    // Every git subprocess Harn spawns must be non-interactive by
    // default so a credential / host-key prompt fails fast instead
    // of hanging a TTY-less runtime (`harn serve`, `@job`, CI). We
    // run a probe through the same `exec_argv` path the receipt git
    // builtins use and assert the guard env reached the child while
    // an inherited env var survived the merge (proving `env_mode:
    // "merge"`, not a clobbering replace). The inherited var uses a
    // unique name so it can't race other tests in a shared process.
    crate::stdlib::reset_stdlib_state();
    std::env::set_var("HARN_GUARD_PROBE_INHERIT", "kept");
    run_on_local(async {
        let result = exec_argv(&GitCommand {
            operation: "git.status",
            action: "git.status",
            cwd: std::env::temp_dir(),
            argv: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf '%s|%s' \"$GIT_TERMINAL_PROMPT\" \"$HARN_GUARD_PROBE_INHERIT\"".to_string(),
            ],
            mutation: GitMutation::Read,
            affected_paths: Vec::new(),
            data_parser: GitDataParser::None,
        })
        .await
        .expect("exec_argv probe");
        let json = crate::llm::vm_value_to_json(&result);
        let stdout = json["stdout"].as_str().unwrap_or_default();
        let (prompt, inherited) = stdout.split_once('|').unwrap_or_default();
        assert_eq!(
            prompt, "0",
            "git subprocess must inherit GIT_TERMINAL_PROMPT=0; stdout was {stdout:?}"
        );
        assert_eq!(
            inherited, "kept",
            "env_mode=merge must preserve inherited env; stdout was {stdout:?}"
        );
    })
    .await;
    std::env::remove_var("HARN_GUARD_PROBE_INHERIT");
}

#[tokio::test(flavor = "current_thread")]
async fn force_with_lease_detects_advanced_remote_before_push() {
    if !require_git() {
        return;
    }
    crate::event_log::reset_active_event_log();
    crate::stdlib::reset_stdlib_state();
    let remote = tempfile::tempdir().expect("remote");
    git(remote.path(), &["init", "--bare"]);
    let one = tempfile::tempdir().expect("clone one");
    let two = tempfile::tempdir().expect("clone two");
    git(
        one.path(),
        &["clone", remote.path().to_str().unwrap(), "repo"],
    );
    git(
        two.path(),
        &["clone", remote.path().to_str().unwrap(), "repo"],
    );
    let one_repo = one.path().join("repo");
    let two_repo = two.path().join("repo");
    git(&one_repo, &["config", "user.email", "harn@example.test"]);
    git(&one_repo, &["config", "user.name", "Harn Test"]);
    git(&two_repo, &["config", "user.email", "harn@example.test"]);
    git(&two_repo, &["config", "user.name", "Harn Test"]);
    fs::write(one_repo.join("file.txt"), "one\n").expect("write one");
    git(&one_repo, &["add", "file.txt"]);
    git(&one_repo, &["commit", "-m", "one"]);
    git(&one_repo, &["push", "origin", "HEAD:refs/heads/main"]);
    let expected_oid = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "refs/remotes/origin/main"])
            .current_dir(&one_repo)
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_string();

    git(&two_repo, &["fetch", "origin", "main"]);
    git(&two_repo, &["checkout", "-B", "main", "origin/main"]);
    fs::write(two_repo.join("file.txt"), "two\n").expect("write two");
    git(&two_repo, &["commit", "-am", "two"]);
    git(&two_repo, &["push", "origin", "HEAD:refs/heads/main"]);

    fs::write(one_repo.join("file.txt"), "one advanced\n").expect("write one advanced");
    git(&one_repo, &["commit", "-am", "one advanced"]);

    run_on_local(async {
        let mismatch =
            verify_force_with_lease(&one_repo, "origin", "refs/heads/main", &expected_oid)
                .await
                .expect("lease check should complete");
        assert!(
            mismatch.is_some(),
            "expected advanced remote to produce a lease mismatch"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn absent_worktree_remove_is_idempotent_receipt() {
    if !require_git() {
        return;
    }
    crate::event_log::reset_active_event_log();
    crate::stdlib::reset_stdlib_state();
    let root = tempfile::tempdir().expect("root");
    let missing = root.path().join("missing-worktree");
    run_on_local(async {
        let receipt = planned_or_noop_receipt(
            "git.worktree.remove",
            "git.worktree.remove",
            GitMutation::Mutating,
            root.path().to_path_buf(),
            vec![
                "git".to_string(),
                "worktree".to_string(),
                "remove".to_string(),
                display_path(&missing),
            ],
            vec![display_path(&missing)],
            json!({"path": display_path(&missing), "removed": false, "idempotent": true}),
            "no_op",
        )
        .await
        .expect("receipt");
        let json = crate::llm::vm_value_to_json(&receipt);
        assert_eq!(json["status"], "no_op");
        assert_eq!(json["success"], true);
        assert_eq!(json["data"]["idempotent"], true);
    })
    .await;
}

#[test]
fn string_list_value_rejects_non_string_entries() {
    let err = string_list_value(&[VmValue::Int(1)], "git.diff", "paths")
        .expect_err("non-string path should be rejected");
    assert!(
        matches!(err, VmError::TypeError(message) if message.contains("paths entries must be strings"))
    );
}

/// Capture the diagnostics emitted while `body` runs by swapping the
/// thread-local event sinks for a [`CollectorSink`], returning only
/// the `stdlib.git` warnings.
fn capture_git_warnings(body: impl FnOnce()) -> Vec<crate::events::LogEvent> {
    use crate::events::{add_event_sink, clear_event_sinks, reset_event_sinks, CollectorSink};
    use std::rc::Rc;
    let sink = Rc::new(CollectorSink::new());
    clear_event_sinks();
    add_event_sink(sink.clone());
    body();
    reset_event_sinks();
    let warnings = sink
        .logs
        .borrow()
        .iter()
        .filter(|event| event.category == "stdlib.git")
        .cloned()
        .collect();
    warnings
}

#[test]
fn warn_git_failure_dedupes_by_operation_exit_and_stderr_line() {
    use crate::events::EventLevel;
    reset_git_state();
    let warnings = capture_git_warnings(|| {
        // First failure warns.
        warn_git_failure("git.diff", 128, "fatal: not a git repository: /x\ntrailing");
        // Same (op, exit, first stderr line) is suppressed even though
        // line 2 differs — the dedup key is the first line only.
        warn_git_failure("git.diff", 128, "fatal: not a git repository: /x\nother");
        // A different exit code is a distinct key and warns again.
        warn_git_failure("git.diff", 1, "error: bad revision");
    });
    reset_git_state();
    assert_eq!(warnings.len(), 2, "unexpected diagnostics: {warnings:?}");
    assert_eq!(warnings[0].level, EventLevel::Warn);
    assert_eq!(warnings[0].category, "stdlib.git");
    assert!(
        warnings[0]
            .message
            .contains("git.diff failed (exit 128): fatal: not a git repository: /x"),
        "message was: {}",
        warnings[0].message
    );
    assert!(
        warnings[0]
            .message
            .contains("receipt success=false; callers see empty data"),
        "message was: {}",
        warnings[0].message
    );
    assert!(warnings[1].message.contains("git.diff failed (exit 1)"));
}

#[test]
fn warn_git_failure_exempts_probe_operations() {
    reset_git_state();
    let warnings = capture_git_warnings(|| {
        warn_git_failure("git.repo.discover", 128, "fatal: not a git repository: /x");
    });
    reset_git_state();
    assert!(
        warnings.is_empty(),
        "probe operations must stay silent on failure: {warnings:?}"
    );
}

#[test]
fn warn_git_failure_truncates_long_stderr() {
    reset_git_state();
    let long = format!("fatal: {}", "x".repeat(500));
    let warnings = capture_git_warnings(|| {
        warn_git_failure("git.diff", 128, &long);
    });
    reset_git_state();
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].message.contains('…'),
        "truncated excerpt should end with an ellipsis: {}",
        warnings[0].message
    );
    // Excerpt is capped at FAILURE_STDERR_MAX_CHARS characters; the
    // surrounding wrapper text adds a bounded, fixed amount.
    assert!(
        warnings[0].message.chars().count() < FAILURE_STDERR_MAX_CHARS + 120,
        "message unexpectedly long: {}",
        warnings[0].message
    );
}

#[tokio::test(flavor = "current_thread")]
async fn failed_git_command_emits_single_diagnostic_end_to_end() {
    if !require_git() {
        return;
    }
    crate::event_log::reset_active_event_log();
    crate::stdlib::reset_stdlib_state();
    // A directory that is definitively not a git repo.
    let not_repo = tempfile::tempdir().expect("temp dir");
    let warnings = run_on_local(async {
        let sink = std::rc::Rc::new(crate::events::CollectorSink::new());
        crate::events::clear_event_sinks();
        crate::events::add_event_sink(sink.clone());
        // Two identical failing calls; the second must be deduped.
        for _ in 0..2 {
            let receipt = run_git_command(
                None,
                GitCommand {
                    operation: "git.diff",
                    action: "git.diff",
                    cwd: not_repo.path().to_path_buf(),
                    argv: vec!["git".to_string(), "diff".to_string(), "HEAD".to_string()],
                    mutation: GitMutation::Read,
                    affected_paths: Vec::new(),
                    data_parser: GitDataParser::Diff,
                },
            )
            .await
            .expect("git.diff returns a receipt, never throws");
            let json = crate::llm::vm_value_to_json(&receipt);
            assert_eq!(json["success"], false);
            // Data stays the empty diff the caller would silently consume.
            assert_eq!(json["data"]["diff"], "");
        }
        crate::events::reset_event_sinks();
        let collected = sink
            .logs
            .borrow()
            .iter()
            .filter(|event| event.category == "stdlib.git")
            .cloned()
            .collect::<Vec<_>>();
        collected
    })
    .await;
    assert_eq!(
        warnings.len(),
        1,
        "two identical failures should warn exactly once: {warnings:?}"
    );
    // The exact exit code and stderr wording vary across git versions
    // (128 "fatal: not a git repository" vs 129 usage-error phrasing),
    // so assert the shape, not the specific code.
    assert!(
        warnings[0].message.contains("git.diff failed (exit "),
        "message was: {}",
        warnings[0].message
    );
    assert!(
        warnings[0]
            .message
            .to_lowercase()
            .contains("not a git repository"),
        "stderr first line should be surfaced: {}",
        warnings[0].message
    );
}

#[tokio::test(flavor = "current_thread")]
async fn discover_probe_failure_stays_silent_end_to_end() {
    if !require_git() {
        return;
    }
    crate::event_log::reset_active_event_log();
    crate::stdlib::reset_stdlib_state();
    let not_repo = tempfile::tempdir().expect("temp dir");
    let warnings = run_on_local(async {
        let sink = std::rc::Rc::new(crate::events::CollectorSink::new());
        crate::events::clear_event_sinks();
        crate::events::add_event_sink(sink.clone());
        let receipt = run_git_command(
            None,
            GitCommand {
                operation: "git.repo.discover",
                action: "git.repo.discover",
                cwd: not_repo.path().to_path_buf(),
                argv: vec![
                    "git".to_string(),
                    "rev-parse".to_string(),
                    "--show-toplevel".to_string(),
                ],
                mutation: GitMutation::Read,
                affected_paths: Vec::new(),
                data_parser: GitDataParser::Discover {
                    input: display_path(not_repo.path()),
                },
            },
        )
        .await
        .expect("git.repo.discover returns a receipt");
        let json = crate::llm::vm_value_to_json(&receipt);
        assert_eq!(json["success"], false);
        crate::events::reset_event_sinks();
        let collected = sink
            .logs
            .borrow()
            .iter()
            .filter(|event| event.category == "stdlib.git")
            .cloned()
            .collect::<Vec<_>>();
        collected
    })
    .await;
    assert!(
        warnings.is_empty(),
        "git.repo.discover is a probe and must stay silent: {warnings:?}"
    );
}
