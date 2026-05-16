//! Integration tests for the production sandbox profiles.
//!
//! These cover the dispatch contract that conformance fixtures cannot
//! reach from a Harn script: directly pushing a `CapabilityPolicy`
//! with each `SandboxProfile` variant onto the orchestration stack and
//! observing how `enforce_process_cwd` and the OS-level spawn helpers
//! react. The OS-level confinement itself (Linux Landlock+seccomp,
//! macOS sandbox-exec, Windows AppContainer) is exercised by the
//! per-platform unit tests inside `crates/harn-vm/src/stdlib/sandbox/`.

use std::collections::BTreeMap;

use harn_vm::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
};
use harn_vm::process_sandbox;

fn policy_with(profile: SandboxProfile, workspace: &std::path::Path) -> CapabilityPolicy {
    CapabilityPolicy {
        tools: Vec::new(),
        capabilities: BTreeMap::new(),
        workspace_roots: vec![workspace.display().to_string()],
        side_effect_level: Some("workspace_write".to_string()),
        recursion_limit: None,
        tool_arg_constraints: Vec::new(),
        tool_annotations: BTreeMap::new(),
        sandbox_profile: profile,
    }
}

struct PolicyGuard;

impl PolicyGuard {
    fn push(profile: SandboxProfile, workspace: &std::path::Path) -> Self {
        push_execution_policy(policy_with(profile, workspace));
        Self
    }
}

impl Drop for PolicyGuard {
    fn drop(&mut self) {
        pop_execution_policy();
    }
}

#[test]
fn worktree_profile_rejects_cwd_outside_workspace_roots() {
    let workspace = tempfile::tempdir().unwrap();
    let _guard = PolicyGuard::push(SandboxProfile::Worktree, workspace.path());

    let outside = std::env::temp_dir().join("harn-sandbox-out-of-tree");
    std::fs::create_dir_all(&outside).unwrap();

    let result = process_sandbox::enforce_process_cwd(&outside);
    assert!(
        result.is_err(),
        "Worktree profile must reject a cwd outside its workspace_roots"
    );
}

#[test]
fn worktree_profile_accepts_cwd_inside_workspace_roots() {
    let workspace = tempfile::tempdir().unwrap();
    let _guard = PolicyGuard::push(SandboxProfile::Worktree, workspace.path());

    let inside = workspace.path().join("subdir");
    std::fs::create_dir_all(&inside).unwrap();
    process_sandbox::enforce_process_cwd(&inside)
        .expect("cwd inside workspace_roots must be accepted");
}

#[test]
fn unrestricted_profile_skips_cwd_enforcement() {
    let workspace = tempfile::tempdir().unwrap();
    let _guard = PolicyGuard::push(SandboxProfile::Unrestricted, workspace.path());

    let outside = std::env::temp_dir().join("harn-sandbox-unrestricted");
    std::fs::create_dir_all(&outside).unwrap();
    process_sandbox::enforce_process_cwd(&outside).expect(
        "Unrestricted profile must skip workspace_root enforcement so escape-hatch \
         workflows still spawn",
    );
}

#[test]
fn os_hardened_profile_rejects_cwd_outside_workspace_roots() {
    let workspace = tempfile::tempdir().unwrap();
    let _guard = PolicyGuard::push(SandboxProfile::OsHardened, workspace.path());

    let outside = std::env::temp_dir().join("harn-sandbox-os-hardened");
    std::fs::create_dir_all(&outside).unwrap();
    let result = process_sandbox::enforce_process_cwd(&outside);
    assert!(
        result.is_err(),
        "OsHardened profile must enforce workspace_root cwd just like Worktree"
    );
}

#[test]
fn active_backend_name_matches_target_os() {
    let expected = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "openbsd") {
        "openbsd"
    } else {
        "noop"
    };
    assert_eq!(process_sandbox::active_backend_name(), expected);
}

#[test]
fn sandbox_profile_string_round_trips() {
    for profile in [
        SandboxProfile::Unrestricted,
        SandboxProfile::Worktree,
        SandboxProfile::OsHardened,
        SandboxProfile::Wasi,
    ] {
        let parsed = SandboxProfile::parse(profile.as_str())
            .unwrap_or_else(|| panic!("profile {profile:?} should round-trip via as_str/parse"));
        assert_eq!(parsed, profile);
    }
    assert!(
        SandboxProfile::parse("definitely-not-a-profile").is_none(),
        "unknown profile names must not be silently accepted"
    );
}

#[test]
fn worktree_intersects_to_os_hardened_under_stricter_request() {
    let workspace = tempfile::tempdir().unwrap();
    let parent = policy_with(SandboxProfile::Worktree, workspace.path());
    let requested = policy_with(SandboxProfile::OsHardened, workspace.path());
    let merged = parent
        .intersect(&requested)
        .expect("intersect should succeed");
    assert_eq!(
        merged.sandbox_profile,
        SandboxProfile::OsHardened,
        "intersect must take the strictest of the two profiles so a \
         lenient parent cannot weaken a child request"
    );
}

#[test]
fn unrestricted_intersects_with_worktree_to_worktree() {
    let workspace = tempfile::tempdir().unwrap();
    let parent = policy_with(SandboxProfile::Unrestricted, workspace.path());
    let requested = policy_with(SandboxProfile::Worktree, workspace.path());
    let merged = parent
        .intersect(&requested)
        .expect("intersect should succeed");
    assert_eq!(merged.sandbox_profile, SandboxProfile::Worktree);
}
