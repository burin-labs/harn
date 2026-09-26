//! The funnel every child-process command is built through: sandbox
//! confinement, the session environment closing, and the managed egress proxy.

use std::process::Command;

use crate::VmError;

use super::backend::ActiveBackend;
use super::{active_sandbox_policy, build_std_command, build_tokio_command, process_output};

/// Close a freshly built command's environment under an active session policy:
/// the choke point that makes the environment contract structural, since every
/// spawn seam in the VM and `harn-hostlib` reaches a child through the three
/// funnel fns below. Callers still layer `env`/`env_remove` on top afterward;
/// sandbox confinement sets no env vars, so clearing cannot weaken it.
///
/// Evaluates to whether it closed the environment, because a command's cleared
/// state is not readable back off it: a seam that re-creates the command
/// elsewhere (the process-owner guardian) has to be told.
macro_rules! close_env_for_session {
    ($command:expr, $program:expr) => {
        if let Some(env) =
            crate::stdlib::process::session_closed_env_for_command($program, std::iter::empty())?
        {
            $command.env_clear();
            for (key, value) in env {
                $command.env(key, value);
            }
            true
        } else {
            false
        }
    };
}

pub fn std_command_for(program: &str, args: &[String]) -> Result<Command, VmError> {
    std_command_for_with_env_state(program, args).map(|(command, _)| command)
}

/// [`std_command_for`], also reporting whether the session policy CLEARED the
/// command's environment and rebuilt it from the session's resolved set.
///
/// When it did, `get_envs()` holds that whole set and nothing else may be
/// inherited. A seam that serializes the command to spawn it in another
/// process must carry this flag with it, or the receiving side inherits its
/// own environment behind the explicit entries.
pub fn std_command_for_with_env_state(
    program: &str,
    args: &[String],
) -> Result<(Command, bool), VmError> {
    let resolved_program = crate::stdlib::process::resolve_program_path_for_spawn(program);
    let active = active_sandbox_policy();
    let mut command = match active.as_ref() {
        Some((policy, profile)) => {
            build_std_command::<ActiveBackend>(&resolved_program, args, policy, *profile)?
        }
        None => {
            let mut command = Command::new(&resolved_program);
            command.args(args);
            command
        }
    };
    let env_closed = close_env_for_session!(command, program);
    if let Some(proxy) = active.and_then(|(policy, _)| policy.process_network_proxy) {
        process_output::apply_managed_proxy_env(&mut command, proxy);
    }
    Ok((command, env_closed))
}

/// [`std_command_for_with_env_state`] for a command the caller will launch
/// through [`crate::process_sandbox::spawn_confined`].
///
/// A `Command` cannot carry a restricted token, so building one through the
/// backend reports confinement as unavailable. This builds the same program,
/// arguments and session environment without that report; the confined
/// launch attaches the token itself and refuses what it cannot render.
#[cfg(target_os = "windows")]
pub fn std_command_for_confined_launch(
    program: &str,
    args: &[String],
) -> Result<(Command, bool), VmError> {
    let resolved_program = crate::stdlib::process::resolve_program_path_for_spawn(program);
    let mut command = Command::new(&resolved_program);
    command.args(args);
    let env_closed = close_env_for_session!(command, program);
    Ok((command, env_closed))
}

pub fn tokio_command_for(
    program: &str,
    args: &[String],
) -> Result<tokio::process::Command, VmError> {
    let resolved_program = crate::stdlib::process::resolve_program_path_for_spawn(program);
    let active = active_sandbox_policy();
    let mut command = match active.as_ref() {
        Some((policy, profile)) => {
            build_tokio_command::<ActiveBackend>(&resolved_program, args, policy, *profile)?
        }
        None => {
            let mut command = tokio::process::Command::new(&resolved_program);
            command.args(args);
            command
        }
    };
    close_env_for_session!(command, program);
    if let Some(proxy) = active.and_then(|(policy, _)| policy.process_network_proxy) {
        process_output::apply_managed_proxy_env_tokio(&mut command, proxy);
    }
    Ok(command)
}
