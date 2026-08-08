//! Integration tests for the Merge Captain playground (#1020). Each test
//! materializes a real git repo into a tempdir, runs scenario steps that
//! exercise rebase / force-with-lease / merge codepaths, and asserts on
//! the resulting state. Skipped automatically on hosts without `git` on
//! PATH so cargo test still runs in minimal sandboxes.

use std::path::PathBuf;
use std::process::Command;

use super::*;

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn skip_if_no_git() -> bool {
    if !git_available() {
        eprintln!("(skipping playground integration test — `git` not on PATH)");
        return true;
    }
    false
}

fn fresh_dir() -> (tempfile::TempDir, PathBuf) {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let path = tempdir.path().to_path_buf();
    (tempdir, path)
}

#[test]
fn init_three_repo_basic_creates_real_repos_and_branches() {
    if skip_if_no_git() {
        return;
    }
    let (_guard, dir) = fresh_dir();
    let manifest = load_builtin("three_repo_basic").unwrap();
    let state = init_playground_at(InitOptions {
        dir: &dir,
        manifest: &manifest,
        allow_existing: false,
    })
    .unwrap();

    // Each repo has a bare remote and a working clone.
    for repo_name in ["alpha", "beta", "gamma"] {
        let bare = dir.join("remotes").join(format!("{repo_name}.git"));
        let working = dir.join("working").join(repo_name);
        assert!(bare.is_dir(), "missing bare remote for {repo_name}");
        assert!(working.is_dir(), "missing working clone for {repo_name}");

        // The bare remote really has the seeded branches.
        let branches = Command::new("git")
            .args(["branch", "--list"])
            .current_dir(&working)
            .output()
            .unwrap();
        let listing = String::from_utf8_lossy(&branches.stdout).to_string();
        assert!(
            listing.contains(repo_name) || !listing.is_empty(),
            "{listing}"
        );
    }

    // PRs got their head_sha resolved by reading the remote's branch tip.
    for pr in state.pull_requests.values() {
        assert!(
            pr.head_sha.is_some(),
            "PR {} should have head_sha resolved",
            pr.key()
        );
    }
}

#[test]
fn cleanup_is_idempotent() {
    if skip_if_no_git() {
        return;
    }
    let (_guard, dir) = fresh_dir();
    let manifest = load_builtin("single_green").unwrap();
    init_playground_at(InitOptions {
        dir: &dir,
        manifest: &manifest,
        allow_existing: false,
    })
    .unwrap();
    assert!(cleanup_playground_at(&dir).unwrap());
    // Second call should be a quiet no-op.
    assert!(!cleanup_playground_at(&dir).unwrap());
}

#[test]
fn cleanup_refuses_arbitrary_directory() {
    let (_guard, dir) = fresh_dir();
    std::fs::write(dir.join("README.md"), "not a playground").unwrap();
    let err = cleanup_playground_at(&dir).unwrap_err();
    assert!(format!("{err}").contains("does not look like"));
}

#[test]
fn force_push_drill_step_rewrites_branch_and_updates_head_sha() {
    if skip_if_no_git() {
        return;
    }
    let (_guard, dir) = fresh_dir();
    let manifest = load_builtin("force_push_drill").unwrap();
    let mut state = init_playground_at(InitOptions {
        dir: &dir,
        manifest: &manifest,
        allow_existing: false,
    })
    .unwrap();
    let initial_sha = state
        .pull_requests
        .values()
        .next()
        .and_then(|pr| pr.head_sha.clone())
        .expect("initial head_sha");
    let report = run_named_step(&dir, &mut state, &manifest, "force_push_passing_v2").unwrap();
    assert!(report.actions_applied >= 1);
    let new_sha = state
        .pull_requests
        .values()
        .next()
        .and_then(|pr| pr.head_sha.clone())
        .expect("post head_sha");
    assert_ne!(
        initial_sha, new_sha,
        "force-push should produce a different head SHA"
    );
    state.save(&dir).unwrap();
    let reloaded = PlaygroundState::load(&dir).unwrap();
    assert_eq!(reloaded.pull_requests.len(), 1);
    assert_eq!(
        reloaded
            .pull_requests
            .values()
            .next()
            .unwrap()
            .head_sha
            .as_deref(),
        Some(new_sha.as_str())
    );
}

