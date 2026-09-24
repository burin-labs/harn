//! Launching a confined child that outlives the call.
//!
//! `std::process::Command` cannot carry an AppContainer, so every confined
//! Windows child is created here with `CreateProcessW` and returned as a live
//! [`ConfinedChild`]. `command_output` waits on one and collects its pipes;
//! the process tools keep one as the handle they stream, time out and cancel.
//! Both therefore get the same container, grants, environment and Job
//! Object, and neither can reach a child any other way.

use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::os::windows::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::Arc;

use windows_sys::Win32::Foundation::{
    DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_CAPABILITIES;
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcess, GetExitCodeProcess, ResumeThread, TerminateProcess,
    WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use super::acl_grants::WorkspaceAclGrants;
use super::{
    command_line, environment_block, path_to_wide, resolve_application_name, sandbox_trace,
    AppContainerProfile, InheritablePipe, InheritableStdinPipe, JobLimits, JobObject, OwnedHandle,
    ProcThreadAttributes, ProcessCapabilities,
};
use crate::orchestration::CapabilityPolicy;
use crate::stdlib::sandbox::{path_is_within, process_sandbox_roots, ProcessCommandConfig};

/// Where the child's standard input comes from.
pub enum ChildInput {
    Null,
    /// A pipe the caller writes through [`ConfinedChild::take_stdin`].
    Pipe,
    /// This process's own standard input.
    Inherit,
}

/// Where one of the child's output streams goes.
pub enum ChildOutput {
    /// A pipe the caller reads through [`ConfinedChild::take_stdout`] or
    /// [`ConfinedChild::take_stderr`].
    Pipe,
    /// This process's own stream.
    Inherit,
    /// An already open file. The child writes through the open handle, so
    /// the file need not be readable by the container.
    File(File),
}

/// The child's three standard streams.
pub struct ChildStdio {
    pub stdin: ChildInput,
    pub stdout: ChildOutput,
    pub stderr: ChildOutput,
}

/// A confined child and the Job Object that contains its whole tree.
///
/// Dropping it does not stop the child; dropping every [`ConfinedTerminator`]
/// and the child closes the job, and the job kills what is left on close.
pub struct ConfinedChild {
    pid: u32,
    process: OwnedHandle,
    job: Arc<JobObject>,
    stdin: Option<File>,
    stdout: Option<File>,
    stderr: Option<File>,
}

/// Stops a confined child's whole tree from any thread.
#[derive(Clone)]
pub struct ConfinedTerminator(Arc<JobObject>);

impl ConfinedTerminator {
    /// Terminate every process in the child's Job Object.
    pub fn terminate(&self) {
        self.0.terminate();
    }
}

impl ConfinedChild {
    pub fn id(&self) -> u32 {
        self.pid
    }

    pub fn take_stdin(&mut self) -> Option<File> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<File> {
        self.stderr.take()
    }

    pub fn terminator(&self) -> ConfinedTerminator {
        ConfinedTerminator(Arc::clone(&self.job))
    }

    /// The exit status if the child has exited, without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.wait_for(0)
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.wait_for(INFINITE)
            .map(|status| status.expect("an infinite wait returns only on exit"))
    }

    fn wait_for(&mut self, millis: u32) -> io::Result<Option<ExitStatus>> {
        match unsafe { WaitForSingleObject(self.process.raw(), millis) } {
            WAIT_OBJECT_0 => {
                let mut code = 1u32;
                if unsafe { GetExitCodeProcess(self.process.raw(), &mut code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(ExitStatus::from_raw(code)))
            }
            WAIT_TIMEOUT => Ok(None),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            other => Err(io::Error::other(format!("unexpected wait result {other}"))),
        }
    }
}

/// The checks that decide whether AppContainer can render a policy at all.
/// A policy it cannot render is refused before anything is created, and the
/// refusal is `Unsupported` so the caller reports a spawn refusal rather
/// than running the child wider than asked.
pub(super) fn ensure_renderable(policy: &CapabilityPolicy) -> io::Result<()> {
    if policy.process_network_proxy.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "managed child-process egress requires a proxy-only Windows network boundary; this build cannot enforce it",
        ));
    }
    if policy.process_sandbox.allow_tcp_loopback {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "TCP loopback-only child networking is not enforceable by AppContainer capabilities",
        ));
    }
    if !policy.process_sandbox.unix_socket_roots.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "path-scoped Unix-domain sockets for child processes are not enforceable by AppContainer capabilities",
        ));
    }
    Ok(())
}

