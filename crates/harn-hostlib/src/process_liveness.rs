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
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    if unsafe { kill(pid as i32, 0) } == 0 {
        return ProcessLiveness::Alive;
    }
    const ESRCH: i32 = 3;
    match std::io::Error::last_os_error().raw_os_error() {
        Some(ESRCH) => ProcessLiveness::Dead,
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
    const MAXCOMLEN: usize = 16;
    const PROC_PIDTBSDINFO: i32 = 3;

    #[repr(C)]
    struct ProcBsdInfo {
        pbi_flags: u32,
        pbi_status: u32,
        pbi_xstatus: u32,
        pbi_pid: u32,
        pbi_ppid: u32,
        pbi_uid: u32,
        pbi_gid: u32,
        pbi_ruid: u32,
        pbi_rgid: u32,
        pbi_svuid: u32,
        pbi_svgid: u32,
        rfu_1: u32,
        pbi_comm: [i8; MAXCOMLEN],
        pbi_name: [i8; 2 * MAXCOMLEN],
        pbi_nfiles: u32,
        pbi_pgid: u32,
        pbi_pjobc: u32,
        e_tdev: u32,
        e_tpgid: u32,
        pbi_nice: i32,
        pbi_start_tvsec: u64,
        pbi_start_tvusec: u64,
    }

    extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut std::ffi::c_void,
            buffersize: i32,
        ) -> i32;
    }

    let mut info = std::mem::MaybeUninit::<ProcBsdInfo>::zeroed();
    let size = std::mem::size_of::<ProcBsdInfo>();
    let written = unsafe {
        proc_pidinfo(
            i32::try_from(pid).ok()?,
            PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            i32::try_from(size).ok()?,
        )
    };
    if written != i32::try_from(size).ok()? {
        return None;
    }
    let info = unsafe { info.assume_init() };
    Some(
        info.pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(info.pbi_start_tvusec),
    )
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
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn process_liveness(pid: u32) -> ProcessLiveness {
    let _ = pid;
    ProcessLiveness::Unknown
}
