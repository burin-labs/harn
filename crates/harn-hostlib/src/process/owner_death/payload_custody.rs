//! How the guardian waits for its payload and retires what it leaves behind.

use super::*;

/// Become the subreaper of every descendant, and report whether the kernel
/// confirms it: the set succeeded and reads back as set. A guardian without
/// confirmed custody (a sandbox may deny the call) falls back to the host
/// census when its payload finishes, and its record says so.
#[cfg(target_os = "linux")]
pub(super) fn configure_child_reaper() -> bool {
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return false;
    }
    let mut subreaper: libc::c_int = 0;
    let read = unsafe {
        libc::prctl(
            libc::PR_GET_CHILD_SUBREAPER,
            &raw mut subreaper as libc::c_ulong,
            0,
            0,
            0,
        )
    };
    read == 0 && subreaper != 0
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(super) fn configure_child_reaper() -> bool {
    false
}

#[cfg(unix)]
pub(super) fn wait_for_payload_while_reaping_adopted(payload: Child) -> io::Result<ExitStatus> {
    let payload_pid = payload.id() as libc::pid_t;
    // Keep the Child handle alive while waitpid(2) owns reaping. On Linux the
    // guardian is a subreaper, so detached helpers become its direct children.
    // Reaping all children here prevents successfully terminated helpers from
    // remaining as zombies until the conformance payload itself exits.
    let _payload = payload;
    loop {
        let mut status = 0;
        let waited = unsafe { libc::waitpid(-1, &raw mut status, 0) };
        if waited == payload_pid {
            return Ok(ExitStatus::from_raw(status));
        }
        if waited > 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            _ => return Err(error),
        }
    }
}

#[cfg(unix)]
pub(super) fn wait_for_payload_exit(
    events: &std::sync::mpsc::Receiver<GuardianEvent>,
    payload_status: &mut Option<ExitStatus>,
) -> io::Result<()> {
    while payload_status.is_none() {
        match events.recv() {
            Ok(GuardianEvent::PayloadExited(status)) => *payload_status = Some(status?),
            Ok(
                GuardianEvent::OutputClosed(_)
                | GuardianEvent::OwnerClosed
                | GuardianEvent::InputFailed(_),
            ) => {}
            #[cfg(target_os = "linux")]
            Ok(GuardianEvent::CancelRequested) => {}
            Err(_) => {
                return Err(io::Error::other(
                    "guardian events closed before payload exit",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn reap_adopted_children() -> io::Result<()> {
    retire_adopted_children().map(drop)
}

#[cfg(target_os = "linux")]
pub(super) fn retire_adopted_children() -> io::Result<Vec<adopted_children::RetiredChild>> {
    // The payload waiter has returned before either caller reaches this
    // point. Only this thread now reaps children adopted by the subreaper.
    let error =
        match adopted_children::terminate_and_reap(harn_vm::op_interrupt::SUBPROCESS_TERM_GRACE) {
            Ok(retired) => return Ok(retired),
            Err(error) => error,
        };
    retain_adopted_children(error).map(|()| Vec::new())
}

/// Kill and account for everything a finished payload left behind.
///
/// With confirmed subreaper custody the record covers the guardian's kernel
/// child custody, proved by ECHILD, and its census fields stay unmeasured.
/// Without it, only a host census can find a descendant that outlived its
/// parent, so the record carries that census and claims no custody.
#[cfg(unix)]
pub(super) fn finished_payload_cleanup(
    payload_pid: u32,
    cleanup_token: &str,
    subreaper_custody: bool,
    cancelled: Option<harn_vm::op_interrupt::ProcessCleanupReport>,
) -> io::Result<harn_vm::op_interrupt::ProcessCleanupReport> {
    #[cfg(target_os = "linux")]
    if subreaper_custody {
        let mut cleanup = retire_finished_payload_descendants(payload_pid, cleanup_token)?;
        if let Some(cancelled) = cancelled {
            cleanup.merge(cancelled);
        }
        cleanup.refresh_survivor_status();
        cleanup.guardian_custody_complete = true;
        return Ok(cleanup);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = subreaper_custody;
    let mut cleanup = harn_vm::op_interrupt::signal_pid_tree_and_token_preserving_group_with_report(
        payload_pid,
        Some(cleanup_token),
        unsafe { libc::getpgrp() as u32 },
        libc::SIGKILL,
    );
    if let Some(cancelled) = cancelled {
        cleanup.merge(cancelled);
    }
    reap_adopted_children()?;
    harn_vm::op_interrupt::finish_process_owner_cleanup(&mut cleanup, cleanup_token);
    Ok(cleanup)
}

/// Retire everything a finished payload left behind, without a host scan.
///
/// The payload has exited and been reaped, so the kernel has reparented every
/// descendant still alive to this subreaper, including one that called
/// `setsid` or double-forked. Retiring adopted children until ECHILD therefore
/// reaches all of them. A host-wide census could add only a process outside
/// this subtree that still carries the command's token in its environment:
/// one another process started on the command's behalf. That process was
/// never in the command's lifetime, and one that scrubbed its environment was
/// never findable either, so the census is not part of this guarantee.
#[cfg(target_os = "linux")]
pub(super) fn retire_finished_payload_descendants(
    payload_pid: u32,
    cleanup_token: &str,
) -> io::Result<harn_vm::op_interrupt::ProcessCleanupReport> {
    let guardian_pid = std::process::id();
    harn_vm::op_interrupt::signal_owner_process_groups(
        cleanup_token,
        unsafe { libc::getpgrp() as u32 },
        libc::SIGKILL,
    );
    let mut report =
        harn_vm::op_interrupt::ProcessCleanupReport::for_signal(Some(payload_pid), libc::SIGKILL);
    for child in retire_adopted_children()? {
        report.record_retired_child(
            child.pid,
            Some(guardian_pid),
            child.command_name,
            libc::SIGKILL,
        );
    }
    Ok(report)
}

#[cfg(target_os = "linux")]
pub(super) fn retain_adopted_children(error: io::Error) -> io::Result<()> {
    eprintln!("guardian cleanup incomplete; retaining child custody: {error}");
    // An unproved cleanup must not release the guardian or its journal while
    // children remain. Keep the original native reaper as an integrity hold;
    // the operation failed, and its caller may still need external cleanup.
    loop {
        let result = unsafe { libc::waitpid(-1, std::ptr::null_mut(), 0) };
        if result > 0 {
            continue;
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ECHILD) => return Err(error),
            _ => return Err(io::Error::last_os_error()),
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(super) fn reap_adopted_children() -> io::Result<()> {
    Ok(())
}
