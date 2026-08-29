//! Capability-policy type unit tests.
//!
//! Split out of `types.rs` to keep that module within the
//! source-file-length ratchet; the module path is unchanged.

use std::collections::BTreeMap;

use super::{
    intersect_roots, sandbox_profile_strictness, CapabilityPolicy, ModelPolicy,
    ProcessNetworkProxy, ProcessSandboxPolicy, RequiredSuccessfulTool, SandboxProfile,
};

#[test]
fn serialized_policy_cannot_inject_or_disclose_host_proxy_endpoints() {
    let requested: CapabilityPolicy = serde_json::from_value(serde_json::json!({
        "process_network_proxy": {"http_port": 3128, "socks_port": 1080}
    }))
    .unwrap();
    assert_eq!(requested.process_network_proxy, None);

    let installed = CapabilityPolicy {
        process_network_proxy: Some(ProcessNetworkProxy {
            http_port: 3128,
            socks_port: 1080,
        }),
        ..Default::default()
    };
    let serialized = serde_json::to_value(installed).unwrap();
    assert!(serialized.get("process_network_proxy").is_none());
}

#[test]
fn policy_intersection_preserves_only_the_outer_host_proxy() {
    let endpoint = |http_port, socks_port| ProcessNetworkProxy {
        http_port,
        socks_port,
    };
    let outer = CapabilityPolicy {
        process_network_proxy: Some(endpoint(3128, 1080)),
        ..Default::default()
    };
    let requested = CapabilityPolicy {
        process_network_proxy: Some(endpoint(9000, 9001)),
        ..Default::default()
    };
    assert_eq!(
        outer.intersect(&requested).unwrap().process_network_proxy,
        Some(endpoint(3128, 1080))
    );
    assert_eq!(
        CapabilityPolicy::default()
            .intersect(&requested)
            .unwrap()
            .process_network_proxy,
        None
    );
}

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
fn tcp_loopback_is_host_owned_process_authority() {
    let allowed = CapabilityPolicy {
        process_sandbox: ProcessSandboxPolicy {
            allow_tcp_loopback: true,
            ..ProcessSandboxPolicy::default()
        },
        ..CapabilityPolicy::default()
    };
    let denied = CapabilityPolicy::default();

    assert!(
        allowed
            .intersect(&allowed)
            .expect("matching loopback grants intersect")
            .process_sandbox
            .allow_tcp_loopback
    );
    assert!(
        allowed
            .intersect(&denied)
            .expect("a nested policy cannot erase host loopback authority")
            .process_sandbox
            .allow_tcp_loopback
    );
    assert!(
        !denied
            .intersect(&allowed)
            .expect("a nested policy cannot invent loopback authority")
            .process_sandbox
            .allow_tcp_loopback
    );
    denied
        .assert_within_ceiling(&allowed)
        .expect_err("a flattened stage cannot invent loopback authority");
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

fn authority_policy(operation: &str) -> CapabilityPolicy {
    let mut capabilities = BTreeMap::new();
    capabilities.insert("authority".to_string(), vec![operation.to_string()]);
    CapabilityPolicy {
        capabilities,
        ..CapabilityPolicy::default()
    }
}

#[test]
fn broad_authority_write_covers_scoped_delegation_and_flattening() {
    let ceiling = authority_policy("write");
    let scoped = authority_policy("write@plan_admission");

    let merged = ceiling
        .intersect(&scoped)
        .expect("broad authority ceiling covers a scoped request");
    assert_eq!(
        merged.capability_operations("authority"),
        Some(["write@plan_admission".to_string()].as_slice())
    );
    ceiling
        .assert_within_ceiling(&scoped)
        .expect("flattened scoped authority remains within the broad ceiling");

    let sibling = authority_policy("write@native_approval");
    scoped
        .intersect(&sibling)
        .expect_err("a scoped grant must not authorize a sibling authority kind");
    assert!(scoped.assert_within_ceiling(&sibling).is_err());
}

// ---- the default denylist is data, and the data is actually there ----------

/// A non-null control on the parse.
///
/// `default_read_deny_home_paths()` reads a TOML file. Every failure mode of
/// that read — a renamed table, a retyped array, a file that stopped being
/// included — produces an EMPTY list, and an empty denylist denies nothing
/// while every other signature of a working feature stays intact: the field
/// exists, the profile renders, the policy composes, and every test that does
/// not inspect the contents still passes. So the list is asserted to be
/// non-empty, to clear a floor, and to contain specific known members. A parse
/// that silently produced nothing would fail here instead of shipping.
#[test]
fn the_default_denylist_parses_to_real_entries() {
    let defaults = super::default_read_deny_home_paths();

    assert!(
        defaults.len() >= 12,
        "the default denylist parsed to {} entries; a shrunken or empty parse denies \
         credentials nothing and looks exactly like a working one: {defaults:?}",
        defaults.len()
    );
    for required in [
        ".ssh",
        ".aws",
        ".gnupg",
        ".netrc",
        ".docker/config.json",
        ".config/gh/hosts.yml",
        ".config/gcloud",
    ] {
        assert!(
            defaults.iter().any(|entry| entry == required),
            "`{required}` must be denied by default: {defaults:?}"
        );
    }
    assert!(
        defaults.iter().all(|entry| !entry.starts_with('/')),
        "entries are home-relative; an absolute path here would be resolved against the wrong \
         root: {defaults:?}"
    );
}

/// The denial term is the one field that must UNION as it nests. Every other
/// axis narrows, and narrowing a denial would widen the resulting authority.
#[test]
fn a_nested_policy_may_add_a_denial_and_may_never_drop_one() {
    let outer = ProcessSandboxPolicy {
        read_deny_roots: vec!["/outer-secret".to_string()],
        ..ProcessSandboxPolicy::default()
    };
    let inner = ProcessSandboxPolicy {
        read_deny_roots: vec!["/inner-secret".to_string()],
        ..ProcessSandboxPolicy::default()
    };

    let nested = outer.intersect(&inner);

    assert!(
        nested
            .read_deny_roots
            .contains(&"/outer-secret".to_string()),
        "a nested request must not be able to drop an outer denial: {nested:?}"
    );
    assert!(
        nested
            .read_deny_roots
            .contains(&"/inner-secret".to_string()),
        "a nested request may add its own denial: {nested:?}"
    );
}
