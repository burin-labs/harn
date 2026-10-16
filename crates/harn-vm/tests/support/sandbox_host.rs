// Shared test-only host expectations. Each including module binds `vm` to
// harn-vm (or `crate` inside the VM's own unit tests).
use std::sync::LazyLock;

use self::vm::process_sandbox::{SandboxMechanismUnavailable, SandboxRequirement};

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
        assert!(!policy.profile.is_empty(), "qualification profile must be named");
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
    refusal.profile = vm::orchestration::SandboxProfile::Worktree;
    refusal.requirement = SandboxRequirement::Fallback;
    Some(refusal)
}

pub fn unavailable_worktree_sandbox() -> Option<SandboxMechanismUnavailable> {
    let status = vm::value::json::vm_value_to_json(&vm::process_sandbox::host_confinement());
    worktree_sandbox_for_host(status, &std::env::var("NEXTEST_PROFILE").unwrap_or_default())
}

/// Invoke only after the actual sandboxed operation. An unsupported host must
/// return this exact typed refusal, never a successful or unrelated result.
#[allow(dead_code)] // Shared with CLI suites that assert their command-level outcome instead.
pub fn assert_unavailable_result<T>(result: &Result<T, vm::VmError>) -> bool {
    let Some(expected) = unavailable_worktree_sandbox() else {
        return false;
    };
    match result {
        Err(vm::VmError::SandboxMechanismUnavailable(actual)) => assert_eq!(actual.as_ref(), &expected),
        Err(error) => panic!("expected measured sandbox refusal {expected}, got {error}"),
        Ok(_) => panic!("unsupported host executed a command instead of refusing {expected}"),
    }
    true
}
