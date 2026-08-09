//! Conservative, cross-platform process-liveness probes.

/// What the operating system proved about a process identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessLiveness {
    /// The process exists or access was denied while probing it.
    Alive,
    /// The operating system confirmed that the process does not exist.
    Dead,
    /// This platform or probe could not prove either state.
    Unknown,
}

#[cfg(unix)]
pub(crate) fn process_liveness(pid: u32) -> ProcessLiveness {
    if pid == 0 || pid > i32::MAX as u32 {
        return ProcessLiveness::Unknown;
    }
    if unsafe { libc::kill(pid as i32, 0) } == 0 {
        return ProcessLiveness::Alive;
    }
    // `ESRCH` is the only errno that proves absence. Anything else (notably
    // `EPERM` for a live process we may not signal) must stay `Alive` so lease
    // recovery never steals from an owner it merely cannot inspect.
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => ProcessLiveness::Dead,
        Some(_) => ProcessLiveness::Alive,
        None => ProcessLiveness::Unknown,
    }
}

#[cfg(windows)]
pub(crate) fn process_liveness(pid: u32) -> ProcessLiveness {
    use std::ffi::c_void;

    if pid == 0 {
        return ProcessLiveness::Unknown;
    }

    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_INVALID_PARAMETER: i32 = 87;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;

    type Handle = *mut c_void;

    extern "system" {
        fn CloseHandle(hObject: Handle) -> i32;
        fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> Handle;
        fn WaitForSingleObject(hHandle: Handle, dwMilliseconds: u32) -> u32;
    }

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return match std::io::Error::last_os_error().raw_os_error() {
            Some(ERROR_ACCESS_DENIED) => ProcessLiveness::Alive,
            Some(ERROR_INVALID_PARAMETER) => ProcessLiveness::Dead,
            _ => ProcessLiveness::Unknown,
        };
    }

    let result = match unsafe { WaitForSingleObject(handle, 0) } {
        WAIT_OBJECT_0 => ProcessLiveness::Dead,
        WAIT_TIMEOUT => ProcessLiveness::Alive,
        _ => ProcessLiveness::Unknown,
    };
    let _ = unsafe { CloseHandle(handle) };
    result
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn process_identity(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_command = stat.get(stat.rfind(')')? + 1..)?;
    after_command.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) fn process_identity(pid: u32) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let written = unsafe {
        libc::proc_pidinfo(
            i32::try_from(pid).ok()?,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            i32::try_from(size).ok()?,
        )
    };
    if written != i32::try_from(size).ok()? {
        return None;
    }
    let info = unsafe { info.assume_init() };
    Some(fold_bsd_start_time(
        info.pbi_start_tvsec,
        info.pbi_start_tvusec,
    ))
}

/// Fold a BSD process start time into one microsecond-resolution identity.
///
/// Both fields must contribute: the seconds anchor the epoch and the
/// microseconds preserve the sub-second resolution that lets two processes
/// which reused a PID within the same wall-clock second be told apart. A fold
/// that dropped `tv_usec` would silently coarsen this to seconds and reopen the
/// PID-reuse window this identity exists to close.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn fold_bsd_start_time(tv_sec: u64, tv_usec: u64) -> u64 {
    tv_sec.saturating_mul(1_000_000).saturating_add(tv_usec)
}

#[cfg(windows)]
pub(crate) fn process_identity(pid: u32) -> Option<u64> {
    use std::ffi::c_void;

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    type Handle = *mut c_void;

    extern "system" {
        fn CloseHandle(hObject: Handle) -> i32;
        fn GetProcessTimes(
            hProcess: Handle,
            lpCreationTime: *mut FileTime,
            lpExitTime: *mut FileTime,
            lpKernelTime: *mut FileTime,
            lpUserTime: *mut FileTime,
        ) -> i32;
        fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> Handle;
    }

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut creation = FileTime { low: 0, high: 0 };
    let mut exit = FileTime { low: 0, high: 0 };
    let mut kernel = FileTime { low: 0, high: 0 };
    let mut user = FileTime { low: 0, high: 0 };
    let ok =
        unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) } != 0;
    let _ = unsafe { CloseHandle(handle) };
    ok.then_some((u64::from(creation.high) << 32) | u64::from(creation.low))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    windows
)))]
pub(crate) fn process_identity(pid: u32) -> Option<u64> {
    let _ = pid;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        assert_eq!(process_liveness(std::process::id()), ProcessLiveness::Alive);
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        windows
    ))]
    #[test]
    fn current_process_has_a_native_identity() {
        assert!(process_identity(std::process::id()).is_some());
    }

    // Guards the microsecond component of the Apple start-time identity. If the
    // fold ever drops `tv_usec`, two processes that reuse a PID within the same
    // second would collapse to one identity and the PID-reuse defense would
    // silently regress to second resolution.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[test]
    fn bsd_start_time_fold_preserves_microseconds() {
        // The seconds anchor the value...
        assert_eq!(fold_bsd_start_time(1, 0), 1_000_000);
        // ...and each microsecond shifts it, so same-second neighbours differ.
        assert_eq!(fold_bsd_start_time(1, 1), 1_000_001);
        assert_ne!(fold_bsd_start_time(42, 0), fold_bsd_start_time(42, 1));
        assert_eq!(
            fold_bsd_start_time(42, 999_999) - fold_bsd_start_time(42, 0),
            999_999
        );
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn process_liveness(pid: u32) -> ProcessLiveness {
    let _ = pid;
    ProcessLiveness::Unknown
}