/// Create `program` confined by `policy`, started and contained in a Job
/// Object, with its streams connected as `stdio` asks.
pub(super) fn launch(
    program: &str,
    args: &[String],
    config: &ProcessCommandConfig,
    policy: &CapabilityPolicy,
    stdio: ChildStdio,
    limits: JobLimits,
) -> io::Result<ConfinedChild> {
    ensure_renderable(policy)?;
    sandbox_trace(
        "pending",
        format!("start program={program:?} argc={}", args.len()),
    );
    let mut process_capabilities = ProcessCapabilities::for_policy(policy)?;
    let profile = AppContainerProfile::for_policy(policy, &mut process_capabilities)?;
    let trace_label = profile.label().to_string();
    sandbox_trace(&trace_label, "profile ready");
    let sid_string = profile.sid_string()?;
    let grants = WorkspaceAclGrants::grant(&trace_label, &sid_string, policy)?;
    sandbox_trace(
        &trace_label,
        format!("workspace ACL grants ready rewrites={}", grants.rewrites),
    );

    // Parent ends are kept; child ends are inherited and closed after create.
    let mut stdin_pipe = None;
    let stdin_child = match stdio.stdin {
        ChildInput::Null => OwnedHandle::nul_read()?,
        ChildInput::Pipe => {
            let mut pipe = InheritableStdinPipe::new()?;
            let child = pipe.take_child_read();
            stdin_pipe = Some(pipe);
            child
        }
        ChildInput::Inherit => inheritable_std_handle(STD_INPUT_HANDLE)?,
    };
    let (stdout_parent, stdout_child) = output_handles(stdio.stdout, STD_OUTPUT_HANDLE)?;
    let (stderr_parent, stderr_child) = output_handles(stdio.stderr, STD_ERROR_HANDLE)?;
    let inherited_handles = [stdin_child.raw(), stdout_child.raw(), stderr_child.raw()];

    let mut security_capabilities = profile.security_capabilities(&mut process_capabilities);
    let mut attributes = ProcThreadAttributes::new(2)?;
    attributes.update(
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
        (&mut security_capabilities as *mut SECURITY_CAPABILITIES).cast(),
        std::mem::size_of::<SECURITY_CAPABILITIES>(),
    )?;
    attributes.update(
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
        inherited_handles.as_ptr().cast(),
        std::mem::size_of_val(&inherited_handles),
    )?;

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin_child.raw();
    startup.StartupInfo.hStdOutput = stdout_child.raw();
    startup.StartupInfo.hStdError = stderr_child.raw();
    startup.lpAttributeList = attributes.as_mut_ptr();

    let mut command_line = command_line(program, args);
    let application = resolve_application_name(program);
    let sandbox_env = container_environment(&profile, &sid_string, config, policy)?;
    let mut environment = environment_block(
        &config.env,
        &sandbox_env,
        config.closed_env,
        &config.env_remove,
    );
    let cwd = config.cwd.as_ref().map(|path| path_to_wide(path));
    let job = JobObject::create(limits)?;

    let mut process_info = PROCESS_INFORMATION::default();
    sandbox_trace(&trace_label, "CreateProcessW begin");
    let created = unsafe {
        CreateProcessW(
            application
                .as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr()),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT
                | CREATE_UNICODE_ENVIRONMENT
                | CREATE_SUSPENDED
                | CREATE_NO_WINDOW,
            if environment.is_empty() {
                std::ptr::null()
            } else {
                environment.as_mut_ptr().cast()
            },
            cwd.as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr()),
            std::ptr::addr_of!(startup.StartupInfo),
            &mut process_info,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    let process = OwnedHandle::new(process_info.hProcess);
    let thread = OwnedHandle::new(process_info.hThread);
    // Contained before its first instruction, so nothing it starts escapes.
    if let Err(error) = job.assign(process.raw()) {
        unsafe {
            TerminateProcess(process.raw(), 1);
        }
        return Err(error);
    }
    drop((stdin_child, stdout_child, stderr_child));
    if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
        let error = io::Error::last_os_error();
        job.terminate();
        return Err(error);
    }
    sandbox_trace(&trace_label, "process resumed");

    Ok(ConfinedChild {
        pid: process_info.dwProcessId,
        process,
        job: Arc::new(job),
        stdin: stdin_pipe.map(InheritableStdinPipe::into_writer),
        stdout: stdout_parent,
        stderr: stderr_parent,
    })
}

