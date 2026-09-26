//! Turning a prepared spawn into the command that will actually run.
//!
//! One place reads [`PrepareOutcome`] and nothing else does. The variants
//! differ in who the confinement belongs to -- the child itself, a wrapper
//! this process confines, or a wrapper that must build a namespace before it
//! can be confined at all -- and keeping that reading in one module is what
//! stops a new variant from being handled on one spawn path and forgotten on
//! the other.

use std::process::Command;

use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::VmError;

use super::backend::{PrepareOutcome, SandboxBackend};
use super::ensure_spawn_enforceable;
#[cfg(target_os = "linux")]
use super::linux;

pub(crate) fn build_std_command<B: SandboxBackend + ?Sized>(
    program: &str,
    args: &[String],
    policy: &CapabilityPolicy,
    profile: SandboxProfile,
) -> Result<Command, VmError> {
    ensure_spawn_enforceable::<B>(policy)?;
    let mut command = Command::new(program);
    command.args(args);
    match B::prepare_std_command(program, args, &mut command, policy, profile)? {
        PrepareOutcome::Direct => Ok(command),
        PrepareOutcome::WrappedExec { wrapper, args } => {
            let mut wrapped = Command::new(wrapper);
            wrapped.args(args);
            Ok(wrapped)
        }
        #[cfg(target_os = "linux")]
        PrepareOutcome::NamespacedExec {
            wrapper,
            args,
            confinement,
        } => {
            let mut wrapped = Command::new(wrapper);
            wrapped.args(args);
            linux::keep_ruleset_across_exec(&mut wrapped, confinement);
            Ok(wrapped)
        }
    }
}

pub(crate) fn build_tokio_command<B: SandboxBackend + ?Sized>(
    program: &str,
    args: &[String],
    policy: &CapabilityPolicy,
    profile: SandboxProfile,
) -> Result<tokio::process::Command, VmError> {
    ensure_spawn_enforceable::<B>(policy)?;
    let mut command = tokio::process::Command::new(program);
    command.args(args);
    match B::prepare_tokio_command(program, args, &mut command, policy, profile)? {
        PrepareOutcome::Direct => Ok(command),
        PrepareOutcome::WrappedExec { wrapper, args } => {
            let mut wrapped = tokio::process::Command::new(wrapper);
            wrapped.args(args);
            Ok(wrapped)
        }
        #[cfg(target_os = "linux")]
        PrepareOutcome::NamespacedExec {
            wrapper,
            args,
            confinement,
        } => {
            let mut wrapped = tokio::process::Command::new(wrapper);
            wrapped.args(args);
            linux::keep_ruleset_across_exec_tokio(&mut wrapped, confinement);
            Ok(wrapped)
        }
    }
}
