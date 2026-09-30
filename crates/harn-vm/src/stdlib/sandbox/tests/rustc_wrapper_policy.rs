use super::*;

const WRAPPER_KEYS: [&str; 4] = [
    "RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
];

/// A wrapper that cannot run under the profile is switched off, and the
/// decision says which wrapper and why.
#[test]
fn sandboxed_process_config_switches_off_a_wrapper_that_cannot_run() {
    let workspace = tempfile::tempdir().expect("workspace");
    let cwd = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![cwd.to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    };
    let config = ProcessCommandConfig {
        cwd: Some(cwd.clone()),
        env: vec![(
            "RUSTC_WRAPPER".to_string(),
            "/definitely/missing/wrapper".to_string(),
        )],
        ..ProcessCommandConfig::default()
    };

    crate::orchestration::push_execution_policy(policy.clone());
    let resolved = sandboxed_process_config(&config, &policy);
    crate::orchestration::pop_execution_policy();
    let resolved = resolved.unwrap();
    let env: std::collections::BTreeMap<_, _> = resolved.env.into_iter().collect();
    let decision = rustc_wrapper::rustc_wrapper_decision(&policy, &cwd, &config.env);
    assert_eq!(
        decision.disposition,
        rustc_wrapper::RustcWrapperDisposition::Disabled,
        "{decision:?}"
    );
    assert!(decision.disables(), "{decision:?}");
    assert!(
        decision.wrapper.as_deref().is_some_and(|wrapper| wrapper
            .replace('\\', "/")
            .ends_with("/definitely/missing/wrapper")),
        "{decision:?}"
    );
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
    process_config::neutralize_rustc_wrapper(&mut env, &mut env_remove);
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

/// Only a decision that took away a configured wrapper is a warning. Having no
/// wrapper to begin with is the ordinary case and must not reach stderr as one.
#[test]
fn only_a_dropped_configured_wrapper_is_a_warning() {
    use rustc_wrapper::{RustcWrapperDecision, RustcWrapperDisposition};
    let decision = |disposition| RustcWrapperDecision {
        disposition,
        wrapper: None,
        reason: String::new(),
        cwd: String::new(),
    };
    assert!(!decision(RustcWrapperDisposition::NotConfigured).drops_configured_wrapper());
    assert!(!decision(RustcWrapperDisposition::Kept).drops_configured_wrapper());
    assert!(decision(RustcWrapperDisposition::Disabled).drops_configured_wrapper());
    assert!(decision(RustcWrapperDisposition::Unmeasured).drops_configured_wrapper());
}

/// When the wrapper-free build fails too, the reason says why. Without it an
/// `unmeasured` decision cannot tell a broken toolchain from a broken wrapper.
#[test]
fn an_unmeasured_decision_carries_the_wrapper_free_build_error() {
    let workspace = tempfile::tempdir().expect("workspace");
    let cwd = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    // A compiler Cargo cannot run fails both builds, and only the second
    // build's error names it without the wrapper in front of it.
    std::fs::create_dir_all(cwd.join(".cargo")).expect("cargo config dir");
    std::fs::write(
        cwd.join(".cargo").join("config.toml"),
        "[build]\nrustc = \"/definitely/missing/rustc\"\n",
    )
    .expect("cargo config");
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![cwd.to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    };
    let env = vec![(
        "RUSTC_WRAPPER".to_string(),
        "/definitely/missing/wrapper".to_string(),
    )];

    crate::orchestration::push_execution_policy(policy.clone());
    let decision = rustc_wrapper::rustc_wrapper_decision(&policy, &cwd, &env);
    crate::orchestration::pop_execution_policy();
    assert_eq!(
        decision.disposition,
        rustc_wrapper::RustcWrapperDisposition::Unmeasured,
        "{decision:?}"
    );
    let without = decision
        .reason
        .split_once("without it: ")
        .map(|(_, without)| without)
        .unwrap_or_else(|| panic!("the reason must carry the wrapper-free error: {decision:?}"));
    assert!(
        without.contains("/definitely/missing/rustc") && !without.contains("missing/wrapper"),
        "{decision:?}"
    );
}
