//! The namespace helper: the one place a confined child's loopback-only
//! network is actually built.
//!
//! # Order, and why it is the whole design
//!
//! Three things must happen and only one order works.
//!
//! 1. Create the namespaces and raise loopback. This needs `unshare`, which
//!    the sandbox's syscall filter does not admit, so it must happen before
//!    any confinement is installed.
//! 2. Enter the confinement handed over from the parent.
//! 3. Exec the payload.
//!
//! Doing (2) before (1) is what the obvious implementation does and it cannot
//! work: the filter is a default-deny allowlist carrying no namespace calls,
//! so the helper dies at `unshare` with a bare permission error. Doing (3)
//! before (2) runs the payload unconfined while every layer above reports the
//! profile as enforced, which is the silent failure this handover exists to
//! end. So the confinement travels as data, and this process applies it in
//! the window between the namespace existing and the payload starting.
//!
//! A child whose network stops at the proxy also needs a way to reach it. That
//! is a relay forked between (1) and (2); see
//! `super::start_namespace_relay`.
//!
//! # What the namespace buys
//!
//! A private network namespace with only loopback raised is the only
//! mechanism that gives a build tool what it asks for without leaving a hole.
//! Admitting the IP socket family in the filter alone lets a child complete an
//! outbound connection to a public address. Pairing that with the kernel's
//! network access rights at zero permitted ports denies streams in both
//! directions, but those rights scope by port and never by address, so
//! "loopback only" is not expressible, and they do not mediate datagrams at
//! all, so a packet still leaves the host. Inside a private namespace there is
//! no route off the host to deny in the first place.

use std::convert::Infallible;
#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "linux")]
use std::process::Command;

use super::{
    DeviceMountFinalization, EgressRelayTarget, NETNS_CLOSE_FD_FLAG, NETNS_EGRESS_RELAY_FLAG,
    NETNS_LAUNCH_SUBCOMMAND, NETNS_PRIVATE_TMP_FLAG, NETNS_RULESET_FD_FLAG, NETNS_SECCOMP_FLAG,
};

/// Run the helper if this process was started as one, or return `None` so the
/// program carries on.
///
/// Call this first in `main`, before any runtime or thread exists. Creating a
/// user namespace refuses a multi-threaded caller outright, and that refusal
/// is a bare "invalid argument" that reads as a host forbidding namespaces.
/// Any binary that names itself as a policy's namespace launcher must call it.
///
/// The success type is uninhabited on purpose: a launch that worked has
/// replaced this process, so there is no "finished successfully" state for a
/// caller to mishandle. `Some` is always a failure to report.
pub fn run_namespace_helper_if_invoked(raw_args: &[String]) -> Option<Result<Infallible, String>> {
    if raw_args.get(1).map(String::as_str) != Some(NETNS_LAUNCH_SUBCOMMAND) {
        return None;
    }
    if raw_args.get(2).map(String::as_str) == Some(PROBE_FLAG) {
        return Some(probe());
    }
    Some(HelperArgs::parse(&raw_args[2..]).and_then(run))
}

const PROBE_FLAG: &str = "--probe";

