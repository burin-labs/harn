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
    check_fs_path_scope, command_output, deterministic_message_locale_env, enforce_process_cwd,
    process_spawn_error, process_violation_error, push_process_sandbox_scope, render_policy_root,
    std_command_for, tokio_command_for, FsAccess, ProcessCommandConfig, ProcessSandboxScope,
    ProcessSandboxScopeGuard, SandboxViolation, MESSAGE_LOCALE_OVERRIDE_ENV,
};

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
