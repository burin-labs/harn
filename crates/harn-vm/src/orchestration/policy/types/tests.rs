//! Capability-policy type unit tests.
//!
//! Split out of `types.rs` to keep that module within the
//! source-file-length ratchet; the module path is unchanged.

use super::{
    intersect_roots, sandbox_profile_strictness, CapabilityPolicy, ModelPolicy,
    RequiredSuccessfulTool, SandboxProfile,
};

#[test]
fn a_neutral_policy_leaves_the_parent_confinement_untouched() {
    for parent_profile in [
        SandboxProfile::Unrestricted,
        SandboxProfile::Worktree,
        SandboxProfile::Wasi,
        SandboxProfile::OsHardened,
    ] {
        let parent = CapabilityPolicy {
            sandbox_profile: parent_profile,
            ..CapabilityPolicy::default()
        };
        let merged = parent
            .intersect(&CapabilityPolicy::neutral())
            .expect("a neutral overlay always fits its parent");
        assert_eq!(
            merged.sandbox_profile, parent_profile,
            "neutral overlay must be the identity for {parent_profile:?}"
        );
    }
}

#[test]
fn a_neutral_policy_asserts_no_confinement_of_its_own() {
    // The whole point of `neutral()`: pushed with no parent to intersect
    // against, it must not invent confinement the run never asked for.
    assert_eq!(
        CapabilityPolicy::neutral().sandbox_profile,
        SandboxProfile::Unrestricted
    );
    // `default()` stays fail-closed for policies that *are* the decision.
    assert_eq!(
        CapabilityPolicy::default().sandbox_profile,
        SandboxProfile::Worktree
    );
}

#[test]
fn model_policy_round_trips_required_successful_tool_or_groups() {
    let value = serde_json::json!({
        "require_successful_tools": ["verify", ["edit", "scaffold"]]
    });

    let policy: ModelPolicy = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        policy.require_successful_tools,
        Some(vec![
            RequiredSuccessfulTool::Tool("verify".to_string()),
            RequiredSuccessfulTool::AnyOf(vec!["edit".to_string(), "scaffold".to_string()]),
        ])
    );
    let serialized = serde_json::to_value(policy).unwrap();
    assert_eq!(
        serialized.get("require_successful_tools"),
        value.get("require_successful_tools")
    );
}

#[test]
fn a_narrower_requested_root_survives_intersection() {
    let roots = intersect_roots(
        &["/repo".to_string()],
        &["/repo/crates/harn-vm".to_string()],
    );
    assert_eq!(roots, vec!["/repo/crates/harn-vm".to_string()]);
}

#[test]
fn a_wider_requested_root_is_clamped_to_the_host_root() {
    let roots = intersect_roots(&["/repo/crates".to_string()], &["/repo".to_string()]);
    assert_eq!(roots, vec!["/repo/crates".to_string()]);
}

#[test]
fn a_disjoint_requested_root_is_dropped() {
    let roots = intersect_roots(&["/repo".to_string()], &["/elsewhere".to_string()]);
    assert!(roots.is_empty(), "{roots:?}");
}

#[test]
fn a_sibling_sharing_a_name_prefix_is_not_treated_as_nested() {
    let roots = intersect_roots(&["/repo".to_string()], &["/repo-backup".to_string()]);
    assert!(
        roots.is_empty(),
        "/repo-backup is beside /repo, not inside it: {roots:?}"
    );
}

#[test]
fn workspace_paths_enforces_paths_without_confining_processes() {
    let profile = SandboxProfile::WorkspacePaths;
    assert!(profile.enforces_path_scope());
    assert!(
        !profile.confines_processes(),
        "the point of this rung is that a subprocess runs unconfined"
    );
}

#[test]
fn wasi_does_not_claim_to_confine_host_processes() {
    // Testbench mode intercepts subprocesses before the host spawn
    // path, so nothing there applies an OS mechanism. Reporting a
    // child's permission error as an OS sandbox denial would be a
    // misattribution.
    assert!(!SandboxProfile::Wasi.confines_processes());
    assert!(SandboxProfile::Wasi.enforces_path_scope());
}

#[test]
fn only_unrestricted_opts_out_of_path_scope() {
    for profile in SandboxProfile::all() {
        assert_eq!(
            profile.enforces_path_scope(),
            *profile != SandboxProfile::Unrestricted,
            "{profile:?}"
        );
    }
}

#[test]
fn every_profile_is_on_the_ladder_weakest_first() {
    let ladder = SandboxProfile::all();
    assert_eq!(
        ladder.len(),
        5,
        "a profile was added without placing it in `all()`: {ladder:?}"
    );
    for pair in ladder.windows(2) {
        assert!(
            sandbox_profile_strictness(pair[0]) < sandbox_profile_strictness(pair[1]),
            "`all()` must be ordered weakest first: {:?} is not weaker than {:?}",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn every_profile_round_trips_through_its_wire_name() {
    for profile in SandboxProfile::all() {
        assert_eq!(SandboxProfile::parse(profile.as_str()), Some(*profile));
    }
}

#[test]
fn a_path_only_profile_loses_to_a_process_confining_parent() {
    // Intersect takes the stricter side, so a child asking for
    // path-only confinement cannot shed the OS sandbox its parent
    // imposed.
    let parent = CapabilityPolicy {
        sandbox_profile: SandboxProfile::OsHardened,
        ..CapabilityPolicy::default()
    };
    let child = CapabilityPolicy {
        sandbox_profile: SandboxProfile::WorkspacePaths,
        ..CapabilityPolicy::default()
    };
    let merged = parent.intersect(&child).expect("intersect");
    assert_eq!(merged.sandbox_profile, SandboxProfile::OsHardened);
}
