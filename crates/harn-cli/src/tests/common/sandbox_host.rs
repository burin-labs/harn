//! Host capability expectations shared by CLI unit and integration product tests.

use std::sync::LazyLock;

use harn_vm::process_sandbox::{SandboxMechanismUnavailable, SandboxRequirement};

/// An unsupported compatibility host must produce a measured refusal; release
/// qualification still requires the positive confinement path.
pub fn unavailable_worktree_sandbox() -> Option<SandboxMechanismUnavailable> {
    let status =
        harn_vm::value::json::vm_value_to_json(&harn_vm::process_sandbox::host_confinement());
    worktree_sandbox_for_host(
        status,
        &std::env::var("NEXTEST_PROFILE").unwrap_or_default(),
    )
}

pub fn native_qualification_profile() -> &'static str {
    #[derive(serde::Deserialize)]
    struct Policy {
        profile: String,
    }
    static PROFILE: LazyLock<String> = LazyLock::new(|| {
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

pub fn worktree_sandbox_for_host(
    status: serde_json::Value,
    profile: &str,
) -> Option<SandboxMechanismUnavailable> {
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

/// Called only after the real command path ran. Returning true means its exact
/// unsupported-host refusal was asserted, never that execution was skipped.
pub fn assert_unavailable_worktree_refusal(outcome: &crate::commands::run::RunOutcome) -> bool {
    let Some(refusal) = unavailable_worktree_sandbox() else {
        return false;
    };
    assert_ne!(
        outcome.exit_code, 0,
        "unsupported host must refuse the command"
    );
    assert!(
        outcome.stderr.contains(&refusal.to_string()),
        "{}",
        outcome.stderr
    );
    assert!(
        outcome.stdout.is_empty(),
        "refused command produced output: {}",
        outcome.stdout
    );
    true
}
