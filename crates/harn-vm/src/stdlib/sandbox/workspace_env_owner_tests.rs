//! The rule that lets a workspace's runtime dir count as part of the
//! workspace. A confined child can write its runtime dir, marker included, so
//! every way it can rewrite the marker must leave no coverage, and none may
//! block the host that reads it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;
use crate::orchestration::{
    pop_execution_policy, push_execution_policy, ProcessSandboxPolicy, SandboxProfile,
};

/// A canonical runtime base in a fresh temp dir.
fn temp_base() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = normalize_for_policy(dir.path());
    (dir, path)
}

/// Create the runtime dir named for `root` under `base`, without a marker.
fn runtime_dir_for(base: &Path, root: &Path) -> PathBuf {
    let dir = base.join(runtime_digest(root));
    std::fs::create_dir_all(dir.join("tmp")).unwrap();
    dir
}

fn write_marker(runtime: &Path, contents: &[u8]) {
    std::fs::write(runtime.join(RUNTIME_DIR_MARKER), contents).unwrap();
}

/// Resolve owners on another thread, so a reader that blocks fails the test
/// at the deadline instead of stalling the suite.
fn owners_within_deadline(base: &Path, path: &Path) -> Vec<PathBuf> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let (base, path) = (base.to_path_buf(), path.to_path_buf());
    crate::runtime_stack::spawn(move || {
        let _ = sender.send(runtime_dir_owners_in(&base, &path));
    });
    receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("the marker reader must return within the deadline")
}

#[cfg(unix)]
fn root_bytes(root: &Path) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    root.as_os_str().as_bytes()
}

#[cfg(not(unix))]
fn root_bytes(root: &Path) -> &[u8] {
    root.to_str().unwrap().as_bytes()
}

/// Positive control: an honest marker names its workspace, and a root made in
/// one workspace's temp dir chains out to that workspace.
#[test]
fn an_honest_marker_names_its_workspace_and_chains_outward() {
    let (_guard, base) = temp_base();
    let outer = std::env::temp_dir().join("harn-owner-outer");
    let outer_runtime = runtime_dir_for(&base, &outer);
    write_marker(&outer_runtime, root_bytes(&outer));
    let inner = outer_runtime.join("tmp").join("inner");
    let inner_runtime = runtime_dir_for(&base, &inner);
    write_marker(&inner_runtime, root_bytes(&inner));

    let file = inner_runtime.join("tmp").join("file");
    assert_eq!(owners_within_deadline(&base, &file), vec![inner, outer]);
    assert!(owners_within_deadline(&base, &base.join("not-a-digest").join("file")).is_empty());
}

/// A child can replace its marker with a FIFO. Opening that for reading with
/// no writer blocks forever, and no stop control can interrupt the syscall.
#[cfg(unix)]
#[test]
fn a_fifo_marker_returns_promptly_with_no_owner() {
    let (_guard, base) = temp_base();
    let root = std::env::temp_dir().join("harn-owner-fifo");
    let runtime = runtime_dir_for(&base, &root);
    let marker = std::ffi::CString::new(root_bytes(&runtime.join(RUNTIME_DIR_MARKER))).unwrap();
    // SAFETY: `marker` is a valid NUL-terminated path for the whole call.
    assert_eq!(unsafe { libc::mkfifo(marker.as_ptr(), 0o600) }, 0);

    assert!(owners_within_deadline(&base, &runtime.join("tmp/file")).is_empty());
}

/// A marker symlinked to an honest marker elsewhere is still refused: the
/// reader never follows a link the child planted.
#[cfg(unix)]
#[test]
fn a_symlinked_marker_is_not_followed() {
    let (_guard, base) = temp_base();
    let elsewhere = tempfile::tempdir().unwrap();
    let root = std::env::temp_dir().join("harn-owner-symlink");
    let runtime = runtime_dir_for(&base, &root);
    let honest = elsewhere.path().join("honest-marker");
    std::fs::write(&honest, root_bytes(&root)).unwrap();
    std::os::unix::fs::symlink(&honest, runtime.join(RUNTIME_DIR_MARKER)).unwrap();

    assert!(owners_within_deadline(&base, &runtime.join("tmp/file")).is_empty());
}

