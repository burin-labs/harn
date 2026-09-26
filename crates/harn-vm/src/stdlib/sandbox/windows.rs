use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::process::Output;

use windows_sys::Win32::Foundation::{
    CloseHandle, SetHandleInformation, GENERIC_READ, GENERIC_WRITE, HANDLE, HANDLE_FLAG_INHERIT,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicUIRestrictions,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
    JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, UpdateProcThreadAttribute,
};

use super::{
    process_spawn_error, sandbox_rejection, unavailable, PrepareOutcome, ProcessCommandConfig,
    SandboxBackend,
};
use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::value::VmError;

// Declared here rather than in the sandbox module index: this backend is its
// only consumer, and the index is a platform-neutral surface.
/// The identity a confined child runs as.
#[path = "windows_token.rs"]
mod token;

/// Where that identity may write, granted once per policy.
#[path = "windows_acl_grants.rs"]
mod acl_grants;

/// The one place a confined child is created, for every caller.
#[path = "windows_launch.rs"]
pub(crate) mod launch;

pub(super) struct Backend;

impl SandboxBackend for Backend {
    fn name() -> &'static str {
        "windows"
    }

    fn filesystem_mechanism() -> &'static str {
        "windows_restricted_token"
    }

    fn available() -> bool {
        true
    }

    /// `std::process::Command` cannot launch under a restricted token:
    /// Windows requires `CreateProcessAsUserW` with `STARTUPINFOEX`
    /// plumbing, which only this backend's launch owns.
    /// Callers that need an `Output` go through [`Backend::run_to_output`],
    /// and callers that keep the child (the process tools) through
    /// [`spawn_confined`]. A caller that still asks for a confined `Command`
    /// gets the warn-or-error fallback below.
    fn prepare_std_command(
        _program: &str,
        _args: &[String],
        _command: &mut std::process::Command,
        _policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        // Only `command_output()` owns the restricted-token launch;
        // `std_command_for()` cannot carry one.
        unavailable(
            super::SandboxMechanism::WindowsRestrictedToken,
            super::SandboxMechanismAvailability::EntryPointCannotAttach,
            profile,
        )
    }

    fn prepare_tokio_command(
        _program: &str,
        _args: &[String],
        _command: &mut tokio::process::Command,
        _policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        // As above: `tokio_command_for()` cannot carry a restricted token either.
        unavailable(
            super::SandboxMechanism::WindowsRestrictedToken,
            super::SandboxMechanismAvailability::EntryPointCannotAttach,
            profile,
        )
    }

    fn run_to_output(
        program: &str,
        args: &[String],
        config: &ProcessCommandConfig,
        policy: &CapabilityPolicy,
        _profile: SandboxProfile,
    ) -> Result<Output, VmError> {
        // `mod.rs::command_output` only routes here after
        // `active_sandbox_policy()` decides the spawn should be
        // confined (profile is `Worktree` or `OsHardened` and
        // `HARN_HANDLER_SANDBOX` is not `off`). The restricted-token
        // launch is the only meaningful path on Windows.
        sandboxed_output(program, args, config, policy).map_err(|error| {
            process_spawn_error(&error)
                .unwrap_or_else(|| sandbox_rejection(format!("process sandbox failed: {error}")))
        })
    }
}

/// Whether a spawn now runs confined, so a caller that keeps its child must
/// launch it through [`spawn_confined`] rather than a `Command`.
pub fn confined_launch_applies() -> bool {
    super::active_sandbox_policy().is_some()
}

