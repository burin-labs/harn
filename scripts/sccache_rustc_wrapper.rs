//! Cargo rustc wrapper for Harn's shared sccache setup.
//!
//! Cargo supplies `CARGO_BIN_EXE_*` only to compilation units that may embed a
//! package binary path. sccache 0.17's daemon-side compiler path can lose those
//! synthetic variables, so those rare units must stay in Cargo's process tree.
//! Every other compilation keeps using sccache, with the worktree-specific
//! target directory removed from its cache identity.

use std::env;
use std::ffi::{OsStr, OsString};
use std::process::{self, Command};

const CARGO_BIN_EXE_PREFIX: &str = "CARGO_BIN_EXE_";
const TRACE_ENV: &str = "HARN_SCCACHE_WRAPPER_TRACE";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Route {
    Direct,
    Sccache,
}

fn route_for_environment<I, K, V>(variables: I) -> Route
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
{
    if variables.into_iter().any(|(name, _)| {
        name.as_ref()
            .to_string_lossy()
            .starts_with(CARGO_BIN_EXE_PREFIX)
    }) {
        Route::Direct
    } else {
        Route::Sccache
    }
}

#[cfg(unix)]
fn run_command(mut command: Command) -> ! {
    use std::os::unix::process::CommandExt;

    let error = command.exec();
    eprintln!("harn-sccache-wrapper: failed to replace wrapper with compiler: {error}");
    process::exit(1);
}

#[cfg(windows)]
fn run_command(mut command: Command) -> ! {
    if let Err(error) = windows_lifetime::bind_descendants_to_wrapper() {
        eprintln!("harn-sccache-wrapper: failed to bind compiler lifetime: {error}");
        process::exit(1);
    }

    match command.status() {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("harn-sccache-wrapper: failed to start compiler: {error}");
            process::exit(1);
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn run_command(mut command: Command) -> ! {
    match command.status() {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("harn-sccache-wrapper: failed to start compiler: {error}");
            process::exit(1);
        }
    }
}

#[cfg(windows)]
mod windows_lifetime {
    use std::ffi::c_void;
    use std::io;

    type Handle = *mut c_void;

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimitInformation {
        basic_limit_information: BasicLimitInformation,
        io_info: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn CloseHandle(object: Handle) -> i32;
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn GetCurrentProcess() -> Handle;
        fn SetInformationJobObject(
            job: Handle,
            information_class: i32,
            information: *const c_void,
            information_length: u32,
        ) -> i32;
    }

    pub(super) fn bind_descendants_to_wrapper() -> io::Result<()> {
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }

        let mut limits = ExtendedLimitInformation::default();
        limits.basic_limit_information.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                std::ptr::from_ref(&limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        let assigned =
            configured != 0 && unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) } != 0;
        if !assigned {
            let error = io::Error::last_os_error();
            unsafe {
                CloseHandle(job);
            }
            return Err(error);
        }

        // Keep the only job handle open until process teardown. Windows then
        // kills any compiler descendant if this wrapper is terminated early.
        Ok(())
    }
}

fn main() {
    let mut arguments = env::args_os();
    let _wrapper = arguments.next();
    let Some(rustc) = arguments.next() else {
        eprintln!("harn-sccache-wrapper: missing rustc executable");
        process::exit(2);
    };
    let rustc_arguments: Vec<OsString> = arguments.collect();
    let route = route_for_environment(env::vars_os());

    let mut command = match route {
        Route::Direct => Command::new(&rustc),
        Route::Sccache => {
            let mut command = Command::new("sccache");
            command.arg(&rustc).env_remove("CARGO_TARGET_DIR");
            command
        }
    };
    command.args(&rustc_arguments);

    if env::var_os(TRACE_ENV).is_some() {
        let label = match route {
            Route::Direct => "direct cargo-binary environment",
            Route::Sccache => "sccache",
        };
        eprintln!("harn-sccache-wrapper: route={label}");
    }

    run_command(command);
}

#[cfg(test)]
mod tests {
    use super::{route_for_environment, Route};
    use std::ffi::OsString;

    #[test]
    fn cargo_binary_environment_routes_directly() {
        let variables = [
            (OsString::from("CARGO_PKG_NAME"), OsString::from("probe")),
            (
                OsString::from("CARGO_BIN_EXE_probe-bin"),
                OsString::from("placeholder:probe-bin"),
            ),
        ];

        assert_eq!(route_for_environment(variables), Route::Direct);
    }

    #[test]
    fn ordinary_compilation_routes_through_sccache() {
        let variables = [
            (OsString::from("CARGO_PKG_NAME"), OsString::from("probe")),
            (OsString::from("CARGO_TARGET_DIR"), OsString::from("target")),
        ];

        assert_eq!(route_for_environment(variables), Route::Sccache);
    }
}
