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
    active_backend_available, active_backend_name, active_workspace_tmpdir_env,
    check_fs_path_scope, command_output, deterministic_message_locale_env, enforce_process_cwd,
    process_spawn_error, process_violation_error, push_process_sandbox_scope, render_policy_root,
    std_command_for, tokio_command_for, FsAccess, ProcessCommandConfig, ProcessSandboxScope,
    ProcessSandboxScopeGuard, SandboxViolation, MESSAGE_LOCALE_OVERRIDE_ENV,
};
