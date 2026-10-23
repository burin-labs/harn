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
use std::io;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::cli::NetnsLaunchArgs;

/// Exec the payload inside a private network namespace, confined.
///
/// The success type is uninhabited on purpose: a launch that worked has
/// replaced this process, so there is no "finished successfully" state for a
/// caller to mishandle. Every return is a failure.
pub(crate) fn run(args: NetnsLaunchArgs) -> Result<Infallible, String> {
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
        program,
        payload_args,
    )
}

#[cfg(target_os = "linux")]
fn finalize_exec(
    descriptors: &[harn_vm::process_sandbox::DeviceMountFinalization],
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
    _descriptors: &[harn_vm::process_sandbox::DeviceMountFinalization],
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
    program: &str,
    payload_args: &[String],
) -> Result<Infallible, String> {
    use harn_vm::process_sandbox::TransferableConfinement;

    let seccomp = harn_vm::process_sandbox::decode_seccomp_hex(seccomp_hex)
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
    if let Some(dir) = private_tmp {
        mount_private_tmp(dir).map_err(|error| {
            format!(
                "namespace helper: could not mount {} over /tmp: {error}",
                dir.display()
            )
        })?;
    }

    let mut command = Command::new(program);
    command.args(payload_args);
    // The confinement is entered in the child of this fork, immediately before
    // exec, exactly as the direct spawn path does it. Entering it here in the
    // parent instead would work too, but it would also confine this process's
    // own error reporting, so a failure to exec could no longer be explained.
    //
    // SAFETY: `pre_exec` may only call async-signal-safe functions. `enter`
    // makes two raw Landlock syscalls and one seccomp syscall and performs no
    // allocation, locking, or I/O.
    unsafe {
        command.pre_exec(move || confinement.enter());
    }
    Err(format!(
        "namespace helper: could not exec {program}: {}",
        command.exec()
    ))
}

/// Unshare into a new user and network namespace (and a mount namespace when
/// the child gets a private `/tmp`), keeping the caller's own ids inside.
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
    _program: &str,
    _payload_args: &[String],
) -> Result<Infallible, String> {
    let _ = (io::ErrorKind::Unsupported, Command::new("true"));
    Err("namespace helper: private network namespaces are a Linux mechanism".to_string())
}
