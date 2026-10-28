//! The namespace handover: everything that crosses from this process into the
//! helper that builds a private network namespace for a confined child.
//!
//! Split from the backend because it is one concern with one shape. The
//! backend decides that a child needs loopback; this module decides whether
//! that is possible, what the helper is called with, and how the confinement
//! survives the `exec` in between. Its pieces are meaningless apart: the
//! resolver's refusal is what makes the widened syscall filter safe, and the
//! descriptor hook is what makes the handover real rather than reported.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::orchestration::CapabilityPolicy;
use crate::process_sandbox::{
    NETNS_LAUNCH_SUBCOMMAND, NETNS_PRIVATE_TMP_FLAG, NETNS_RULESET_FD_FLAG, NETNS_SECCOMP_FLAG,
};
use crate::stdlib::sandbox::policy::ProcessNetworkGrant;
use crate::VmError;

use super::{sandbox_rejection, ProcessProfile, TransferableConfinement};
use crate::stdlib::sandbox::backend::PrepareOutcome;

/// Who enters a transferred Linux confinement before the payload starts.
pub enum ReexecConfinement {
    /// The supervisor enters confinement immediately before executing the payload.
    BeforeExec(TransferableConfinement),
    /// The supervisor passes the ruleset to the helper without entering it.
    AfterNamespace(TransferableConfinement),
    /// Bubblewrap installs the filter after mounts. The wrapper argv names
    /// these pinned filter and mount descriptors, which must survive reexec.
    Bubblewrap(super::DescriptorTransfer),
}

/// Turn a prepared profile into the helper invocation that will enter it.
///
/// The profile is consumed rather than borrowed: the ruleset descriptor has to
/// outlive this call and be inherited by the helper, so ownership moves into
/// the confinement that the spawn keeps alive.
pub(super) fn namespaced_outcome(
    launcher: PathBuf,
    program: &str,
    args: &[String],
    prep: &mut ProcessProfile,
) -> PrepareOutcome {
    let confinement = TransferableConfinement::take_from(prep);
    let argv = launcher_argv_with_ruleset(program, args, &confinement, confinement.ruleset_fd());
    PrepareOutcome::NamespacedExec {
        wrapper: launcher.display().to_string(),
        args: argv,
        confinement,
    }
}

/// The helper this spawn must go through, or `None` when it needs none.
///
/// `Ok(None)` means loopback was not requested. The spawn path withholds a grant
/// the host cannot honor before it gets here (`withhold_unsupported_loopback`),
/// so reaching the error means a caller bypassed that rule. It never means loopback was
/// requested and the helper was missing: that is an error and is returned as
/// one, naming the path that was looked for, because the alternative grants
/// this backend could reach instead all leak datagram egress and a reader of
/// the receipt could not tell which one had been applied.
pub(in crate::stdlib::sandbox) fn resolve_netns_launcher(
    policy: &CapabilityPolicy,
) -> Result<Option<PathBuf>, VmError> {
    if !ProcessNetworkGrant::for_policy(policy).loopback_only() {
        return Ok(None);
    }
    match crate::process_sandbox::loopback_support(&policy.process_sandbox) {
        crate::process_sandbox::LoopbackSupport::Supported { launcher } => Ok(launcher),
        crate::process_sandbox::LoopbackSupport::Unavailable(unavailable) => {
            Err(sandbox_rejection(format!(
                "TCP loopback-only child networking is refused rather than widened: {}",
                unavailable.notice()
            )))
        }
    }
}

/// Assemble the helper's argv: how to confine, then what to run.
///
/// The filter travels hex-encoded in argv rather than over a pipe, unlike the
/// owner-death handover next to it. That handover carries the payload command,
/// which may contain credentials; a compiled syscall filter is a public fact
/// about the policy and reveals nothing the receipt does not already state, so
/// it does not need the pipe's protection and argv keeps the helper a plain
/// exec with no setup protocol.
pub(in crate::stdlib::sandbox) fn launcher_argv_with_ruleset(
    payload_program: &str,
    payload_args: &[String],
    confinement: &TransferableConfinement,
    ruleset_fd: Option<i32>,
) -> Vec<String> {
    let mut argv = vec![NETNS_LAUNCH_SUBCOMMAND.to_string()];
    if let Some(fd) = ruleset_fd {
        argv.push(NETNS_RULESET_FD_FLAG.to_string());
        argv.push(fd.to_string());
    }
    if let Some(tmp) = &confinement.private_tmp {
        argv.push(NETNS_PRIVATE_TMP_FLAG.to_string());
        argv.push(tmp.display().to_string());
    }
    argv.push(NETNS_SECCOMP_FLAG.to_string());
    argv.push(hex::encode(confinement.seccomp_bytes()));
    argv.push("--".to_string());
    argv.push(payload_program.to_string());
    argv.extend(payload_args.iter().cloned());
    argv
}

