// Shared test-only host expectations. Each including module binds `vm` to
// harn-vm (or `crate` inside the VM's own unit tests).
use std::sync::LazyLock;

use self::vm::process_sandbox::{SandboxMechanism, SandboxMechanismUnavailable};

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
    let refusal: SandboxMechanismUnavailable =
        serde_json::from_value(status["os_hardened_refusal"]["sandbox_mechanism"].clone())
            .expect("an unsupported host must report its typed refusal");
    // Hardened dimension gaps do not belong to the Worktree fallback refusal;
    // the host-protection gap is a host fact and does.
    Some(SandboxMechanismUnavailable {
        host_protection_gap: refusal.host_protection_gap,
        ..SandboxMechanismUnavailable::new(
            refusal.mechanism,
            refusal.availability,
            vm::orchestration::SandboxProfile::Worktree,
        )
    })
}

pub fn unavailable_worktree_sandbox() -> Option<SandboxMechanismUnavailable> {
    let status = vm::value::json::vm_value_to_json(&vm::process_sandbox::host_confinement());
    worktree_sandbox_for_host(
        status,
        &std::env::var("NEXTEST_PROFILE").unwrap_or_default(),
    )
}

/// The built-in Worktree fallback warns on an unconfined backend. A backend
/// that cannot provide its configured mechanism still refuses. Explicit
/// Enforce checks use `unavailable_worktree_sandbox` instead.
#[allow(dead_code)] // This support module also serves suites that require enforcement.
pub fn unavailable_default_warn_worktree_sandbox() -> Option<SandboxMechanismUnavailable> {
    unavailable_worktree_sandbox()
        .filter(|refusal| refusal.mechanism != SandboxMechanism::Unconfined)
}

/// Invoke only after the actual sandboxed operation. An unsupported host must
/// return this exact typed refusal, never a successful or unrelated result.
#[allow(dead_code)] // Shared with CLI suites that assert their command-level outcome instead.
pub fn assert_unavailable_result<T>(result: &Result<T, vm::VmError>) -> bool {
    let Some(expected) = unavailable_worktree_sandbox() else {
        return false;
    };
    match result {
        Err(vm::VmError::SandboxMechanismUnavailable(actual)) => {
            assert_eq!(actual.as_ref(), &expected);
        }
        Err(error) => panic!("expected measured sandbox refusal {expected}, got {error}"),
        Ok(_) => panic!("unsupported host executed a command instead of refusing {expected}"),
    }
    true
}

/// The declaration that this runner class must enforce Landlock. One CI
/// setting governs every Linux boundary test that reads it through here.
pub const REQUIRE_LANDLOCK_ENV: &str = "HARN_REQUIRE_LANDLOCK_TESTS";

/// Whether the runner declares Landlock enforcement required. Read from the
/// real process environment: it is a fact the CI job states about its own
/// runner, not a selector a test injects.
#[allow(dead_code)] // Only the Linux boundary suites call this.
pub fn landlock_enforcement_required() -> bool {
    std::env::var(REQUIRE_LANDLOCK_ENV).is_ok_and(|value| {
        let value = value.trim().to_ascii_lowercase();
        !matches!(value.as_str(), "" | "0" | "false" | "off" | "no")
    })
}

/// Report that `test` could not measure its claim on this host because
/// `reason`. A runner that declares Landlock enforcement must be able to
/// measure every boundary, so there this fails instead of passing vacuously.
#[allow(dead_code)] // Only the Linux boundary suites call this.
pub fn boundary_not_measured(test: &str, reason: &str) {
    assert!(
        !landlock_enforcement_required(),
        "[{test}] {reason}; {REQUIRE_LANDLOCK_ENV} declares this host must measure it"
    );
    eprintln!("[{test}] not_measured: {reason}");
}

/// Whether Unix permission bits bind this process. A privileged runner (root,
/// or any holder of `CAP_DAC_OVERRIDE`) opens a mode-0 file anyway, so a test
/// whose fixture is "a path this process cannot open" has nothing to build
/// there and must say so instead of reporting a product defect.
#[cfg(unix)]
#[allow(dead_code)] // Only the permission-fixture suites call this.
pub fn file_modes_bind_this_process() -> bool {
    static BINDS: LazyLock<bool> = LazyLock::new(|| {
        use std::os::unix::fs::PermissionsExt;
        let probe =
            std::env::temp_dir().join(format!("harn-file-mode-probe-{}", std::process::id()));
        // A crashed earlier run may have left a mode-0 probe under this pid;
        // unlinking needs only directory write access.
        let _ = std::fs::remove_file(&probe);
        std::fs::write(&probe, b"").expect("write file-mode probe");
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o000))
            .expect("lock file-mode probe");
        let binds = std::fs::File::open(&probe).is_err();
        let _ = std::fs::remove_file(&probe);
        binds
    });
    *BINDS
}
