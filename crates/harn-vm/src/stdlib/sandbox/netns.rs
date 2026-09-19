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
use std::path::PathBuf;
use std::process::Command;

use crate::orchestration::CapabilityPolicy;
use crate::process_sandbox::{NETNS_LAUNCH_SUBCOMMAND, NETNS_RULESET_FD_FLAG, NETNS_SECCOMP_FLAG};
use crate::VmError;

use super::backend::PrepareOutcome;
use super::linux::{LandlockProfile, ProcessProfile, TransferableConfinement};
use super::refusal::sandbox_rejection;

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
    let confinement = TransferableConfinement {
        ruleset: prep.landlock.take().map(LandlockProfile::take_ruleset),
        seccomp: std::mem::take(&mut prep.seccomp),
    };
    let argv = namespaced_launcher_argv(program, args, &confinement);
    PrepareOutcome::NamespacedExec {
        wrapper: launcher.display().to_string(),
        args: argv,
        confinement,
    }
}

/// The helper this spawn must go through, or `None` when it needs none.
///
/// `Ok(None)` means loopback was not requested. It never means loopback was
/// requested and the helper was missing: that is an error and is returned as
/// one, naming the path that was looked for, because the alternative grants
/// this backend could reach instead all leak datagram egress and a reader of
/// the receipt could not tell which one had been applied.
pub(super) fn resolve_netns_launcher(
    policy: &CapabilityPolicy,
) -> Result<Option<PathBuf>, VmError> {
    if !policy.process_sandbox.allow_tcp_loopback {
        return Ok(None);
    }
    let Some(path) = policy.process_sandbox.netns_launcher_path.as_ref() else {
        return Err(sandbox_rejection(
            "TCP loopback-only child networking needs a private network namespace, which only \
             the namespace helper can build; no helper path was supplied, so the grant is \
             refused rather than widened"
                .to_string(),
        ));
    };
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(sandbox_rejection(format!(
            "the namespace helper path must be absolute so the host policy grant names one \
             file; got {}",
            path.display()
        )));
    }
    if !path.is_file() {
        return Err(sandbox_rejection(format!(
            "TCP loopback-only child networking needs the namespace helper at {}, which is not \
             an existing file on this host; the grant is refused rather than widened",
            path.display()
        )));
    }
    Ok(Some(path))
}

/// Assemble the helper's argv: how to confine, then what to run.
///
/// The filter travels hex-encoded in argv rather than over a pipe, unlike the
/// owner-death handover next to it. That handover carries the payload command,
/// which may contain credentials; a compiled syscall filter is a public fact
/// about the policy and reveals nothing the receipt does not already state, so
/// it does not need the pipe's protection and argv keeps the helper a plain
/// exec with no setup protocol.
pub(super) fn namespaced_launcher_argv(
    payload_program: &str,
    payload_args: &[String],
    confinement: &TransferableConfinement,
) -> Vec<String> {
    let mut argv = vec![NETNS_LAUNCH_SUBCOMMAND.to_string()];
    if let Some(fd) = confinement.ruleset_fd() {
        argv.push(NETNS_RULESET_FD_FLAG.to_string());
        argv.push(fd.to_string());
    }
    argv.push(NETNS_SECCOMP_FLAG.to_string());
    argv.push(hex_encode(&confinement.seccomp_bytes()));
    argv.push("--".to_string());
    argv.push(payload_program.to_string());
    argv.extend(payload_args.iter().cloned());
    argv
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
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
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let raw = text.as_bytes();
    for pair in raw.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16);
        let lo = (pair[1] as char).to_digit(16);
        match (hi, lo) {
            (Some(hi), Some(lo)) => bytes.push(((hi << 4) | lo) as u8),
            _ => {
                return Err(io::Error::other(
                    "transferred seccomp program is not hexadecimal",
                ))
            }
        }
    }
    Ok(bytes)
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
pub(super) fn keep_ruleset_across_exec(
    command: &mut Command,
    confinement: TransferableConfinement,
) {
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
pub(super) fn keep_ruleset_across_exec_tokio(
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
    policy.process_sandbox.allow_tcp_loopback
}
