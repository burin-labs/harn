use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::process::Output;

use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, SetHandleInformation, GENERIC_READ, GENERIC_WRITE, HANDLE,
    HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName, GetAppContainerFolderPath,
};
use windows_sys::Win32::Security::{
    CreateWellKnownSid, WinCapabilityInternetClientSid, WinCapabilityPrivateNetworkClientServerSid,
    PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES, SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
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
use windows_sys::Win32::System::SystemServices::SE_GROUP_ENABLED;
use windows_sys::Win32::System::Threading::{
    DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, UpdateProcThreadAttribute,
};

use super::{
    policy_allows_network, process_sandbox_developer_toolchain_read_roots,
    process_sandbox_package_manager_config_read_roots, process_sandbox_path_read_roots,
    process_spawn_error, sandbox_rejection, unavailable, PrepareOutcome, ProcessCommandConfig,
    SandboxBackend,
};
use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::value::VmError;

// Declared here rather than in the sandbox module index: this backend is its
// only consumer, and the index is a platform-neutral surface.
#[path = "windows_system_roots.rs"]
mod system_roots;

// The ACL grant machinery answers a different question from process launch, and
// carries the measurements that justify each rule.
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
        "windows_app_container"
    }

    fn available() -> bool {
        true
    }

    /// `std::process::Command` cannot carry an AppContainer
    /// `SECURITY_CAPABILITIES` block — Windows requires
    /// `STARTUPINFOEX` plumbing handled directly by `CreateProcessW`.
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
        // Only `command_output()` owns the `STARTUPINFOEX` plumbing an
        // AppContainer needs; `std_command_for()` cannot carry one.
        unavailable(
            super::SandboxMechanism::WindowsAppContainer,
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
        // As above: `tokio_command_for()` cannot carry an AppContainer either.
        unavailable(
            super::SandboxMechanism::WindowsAppContainer,
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
        // `HARN_HANDLER_SANDBOX` is not `off`). The AppContainer
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

struct AppContainerProfile {
    name: Vec<u16>,
    label: String,
    sid: PSID,
}

impl AppContainerProfile {
    /// The container for `policy`, created on first use and reused after.
    /// See [`acl_grants::container_identity`] for why it is not per spawn.
    fn for_policy(
        policy: &CapabilityPolicy,
        process_capabilities: &mut ProcessCapabilities,
    ) -> io::Result<Self> {
        let name = acl_grants::container_identity(policy, policy_allows_network(policy));
        let wide_name = str_to_wide(&name);
        let display = str_to_wide("Harn Sandbox");
        let description = str_to_wide("Harn per-process capability sandbox");
        let mut sid = std::ptr::null_mut();
        let hr = unsafe {
            CreateAppContainerProfile(
                wide_name.as_ptr(),
                display.as_ptr(),
                description.as_ptr(),
                process_capabilities.attributes_mut_ptr(),
                process_capabilities.count(),
                &mut sid,
            )
        };
        if failed(hr) {
            let derived =
                unsafe { DeriveAppContainerSidFromAppContainerName(wide_name.as_ptr(), &mut sid) };
            if failed(derived) {
                return Err(io::Error::from_raw_os_error(derived));
            }
        }
        Ok(Self {
            name: wide_name,
            label: name,
            sid,
        })
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn security_capabilities(
        &self,
        process_capabilities: &mut ProcessCapabilities,
    ) -> SECURITY_CAPABILITIES {
        SECURITY_CAPABILITIES {
            AppContainerSid: self.sid,
            Capabilities: process_capabilities.attributes_mut_ptr(),
            CapabilityCount: process_capabilities.count(),
            Reserved: 0,
        }
    }

    fn sid_string(&self) -> io::Result<String> {
        let mut raw = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(self.sid, &mut raw) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = wide_ptr_to_string(raw);
        unsafe {
            LocalFree(raw.cast());
        }
        Ok(result)
    }

    fn local_app_data(&self, sid_string: &str) -> io::Result<PathBuf> {
        let wide_sid = str_to_wide(sid_string);
        let mut raw = std::ptr::null_mut();
        let hr = unsafe { GetAppContainerFolderPath(wide_sid.as_ptr(), &mut raw) };
        if failed(hr) {
            return Err(io::Error::from_raw_os_error(hr));
        }
        let path = wide_ptr_to_string(raw);
        unsafe {
            CoTaskMemFree(raw.cast());
        }
        Ok(PathBuf::from(path))
    }

    fn environment_overrides(&self, sid_string: &str) -> io::Result<Vec<(String, String)>> {
        let local_app_data = self.local_app_data(sid_string)?;
        let temp = local_app_data.join("Temp");
        std::fs::create_dir_all(&temp)?;
        Ok(vec![
            (
                "LOCALAPPDATA".to_string(),
                local_app_data.to_string_lossy().into_owned(),
            ),
            ("TEMP".to_string(), temp.to_string_lossy().into_owned()),
            ("TMP".to_string(), temp.to_string_lossy().into_owned()),
        ])
    }
}

struct ProcessCapabilities {
    // The attribute records point into these allocations. Boxes keep the SID
    // addresses stable if the owning vector or this struct moves.
    _sid_storage: Vec<Box<[u8; SECURITY_MAX_SID_SIZE as usize]>>,
    attributes: Vec<SID_AND_ATTRIBUTES>,
}

impl ProcessCapabilities {
    fn for_policy(policy: &CapabilityPolicy) -> io::Result<Self> {
        if !policy_allows_network(policy) {
            return Ok(Self {
                _sid_storage: Vec::new(),
                attributes: Vec::new(),
            });
        }

        let mut sid_storage = Vec::with_capacity(2);
        let mut attributes = Vec::with_capacity(2);
        for sid_type in [
            WinCapabilityInternetClientSid,
            WinCapabilityPrivateNetworkClientServerSid,
        ] {
            let mut sid = Box::new([0u8; SECURITY_MAX_SID_SIZE as usize]);
            let mut sid_size = SECURITY_MAX_SID_SIZE;
            if unsafe {
                CreateWellKnownSid(
                    sid_type,
                    std::ptr::null_mut(),
                    sid.as_mut_ptr().cast(),
                    &mut sid_size,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            attributes.push(SID_AND_ATTRIBUTES {
                Sid: sid.as_mut_ptr().cast(),
                Attributes: SE_GROUP_ENABLED as u32,
            });
            sid_storage.push(sid);
        }

        Ok(Self {
            _sid_storage: sid_storage,
            attributes,
        })
    }

    fn attributes_mut_ptr(&mut self) -> *mut SID_AND_ATTRIBUTES {
        if self.attributes.is_empty() {
            std::ptr::null_mut()
        } else {
            self.attributes.as_mut_ptr()
        }
    }

    fn count(&self) -> u32 {
        u32::try_from(self.attributes.len()).expect("process capability count fits in u32")
    }
}

/// Frees the SID only. The profile itself persists with its grants, which is
/// what lets the next spawn under the same policy skip them.
impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        unsafe {
            if !self.sid.is_null() {
                LocalFree(self.sid.cast());
            }
        }
    }
}

fn process_sandbox_preset_acl_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    // `presets: None` means "use the runtime defaults" per
    // `ProcessSandboxPolicy`'s own documented contract (types.rs), and those
    // defaults include `DeveloperToolchains` and `PackageManagerConfig`. This
    // used to short-circuit on the raw `None` field and return nothing,
    // silently granting neither preset's read roots on Windows for every
    // policy that never explicitly customized `process_sandbox.presets` —
    // the common case, since nothing in the burin-mini/playground path sets
    // it. `process_sandbox_developer_toolchain_read_roots` and
    // `process_sandbox_package_manager_config_read_roots` already resolve
    // presets correctly via `effective_presets()`, so this guard was both
    // redundant with their own checks and wrong when it disagreed with them
    // (harn#7993).
    process_sandbox_developer_toolchain_read_roots(policy)
        .into_iter()
        .chain(process_sandbox_package_manager_config_read_roots(policy))
        // Owned by `mod.rs` so the pre-launch coverage check reads the same
        // set this loop grants an ACE for; see
        // `process_sandbox_path_read_roots`. `optional: true` in the grant
        // loop above already skips an entry that is not on disk.
        .chain(process_sandbox_path_read_roots(policy))
        .collect()
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

fn run_icacls<const N: usize>(path: &Path, args: [&str; N]) -> io::Result<()> {
    let output = std::process::Command::new("icacls")
        .arg(path)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "icacls failed for '{}': {}{}",
                path.display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }
    Ok(())
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

fn failed(hr: i32) -> bool {
    hr < 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{ProcessSandboxPolicy, ProcessSandboxPreset};

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

    /// One policy names one container, so its grants can be reused; a policy
    /// that may not write names another, so it never inherits a write grant.
    #[test]
    fn a_container_is_named_by_its_grant_plan() {
        let workspace = tempfile::tempdir().expect("workspace");
        let writable = workspace_policy(workspace.path(), true);
        let read_only = workspace_policy(workspace.path(), false);
        assert_eq!(
            acl_grants::container_identity(&writable, false),
            acl_grants::container_identity(&writable, false)
        );
        assert_ne!(
            acl_grants::container_identity(&writable, false),
            acl_grants::container_identity(&read_only, false)
        );
        assert_ne!(
            acl_grants::container_identity(&writable, false),
            acl_grants::container_identity(&writable, true)
        );
    }

    /// The per-run grant is gone: the second spawn under a policy rewrites
    /// nothing, and it learns that from the disk, as a fresh process would.
    #[test]
    fn a_policy_pays_for_its_grants_once() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "x").expect("workspace file");
        let policy = workspace_policy(workspace.path(), true);
        let mut capabilities = ProcessCapabilities::for_policy(&policy).expect("capabilities");
        let profile =
            AppContainerProfile::for_policy(&policy, &mut capabilities).expect("container");
        let sid = profile.sid_string().expect("sid");
        let first = acl_grants::WorkspaceAclGrants::grant("test", &sid, &policy).expect("grant");
        assert!(first.rewrites > 0, "the first spawn grants the workspace");
        acl_grants::forget_container_grants();
        let second = acl_grants::WorkspaceAclGrants::grant("test", &sid, &policy).expect("grant");
        assert_eq!(
            second.rewrites, 0,
            "the second spawn found the grants in place"
        );
    }

    #[test]
    fn environment_block_forces_appcontainer_temp_roots() {
        let overrides = vec![
            ("TEMP".to_string(), "C:\\outside".to_string()),
            ("TMP".to_string(), "C:\\outside".to_string()),
            ("CUSTOM".to_string(), "kept".to_string()),
        ];
        let sandbox_overrides = vec![
            (
                "LOCALAPPDATA".to_string(),
                "C:\\Users\\runneradmin\\AppData\\Local\\Packages\\harn\\AC".to_string(),
            ),
            (
                "TEMP".to_string(),
                "C:\\Users\\runneradmin\\AppData\\Local\\Packages\\harn\\AC\\Temp".to_string(),
            ),
            (
                "TMP".to_string(),
                "C:\\Users\\runneradmin\\AppData\\Local\\Packages\\harn\\AC\\Temp".to_string(),
            ),
        ];

        let decoded = decode_environment_block(&environment_block(
            &overrides,
            &sandbox_overrides,
            false,
            &[],
        ));

        assert!(decoded.iter().any(|entry| entry == "CUSTOM=kept"));
        assert!(decoded.iter().any(|entry| entry
            == "LOCALAPPDATA=C:\\Users\\runneradmin\\AppData\\Local\\Packages\\harn\\AC"));
        assert!(decoded.iter().any(|entry| entry
            == "TEMP=C:\\Users\\runneradmin\\AppData\\Local\\Packages\\harn\\AC\\Temp"));
        assert!(decoded
            .iter()
            .any(|entry| entry
                == "TMP=C:\\Users\\runneradmin\\AppData\\Local\\Packages\\harn\\AC\\Temp"));
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

    /// `presets: None` means "use the runtime defaults", so an untouched policy
    /// must resolve to exactly what naming those defaults resolves to. Before
    /// harn#7993 this function short-circuited on the raw `None` field and
    /// returned nothing, so every policy that never customized
    /// `process_sandbox.presets` -- the common case -- silently lost both
    /// presets' read roots on Windows. Comparing the two policies rather than
    /// asserting a concrete path keeps the case meaningful on a host with no
    /// home directory, where both sides are legitimately empty.
    #[test]
    fn implicit_default_presets_match_explicitly_named_defaults() {
        let implicit = CapabilityPolicy::default();
        let explicit = CapabilityPolicy {
            process_sandbox: Box::new(ProcessSandboxPolicy {
                presets: Some(ProcessSandboxPreset::default_presets().to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(
            process_sandbox_preset_acl_roots(&implicit),
            process_sandbox_preset_acl_roots(&explicit),
            "an untouched policy must resolve the same Windows ACL roots as one naming the default presets"
        );
    }

    #[test]
    fn explicit_empty_presets_do_not_materialize_home_acl_roots() {
        let policy = CapabilityPolicy {
            process_sandbox: Box::new(ProcessSandboxPolicy {
                presets: Some(Vec::new()),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert!(process_sandbox_preset_acl_roots(&policy).is_empty());
    }

    #[test]
    fn explicit_home_presets_materialize_acl_roots_when_home_is_available() {
        if crate::user_dirs::home_dir().is_none() {
            return;
        }

        let policy = CapabilityPolicy {
            process_sandbox: Box::new(ProcessSandboxPolicy {
                presets: Some(vec![
                    ProcessSandboxPreset::DeveloperToolchains,
                    ProcessSandboxPreset::PackageManagerConfig,
                ]),
                ..Default::default()
            }),
            ..Default::default()
        };

        let roots = process_sandbox_preset_acl_roots(&policy);
        assert!(
            roots.iter().any(|path| path.ends_with(".cargo")),
            "explicit Windows preset requests should still materialize developer/package roots"
        );
    }

    #[test]
    fn network_policy_materializes_public_and_private_capabilities() {
        let denied = ProcessCapabilities::for_policy(&CapabilityPolicy {
            side_effect_level: Some("process_exec".to_string()),
            ..Default::default()
        })
        .expect("construct denied capability set");
        assert_eq!(denied.count(), 0);

        let allowed = ProcessCapabilities::for_policy(&CapabilityPolicy {
            side_effect_level: Some("network".to_string()),
            ..Default::default()
        })
        .expect("construct network capability set");
        assert_eq!(allowed.count(), 2);
        assert!(allowed.attributes.iter().all(|entry| !entry.Sid.is_null()));
        assert!(allowed
            .attributes
            .iter()
            .all(|entry| entry.Attributes == SE_GROUP_ENABLED as u32));
    }
}