/// The workspace temp dir (`.harn-tmp`, shared by runs in that workspace) the
/// helper mounts over the child's `/tmp`, when this
/// spawn goes through the helper at all.
///
/// `granted` is every path the finished ruleset grants, the payload program
/// and its `PATH` entries included. `None` when any lies under `/tmp`: the
/// mount hides every host path beneath it, so such a child keeps the host's
/// `/tmp` (where it can write nothing) rather than lose what it was granted.
pub(super) fn private_tmp_dir<'a>(
    policy: &CapabilityPolicy,
    mut granted: impl Iterator<Item = &'a Path>,
) -> Option<PathBuf> {
    if !policy.process_sandbox.allow_tcp_loopback || granted.any(|p| p.starts_with("/tmp")) {
        return None;
    }
    super::super::workspace_local_tmpdir(policy)
}

/// Decode the filter the helper was handed. Rejects anything that is not a
/// whole number of bytes rather than silently truncating, because a truncated
/// filter still installs and still reports success while denying the wrong
/// syscalls.
pub fn decode_seccomp_hex(text: &str) -> io::Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return Err(io::Error::other(
            "transferred seccomp program is not a whole number of bytes",
        ));
    }
    hex::decode(text)
        .map_err(|_| io::Error::other("transferred seccomp program is not hexadecimal"))
}

/// Keep the ruleset descriptor open across the helper's `exec`.
///
/// Rust marks every descriptor it opens close-on-exec, which is the right
/// default everywhere else and is fatal here: the helper would be handed a
/// number naming nothing and would enter no ruleset, leaving the payload
/// confined by seccomp alone while every layer above still reported the
/// filesystem boundary as enforced. Clearing the flag in the child, after
/// fork, keeps the parent's own descriptor table untouched.
///
/// The confinement is moved into the closure so the descriptor stays owned,
/// and therefore open, until the spawn is done with it.
pub fn keep_ruleset_across_exec(command: &mut Command, confinement: TransferableConfinement) {
    let Some(hook) = clear_cloexec_hook(confinement) else {
        return;
    };
    // SAFETY: `pre_exec` may only call async-signal-safe functions. `fcntl` is
    // async-signal-safe, and the hook allocates, locks and performs no I/O.
    unsafe {
        command.pre_exec(hook);
    }
}

/// The tokio twin of [`keep_ruleset_across_exec`].
pub(crate) fn keep_ruleset_across_exec_tokio(
    command: &mut tokio::process::Command,
    confinement: TransferableConfinement,
) {
    let Some(hook) = clear_cloexec_hook(confinement) else {
        return;
    };
    // SAFETY: see `keep_ruleset_across_exec`.
    unsafe {
        command.pre_exec(hook);
    }
}

/// `None` when there is no descriptor to carry, which is the no-Landlock host
/// whose resolved fallback lets the run proceed on seccomp alone.
///
/// The confinement is moved into the closure so the descriptor stays owned,
/// and therefore open, for as long as the command can be spawned.
fn clear_cloexec_hook(
    confinement: TransferableConfinement,
) -> Option<impl FnMut() -> io::Result<()> + Send + Sync + 'static> {
    let fd = confinement.ruleset_fd()?;
    Some(move || {
        let _keep_open = &confinement;
        // SAFETY: async-signal-safe; `fd` is owned by the moved confinement.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })
}

/// A loopback grant rendered inside a private network namespace, where the
/// namespace and not the filter is the boundary.
///
/// The child must be able to create and address sockets or the grant it was
/// given is worth nothing: a filter that refuses `socket` refuses loopback
/// exactly as hard as it refuses the internet, and the run reads as a working
/// sandbox while the tool it was opened for still cannot start. What makes
/// admitting them safe is that there is nowhere for them to reach. Inside the
/// namespace the only interface is loopback, so no packet of any protocol has
/// a route off the host, which is the guarantee the filter could not express:
/// its network terms carry no address condition, and they do not mediate
/// datagrams at all.
///
/// Safe only because the two always travel together. A loopback grant with no
/// helper to build the namespace is refused outright by
/// [`resolve_netns_launcher`], on this path as well as on the spawn path, so
/// this predicate cannot be true for a child that is about to run on the host
/// network.
pub(super) fn namespaced_loopback_grant(policy: &CapabilityPolicy) -> bool {
    ProcessNetworkGrant::for_policy(policy).loopback_only()
}
