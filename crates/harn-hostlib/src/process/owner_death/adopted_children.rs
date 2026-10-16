//! Reap only children whose custody the kernel assigns to this guardian.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

/// Called only after the payload waiter has returned. No other thread may
/// reap children while these WNOWAIT witnesses reserve their identities.
pub(super) fn terminate_and_reap(timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let children = adopted_children()?;
        if children.is_empty() {
            loop {
                let mut event = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
                let result = unsafe {
                    libc::waitid(
                        libc::P_ALL,
                        0,
                        &raw mut event,
                        libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                    )
                };
                if result == 0 {
                    break;
                }
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    return Ok(());
                }
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            return Err(io::Error::other(
                "guardian child census incomplete: kernel child custody remains",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("guardian children remain pending: {children:?}"),
            ));
        }
        for pid in children {
            terminate_child(pid, deadline)?;
        }
        // Killing a direct child can adopt its children. Repeat from the
        // guardian's kernel list, never a dead payload PID or a host census.
    }
}

fn adopted_children() -> io::Result<Vec<libc::pid_t>> {
    let leader = std::process::id();
    let text = std::fs::read_to_string(format!("/proc/self/task/{leader}/children"))?;
    let mut children = std::collections::BTreeSet::new();
    for value in text.split_whitespace() {
        let pid = value.parse::<libc::pid_t>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid guardian child identity",
            )
        })?;
        if pid <= 0 || pid as u32 == leader || !children.insert(pid) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid or repeated guardian child identity",
            ));
        }
    }
    Ok(children.into_iter().collect())
}

fn child_event(
    kind: libc::idtype_t,
    identity: libc::id_t,
    options: libc::c_int,
) -> io::Result<libc::siginfo_t> {
    let mut event = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    loop {
        let result = unsafe { libc::waitid(kind, identity, &raw mut event, options) };
        if result == 0 {
            return Ok(event);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn terminate_child(pid: libc::pid_t, deadline: Instant) -> io::Result<()> {
    let options = libc::WEXITED | libc::WNOHANG;
    let witnessed = child_event(libc::P_PID, pid as libc::id_t, options | libc::WNOWAIT)?;
    if unsafe { witnessed.si_pid() } == pid {
        let reaped = child_event(libc::P_PID, pid as libc::id_t, options)?;
        return if unsafe { reaped.si_pid() } == pid {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "guardian child {pid} was not reaped"
            )))
        };
    }
    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pidfd_open returned a new owning descriptor. Every operation
    // below binds that identity, including the non-reaping child-custody check.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor as libc::c_int) };
    let identity = descriptor.as_raw_fd() as libc::id_t;
    let event = child_event(libc::P_PIDFD, identity, options | libc::WNOWAIT)?;
    if unsafe { event.si_pid() } == 0 {
        if unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                descriptor.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        } < 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("guardian child {pid} pending after SIGKILL"),
                ));
            }
            let mut ready = libc::pollfd {
                fd: descriptor.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = remaining
                .as_millis()
                .saturating_add(1)
                .min(i32::MAX as u128) as i32;
            let result = unsafe { libc::poll(&raw mut ready, 1, millis) };
            if result > 0 && ready.revents & libc::POLLIN != 0 {
                break;
            }
            if result < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if result == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("guardian child {pid} pending after SIGKILL"),
                ));
            }
            return Err(io::Error::other(format!(
                "guardian child {pid} exit remains unproved"
            )));
        }
    }
    let reaped = child_event(libc::P_PIDFD, identity, options)?;
    if unsafe { reaped.si_pid() } != pid {
        return Err(io::Error::other(format!(
            "guardian child {pid} was not reaped"
        )));
    }
    Ok(())
}