/// Launch `program` confined by the active policy and return the live child,
/// for a caller that streams, times out or cancels it. `None` when no policy
/// confines this spawn, so the caller spawns it the ordinary way.
///
/// The child's Job Object has no process or memory cap: an agent's command
/// runs builds that a script's one-shot cap would kill.
pub fn spawn_confined(
    program: &str,
    args: &[String],
    config: &ProcessCommandConfig,
    stdio: launch::ChildStdio,
) -> Result<Option<launch::ConfinedChild>, VmError> {
    let Some((policy, _)) = super::active_sandbox_policy() else {
        return Ok(None);
    };
    launch::launch(program, args, config, &policy, stdio, JobLimits::Unbounded)
        .map(Some)
        .map_err(|error| {
            process_spawn_error(&error)
                .unwrap_or_else(|| sandbox_rejection(format!("process sandbox failed: {error}")))
        })
}

pub(super) fn sandboxed_output(
    program: &str,
    args: &[String],
    config: &ProcessCommandConfig,
    policy: &CapabilityPolicy,
) -> io::Result<Output> {
    let stdin = match &config.stdin {
        super::ProcessStdin::Null => launch::ChildInput::Null,
        super::ProcessStdin::Bytes(_) => launch::ChildInput::Pipe,
    };
    let mut child = launch::launch(
        program,
        args,
        config,
        policy,
        launch::ChildStdio {
            stdin,
            stdout: launch::ChildOutput::Pipe,
            stderr: launch::ChildOutput::Pipe,
        },
        JobLimits::Bounded,
    )?;
    let stdin_writer = match (&config.stdin, child.take_stdin()) {
        (super::ProcessStdin::Bytes(input), Some(mut pipe)) => {
            let input = input.clone();
            Some(std::thread::spawn(move || pipe.write_all(&input)))
        }
        _ => None,
    };
    let stdout = child.take_stdout().map(read_to_end_async);
    let stderr = child.take_stderr().map(read_to_end_async);
    let status = child.wait()?;
    let stdout = stdout.map(join_reader).transpose()?.unwrap_or_default();
    let stderr = stderr.map(join_reader).transpose()?.unwrap_or_default();
    if let Some(stdin_writer) = stdin_writer {
        stdin_writer
            .join()
            .map_err(|_| io::Error::other("stdin writer thread panicked"))??;
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn read_to_end_async(mut file: std::fs::File) -> std::thread::JoinHandle<io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut output = Vec::new();
        file.read_to_end(&mut output)?;
        Ok(output)
    })
}

/// How much a confined child's Job Object lets its tree use.
#[derive(Clone, Copy)]
pub(crate) enum JobLimits {
    /// At most 32 processes of 512 MiB each: a script's one-shot command.
    Bounded,
    /// No process or memory cap: an agent's command, such as a build, which
    /// runs today without one and would be killed mid-link under the caps.
    Unbounded,
}

struct JobObject {
    handle: OwnedHandle,
}

// A Job Object handle may be used and closed from any thread.
unsafe impl Sync for JobObject {}