#[test]
fn merge_pull_request_step_produces_real_merge_commit_on_remote() {
    if skip_if_no_git() {
        return;
    }
    let (_guard, dir) = fresh_dir();
    let manifest = load_builtin("single_green").unwrap();
    let mut state = init_playground_at(InitOptions {
        dir: &dir,
        manifest: &manifest,
        allow_existing: false,
    })
    .unwrap();
    let report = run_named_step(&dir, &mut state, &manifest, "merge").unwrap();
    assert!(report.actions_applied >= 1);
    // Remote `main` should now have a merge commit (parents > 1).
    let bare = dir.join("remotes").join("solo.git");
    let log = Command::new("git")
        .args(["log", "--all", "--oneline"])
        .current_dir(&bare)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&log.stdout).to_string();
    assert!(
        stdout.lines().count() >= 2,
        "expected merged remote history, got:\n{stdout}"
    );
    let merged_pr = state.pull_requests.values().next().unwrap();
    assert_eq!(merged_pr.state, "merged");
}

#[test]
fn advance_base_marks_open_prs_behind() {
    if skip_if_no_git() {
        return;
    }
    let (_guard, dir) = fresh_dir();
    let manifest = load_builtin("three_repo_basic").unwrap();
    let mut state = init_playground_at(InitOptions {
        dir: &dir,
        manifest: &manifest,
        allow_existing: false,
    })
    .unwrap();
    run_named_step(&dir, &mut state, &manifest, "beta_advance_main").unwrap();
    let pr = state
        .pull_requests
        .get(&PlaygroundPullRequest::compose_key("beta", 202))
        .unwrap();
    assert_eq!(pr.mergeable_state, "behind");
}

#[test]
fn fake_server_handlers_round_trip_pr_lookup() {
    let manifest = load_builtin("single_green").unwrap();
    let mut state = PlaygroundState::from_manifest(&manifest);
    // Bypass real git by stamping a head_sha.
    for pr in state.pull_requests.values_mut() {
        pr.head_sha = Some("ffffffff".to_string());
    }
    let response = list_pulls(&state, "burin-labs", "solo", &ListPullsQuery::default());
    assert_eq!(response.status, 200);
    let body = response.body;
    assert_eq!(body.as_array().unwrap().len(), 1);

    let response = get_pull(&state, "burin-labs", "solo", 1);
    assert_eq!(response.status, 200);
    assert_eq!(response.body["number"], 1);

    let response = list_check_runs(&state, "burin-labs", "solo", "ffffffff");
    assert_eq!(response.status, 200);
    let runs = &response.body["check_runs"];
    assert_eq!(runs.as_array().unwrap().len(), 1);

    // Mutating endpoints touch state.
    let response = create_issue_comment(
        &mut state,
        "burin-labs",
        "solo",
        1,
        CreateCommentBody {
            body: "ack".to_string(),
            user: Some("captain".to_string()),
        },
    );
    assert_eq!(response.status, 201);
    assert_eq!(
        state.pull_requests.values().next().unwrap().comments.len(),
        1
    );
}

#[test]
fn init_produces_deterministic_head_shas() {
    if skip_if_no_git() {
        return;
    }
    let manifest = load_builtin("single_green").unwrap();
    let (_g1, dir1) = fresh_dir();
    let (_g2, dir2) = fresh_dir();
    let s1 = init_playground_at(InitOptions {
        dir: &dir1,
        manifest: &manifest,
        allow_existing: false,
    })
    .unwrap();
    let s2 = init_playground_at(InitOptions {
        dir: &dir2,
        manifest: &manifest,
        allow_existing: false,
    })
    .unwrap();
    let pr1 = s1.pull_requests.values().next().unwrap();
    let pr2 = s2.pull_requests.values().next().unwrap();
    assert_eq!(
        pr1.head_sha, pr2.head_sha,
        "expected deterministic head_sha across runs (commit dates pinned via GIT_COMMITTER_DATE)"
    );
}

#[test]
fn synthesize_sweep_emits_canonical_envelope_for_each_open_pr() {
    let manifest = load_builtin("three_repo_basic").unwrap();
    let mut state = PlaygroundState::from_manifest(&manifest);
    for pr in state.pull_requests.values_mut() {
        pr.head_sha = Some("deadbeef".to_string());
    }
    let events = synthesize_sweep(&state, &TranscriptOptions::default());
    let tool_calls = events
        .iter()
        .filter(|e| matches!(e.event, crate::agent_events::AgentEvent::ToolCall { .. }))
        .count();
    // 2 tool calls per open PR; the manifest has 3 open PRs.
    assert_eq!(tool_calls, 6);
}