/// A dir named for one workspace whose marker names another workspace.
#[test]
fn a_forged_marker_names_no_owner() {
    let (_guard, base) = temp_base();
    let named = std::env::temp_dir().join("harn-owner-named");
    let claimed = std::env::temp_dir().join("harn-owner-claimed");
    let runtime = runtime_dir_for(&base, &named);
    write_marker(&runtime, root_bytes(&claimed));

    assert!(owners_within_deadline(&base, &runtime.join("tmp/file")).is_empty());
}

/// A marker past the read bound is refused even when it digests correctly.
#[test]
fn an_oversized_marker_names_no_owner() {
    let (_guard, base) = temp_base();
    let mut long = std::env::temp_dir().into_os_string();
    long.push("/");
    long.push("a".repeat(RUNTIME_DIR_MARKER_MAX as usize + 16));
    let root = PathBuf::from(long);
    let runtime = runtime_dir_for(&base, &root);
    write_marker(&runtime, root_bytes(&root));

    assert!(owners_within_deadline(&base, &runtime.join("tmp/file")).is_empty());
}

/// A relative root names no workspace, even under its own digest.
#[test]
fn a_relative_marker_names_no_owner() {
    let (_guard, base) = temp_base();
    let root = PathBuf::from("relative").join("workspace");
    let runtime = runtime_dir_for(&base, &root);
    write_marker(&runtime, root_bytes(&root));

    assert!(owners_within_deadline(&base, &runtime.join("tmp/file")).is_empty());
}

fn worktree_policy(roots: &[&Path], read_deny: &[&Path]) -> CapabilityPolicy {
    CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: roots
            .iter()
            .map(|root| root.display().to_string())
            .collect(),
        process_sandbox: Box::new(ProcessSandboxPolicy {
            read_deny_roots: read_deny.iter().map(|p| p.display().to_string()).collect(),
            ..ProcessSandboxPolicy::default()
        }),
        ..CapabilityPolicy::default()
    }
}

fn scope_allows(policy: CapabilityPolicy, path: &Path, access: super::super::FsAccess) -> bool {
    push_execution_policy(policy);
    let allowed = super::super::check_fs_path_scope(path, access).is_ok();
    pop_execution_policy();
    allowed
}

/// Through the real scope check: a root covering a workspace covers its
/// runtime dir, a sibling workspace's root does not, and an explicit read
/// deny still wins over the coverage.
#[test]
fn coverage_reaches_enclosing_roots_only_and_never_beats_a_read_deny() {
    use super::super::FsAccess;

    let parent = tempfile::tempdir().unwrap();
    let parent_root = normalize_for_policy(parent.path());
    let owner = parent_root.join("owner");
    let sibling = parent_root.join("sibling");
    std::fs::create_dir_all(&owner).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();
    let tmp = workspace_local_tmpdir(&worktree_policy(&[&owner], &[])).expect("runtime temp dir");
    let file = tmp.join("file");
    std::fs::write(&file, "x").unwrap();

    let enclosing = || worktree_policy(&[&parent_root], &[]);
    let beside = || worktree_policy(&[&sibling], &[]);
    let read = scope_allows(enclosing(), &file, FsAccess::Read);
    let write = scope_allows(enclosing(), &file, FsAccess::Write);
    let sibling_read = scope_allows(beside(), &file, FsAccess::Read);
    let sibling_write = scope_allows(beside(), &file, FsAccess::Write);
    let denied_read = scope_allows(
        worktree_policy(&[&parent_root], &[&file]),
        &file,
        FsAccess::Read,
    );
    std::fs::remove_dir_all(tmp.parent().unwrap()).unwrap();

    assert!(read && write, "an enclosing root covers the runtime dir");
    assert!(!sibling_read && !sibling_write, "a sibling root does not");
    assert!(!denied_read, "an explicit read deny wins over coverage");
}
