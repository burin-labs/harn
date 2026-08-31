use super::*;

const WRAPPER_KEYS: [&str; 4] = [
    "RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
];

#[test]
fn sandboxed_process_config_neutralizes_rustc_wrapper() {
    let cwd = std::env::current_dir().unwrap();
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![cwd.to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    };

    // A sandboxed spawn must bypass sccache so it can never spawn (and
    // thereby permanently confine) the shared daemon.
    let resolved = sandboxed_process_config(&ProcessCommandConfig::default(), &policy).unwrap();
    let env: std::collections::BTreeMap<_, _> = resolved.env.into_iter().collect();
    for key in WRAPPER_KEYS {
        assert_eq!(env.get(key).map(String::as_str), Some(""), "{key}");
    }
}

#[test]
fn neutralize_rustc_wrapper_overrides_caller_supplied_wrapper() {
    // Even if a caller (or inherited env) asked for sccache, the sandboxed
    // config forces it off rather than appending a duplicate entry.
    let mut env = vec![
        ("RUSTC_WRAPPER".to_string(), "sccache".to_string()),
        (
            "CARGO_BUILD_RUSTC_WRAPPER".to_string(),
            "cargo-sccache".to_string(),
        ),
        (
            "RUSTC_WORKSPACE_WRAPPER".to_string(),
            "workspace-sccache".to_string(),
        ),
        (
            "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER".to_string(),
            "cargo-workspace-sccache".to_string(),
        ),
        ("PATH".to_string(), "/usr/bin".to_string()),
    ];
    let mut env_remove = vec![
        "rustc_wrapper".to_string(),
        "cargo_build_rustc_wrapper".to_string(),
        "rustc_workspace_wrapper".to_string(),
        "cargo_build_rustc_workspace_wrapper".to_string(),
    ];
    neutralize_rustc_wrapper(&mut env, &mut env_remove);
    let collected: std::collections::BTreeMap<_, _> = env.iter().cloned().collect();
    for key in WRAPPER_KEYS {
        assert_eq!(collected.get(key).map(String::as_str), Some(""), "{key}");
        assert_eq!(
            env.iter().filter(|(existing, _)| existing == key).count(),
            1
        );
    }
    assert_eq!(collected.get("PATH").map(String::as_str), Some("/usr/bin"));
    assert!(
        env_remove.is_empty(),
        "caller removal must not reveal a Cargo-configured wrapper"
    );
}