impl JobObject {
    fn create(bounds: JobLimits) -> io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        let handle = OwnedHandle::new_checked(handle)?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        if let JobLimits::Bounded = bounds {
            limits.BasicLimitInformation.LimitFlags |=
                JOB_OBJECT_LIMIT_ACTIVE_PROCESS | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            limits.BasicLimitInformation.ActiveProcessLimit = 32;
            limits.ProcessMemoryLimit = 512 * 1024 * 1024;
        }
        set_job_info(handle.raw(), JobObjectExtendedLimitInformation, &limits)?;
        let restrictions = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT_HANDLES
                | JOB_OBJECT_UILIMIT_READCLIPBOARD
                | JOB_OBJECT_UILIMIT_WRITECLIPBOARD
                | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                | JOB_OBJECT_UILIMIT_GLOBALATOMS
                | JOB_OBJECT_UILIMIT_DESKTOP
                | JOB_OBJECT_UILIMIT_EXITWINDOWS,
        };
        set_job_info(handle.raw(), JobObjectBasicUIRestrictions, &restrictions)?;
        Ok(Self { handle })
    }

    fn assign(&self, process: HANDLE) -> io::Result<()> {
        if unsafe { AssignProcessToJobObject(self.handle.raw(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn terminate(&self) {
        unsafe {
            TerminateJobObject(self.handle.raw(), 1);
        }
    }
}

fn set_job_info<T>(job: HANDLE, class: i32, value: &T) -> io::Result<()> {
    if unsafe {
        SetInformationJobObject(
            job,
            class,
            std::ptr::from_ref(value).cast(),
            std::mem::size_of::<T>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct InheritablePipe {
    read: OwnedHandle,
    write: OwnedHandle,
}

impl InheritablePipe {
    fn new() -> io::Result<Self> {
        let mut sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let mut read = std::ptr::null_mut();
        let mut write = std::ptr::null_mut();
        if unsafe { CreatePipe(&mut read, &mut write, &mut sa, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { SetHandleInformation(read, HANDLE_FLAG_INHERIT, 0) } == 0 {
            unsafe {
                CloseHandle(read);
                CloseHandle(write);
            }
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            read: OwnedHandle::new(read),
            write: OwnedHandle::new(write),
        })
    }

    /// The parent's read end and the child's inheritable write end.
    fn into_parts(self) -> (OwnedHandle, OwnedHandle) {
        (self.read, self.write)
    }
}

struct InheritableStdinPipe {
    child_read: Option<OwnedHandle>,
    write: Option<OwnedHandle>,
}

impl InheritableStdinPipe {
    fn new() -> io::Result<Self> {
        let mut sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let mut read = std::ptr::null_mut();
        let mut write = std::ptr::null_mut();
        if unsafe { CreatePipe(&mut read, &mut write, &mut sa, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { SetHandleInformation(write, HANDLE_FLAG_INHERIT, 0) } == 0 {
            unsafe {
                CloseHandle(read);
                CloseHandle(write);
            }
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            child_read: Some(OwnedHandle::new(read)),
            write: Some(OwnedHandle::new(write)),
        })
    }

    /// The child's inheritable read end.
    fn take_child_read(&mut self) -> OwnedHandle {
        self.child_read
            .take()
            .expect("child read end already taken")
    }

    /// The parent's write end. Dropping it closes the child's input.
    fn into_writer(mut self) -> std::fs::File {
        let handle = self.write.take().expect("stdin writer already consumed");
        unsafe { std::fs::File::from_raw_handle(handle.into_raw().cast()) }
    }
}

struct OwnedHandle(HANDLE);

unsafe impl Send for OwnedHandle {}

impl OwnedHandle {
    fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    fn new_checked(handle: HANDLE) -> io::Result<Self> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }

    fn nul_read() -> io::Result<Self> {
        Self::nul(GENERIC_READ)
    }

    fn nul_write() -> io::Result<Self> {
        Self::nul(GENERIC_WRITE)
    }

    fn nul(access: u32) -> io::Result<Self> {
        let mut sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let path = str_to_wide("NUL");
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &mut sa,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        Self::new_checked(handle)
    }

    fn raw(&self) -> HANDLE {
        self.0
    }

    fn into_raw(mut self) -> HANDLE {
        let handle = self.0;
        self.0 = std::ptr::null_mut();
        handle
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

fn join_reader(handle: std::thread::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    handle
        .join()
        .map_err(|_| io::Error::other("process pipe reader thread panicked"))?
}

struct ProcThreadAttributes {
    buffer: Vec<u8>,
}

impl ProcThreadAttributes {
    fn new(count: u32) -> io::Result<Self> {
        let mut size = 0usize;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size);
        }
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; size];
        if unsafe {
            InitializeProcThreadAttributeList(buffer.as_mut_ptr().cast(), count, 0, &mut size)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { buffer })
    }

    fn update(
        &mut self,
        attribute: usize,
        value: *const std::ffi::c_void,
        size: usize,
    ) -> io::Result<()> {
        if unsafe {
            UpdateProcThreadAttribute(
                self.as_mut_ptr(),
                0,
                attribute,
                value,
                size,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn as_mut_ptr(&mut self) -> *mut std::ffi::c_void {
        self.buffer.as_mut_ptr().cast()
    }
}

impl Drop for ProcThreadAttributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.buffer.as_mut_ptr().cast());
        }
    }
}

fn sandbox_trace(label: &str, message: impl AsRef<str>) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var_os("HARN_WINDOWS_SANDBOX_TRACE").is_some()) {
        return;
    }
    eprintln!("[harn windows sandbox {label}] {}", message.as_ref());
}

fn command_line(program: &str, args: &[String]) -> Vec<u16> {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(quote_arg(OsStr::new(program)));
    parts.extend(args.iter().map(|arg| quote_arg(OsStr::new(arg))));
    str_to_wide(&parts.join(" "))
}

fn quote_arg(arg: &OsStr) -> String {
    let value: Vec<u16> = arg.encode_wide().collect();
    if value.is_empty() {
        return "\"\"".to_string();
    }
    let needs_quotes = value.iter().any(|ch| {
        *ch == b' ' as u16 || *ch == b'\t' as u16 || *ch == b'\n' as u16 || *ch == b'"' as u16
    });
    if !needs_quotes {
        return OsString::from_wide(&value).to_string_lossy().into_owned();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0usize;
    for ch in OsString::from_wide(&value).to_string_lossy().chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                quoted.push(ch);
            }
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

fn resolve_application_name(program: &str) -> Option<Vec<u16>> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        Some(path_to_wide(path))
    } else {
        None
    }
}

fn environment_block(
    overrides: &[(String, String)],
    sandbox_overrides: &[(String, String)],
    closed_env: bool,
    removed: &[String],
) -> Vec<u16> {
    let mut values: Vec<(String, String)> = if closed_env {
        Vec::new()
    } else {
        std::env::vars().collect()
    };
    upsert_env_pairs(&mut values, overrides);
    values.retain(|(key, _)| {
        !removed
            .iter()
            .any(|removed| key.eq_ignore_ascii_case(removed))
    });
    upsert_env_pairs(&mut values, sandbox_overrides);
    values.sort_by(|left, right| {
        left.0
            .to_ascii_uppercase()
            .cmp(&right.0.to_ascii_uppercase())
    });

    let mut block = Vec::new();
    for (key, value) in values {
        block.extend(OsStr::new(&format!("{key}={value}")).encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

fn upsert_env_pairs(values: &mut Vec<(String, String)>, updates: &[(String, String)]) {
    for (key, value) in updates {
        if let Some(existing) = values
            .iter_mut()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        {
            existing.1 = value.clone();
        } else {
            values.push((key.clone(), value.clone()));
        }
    }
}

fn path_to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn str_to_wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn wide_ptr_to_string(raw: *const u16) -> String {
    let mut len = 0usize;
    unsafe {
        while *raw.add(len) != 0 {
            len += 1;
        }
        OsString::from_wide(std::slice::from_raw_parts(raw, len))
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn workspace_policy(root: &Path, write: bool) -> CapabilityPolicy {
        let mut policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![root.display().to_string()],
            ..CapabilityPolicy::default()
        };
        if !write {
            policy.capabilities = std::collections::BTreeMap::from([(
                "workspace".to_string(),
                vec!["read_text".to_string()],
            )]);
        }
        policy
    }

    /// Removes a policy's scratch directory, and the entry granted on it, so
    /// a test leaves nothing outside its own temp dirs.
    struct ScratchCleanup(PathBuf);

    impl ScratchCleanup {
        fn for_policy(policy: &CapabilityPolicy) -> Self {
            let label = launch::policy_label(&acl_grants::policy_digest(policy));
            Self(launch::scratch_dir(&label))
        }
    }

    impl Drop for ScratchCleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn run(policy: &CapabilityPolicy, cwd: &Path, program: &str, args: &[&str]) -> Output {
        let config = ProcessCommandConfig {
            cwd: Some(cwd.to_path_buf()),
            ..ProcessCommandConfig::default()
        };
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        sandboxed_output(program, &args, &config, policy)
            .unwrap_or_else(|error| panic!("{program} {args:?} did not launch: {error}"))
    }

    fn describe(output: &Output) -> String {
        format!(
            "status={:#x} stdout={:?} stderr={:?}",
            output.status.code().unwrap_or_default() as u32,
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }

    /// One policy names one SID, so its grants can be reused; a policy that
    /// may not write names another, so it never inherits a write grant.
    #[test]
    fn a_policy_sid_is_named_by_its_grant_plan() {
        let workspace = tempfile::tempdir().expect("workspace");
        let writable = workspace_policy(workspace.path(), true);
        let read_only = workspace_policy(workspace.path(), false);
        let sid = |policy: &CapabilityPolicy| {
            token::Sid::for_policy_digest(&acl_grants::policy_digest(policy))
                .and_then(|sid| sid.to_sddl())
                .expect("policy sid")
        };
        assert_eq!(sid(&writable), sid(&writable));
        assert_ne!(sid(&writable), sid(&read_only));
        assert!(sid(&writable).starts_with("S-1-5-21-"));
    }

    /// The second spawn under a policy rewrites nothing, and it learns that
    /// from the disk, as a fresh process would.
    #[test]
    fn a_policy_pays_for_its_grants_once() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "x").expect("workspace file");
        let policy = workspace_policy(workspace.path(), true);
        let sid = token::Sid::for_policy_digest(&acl_grants::policy_digest(&policy)).expect("sid");
        let first =
            acl_grants::PolicyWriteGrants::grant("test", &sid, &policy, None).expect("grant");
        println!("first grant rewrote {:?}", first.rewritten);
        assert!(first.rewrites > 0, "the first spawn grants the workspace");
        acl_grants::forget_grants();
        let second =
            acl_grants::PolicyWriteGrants::grant("test", &sid, &policy, None).expect("grant");
        assert_eq!(
            second.rewrites, 0,
            "the second spawn found the grants in place; it rewrote {:?}",
            second.rewritten
        );
    }

    /// The programs an agent's commands are made of start under the
    /// restricted token and exit cleanly.
    #[test]
    fn windows_process_sandbox_starts_common_programs() {
        let workspace = tempfile::tempdir().expect("workspace");
        let policy = workspace_policy(workspace.path(), true);
        let _cleanup = ScratchCleanup::for_policy(&policy);
        let mut cases: Vec<(&str, Vec<&str>, Option<&str>)> = vec![
            ("cmd", vec!["/c", "echo", "ok"], Some("ok")),
            ("cmd", vec!["/c", "echo", "x>nul"], None),
            (
                "powershell",
                vec!["-NoProfile", "-Command", "'ps-ok'"],
                Some("ps-ok"),
            ),
        ];
        // git is on every CI runner; a host without it has nothing to start.
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            cases.push(("git", vec!["--version"], Some("git version")));
        }
        let failures: Vec<String> = cases
            .iter()
            .filter_map(|(program, args, expected)| {
                let output = run(&policy, workspace.path(), program, args);
                println!("observed {program} {args:?}: {}", describe(&output));
                let stdout = String::from_utf8_lossy(&output.stdout);
                let ok = output.status.success()
                    && expected.is_none_or(|expected| stdout.contains(expected));
                (!ok).then(|| format!("{program} {args:?}: {}", describe(&output)))
            })
            .collect();
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// The token confines writes: the workspace is writable, a directory the
    /// user may write but the policy was not granted is not.
    #[test]
    fn windows_process_sandbox_writes_only_inside_granted_roots() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let policy = workspace_policy(workspace.path(), true);
        let _cleanup = ScratchCleanup::for_policy(&policy);

        let inside_target = workspace.path().join("inside.txt");
        let inside = run(
            &policy,
            workspace.path(),
            "cmd",
            &["/c", "echo", &format!("x>{}", inside_target.display())],
        );
        println!("observed inside write: {}", describe(&inside));
        assert!(
            inside.status.success() && inside_target.exists(),
            "a write inside the workspace must succeed: {}",
            describe(&inside)
        );

        let outside_target = outside.path().join("outside.txt");
        let refused = run(
            &policy,
            workspace.path(),
            "cmd",
            &["/c", "echo", &format!("x>{}", outside_target.display())],
        );
        println!("observed outside write: {}", describe(&refused));
        assert!(
            !refused.status.success() && !outside_target.exists(),
            "a write outside the granted roots must be refused: {}",
            describe(&refused)
        );
    }

    /// MSYS programs (Git for Windows' grep and bash) die creating their
    /// shared-memory section under the restricted token. This asserts the
    /// documented failure so the test flips when harn#8811 fixes it.
    #[test]
    fn windows_process_sandbox_msys_programs_fail_known_issue_8811() {
        let usr_bin = Path::new("C:\\Program Files\\Git\\usr\\bin");
        if !usr_bin.join("bash.exe").exists() {
            println!(
                "skipped: no Git for Windows MSYS tools at {}",
                usr_bin.display()
            );
            return;
        }
        let workspace = tempfile::tempdir().expect("workspace");
        let policy = workspace_policy(workspace.path(), true);
        let _cleanup = ScratchCleanup::for_policy(&policy);
        for (program, args) in [
            (usr_bin.join("grep.exe"), vec!["--version"]),
            (usr_bin.join("bash.exe"), vec!["-c", "echo bash-ok"]),
        ] {
            let output = run(
                &policy,
                workspace.path(),
                &program.display().to_string(),
                &args,
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            println!("observed {}: {}", program.display(), describe(&output));
            assert!(
                !output.status.success() && stderr.contains("CreateFileMapping"),
                "{} changed behavior; if it now succeeds, harn#8811 is fixed and this test \
                 should assert success: {}",
                program.display(),
                describe(&output)
            );
        }
    }

    #[test]
    fn environment_block_sandbox_overrides_win_over_caller_temp() {
        let overrides = vec![
            ("TEMP".to_string(), "C:\\outside".to_string()),
            ("TMP".to_string(), "C:\\outside".to_string()),
            ("CUSTOM".to_string(), "kept".to_string()),
        ];
        let sandbox_overrides = vec![
            ("TEMP".to_string(), "C:\\workspace\\.harn-tmp".to_string()),
            ("TMP".to_string(), "C:\\workspace\\.harn-tmp".to_string()),
        ];

        let decoded = decode_environment_block(&environment_block(
            &overrides,
            &sandbox_overrides,
            false,
            &[],
        ));

        assert!(decoded.iter().any(|entry| entry == "CUSTOM=kept"));
        assert!(decoded
            .iter()
            .any(|entry| entry == "TEMP=C:\\workspace\\.harn-tmp"));
        assert!(decoded
            .iter()
            .any(|entry| entry == "TMP=C:\\workspace\\.harn-tmp"));
        assert!(!decoded.iter().any(|entry| entry == "TEMP=C:\\outside"));
        assert!(!decoded.iter().any(|entry| entry == "TMP=C:\\outside"));
    }

    #[test]
    fn environment_block_honors_closed_environment_and_removals() {
        let overrides = vec![
            ("KEEP".to_string(), "yes".to_string()),
            ("REMOVE".to_string(), "no".to_string()),
        ];
        let decoded = decode_environment_block(&environment_block(
            &overrides,
            &[],
            true,
            &["remove".to_string()],
        ));

        assert_eq!(decoded, vec!["KEEP=yes"]);
    }

    fn decode_environment_block(block: &[u16]) -> Vec<String> {
        block
            .split(|ch| *ch == 0)
            .filter(|part| !part.is_empty())
            .map(|part| OsString::from_wide(part).to_string_lossy().into_owned())
            .collect()
    }
}
