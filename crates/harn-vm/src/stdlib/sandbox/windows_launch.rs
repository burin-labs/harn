//! Launching a confined child that outlives the call.
//!
//! `std::process::Command` cannot launch under a restricted token, so every
//! confined Windows child is created here with `CreateProcessAsUserW` and
//! returned as a live [`ConfinedChild`]. `command_output` waits on one and
//! collects its pipes; the process tools keep one as the handle they stream,
//! time out and cancel. Both therefore get the same token, grants,
//! environment and Job Object, and neither can reach a child any other way.

use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Arc;

use windows_sys::Win32::Foundation::{
    DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, ResumeThread, TerminateProcess,
    WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use super::acl_grants::{
    grant_msys_user_sections, policy_digest, writable_roots, PolicyWriteGrants,
};
use super::token::{current_user_sddl, write_restricted_token, Sid};
use super::{
    command_line, environment_block, path_to_wide, resolve_application_name, sandbox_trace,
    InheritablePipe, InheritableStdinPipe, JobLimits, JobObject, OwnedHandle, ProcThreadAttributes,
};
use crate::orchestration::CapabilityPolicy;
use crate::stdlib::sandbox::{path_is_within, workspace_local_tmpdir, ProcessCommandConfig};

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
    /// the file need not be writable by the restricted token.
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

/// The checks that decide whether the restricted-token launch can render a
/// policy at all.
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
            "TCP loopback-only child networking is not enforceable by the Windows process sandbox",
        ));
    }
    if !policy.process_sandbox.unix_socket_roots.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "path-scoped Unix-domain sockets for child processes are not enforceable by the Windows process sandbox",
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
    let digest = policy_digest(policy);
    let trace_label = policy_label(&digest);
    let policy_sid = Sid::for_policy_digest(&digest)?;
    let temp = child_temp(config, policy, &trace_label)?;
    let grants =
        PolicyWriteGrants::grant(&trace_label, &policy_sid, policy, temp.scratch.as_deref())?;
    sandbox_trace(
        &trace_label,
        format!("write grants ready rewrites={}", grants.rewrites),
    );
    let token = write_restricted_token(&policy_sid)?;
    sandbox_trace(&trace_label, "restricted token ready");
    // Best effort: without it only MSYS programs fail to start.
    let msys = current_user_sddl().and_then(|user| grant_msys_user_sections(&policy_sid, &user));
    sandbox_trace(&trace_label, format!("msys user section {msys:?}"));

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

    let mut attributes = ProcThreadAttributes::new(1)?;
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
    let mut environment = environment_block(
        &config.env,
        &temp.env,
        config.closed_env,
        &config.env_remove,
    );
    let cwd = config.cwd.as_ref().map(|path| path_to_wide(path));
    let job = JobObject::create(limits)?;

    let mut process_info = PROCESS_INFORMATION::default();
    sandbox_trace(&trace_label, "CreateProcessAsUserW begin");
    let created = unsafe {
        CreateProcessAsUserW(
            token.raw(),
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

/// The name a policy's trace lines and scratch directory carry.
pub(super) fn policy_label(digest: &[u8; 32]) -> String {
    let hex: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("harn.sandbox.{hex}")
}

/// The child's temp-dir environment, and the directory it needs granted.
struct ChildTemp {
    env: Vec<(String, String)>,
    scratch: Option<PathBuf>,
}

/// The child's `TEMP` and `TMP`, and the scratch directory to grant when
/// neither the caller nor the workspace provides a writable one.
///
/// A caller's `TEMP`/`TMP` stays when it names a directory the child may
/// write, which is how the session temp dir reaches the child. Otherwise the
/// workspace's own temp dir is used, and a policy that may not write its
/// workspace gets a per-policy directory under `LOCALAPPDATA` instead, since
/// the user's own temp dir is not writable through the restricted token.
fn child_temp(
    config: &ProcessCommandConfig,
    policy: &CapabilityPolicy,
    label: &str,
) -> io::Result<ChildTemp> {
    let writable = writable_roots(policy);
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
    if caller_temp_is_writable("TEMP") && caller_temp_is_writable("TMP") {
        return Ok(ChildTemp {
            env: Vec::new(),
            scratch: None,
        });
    }
    let (temp, scratch) = match workspace_local_tmpdir(policy)
        .filter(|dir| writable.iter().any(|root| path_is_within(dir, root)))
    {
        Some(dir) => (dir, None),
        None => {
            let dir = scratch_dir(label);
            std::fs::create_dir_all(&dir)?;
            (dir.clone(), Some(dir))
        }
    };
    let temp = temp.to_string_lossy().into_owned();
    let overrides = ["TEMP", "TMP"]
        .into_iter()
        .filter(|key| !caller_temp_is_writable(key))
        .map(|key| (key.to_string(), temp.clone()))
        .collect();
    Ok(ChildTemp {
        env: overrides,
        scratch,
    })
}

/// A per-policy scratch directory, for a child with no writable temp dir.
pub(super) fn scratch_dir(label: &str) -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("harn")
        .join("sandbox")
        .join(label)
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
