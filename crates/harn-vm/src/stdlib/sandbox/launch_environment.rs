//! Final environment validation at the existing launch boundaries. Policy
//! composition belongs to security; this adapter supplies the exec view.

use std::process::Command;

use crate::orchestration::CapabilityPolicy;
use crate::security::ProcessEnvironmentBoundary;
use crate::VmError;

pub fn validate_command_environment(command: &Command, closed: bool) -> Result<(), VmError> {
    let active = super::active_sandbox_policy();
    validate_for_policy(command, closed, active.as_ref().map(|(policy, _)| policy))
}

pub(super) fn validate_for_policy(
    command: &Command,
    closed: bool,
    policy: Option<&CapabilityPolicy>,
) -> Result<(), VmError> {
    let boundary = boundary(policy)?;
    if boundary == ProcessEnvironmentBoundary::Payload {
        return Ok(());
    }
    crate::security::validate_process_environment(
        boundary,
        (!closed).then(std::env::vars_os).into_iter().flatten(),
        command
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(ToOwned::to_owned))),
    )
    .map_err(|error| super::sandbox_rejection(error.to_string()))
}

fn boundary(policy: Option<&CapabilityPolicy>) -> Result<ProcessEnvironmentBoundary, VmError> {
    #[cfg(target_os = "linux")]
    if let Some(policy) = policy {
        if !super::linux::landlock_available()
            || super::linux::resolve_netns_launcher(policy)?.is_some()
        {
            return Ok(ProcessEnvironmentBoundary::TrustedSetup);
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = policy;
    Ok(ProcessEnvironmentBoundary::Payload)
}
