//! Final environment validation at the existing launch boundaries. Policy
//! composition belongs to security; this adapter supplies the exec view.

use std::process::Command;

use crate::orchestration::CapabilityPolicy;
use crate::security::ProcessEnvironmentBoundary;
use crate::VmError;

pub fn validate_command_environment(command: &mut Command, closed: bool) -> Result<(), VmError> {
    let active = super::active_sandbox_policy();
    validate_for_policy(command, closed, active.as_ref().map(|(policy, _)| policy))
}

pub(super) fn validate_for_policy(
    command: &mut Command,
    closed: bool,
    policy: Option<&CapabilityPolicy>,
) -> Result<(), VmError> {
    let boundary = boundary(policy)?;
    if boundary == ProcessEnvironmentBoundary::Payload {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    reapply_allocator_tuning(command, closed)?;
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

/// Move allocator tuning out of a bubblewrap launch's own environment and into
/// the options bubblewrap applies to its payload.
///
/// Only a launch that carries the payload options descriptor can do this; any
/// other trusted setup (the namespace helper) keeps the tuning in its
/// environment, and the validator then refuses it as before. The descriptor
/// holds what earlier calls moved, because a launch can be validated more than
/// once and the first call already removed the tuning from the environment.
#[cfg(target_os = "linux")]
pub(super) fn reapply_allocator_tuning(command: &mut Command, closed: bool) -> Result<(), VmError> {
    use std::collections::BTreeMap;
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStrExt;

    let Some(descriptor) = payload_env_descriptor(command) else {
        return Ok(());
    };
    let mut moved: BTreeMap<OsString, OsString> =
        read_setenv_args(descriptor).map_err(|error| payload_env_error(&error))?;
    let explicit: BTreeMap<OsString, Option<OsString>> = command
        .get_envs()
        .filter(|(name, _)| crate::security::is_reapplied_allocator_tuning(name))
        .map(|(name, value)| (name.to_owned(), value.map(ToOwned::to_owned)))
        .collect();
    let inherited = (!closed)
        .then(std::env::vars_os)
        .into_iter()
        .flatten()
        .filter(|(name, _)| {
            crate::security::is_reapplied_allocator_tuning(name) && !explicit.contains_key(name)
        });
    let present: Vec<(OsString, OsString)> = explicit
        .iter()
        .filter_map(|(name, value)| value.clone().map(|value| (name.clone(), value)))
        .chain(inherited)
        .filter(|(_, value)| !value.is_empty())
        .collect();
    if present.is_empty() {
        return Ok(());
    }
    for (name, value) in present {
        command.env_remove(&name);
        moved.insert(name, value);
    }
    let mut options = Vec::new();
    for (name, value) in &moved {
        for part in [OsStr::new("--setenv"), name.as_os_str(), value.as_os_str()] {
            options.extend_from_slice(part.as_bytes());
            options.push(0);
        }
    }
    write_options(descriptor, &options).map_err(|error| payload_env_error(&error))?;
    let mut metadata = std::collections::BTreeMap::new();
    metadata.insert(
        "variables".to_string(),
        serde_json::json!(moved
            .keys()
            .map(|name| name.to_string_lossy())
            .collect::<Vec<_>>()),
    );
    crate::events::log_info_meta(
        "process_sandbox_environment",
        "allocator tuning was kept out of the sandbox setup and re-applied to the confined child",
        metadata,
    );
    Ok(())
}

/// The payload options descriptor named before the payload separator.
#[cfg(target_os = "linux")]
fn payload_env_descriptor(command: &Command) -> Option<std::os::fd::RawFd> {
    let mut args = command.get_args().take_while(|arg| *arg != "--");
    while let Some(arg) = args.next() {
        if arg == super::linux::bwrap::PAYLOAD_ENV_ARGS_FLAG {
            return args.next()?.to_str()?.parse().ok();
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn with_descriptor<T>(
    descriptor: std::os::fd::RawFd,
    use_file: impl FnOnce(&std::fs::File) -> std::io::Result<T>,
) -> std::io::Result<T> {
    use std::os::fd::FromRawFd;
    // Borrowed: the launch's descriptor transfer owns and closes it.
    let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(descriptor) });
    use_file(&file)
}

/// Positional reads and writes leave the descriptor's offset at zero, which is
/// where bubblewrap starts reading.
#[cfg(target_os = "linux")]
fn read_setenv_args(
    descriptor: std::os::fd::RawFd,
) -> std::io::Result<std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>> {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::FileExt;
    let bytes = with_descriptor(descriptor, |file| {
        let mut bytes = vec![0; file.metadata()?.len() as usize];
        file.read_exact_at(&mut bytes, 0)?;
        Ok(bytes)
    })?;
    let mut parts = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| std::ffi::OsString::from_vec(part.to_vec()));
    let mut options = std::collections::BTreeMap::new();
    while let (Some(_flag), Some(name), Some(value)) = (parts.next(), parts.next(), parts.next()) {
        options.insert(name, value);
    }
    Ok(options)
}

#[cfg(target_os = "linux")]
fn write_options(descriptor: std::os::fd::RawFd, options: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    with_descriptor(descriptor, |file| {
        file.set_len(0)?;
        file.write_all_at(options, 0)
    })
}

#[cfg(target_os = "linux")]
fn payload_env_error(error: &std::io::Error) -> VmError {
    super::sandbox_rejection(format!(
        "could not hand allocator tuning to the confined child: {error}"
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    fn launch_with_descriptor() -> (Command, std::os::fd::OwnedFd) {
        let descriptor = super::super::linux::bwrap::payload_env_args().unwrap();
        let mut command = Command::new("bwrap");
        command.args([
            super::super::linux::bwrap::PAYLOAD_ENV_ARGS_FLAG.to_string(),
            descriptor.as_raw_fd().to_string(),
            "--".to_string(),
            "payload".to_string(),
            "--args".to_string(),
            "99".to_string(),
        ]);
        (command, descriptor)
    }

    fn reapplied(descriptor: &std::os::fd::OwnedFd) -> Vec<(String, String)> {
        read_setenv_args(descriptor.as_raw_fd())
            .unwrap()
            .into_iter()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn allocator_tuning_moves_to_the_payload_and_survives_revalidation() {
        let (mut command, descriptor) = launch_with_descriptor();
        command
            .env_clear()
            .env("MALLOC_ARENA_MAX", "2")
            .env("MALLOC_CHECK_", "3")
            .env("LD_PRELOAD", "evil.so");
        reapply_allocator_tuning(&mut command, true).unwrap();
        let env: Vec<_> = command
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(ToOwned::to_owned)))
            .collect();
        // Bubblewrap itself no longer carries it; the payload options do.
        assert!(!env
            .iter()
            .any(|(name, value)| name == "MALLOC_ARENA_MAX" && value.is_some()));
        assert_eq!(
            reapplied(&descriptor),
            [("MALLOC_ARENA_MAX".into(), "2".into())]
        );
        // Negative control: behavior-changing controls are left for the
        // validator to refuse, not moved.
        assert!(env.contains(&("MALLOC_CHECK_".into(), Some("3".into()))));
        assert!(env.contains(&("LD_PRELOAD".into(), Some("evil.so".into()))));

        // A second validation of the same launch keeps what the first moved.
        reapply_allocator_tuning(&mut command, true).unwrap();
        assert_eq!(
            reapplied(&descriptor),
            [("MALLOC_ARENA_MAX".into(), "2".into())]
        );
    }

    #[test]
    fn a_launch_without_the_payload_descriptor_is_left_to_the_validator() {
        let mut command = Command::new("helper");
        command
            .args(["--", "payload", "--args", "3"])
            .env_clear()
            .env("MALLOC_ARENA_MAX", "2");
        reapply_allocator_tuning(&mut command, true).unwrap();
        assert!(command
            .get_envs()
            .any(|(name, value)| name == "MALLOC_ARENA_MAX" && value.is_some()));
    }
}