/// Whether `launcher` can build a private network namespace on this host, or
/// the host's refusal in its own words.
///
/// Asked by running it, because whether a host allows this depends on the
/// executable's path and on policy no setting reveals: a kernel can offer
/// unprivileged namespaces and still refuse this file the capabilities to
/// configure one.
pub fn private_namespace_available(launcher: &str) -> Result<(), String> {
    let output = std::process::Command::new(launcher)
        .args([NETNS_LAUNCH_SUBCOMMAND, PROBE_FLAG])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("could not run {launcher}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
}

#[cfg(target_os = "linux")]
fn probe() -> Result<Infallible, String> {
    enter_private_network_namespace(false)
        .and_then(|()| raise_loopback())
        .map_err(|error| format!("could not build a private network namespace: {error}"))?;
    std::process::exit(0);
}

#[cfg(not(target_os = "linux"))]
fn probe() -> Result<Infallible, String> {
    Err("private network namespaces are a Linux mechanism".to_string())
}

#[derive(Debug, Default)]
struct HelperArgs {
    ruleset_fd: Option<i32>,
    seccomp_hex: Option<String>,
    private_tmp: Option<std::path::PathBuf>,
    egress_relay: Option<EgressRelayTarget>,
    close_fd: Vec<DeviceMountFinalization>,
    payload: Vec<String>,
}

impl HelperArgs {
    /// Flags, then the payload. Only the sandbox backend writes this argv, so
    /// anything it would not write is refused rather than interpreted.
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut parsed = Self::default();
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            if arg == "--" {
                break;
            }
            if !arg.starts_with('-') {
                parsed.payload.push(arg.clone());
                break;
            }
            let value = rest
                .next()
                .ok_or_else(|| format!("namespace helper: {arg} needs a value"))?;
            let invalid = |error: String| format!("namespace helper: {arg}: {error}");
            match arg.as_str() {
                NETNS_RULESET_FD_FLAG => {
                    let fd = value
                        .parse()
                        .map_err(|_| invalid("not a descriptor".into()))?;
                    parsed.ruleset_fd = Some(fd);
                }
                NETNS_SECCOMP_FLAG => parsed.seccomp_hex = Some(value.clone()),
                NETNS_PRIVATE_TMP_FLAG => parsed.private_tmp = Some(value.into()),
                NETNS_EGRESS_RELAY_FLAG => {
                    parsed.egress_relay = Some(value.parse().map_err(invalid)?);
                }
                NETNS_CLOSE_FD_FLAG => parsed.close_fd.push(value.parse().map_err(invalid)?),
                _ => return Err(format!("namespace helper: unknown argument {arg}")),
            }
        }
        parsed.payload.extend(rest.cloned());
        let builds_namespace = parsed.ruleset_fd.is_some()
            || parsed.seccomp_hex.is_some()
            || parsed.private_tmp.is_some()
            || parsed.egress_relay.is_some();
        if builds_namespace && !parsed.close_fd.is_empty() {
            return Err(format!(
                "namespace helper: {NETNS_CLOSE_FD_FLAG} finalizes a mount that is already \
                 confined and takes no namespace arguments"
            ));
        }
        Ok(parsed)
    }
}

fn run(args: HelperArgs) -> Result<Infallible, String> {
    let (program, payload_args) = args
        .payload
        .split_first()
        .ok_or_else(|| "namespace helper: no payload program".to_string())?;
    if !args.close_fd.is_empty() {
        return finalize_exec(&args.close_fd, program, payload_args);
    }
    run_impl(
        args.ruleset_fd,
        args.seccomp_hex
            .as_deref()
            .ok_or("namespace helper: no confinement")?,
        args.private_tmp.as_deref(),
        args.egress_relay.as_ref(),
        program,
        payload_args,
    )
}

#[cfg(target_os = "linux")]
fn finalize_exec(
    descriptors: &[DeviceMountFinalization],
    program: &str,
    args: &[String],
) -> Result<Infallible, String> {
    use std::os::unix::fs::MetadataExt;
    let mut seen = std::collections::BTreeSet::new();
    for device in descriptors {
        let fd = device.descriptor;
        if fd < 3 || !seen.insert(fd) {
            return Err("namespace helper: invalid setup descriptor".into());
        }
        let mut source: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &raw mut source) } != 0 {
            return Err("namespace helper: setup descriptor is not open".into());
        }
        let mounted = std::fs::metadata(&device.destination)
            .map_err(|error| format!("namespace helper: cannot inspect mounted device: {error}"))?;
        if source.st_dev != mounted.dev()
            || source.st_ino != mounted.ino()
            || source.st_rdev != mounted.rdev()
            || source.st_mode != mounted.mode()
        {
            return Err("namespace helper: mounted device differs from its pinned source".into());
        }
    }
    for device in descriptors {
        if unsafe { libc::close(device.descriptor) } != 0 {
            return Err(format!(
                "namespace helper: could not close setup descriptor: {}",
                io::Error::last_os_error()
            ));
        }
    }
    let mut command = Command::new(program);
    command.args(args);
    Err(format!(
        "namespace helper: could not exec {program}: {}",
        command.exec()
    ))
}

#[cfg(not(target_os = "linux"))]
fn finalize_exec(
    _descriptors: &[DeviceMountFinalization],
    _program: &str,
    _args: &[String],
) -> Result<Infallible, String> {
    Err("namespace helper: setup descriptor finalization is a Linux mechanism".into())
}

