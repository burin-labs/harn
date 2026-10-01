//! The funnel every child-process command is built through: sandbox
//! confinement, the session environment closing, and the managed egress proxy.

use std::process::Command;

use crate::VmError;

use super::backend::ActiveBackend;
use super::{active_sandbox_policy, build_std_command, build_tokio_command, process_output};

/// Close a freshly built command's environment under an active session policy:
/// the choke point that makes the environment contract structural, since every
/// spawn seam in the VM and `harn-hostlib` reaches a child through this module's
/// constructors. Callers still layer `env`/`env_remove` on top afterward;
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

/// A `std` command for `program` whose environment is the session policy's
/// child environment for that program, without sandbox confinement.
///
/// This is the constructor for every child the runtime starts on a session's
/// behalf that is not itself a confined tool call: MCP stdio servers, ACP
/// provider transports, git and other plumbing the runtime runs, pagers,
/// verifier commands. Confined tool calls use [`std_command_for`], which
/// closes the environment the same way. Callers layer their own `env` /
/// `env_remove` afterward; an explicit entry wins over the policy's base.
///
/// With no session policy installed the command inherits the parent
/// environment, exactly as `Command::new` does. With one installed, the
/// child sees the policy's allowlist, the grants that reach this program,
/// and nothing else: never an `in_process` grant, and never an engine
/// variable the policy did not admit.
pub fn session_std_command(program: impl AsRef<std::ffi::OsStr>) -> Result<Command, VmError> {
    let program = program.as_ref();
    // A raw constructor is allowed here and only here: this is the funnel.
    let mut command = Command::new(program);
    close_env_for_session!(command, &program.to_string_lossy());
    Ok(command)
}

/// The Tokio counterpart of [`session_std_command`].
pub fn session_tokio_command(
    program: impl AsRef<std::ffi::OsStr>,
) -> Result<tokio::process::Command, VmError> {
    let program = program.as_ref();
    let mut command = tokio::process::Command::new(program);
    close_env_for_session!(command, &program.to_string_lossy());
    Ok(command)
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
    let command = match active.as_ref() {
        Some((policy, profile)) => {
            build_std_command::<ActiveBackend>(&resolved_program, args, policy, *profile)?
        }
        None => {
            let mut command = Command::new(&resolved_program);
            command.args(args);
            command
        }
    };
    let (mut command, env_closed) = close_std_command_environment(command, program)?;
    if let Some(proxy) = active.and_then(|(policy, _)| policy.process_network_proxy) {
        process_output::apply_managed_proxy_env(&mut command, proxy);
    }
    Ok((command, env_closed))
}

fn close_std_command_environment(
    mut command: Command,
    program: &str,
) -> Result<(Command, bool), VmError> {
    let env_closed = close_env_for_session!(command, program);
    Ok((command, env_closed))
}

/// Prepare a command for an exec-based supervisor. The caller carries the
/// returned ruleset on `ruleset_fd`; a namespace helper enters it only after
/// constructing the network boundary. No callback is lost in the projection.
///
/// The boolean reports whether the session policy cleared the environment.
/// `None` means no confinement is active; an unavailable requested mechanism
/// returns an error. The returned command has no confinement callbacks, so
/// the caller must transfer and apply the returned confinement as directed.
#[cfg(target_os = "linux")]
pub fn command_for_reexec(
    program: &str,
    args: &[String],
    ruleset_fd: i32,
) -> Result<(Command, bool, Option<super::linux::ReexecConfinement>), VmError> {
    use super::linux::{launcher_argv_with_ruleset, resolve_netns_launcher, ReexecConfinement};

    let resolved = crate::stdlib::process::resolve_program_path_for_spawn(program);
    let mut command = Command::new(&resolved);
    command.args(args);
    let confinement = match super::linux::transferable_confinement(program)? {
        None => None,
        Some(confinement) => {
            let (policy, _) = active_sandbox_policy().expect("confinement requires policy");
            if let Some(launcher) = resolve_netns_launcher(&policy)? {
                command = Command::new(launcher);
                command.args(launcher_argv_with_ruleset(
                    &resolved,
                    args,
                    &confinement,
                    confinement.ruleset_fd().map(|_| ruleset_fd),
                ));
                Some(ReexecConfinement::AfterNamespace(confinement))
            } else {
                Some(ReexecConfinement::BeforeExec(confinement))
            }
        }
    };
    let (command, env_closed) = close_std_command_environment(command, program)?;
    Ok((command, env_closed, confinement))
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
