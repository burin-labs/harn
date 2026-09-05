//! Platform backend contract and active-backend introspection.

use std::process::{Command, Output};

use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::value::VmError;

use super::{
    apply_process_config, build_std_command, process_spawn_error, spawn_error, ProcessCommandConfig,
};

/// One platform implementation attaches the active capability ceiling to each
/// child process. Callers use the module-level spawn functions, not this trait.
pub(crate) trait SandboxBackend {
    fn name() -> &'static str;
    fn filesystem_mechanism() -> &'static str;

    /// Filesystem availability is narrower than composite backend availability.
    fn filesystem_available() -> bool {
        Self::available()
    }

    fn available() -> bool;

    fn prepare_std_command(
        program: &str,
        args: &[String],
        command: &mut Command,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError>;

    fn prepare_tokio_command(
        program: &str,
        args: &[String],
        command: &mut tokio::process::Command,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError>;

    fn run_to_output(
        program: &str,
        args: &[String],
        config: &ProcessCommandConfig,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<Output, VmError> {
        let mut command = build_std_command::<Self>(program, args, policy, profile)?;
        apply_process_config(&mut command, config, Some(policy));
        crate::op_interrupt::capture_output_interruptible(&mut command)
            .map_err(|error| process_spawn_error(&error).unwrap_or_else(|| spawn_error(error)))
    }
}

/// Whether a backend prepared the original command or a wrapper invocation.
pub(crate) enum PrepareOutcome {
    Direct,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    WrappedExec {
        wrapper: String,
        args: Vec<String>,
    },
}

#[cfg(target_os = "linux")]
pub(super) type ActiveBackend = super::linux::Backend;
#[cfg(target_os = "macos")]
pub(super) type ActiveBackend = super::macos::Backend;
#[cfg(target_os = "openbsd")]
pub(super) type ActiveBackend = super::openbsd::Backend;
#[cfg(target_os = "windows")]
pub(super) type ActiveBackend = super::windows::Backend;
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
)))]
pub(super) type ActiveBackend = NoopBackend;

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
)))]
struct NoopBackend;

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
)))]
impl SandboxBackend for NoopBackend {
    fn name() -> &'static str {
        "noop"
    }
    fn filesystem_mechanism() -> &'static str {
        "none"
    }
    fn available() -> bool {
        false
    }
    fn prepare_std_command(
        _program: &str,
        _args: &[String],
        _command: &mut Command,
        _policy: &CapabilityPolicy,
        _profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        Ok(PrepareOutcome::Direct)
    }
    fn prepare_tokio_command(
        _program: &str,
        _args: &[String],
        _command: &mut tokio::process::Command,
        _policy: &CapabilityPolicy,
        _profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        Ok(PrepareOutcome::Direct)
    }
}

pub fn active_backend_name() -> &'static str {
    ActiveBackend::name()
}

pub fn active_backend_filesystem_mechanism() -> &'static str {
    ActiveBackend::filesystem_mechanism()
}

pub fn active_backend_filesystem_available() -> bool {
    ActiveBackend::filesystem_available()
}

pub fn active_backend_available() -> bool {
    ActiveBackend::available()
}
