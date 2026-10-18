//! A read denial inside a writable grant (harn#9028).
//!
//! Subtracting the denial from a writable grant took away the child's right to
//! create files along the path to it. The grant now keeps its whole root and
//! the denial is reported as not enforced. A read-only grant is still
//! subtracted, which the control below pins.

use std::path::{Path, PathBuf};

use super::filesystem::{filesystem_profile, UnheldReadDenial};
use super::tests::live_landlock_available;
use super::{AccessFs, FilesystemProfile, FsAccess, ProcessFilesystemScope};
use crate::orchestration::{CapabilityPolicy, ProcessSandboxPolicy, SandboxProfile};
use crate::stdlib::sandbox::{command_output, ProcessCommandConfig};
use crate::VmError;

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    denied: PathBuf,
}

/// `<workspace>/a.txt` beside `<workspace>/dir/secret.json`, the embedder's
/// measured layout.
fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("fixture root");
    let base = root.path().canonicalize().expect("canonical root");
    let workspace = base.join("workspace");
    let denied = workspace.join("dir/secret.json");
    std::fs::create_dir_all(denied.parent().unwrap()).expect("denied parent");
    std::fs::write(&denied, "synthetic-secret").expect("denied file");
    std::fs::write(workspace.join("a.txt"), "a").expect("source file");
    Fixture {
        _root: root,
        workspace,
        denied,
    }
}

fn policy(fixture: &Fixture, writable: bool, profile: SandboxProfile) -> CapabilityPolicy {
    let root = fixture.workspace.display().to_string();
    let (workspace_roots, read_only_roots) = if writable {
        (vec![root], Vec::new())
    } else {
        // The workspace grant needs some root; give it one beside the fixture.
        let beside = fixture.workspace.with_file_name("beside");
        std::fs::create_dir_all(&beside).expect("beside");
        (vec![beside.display().to_string()], vec![root])
    };
    CapabilityPolicy {
        sandbox_profile: profile,
        workspace_roots,
        read_only_roots,
        process_sandbox: Box::new(ProcessSandboxPolicy {
            read_deny_roots: vec![fixture.denied.display().to_string()],
            ..ProcessSandboxPolicy::default()
        }),
        ..CapabilityPolicy::default()
    }
}

fn profile_for(policy: &CapabilityPolicy) -> Result<FilesystemProfile, VmError> {
    filesystem_profile(
        "/bin/sh",
        policy,
        FsAccess::all(),
        ProcessFilesystemScope::Host,
    )
}

fn rule_access(profile: &FilesystemProfile, path: &Path) -> Option<FsAccess> {
    profile
        .rules
        .iter()
        .find(|rule| rule.path == path)
        .map(|rule| rule.allowed_access)
}

#[test]
fn a_writable_root_is_granted_whole_and_reports_the_denial_inside_it() {
    let fixture = fixture();
    let profile = profile_for(&policy(&fixture, true, SandboxProfile::Worktree))
        .expect("a denial inside a writable root must not refuse a worktree spawn");

    let access = rule_access(&profile, &fixture.workspace)
        .expect("the writable root must be granted itself, not its children");
    assert!(access.contains(AccessFs::MakeReg), "create authority");
    assert!(
        rule_access(&profile, &fixture.workspace.join("a.txt")).is_none(),
        "the root was expanded into its children"
    );
    assert_eq!(
        profile.unheld_read_denials,
        vec![UnheldReadDenial {
            denied: fixture.denied,
            writable_grant: fixture.workspace,
        }]
    );
}

/// The control: the same layout granted read-only is still subtracted, so the
/// change above is confined to writable grants.
#[test]
fn a_read_only_root_is_still_subtracted_around_the_same_denial() {
    let fixture = fixture();
    let profile =
        profile_for(&policy(&fixture, false, SandboxProfile::Worktree)).expect("read-only profile");

    assert!(rule_access(&profile, &fixture.workspace).is_none());
    assert!(rule_access(&profile, &fixture.workspace.join("a.txt")).is_some());
    assert!(rule_access(&profile, &fixture.denied).is_none());
    assert!(profile.unheld_read_denials.is_empty());
}

#[test]
fn os_hardened_refuses_a_denial_inside_a_writable_root_and_names_both() {
    let fixture = fixture();
    let error = match profile_for(&policy(&fixture, true, SandboxProfile::OsHardened)) {
        Ok(_) => panic!("os_hardened ran with a denial it cannot hold"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains(&fixture.denied.display().to_string())
            && error.contains(&fixture.workspace.display().to_string()),
        "{error}"
    );
}

/// The embedder's measurement: a confined `cp` beside the denial failed with
/// `Permission denied`. It must now create the file.
#[test]
fn a_live_child_creates_files_in_a_writable_root_that_holds_a_denial() {
    if !live_landlock_available("writable-denial-live") {
        return;
    }
    let fixture = fixture();
    crate::orchestration::push_execution_policy(policy(&fixture, true, SandboxProfile::Worktree));
    let output = command_output(
        "/bin/sh",
        &[
            "-c".to_string(),
            "cp a.txt b.txt && touch dir/created.txt".to_string(),
        ],
        &ProcessCommandConfig {
            cwd: Some(fixture.workspace.clone()),
            ..ProcessCommandConfig::default()
        },
    );
    crate::orchestration::pop_execution_policy();
    let output = output.expect("spawn");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("b.txt")).unwrap(),
        "a"
    );
    assert!(fixture.workspace.join("dir/created.txt").exists());
}