#[cfg(target_os = "linux")]
fn run_impl(
    ruleset_fd: Option<i32>,
    seccomp_hex: &str,
    private_tmp: Option<&std::path::Path>,
    egress_relay: Option<&EgressRelayTarget>,
    program: &str,
    payload_args: &[String],
) -> Result<Infallible, String> {
    use super::TransferableConfinement;

    let seccomp = super::decode_seccomp_hex(seccomp_hex)
        .map_err(|error| format!("namespace helper: {error}"))?;
    // Built before the namespace so the allocation it needs happens while
    // allocation is still unambiguously safe, and so a malformed handover is
    // refused before any host state has been touched.
    let confinement = TransferableConfinement::from_parts(ruleset_fd, &seccomp)
        .map_err(|error| format!("namespace helper: {error}"))?;

    enter_private_network_namespace(private_tmp.is_some()).map_err(|error| {
        format!(
            "namespace helper: could not build a private network namespace: {error}. On a host \
             that restricts unprivileged namespaces this executable needs its own policy grant; \
             the grant names this path and no other."
        )
    })?;
    raise_loopback()
        .map_err(|error| format!("namespace helper: could not raise loopback: {error}"))?;
    if let Some(target) = egress_relay {
        super::start_namespace_relay(target, &confinement).map_err(|error| {
            format!("namespace helper: could not relay the egress proxy: {error}")
        })?;
    }
    if let Some(dir) = private_tmp {
        mount_private_tmp(dir).map_err(|error| {
            format!(
                "namespace helper: could not mount {} over /tmp: {error}",
                dir.display()
            )
        })?;
        // The same mount namespace gives POSIX semaphores a private home, so
        // the parent adds no host `/dev/shm` rule for this child.
        confinement.mount_private_shared_memory().map_err(|error| {
            format!("namespace helper: could not mount a private /dev/shm: {error}")
        })?;
    }

    let no_new_unix_sockets = egress_relay
        .map(|_| refuse_new_unix_sockets())
        .transpose()?;

    let mut command = Command::new(program);
    command.args(payload_args);
    // The confinement is entered in the child of this fork, immediately before
    // exec, exactly as the direct spawn path does it. Entering it here in the
    // parent instead would work too, but it would also confine this process's
    // own error reporting, so a failure to exec could no longer be explained.
    //
    // SAFETY: `pre_exec` may only call async-signal-safe functions. `enter`
    // makes two raw Landlock syscalls and one seccomp syscall, the added filter
    // one more, and neither allocates, locks, or does I/O.
    unsafe {
        command.pre_exec(move || {
            confinement.enter()?;
            if let Some(filter) = &no_new_unix_sockets {
                seccompiler::apply_filter(filter).map_err(|error| {
                    io::Error::other(format!("failed to install the socket filter: {error}"))
                })?;
            }
            Ok(())
        });
    }
    Err(format!(
        "namespace helper: could not exec {program}: {}",
        command.exec()
    ))
}

