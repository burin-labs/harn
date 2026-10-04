//! harn#9359: git commands that discard uncommitted working-tree changes carry
//! `git_discards_worktree`, so a policy can require approval for them. They
//! stay off the never-approvable floor.

use super::*;

fn scan(command: &str) -> JsonValue {
    command_risk_scan_json(&ctx(&["sh", "-c", command]), None)
}

fn discards(command: &str) -> bool {
    labels(&scan(command)).contains(&"git_discards_worktree".to_string())
}

#[test]
fn worktree_discards_are_labelled_from_the_parsed_argv() {
    for command in [
        "git checkout -- .",
        "git checkout -- src/main.rs",
        "git checkout HEAD~1 -- src/main.rs",
        "git checkout --ours -- conflicted.rs",
        "git checkout .",
        "git checkout ./src",
        "git checkout '*.rs'",
        "git checkout :/",
        "git checkout -f main",
        "git checkout --force main",
        "git checkout --pathspec-from-file=paths.txt",
        "git restore .",
        "git restore src/main.rs",
        "git restore --worktree .",
        "git restore --staged --worktree .",
        "git restore -SW .",
        "git restore --source=HEAD --worktree .",
        "git switch -f main",
        "git switch --discard-changes main",
        "git -C repo checkout -- .",
        "echo ok && git restore .",
        "env GIT_TRACE=1 git checkout -- .",
        "bash -c 'git restore .'",
    ] {
        assert!(
            discards(command),
            "expected git_discards_worktree: {command}"
        );
    }
}

#[test]
fn branch_switches_and_index_only_restores_are_unlabelled() {
    for command in [
        "git checkout main",
        "git checkout feature/foo",
        "git checkout -b feature/new",
        "git checkout main --",
        "git switch main",
        "git switch -c feature/new",
        "git restore --staged src/main.rs",
        "git restore -S .",
        "git status",
        "git stash",
    ] {
        assert!(
            !discards(command),
            "unexpected git_discards_worktree: {command}"
        );
    }
}

#[test]
fn a_discard_needs_approval_but_is_not_on_the_floor() {
    for command in ["git checkout -- .", "git restore .", "git switch -f main"] {
        let result = scan(command);
        assert_eq!(
            result["recommended_action"], "require_approval",
            "{command}: {result}"
        );
        assert!(
            cat_reason(command, &[ROOT]).is_none(),
            "{command} must stay approvable"
        );
    }
}
