//! One host-support decision for unit and integration product-path tests.

#[cfg(target_os = "macos")]
pub fn unavailable_worktree_sandbox(
) -> Option<harn_vm::process_sandbox::SandboxMechanismUnavailable> {
    let status =
        harn_vm::value::json::vm_value_to_json(&harn_vm::process_sandbox::host_confinement());
    worktree_sandbox_for_host(
        status,
        &std::env::var("NEXTEST_PROFILE").unwrap_or_default(),
    )
}

#[cfg(target_os = "macos")]
pub fn native_qualification_profile() -> &'static str {
    #[derive(serde::Deserialize)]
    struct Policy {
        profile: String,
    }
    static PROFILE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let policy: Policy = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../.config/native-qualification.json"
        )))
        .expect("checked native qualification policy");
        assert!(
            !policy.profile.is_empty(),
            "qualification profile must be named"
        );
        policy.profile
    });
    PROFILE.as_str()
}

#[cfg(target_os = "macos")]
pub fn worktree_sandbox_for_host(
    status: serde_json::Value,
    profile: &str,
) -> Option<harn_vm::process_sandbox::SandboxMechanismUnavailable> {
    use harn_vm::process_sandbox::{SandboxMechanismUnavailable, SandboxRequirement};
    let available = status["confines_processes"]
        .as_bool()
        .expect("measured host confinement");
    assert!(
        profile != native_qualification_profile() || available,
        "native qualification requires actual process confinement: {status}"
    );
    if available {
        return None;
    }
    let mut refusal: SandboxMechanismUnavailable =
        serde_json::from_value(status["os_hardened_refusal"]["sandbox_mechanism"].clone())
            .expect("an unsupported host must report its typed refusal");
    refusal.profile = harn_vm::orchestration::SandboxProfile::Worktree;
    refusal.requirement = SandboxRequirement::Fallback;
    Some(refusal)
}

/// Supported hosts must execute the command; compatibility hosts report its exact refusal.
pub fn assert_run_process_output(outcome: &crate::commands::run::RunOutcome, expected: &str) {
    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    #[cfg(target_os = "macos")]
    if let Some(refusal) = unavailable_worktree_sandbox() {
        let actual: serde_json::Value = serde_json::from_str(outcome.stdout.trim())
            .expect("unsupported host must report the actual caught process refusal");
        assert_eq!(
            actual,
            serde_json::json!({"process_refusal":
                harn_vm::value::json::vm_value_to_json(&refusal.thrown_value())
            }),
            "a different failure or child output is not a sandbox refusal"
        );
        return;
    }
    assert_eq!(outcome.stdout.trim(), expected);
}