/// The container's own directories, and the temp dir the child uses.
///
/// `LOCALAPPDATA` is always the container's. `TEMP` and `TMP` stay as the
/// caller set them when they name a directory the container may write, which
/// is how the session temp dir reaches the child; anywhere else the container
/// could not write, so they fall back to the container's own temp dir.
fn container_environment(
    profile: &AppContainerProfile,
    sid: &str,
    config: &ProcessCommandConfig,
    policy: &CapabilityPolicy,
) -> io::Result<Vec<(String, String)>> {
    let writable = process_sandbox_roots(policy);
    let caller_temp_is_writable = |key: &str| {
        config
            .env
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .is_some_and(|(_, value)| {
                let path = std::path::Path::new(value);
                writable.iter().any(|root| path_is_within(path, root))
            })
    };
    Ok(profile
        .environment_overrides(sid)?
        .into_iter()
        .filter(|(key, _)| {
            !(key.eq_ignore_ascii_case("TEMP") || key.eq_ignore_ascii_case("TMP"))
                || !caller_temp_is_writable(key)
        })
        .collect())
}

/// The parent's end (for a pipe) and the child's inheritable end.
fn output_handles(output: ChildOutput, std: u32) -> io::Result<(Option<File>, OwnedHandle)> {
    match output {
        ChildOutput::Pipe => {
            let pipe = InheritablePipe::new()?;
            let (read, write) = pipe.into_parts();
            let parent = unsafe { File::from_raw_handle(read.into_raw().cast()) };
            Ok((Some(parent), write))
        }
        ChildOutput::Inherit => Ok((None, inheritable_std_handle(std)?)),
        ChildOutput::File(file) => Ok((None, inheritable_duplicate(file.as_raw_handle().cast())?)),
    }
}

/// This process's standard handle, or `NUL` when it has none (a service or
/// a GUI host), duplicated so the child can inherit it.
fn inheritable_std_handle(std: u32) -> io::Result<OwnedHandle> {
    let handle = unsafe { GetStdHandle(std) };
    if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return if std == STD_INPUT_HANDLE {
            OwnedHandle::nul_read()
        } else {
            OwnedHandle::nul_write()
        };
    }
    inheritable_duplicate(handle)
}

/// An inheritable duplicate, so the handle list names a handle only this
/// launch owns and the original's inheritance is left alone.
fn inheritable_duplicate(handle: HANDLE) -> io::Result<OwnedHandle> {
    let mut duplicate = std::ptr::null_mut();
    let current = unsafe { GetCurrentProcess() };
    if unsafe {
        DuplicateHandle(
            current,
            handle,
            current,
            &mut duplicate,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    OwnedHandle::new_checked(duplicate)
}
