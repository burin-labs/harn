//! harn#8930: `git push` is classified from its parsed argv. The label used to
//! come from a substring test, so forced and deleting refspecs went unlabelled
//! and a branch name containing `-f` was labelled.

use super::*;

fn has_git_force_push(command: &str) -> bool {
    let scan = command_risk_scan_json(&ctx(&["sh", "-c", command]), None);
    labels(&scan).contains(&"git_force_push".to_string())
}

#[test]
fn git_force_push_label_follows_the_parsed_push() {
    for command in [
        "git push origin +main",
        "git push origin :main",
        "git push --delete origin main",
        "git push -d origin main",
        "git push --del origin main",
        "git push --prune origin",
        "git push --mirror origin",
        "git -C repo push -f origin main",
        "git push -uf origin main",
        "echo ok && git push --mirror origin",
        "env GIT_TRACE=1 git push origin :main",
    ] {
        assert!(
            has_git_force_push(command),
            "expected git_force_push: {command}"
        );
    }
    for command in [
        "git push origin feature-foo",
        "git push -u origin main",
        "git push origin :",
        "git push -o ci.skip origin main",
        "git push -ofeature origin main",
        "git push --push-option +x origin main",
        "git push --force-if-includes origin main",
        "git log --format=-f && git push origin main",
    ] {
        assert!(
            !has_git_force_push(command),
            "unexpected git_force_push: {command}"
        );
    }
}

#[test]
fn floor_blocks_every_force_push_shape() {
    for command in [
        "git push origin +main",
        "git push origin +HEAD:main",
        "git push 'origin' '+refs/heads/*:refs/heads/*'",
        "git push origin main -- +release",
        "git push --mirror origin",
        "git push --mirr origin",
        "git push --force-w origin main",
        "git push -uf origin main",
        "git -C repo push -f origin main",
        "git --config-env core.x=Y push -f origin main",
        "sudo git push origin +main",
    ] {
        assert!(is_cat_root(command), "expected catastrophic: {command}");
    }
}

/// Deleting a remote ref is labelled `git_force_push` so policy can deny or
/// gate it, but it stays approvable: only a push that can overwrite remote
/// history is on the never-approvable floor.
#[test]
fn floor_allows_ordinary_pushes_and_remote_ref_deletion() {
    for command in [
        "git push origin :",
        "git push -u origin feature-foo",
        "git push -o ci.skip origin main",
        "git push -ofeature origin main",
        "git push origin :main",
        "git push --delete origin main",
        "git push -d origin main",
        "git push --prune origin",
    ] {
        let reason = cat_reason(command, &[ROOT]);
        assert!(
            reason.is_none(),
            "should NOT be catastrophic: {command}; reason: {reason:?}"
        );
    }
}