/// A network namespace does not separate filesystem Unix sockets, and Landlock
/// on most kernels cannot limit where one connects, so a payload that could make
/// one could leave through any service on the host that listens on a socket file.
/// The relay needs them and keeps them: it reaches the host's proxy no other way,
/// which is why this is an extra filter on the payload and not part of the
/// transferred one. Inherited descriptors and `socketpair` are unaffected.
#[cfg(target_os = "linux")]
fn refuse_new_unix_sockets() -> Result<seccompiler::BpfProgram, String> {
    use seccompiler::{
        SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter, SeccompRule,
    };
    let fail = |error: &dyn std::fmt::Display| {
        format!("namespace helper: could not build the socket filter: {error}")
    };
    let unix = SeccompCondition::new(
        0,
        SeccompCmpArgLen::Dword,
        SeccompCmpOp::Eq,
        libc::AF_UNIX as u64,
    )
    .map_err(|error| fail(&error))?;
    let rule = SeccompRule::new(vec![unix]).map_err(|error| fail(&error))?;
    let arch = std::env::consts::ARCH
        .try_into()
        .map_err(|error: seccompiler::BackendError| fail(&error))?;
    let filter = SeccompFilter::new(
        std::collections::BTreeMap::from([(libc::SYS_socket, vec![rule])]),
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .map_err(|error| fail(&error))?;
    seccompiler::BpfProgram::try_from(filter).map_err(|error| fail(&error))
}

/// Unshare into a new user and network namespace (and a mount namespace when
/// the child gets a private `/tmp` and `/dev/shm`), keeping the caller's own
/// ids inside.
///
/// The user namespace is what makes the others available without privilege;
/// creating it grants this process the capabilities to configure them until
/// it execs. The identity mapping keeps the payload the person who ran it:
/// mapped to root, the JVM and anything else resolving the account read
/// `/root` as home, and the payload kept full namespace capabilities across
/// exec. `setgroups` must be denied before the group map is written, which
/// the kernel requires and which also keeps the child from gaining any group
/// it did not already hold.
#[cfg(target_os = "linux")]
fn enter_private_network_namespace(mount_namespace: bool) -> io::Result<()> {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let mut flags = libc::CLONE_NEWUSER | libc::CLONE_NEWNET;
    if mount_namespace {
        flags |= libc::CLONE_NEWNS;
    }
    if unsafe { libc::unshare(flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    std::fs::write("/proc/self/setgroups", "deny")?;
    std::fs::write("/proc/self/uid_map", format!("{uid} {uid} 1\n"))?;
    std::fs::write("/proc/self/gid_map", format!("{gid} {gid} 1\n"))?;
    Ok(())
}

/// Bind the session temp dir over `/tmp` inside the new mount namespace.
///
/// Some tools write `/tmp` by a path compiled in rather than through
/// `TMPDIR`: the JVM's attach socket (`/tmp/.java_pid<pid>`), which Mockito's
/// inline mock maker needs, is one. The host's `/tmp` stays unwritable; the
/// child sees its workspace's temp dir there instead. Propagation is made
/// private first so the bind never reaches the host's mount table.
#[cfg(target_os = "linux")]
fn mount_private_tmp(dir: &std::path::Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let root = CString::new("/").expect("static path");
    let tmp = CString::new("/tmp").expect("static path");
    let source = CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| io::Error::other("temp dir path contains a NUL byte"))?;
    let private = unsafe {
        libc::mount(
            std::ptr::null(),
            root.as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    };
    if private != 0 {
        return Err(io::Error::last_os_error());
    }
    let bound = unsafe {
        libc::mount(
            source.as_ptr(),
            tmp.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        )
    };
    if bound != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Bring `lo` up inside the namespace just created.
///
/// A fresh network namespace has a loopback interface that exists and is
/// down, so a daemon binding `127.0.0.1` fails with a message about an
/// unusable address rather than about a permission. Raising it is what turns
/// the namespace from a denial into the grant that was asked for.
#[cfg(target_os = "linux")]
fn raise_loopback() -> io::Result<()> {
    const IFNAME: &[u8] = b"lo\0";
    let socket = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if socket < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    for (slot, byte) in request.ifr_name.iter_mut().zip(IFNAME.iter()) {
        *slot = *byte as libc::c_char;
    }
    let result = unsafe {
        if libc::ioctl(socket, libc::SIOCGIFFLAGS, &raw mut request) < 0 {
            Err(io::Error::last_os_error())
        } else {
            request.ifr_ifru.ifru_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
            if libc::ioctl(socket, libc::SIOCSIFFLAGS, &raw const request) < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    };
    unsafe {
        libc::close(socket);
    }
    result
}

/// Every other platform refuses rather than pretending.
///
/// The helper is only ever invoked by the Linux backend, so reaching this is a
/// wiring mistake, and saying so plainly beats a silent unconfined exec.
#[cfg(not(target_os = "linux"))]
fn run_impl(
    _ruleset_fd: Option<i32>,
    _seccomp_hex: &str,
    _private_tmp: Option<&std::path::Path>,
    _egress_relay: Option<&EgressRelayTarget>,
    _program: &str,
    _payload_args: &[String],
) -> Result<Infallible, String> {
    Err("namespace helper: private network namespaces are a Linux mechanism".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(ToString::to_string).collect()
    }

    /// Drift between this parser and the argv the backend builds is silent and
    /// total, so the payload must survive with its own flags intact.
    #[test]
    fn flags_stop_at_the_payload() {
        let parsed = HelperArgs::parse(&argv(&[
            NETNS_SECCOMP_FLAG,
            "00",
            "--",
            "/bin/tool",
            "--seccomp-hex",
            "ff",
        ]))
        .expect("parses");
        assert_eq!(parsed.seccomp_hex.as_deref(), Some("00"));
        assert_eq!(parsed.payload, ["/bin/tool", "--seccomp-hex", "ff"]);
    }

    #[test]
    fn a_program_that_is_not_the_helper_carries_on() {
        assert!(run_namespace_helper_if_invoked(&argv(&["tool", "run"])).is_none());
        assert!(run_namespace_helper_if_invoked(&argv(&["tool"])).is_none());
    }

    #[test]
    fn an_argument_the_backend_would_not_write_is_refused() {
        let unknown = HelperArgs::parse(&argv(&["--unknown", "x", "--", "/bin/true"]));
        assert!(unknown.is_err());
        let conflict = HelperArgs::parse(&argv(&[
            NETNS_SECCOMP_FLAG,
            "00",
            NETNS_CLOSE_FD_FLAG,
            "3:/dev/null",
            "--",
            "/bin/true",
        ]));
        assert!(conflict.is_err());
    }

    #[test]
    fn the_helper_refuses_to_run_without_a_payload_or_confinement() {
        let no_payload = HelperArgs::parse(&argv(&[NETNS_SECCOMP_FLAG, "00"])).expect("parses");
        assert!(run(no_payload).is_err());
        let no_confinement = HelperArgs::parse(&argv(&["--", "/bin/true"])).expect("parses");
        assert!(run(no_confinement).is_err());
    }
}
