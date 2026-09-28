//! The scope check derives its root sets once per policy and inputs
//! (harn#8963), and a scoped mutation or the time bound forces a fresh
//! derivation so a credential path re-created as a symlink is still refused.

use std::path::Path;

use super::super::{check_fs_path_scope, scope_memo, FsAccess};
use crate::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
};

fn with_workspace_policy<T>(workspace: &Path, body: impl FnOnce() -> T) -> T {
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });
    scope_memo::clear();
    let out = body();
    pop_execution_policy();
    scope_memo::clear();
    out
}

#[test]
fn repeated_reads_under_one_policy_derive_the_roots_once() {
    // Other tests move HOME and the working directory, which are memo inputs.
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let workspace = tempfile::tempdir().unwrap();
    with_workspace_policy(workspace.path(), || {
        let before = scope_memo::derivations();
        for index in 0..50 {
            let file = workspace.path().join(format!("f{index}.txt"));
            assert!(check_fs_path_scope(&file, FsAccess::Read).is_ok());
        }
        assert_eq!(scope_memo::derivations() - before, 1);
    });
}

#[test]
fn a_memo_older_than_the_bound_is_derived_again() {
    // Other tests move HOME and the working directory, which are memo inputs.
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let workspace = tempfile::tempdir().unwrap();
    with_workspace_policy(workspace.path(), || {
        let file = workspace.path().join("a.txt");
        assert!(check_fs_path_scope(&file, FsAccess::Read).is_ok());
        let before = scope_memo::derivations();
        scope_memo::age(scope_memo::MEMO_TTL + std::time::Duration::from_millis(1));
        assert!(check_fs_path_scope(&file, FsAccess::Read).is_ok());
        assert_eq!(scope_memo::derivations() - before, 1);
    });
}

/// Falsifier for the invalidation: a read allowed under a memo, then a scoped
/// write, then `~/.ssh` re-created as a symlink into the workspace. The next
/// read through that link must be refused. Reusing the first read's memo
/// would allow it, because that memo resolved `~/.ssh` before the link existed.
#[cfg(unix)]
#[test]
fn a_deny_root_linked_into_the_workspace_after_a_write_is_refused() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let saved_home = std::env::var_os("HOME");
    std::env::set_var("HOME", home.path());
    let secret = workspace.path().join("secret");
    std::fs::create_dir(&secret).unwrap();
    let key = secret.join("id_ed25519");

    let refused = with_workspace_policy(workspace.path(), || {
        assert!(check_fs_path_scope(&key, FsAccess::Read).is_ok());
        assert!(check_fs_path_scope(&workspace.path().join("out.txt"), FsAccess::Write).is_ok());
        std::os::unix::fs::symlink(&secret, home.path().join(".ssh")).unwrap();
        check_fs_path_scope(&key, FsAccess::Read).is_err()
    });

    match saved_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    assert!(
        refused,
        "a read through a freshly linked ~/.ssh must be refused"
    );
}

/// The memo holds only the derived root sets, never a verdict for a path.
/// A workspace link re-pointed at `~/.ssh` by another process between two
/// reads (no harness write in between, so the memo is reused) must still be
/// refused, because each requested path is resolved fresh against the roots.
#[cfg(unix)]
#[test]
fn a_workspace_link_re_pointed_at_a_deny_root_between_reads_is_refused() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let saved_home = std::env::var_os("HOME");
    std::env::set_var("HOME", home.path());
    std::fs::create_dir(home.path().join(".ssh")).unwrap();
    let plain = workspace.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let link = workspace.path().join("link");
    std::os::unix::fs::symlink(&plain, &link).unwrap();
    let key = link.join("id_ed25519");

    let (first_allowed, second_refused, derived) = with_workspace_policy(workspace.path(), || {
        let before = scope_memo::derivations();
        let first_allowed = check_fs_path_scope(&key, FsAccess::Read).is_ok();
        // Another process swaps the link; nothing goes through the harness.
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(home.path().join(".ssh"), &link).unwrap();
        let second_refused = check_fs_path_scope(&key, FsAccess::Read).is_err();
        (
            first_allowed,
            second_refused,
            scope_memo::derivations() - before,
        )
    });

    match saved_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    assert!(first_allowed, "the link first points inside the workspace");
    assert_eq!(derived, 1, "the second read reused the memoized roots");
    assert!(second_refused, "a path re-linked to ~/.ssh must be refused");
}
