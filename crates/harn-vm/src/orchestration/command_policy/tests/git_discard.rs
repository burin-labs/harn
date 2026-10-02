//! `git_discard_changes` marks git commands that throw away uncommitted work,
//! untracked files, or stash entries. It is an approval label: the commands
//! outside the catastrophic floor stay approvable rather than denied.

use super::*;

fn scan(command: &str) -> JsonValue {
    command_risk_scan_json(&ctx(&["sh", "-c", command]), None)
}

fn discards(command: &str) -> bool {
    labels(&scan(command)).contains(&"git_discard_changes".to_string())
}

#[test]
fn worktree_and_stash_discards_carry_the_label() {
    for command in [
        "git restore .",
        "git restore src/main.rs",
        "git restore --worktree .",
        "git restore --staged --worktree .",
        "git restore -SW .",
        "git restore --source=HEAD~2 .",
        "git checkout -- .",
        "git checkout .",
        "git checkout -- src/main.rs",
        "git checkout HEAD -- src/main.rs",
        "git checkout HEAD src/main.rs",
        "git checkout ./src",
        "git checkout ':/*.rs'",
        "git checkout --theirs conflicted.txt",
        "git checkout -f main",
        "git checkout --force main",
        "git switch -f main",
        "git switch --discard-changes main",
        "git reset --hard",
        "git reset --hard HEAD~1",
        "git clean -f",
        "git clean -fdx",
        "git stash drop",
        "git stash drop stash@{1}",
        "git stash clear",
        "git -C repo restore .",
        "git status && git restore .",
        "env GIT_TRACE=1 git checkout -- .",
        "bash -c 'git stash clear'",
    ] {
        assert!(discards(command), "expected git_discard_changes: {command}");
    }
}

#[test]
fn commands_that_keep_work_do_not_carry_the_label() {
    for command in [
        "git status",
        "git diff -- .",
        "git log -- src/main.rs",
        "git restore --staged .",
        "git restore -S src/main.rs",
        "git checkout main",
        "git checkout -b fix-foo",
        "git checkout -b feature origin/main",
        "git checkout -B feature origin/main",
        "git checkout --track origin/feature",
        "git checkout -",
        "git switch main",
        "git switch -c feature",
        "git reset",
        "git reset --soft HEAD~1",
        "git reset --keep HEAD~1",
        "git clean -n",
        "git clean -nd",
        "git stash",
        "git stash push -u",
        "git stash list",
        "git stash pop",
        "git stash apply",
        "echo git restore .",
    ] {
        assert!(
            !discards(command),
            "did not expect git_discard_changes: {command}"
        );
    }
}

/// A discard that is not on the catastrophic floor routes to approval, not
/// denial; the floor members keep their hard denial on top of the label.
#[test]
fn discard_label_routes_to_approval_and_leaves_the_floor_alone() {
    let restore = scan("git restore .");
    assert_eq!(restore["recommended_action"], "require_approval");
    assert!(restore.get("catastrophic_reason").is_none());

    let reset = scan("git reset --hard");
    assert!(labels(&reset).contains(&"catastrophic".to_string()));
    assert!(labels(&reset).contains(&"git_discard_changes".to_string()));
}
