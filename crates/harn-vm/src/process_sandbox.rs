//! Public re-exports of the platform-specific process sandbox primitives.
//!
//! Embedders that spawn subprocesses on behalf of Harn scripts (today: the
//! `harn-hostlib` deterministic-tool builtins) must funnel every spawn
//! through these helpers so the active orchestration capability policy is
//! enforced — Linux seccomp/landlock filters via `pre_exec`, macOS
//! `sandbox-exec` wrapping, Windows AppContainer + Job Object launches
//! through `command_output`, plus workspace-root cwd enforcement.
//!
//! The same surface also exposes [`check_fs_path_scope`] so embedders that
//! resolve host *paths* on behalf of Harn scripts (the `harn-hostlib`
//! `fs/*`, `tools/*`, and `ast/*` builtins) can enforce the active policy's
//! workspace-root scope without depending on `VmError`.
//!
//! The helpers themselves live next to the rest of the sandbox state in
//! [`crate::stdlib::sandbox`]. This module exists so external crates have a
//! stable, documented surface to depend on without reaching into
//! `stdlib::*` plumbing.

pub use crate::stdlib::sandbox::{
    active_backend_available, active_backend_name, active_workspace_process_env,
    apply_active_rustc_wrapper_policy, check_fs_path_scope, command_output,
    deterministic_message_locale_env, enforce_process_cwd, process_spawn_error,
    process_violation_error, push_process_sandbox_scope, render_policy_root, std_command_for,
    tokio_command_for, FsAccess, ProcessCommandConfig, ProcessSandboxScope,
    ProcessSandboxScopeGuard, SandboxMechanism, SandboxMechanismAvailability,
    SandboxMechanismUnavailable, SandboxRequirement, SandboxViolation, MESSAGE_LOCALE_OVERRIDE_ENV,
};

/// Push a transient execution policy with `sandbox_profile` replaced by the
/// requested profile. The returned guard restores the surrounding policy on
/// drop.
///
/// This is the single per-process override seam used by both the typed
/// `HarnessProcess` capability and hostlib command tools. It deliberately
/// changes only the profile: workspace roots, capability ceilings, and every
/// other policy field continue to come from the surrounding execution.
pub fn push_sandbox_profile_override(
    value: &str,
) -> Result<SandboxProfileOverrideGuard, crate::VmError> {
    let profile = crate::orchestration::SandboxProfile::parse(value).ok_or_else(|| {
        let expected = crate::orchestration::SandboxProfile::all()
            .iter()
            .map(|profile| format!("{:?}", profile.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        crate::VmError::Thrown(crate::VmValue::String(arcstr::ArcStr::from(format!(
            "unknown sandbox_profile {value:?}; expected one of {expected}"
        ))))
    })?;
    let mut policy = crate::orchestration::current_execution_policy().unwrap_or_default();
    policy.sandbox_profile = profile;
    crate::orchestration::push_execution_policy(policy);
    Ok(SandboxProfileOverrideGuard {
        _private: std::marker::PhantomData,
    })
}

/// Restores the execution policy active before
/// [`push_sandbox_profile_override`] when dropped.
#[must_use = "dropping the guard immediately restores the surrounding sandbox profile"]
#[derive(Debug)]
pub struct SandboxProfileOverrideGuard {
    _private: std::marker::PhantomData<*const ()>,
}

impl Drop for SandboxProfileOverrideGuard {
    fn drop(&mut self) {
        crate::orchestration::pop_execution_policy();
    }
}

/// Recognize an OS-sandbox wrapper that started but could not exec the
/// requested program. Inline-confinement backends return `None`; macOS uses
/// this after `sandbox-exec` exits so callers do not mistake its status for the
/// requested program's exit status.
pub fn wrapped_spawn_io_error(exit_code: i32, stderr: &[u8]) -> Option<std::io::Error> {
    crate::stdlib::sandbox::active_sandbox_policy()?;
    if !active_backend_available() {
        return None;
    }
    #[cfg(target_os = "macos")]
    return macos_wrapped_spawn_io_error(exit_code, stderr);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (exit_code, stderr);
        None
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_wrapped_spawn_io_error(
    exit_code: i32,
    stderr: &[u8],
) -> Option<std::io::Error> {
    const EX_OSERR: i32 = 71;
    const PREFIX: &str = "sandbox-exec: execvp() of '";
    const SUFFIX: &str = "' failed: No such file or directory\n";

    let stderr = std::str::from_utf8(stderr).ok()?;
    if exit_code == EX_OSERR && stderr.starts_with(PREFIX) && stderr.ends_with(SUFFIX) {
        return Some(std::io::Error::from_raw_os_error(libc::ENOENT));
    }
    None
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::macos_wrapped_spawn_io_error;

    #[test]
    fn wrapper_missing_program_is_typed_not_found() {
        let error = macos_wrapped_spawn_io_error(
            71,
            b"sandbox-exec: execvp() of 'missing-command' failed: No such file or directory\n",
        )
        .expect("sandbox wrapper failure");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn genuine_exit_71_is_not_a_wrapper_failure() {
        assert!(macos_wrapped_spawn_io_error(71, b"").is_none());
        assert!(macos_wrapped_spawn_io_error(71, b"application failed\n").is_none());
    }
}

#[cfg(test)]
mod profile_override_tests {
    use crate::orchestration::{
        current_execution_policy, pop_execution_policy, push_execution_policy, CapabilityPolicy,
        SandboxProfile,
    };

    use super::push_sandbox_profile_override;

    #[test]
    fn per_call_override_changes_only_the_profile_and_restores_the_parent() {
        let parent = CapabilityPolicy {
            sandbox_profile: SandboxProfile::OsHardened,
            workspace_roots: vec!["/workspace".to_string()],
            tools: vec!["run_command".to_string()],
            ..CapabilityPolicy::default()
        };
        push_execution_policy(parent.clone());

        {
            let _guard = push_sandbox_profile_override("workspace_paths")
                .expect("workspace_paths is a supported profile");
            let current = current_execution_policy().expect("override policy");
            assert_eq!(current.sandbox_profile, SandboxProfile::WorkspacePaths);
            assert_eq!(current.workspace_roots, parent.workspace_roots);
            assert_eq!(current.tools, parent.tools);
        }

        assert_eq!(current_execution_policy().expect("restored parent"), parent);
        pop_execution_policy();
    }

    #[test]
    fn per_call_override_rejects_unknown_profiles_without_mutating_policy() {
        let parent = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            ..CapabilityPolicy::default()
        };
        push_execution_policy(parent.clone());

        let error = push_sandbox_profile_override("wide_open")
            .expect_err("unknown profiles must fail closed");
        assert!(error.to_string().contains("unknown sandbox_profile"));
        assert_eq!(
            current_execution_policy().expect("unchanged parent"),
            parent
        );
        pop_execution_policy();
    }
}
