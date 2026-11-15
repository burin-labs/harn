//! The writable set a host reads before any sandbox exists.

use std::path::Path;

use super::*;
use crate::orchestration::{CapabilityPolicy, SandboxProfile};

fn writing_policy(workspace: &Path) -> CapabilityPolicy {
    CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([(
            "workspace".to_string(),
            vec!["read_text".to_string(), "write_text".to_string()],
        )]),
        workspace_roots: vec![workspace.display().to_string()],
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    }
}

#[cfg(target_os = "macos")]
#[test]
fn granted_clone_transfer_writes_share_the_backend_root_census() {
    use crate::stdlib::sandbox::{normalize_for_policy, workspace_env};

    let workspace = tempfile::tempdir().unwrap();
    let private = tempfile::tempdir().unwrap();
    let transfer = private.path().join("repository");
    let expected = normalize_for_policy(&transfer);
    let policy = writing_policy(workspace.path());
    assert!(!process_sandbox_writable_roots(&policy).contains(&expected));
    let launch = workspace_env::grant_spawn::OwnSpawnLaunch::begin_with_writes(
        private.path(),
        std::slice::from_ref(&transfer),
    );
    assert!(process_sandbox_writable_roots(&policy).contains(&expected));
    drop(launch);
    assert!(!process_sandbox_writable_roots(&policy).contains(&expected));
}

/// A write root that does not exist yet, under a link, resolves through the
/// link: a reader comparing canonical paths would otherwise miss a file a
/// child creates there later.
#[test]
fn a_root_that_does_not_exist_yet_resolves_through_its_existing_ancestor() {
    let base = tempfile::tempdir().expect("base");
    let real = base.path().join("real");
    std::fs::create_dir(&real).expect("real");
    std::os::unix::fs::symlink(&real, base.path().join("link")).expect("link");
    let workspace = tempfile::tempdir().expect("workspace");
    let mut policy = writing_policy(workspace.path());
    policy.process_sandbox.write_roots = vec![base
        .path()
        .join("link/missing/deeper")
        .display()
        .to_string()];

    let writable = process_sandbox_writable_roots(&policy);
    let expected = std::fs::canonicalize(&real)
        .expect("canonical real")
        .join("missing/deeper");
    assert!(
        writable.contains(&expected),
        "{} missing from {writable:?}",
        expected.display()
    );
}

/// A direct Landlock launch grants the host `/dev/shm`, so the set names it
/// whenever this host has one.
#[cfg(target_os = "linux")]
#[test]
fn the_host_shared_memory_dir_counts_as_writable_on_linux() {
    if !Path::new("/dev/shm").is_dir() {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace");
    let writable = process_sandbox_writable_roots(&writing_policy(workspace.path()));
    let shm = std::fs::canonicalize("/dev/shm").expect("canonical shm");
    assert!(
        writable.contains(&shm),
        "/dev/shm missing from {writable:?}"
    );
}

/// Every directory the Landlock ruleset grants a write right on falls inside
/// the set, under the default presets with a named write root.
#[cfg(target_os = "linux")]
#[test]
fn the_writable_set_covers_every_installed_landlock_write_grant() {
    let workspace = tempfile::tempdir().expect("workspace");
    let named = tempfile::tempdir().expect("write root");
    let mut policy = writing_policy(workspace.path());
    policy.process_sandbox.write_roots = vec![named.path().display().to_string()];
    let Some(grants) = super::super::linux::installed_write_grants(&policy) else {
        crate::sandbox_host::boundary_not_measured(
            "the_writable_set_covers_every_installed_landlock_write_grant",
            "this host has no Landlock",
        );
        return;
    };
    let writable = process_sandbox_writable_roots(&policy);
    let mut checked = 0;
    for (path, access) in &grants.rules {
        // Device and program files carry write bits but are not places a
        // child can plant a file.
        if access & grants.write_access == 0 || !path.is_dir() {
            continue;
        }
        checked += 1;
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        assert!(
            writable.iter().any(|root| path.starts_with(root)),
            "{} is granted write but missing from {writable:?}",
            path.display()
        );
    }
    assert!(
        checked > 1,
        "the ruleset granted too few writable directories"
    );
}
