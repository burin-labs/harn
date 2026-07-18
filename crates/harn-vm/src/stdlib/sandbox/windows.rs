use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::FromRawHandle;
use std::os::windows::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, SetHandleInformation, GENERIC_READ, HANDLE, HANDLE_FLAG_INHERIT,
    INVALID_HANDLE_VALUE, WAIT_FAILED,
};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
    DeriveAppContainerSidFromAppContainerName, GetAppContainerFolderPath,
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
    JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_BASIC_UI_RESTRICTIONS,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_UILIMIT_DESKTOP,
    JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
    JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemServices::SE_GROUP_ENABLED;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ResumeThread, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use super::{
    policy_allows_network, policy_allows_workspace_write,
    process_sandbox_developer_toolchain_read_roots,
    process_sandbox_package_manager_config_read_roots, process_sandbox_policy_read_roots,
    process_sandbox_policy_write_roots, process_sandbox_readonly_roots, process_sandbox_roots,
    process_spawn_error, sandbox_rejection, unavailable, PrepareOutcome, ProcessCommandConfig,
    SandboxBackend,
};
use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::value::VmError;

pub(super) struct Backend;

impl SandboxBackend for Backend {
    fn name() -> &'static str {
        "windows"
    }

    fn available() -> bool {
        true
    }

    /// `std::process::Command` cannot carry an AppContainer
    /// `SECURITY_CAPABILITIES` block — Windows requires
    /// `STARTUPINFOEX` plumbing handled directly by `CreateProcessW`.
    /// Callers that need an `Output` go through [`Backend::run_to_output`];
    /// callers that need a `Command` (e.g. `harn-hostlib`'s background
    /// process spawner) get the warn-or-error fallback below.
    fn prepare_std_command(
        _program: &str,
        _args: &[String],
        _command: &mut std::process::Command,
        _policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        unavailable(
            "Windows process sandboxing requires command_output(); std_command_for() cannot attach an AppContainer to std::process::Command",
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
        unavailable(
            "Windows process sandboxing requires command_output(); tokio_command_for() cannot attach an AppContainer to tokio::process::Command",
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

static PROFILE_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(super) fn sandboxed_output(
    program: &str,
    args: &[String],
    config: &ProcessCommandConfig,
    policy: &CapabilityPolicy,
) -> io::Result<Output> {
    sandbox_trace(
        "pending",
        format!("start program={program:?} argc={}", args.len()),
    );
    let mut process_capabilities = ProcessCapabilities::for_policy(policy)?;
    let profile = AppContainerProfile::create(&mut process_capabilities)?;
    let trace_label = profile.label().to_string();
    sandbox_trace(&trace_label, "profile created");
    let sid_string = profile.sid_string()?;
    sandbox_trace(&trace_label, "sid resolved");
    let grants = WorkspaceAclGrants::grant(&trace_label, &sid_string, policy)?;
    let _grants = grants;
    sandbox_trace(&trace_label, "workspace ACL grants installed");

    let stdout_pipe = InheritablePipe::new()?;
    let stderr_pipe = InheritablePipe::new()?;
    let stdin = OwnedHandle::nul_read()?;
    sandbox_trace(&trace_label, "stdio handles prepared");
    let inherited_handles = [
        stdin.raw(),
        stdout_pipe.write.raw(),
        stderr_pipe.write.raw(),
    ];
    let mut security_capabilities = profile.security_capabilities(&mut process_capabilities);
    let mut attributes = ProcThreadAttributes::new(2)?;
    sandbox_trace(&trace_label, "process attributes allocated");
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
    sandbox_trace(&trace_label, "process attributes configured");

    let mut stdout_reader = stdout_pipe.into_reader();
    let mut stderr_reader = stderr_pipe.into_reader();

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin.raw();
    startup.StartupInfo.hStdOutput = stdout_reader.child_write_handle();
    startup.StartupInfo.hStdError = stderr_reader.child_write_handle();
    startup.lpAttributeList = attributes.as_mut_ptr();

    let mut process_info = PROCESS_INFORMATION::default();
    let mut command_line = command_line(program, args);
    let application = resolve_application_name(program);
    let sandbox_env = profile.environment_overrides(&sid_string)?;
    sandbox_trace(&trace_label, "AppContainer environment prepared");
    let mut environment = environment_block(&config.env, &sandbox_env);
    let cwd = config.cwd.as_ref().map(|path| path_to_wide(path));
    let job = JobObject::create()?;
    sandbox_trace(&trace_label, "job object prepared");

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
    sandbox_trace(&trace_label, "CreateProcessW ok");

    let process = OwnedHandle::new(process_info.hProcess);
    let thread = OwnedHandle::new(process_info.hThread);
    if let Err(error) = job.assign(process.raw()) {
        unsafe {
            TerminateProcess(process.raw(), 1);
        }
        return Err(error);
    }
    sandbox_trace(&trace_label, "job assigned");
    stdout_reader.close_child_write();
    stderr_reader.close_child_write();
    sandbox_trace(&trace_label, "parent child-write handles closed");

    if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    sandbox_trace(&trace_label, "process resumed");

    let stdout = stdout_reader.read_async();
    let stderr = stderr_reader.read_async();
    sandbox_trace(&trace_label, "waiting for process");
    let wait = unsafe { WaitForSingleObject(process.raw(), INFINITE) };
    if wait == WAIT_FAILED {
        return Err(io::Error::last_os_error());
    }
    sandbox_trace(&trace_label, "process signaled");

    let mut code = 1u32;
    if unsafe { GetExitCodeProcess(process.raw(), &mut code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    sandbox_trace(&trace_label, format!("exit code {code}"));

    sandbox_trace(&trace_label, "joining stdout reader");
    let stdout = join_reader(stdout)?;
    sandbox_trace(&trace_label, "joining stderr reader");
    let stderr = join_reader(stderr)?;
    sandbox_trace(&trace_label, "complete");
    Ok(Output {
        status: ExitStatus::from_raw(code),
        stdout,
        stderr,
    })
}

struct AppContainerProfile {
    name: Vec<u16>,
    label: String,
    sid: PSID,
}

impl AppContainerProfile {
    fn create(process_capabilities: &mut ProcessCapabilities) -> io::Result<Self> {
        let id = PROFILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("harn.sandbox.{}.{}", std::process::id(), id);
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

impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        unsafe {
            if !self.sid.is_null() {
                LocalFree(self.sid.cast());
            }
            DeleteAppContainerProfile(self.name.as_ptr());
        }
    }
}

struct WorkspaceAclGrants {
    label: String,
    sid: String,
    paths: Vec<PathBuf>,
}

impl WorkspaceAclGrants {
    fn grant(label: &str, sid: &str, policy: &CapabilityPolicy) -> io::Result<Self> {
        // Read-execute for the entire profile when writes are denied;
        // otherwise Modify on the writable roots. Read-only roots always
        // get read-execute regardless of the workspace-write capability.
        let workspace_permission = if policy_allows_workspace_write(policy) {
            "(OI)(CI)M"
        } else {
            "(OI)(CI)RX"
        };
        let mut paths = Vec::new();
        let writable = process_sandbox_roots(policy)
            .into_iter()
            .map(|root| (root, workspace_permission, false));
        let read_only = process_sandbox_readonly_roots(policy)
            .into_iter()
            .map(|root| (root, "(OI)(CI)RX", false));
        let process_read = process_sandbox_policy_read_roots(policy)
            .into_iter()
            .map(|root| (root, "(OI)(CI)RX", false));
        let preset_roots = process_sandbox_preset_acl_roots(policy)
            .into_iter()
            .map(|root| (root, "(OI)(CI)RX", true));
        let process_write = if policy_allows_workspace_write(policy) {
            process_sandbox_policy_write_roots(policy)
                .into_iter()
                .map(|root| (root, workspace_permission, false))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for (root, permission, optional) in writable
            .chain(read_only)
            .chain(process_read)
            .chain(preset_roots)
            .chain(process_write)
        {
            if !root.exists() {
                if optional {
                    continue;
                }
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("sandbox workspace root '{}' does not exist", root.display()),
                ));
            }
            sandbox_trace(label, format!("icacls grant begin path={}", root.display()));
            run_icacls(
                &root,
                ["/grant", &format!("*{sid}:{permission}"), "/T", "/C"],
            )?;
            sandbox_trace(label, "icacls grant ok");
            paths.push(root);
        }
        Ok(Self {
            label: label.to_string(),
            sid: sid.to_string(),
            paths,
        })
    }
}

impl Drop for WorkspaceAclGrants {
    fn drop(&mut self) {
        for path in &self.paths {
            sandbox_trace(
                &self.label,
                format!("icacls remove begin path={}", path.display()),
            );
            match run_icacls(path, ["/remove:g", &format!("*{}", self.sid), "/T", "/C"]) {
                Ok(()) => sandbox_trace(&self.label, "icacls remove ok"),
                Err(error) => sandbox_trace(&self.label, format!("icacls remove failed: {error}")),
            }
        }
    }
}

fn process_sandbox_preset_acl_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    if policy.process_sandbox.presets.is_none() {
        return Vec::new();
    }

    process_sandbox_developer_toolchain_read_roots(policy)
        .into_iter()
        .chain(process_sandbox_package_manager_config_read_roots(policy))
        .collect()
}

struct JobObject {
    handle: OwnedHandle,
}

impl JobObject {
    fn create() -> io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        let handle = OwnedHandle::new_checked(handle)?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        limits.BasicLimitInformation.ActiveProcessLimit = 32;
        limits.ProcessMemoryLimit = 512 * 1024 * 1024;
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

    fn into_reader(self) -> PipeReader {
        PipeReader {
            read: Some(self.read),
            child_write: Some(self.write),
        }
    }
}

struct PipeReader {
    read: Option<OwnedHandle>,
    child_write: Option<OwnedHandle>,
}

impl PipeReader {
    fn child_write_handle(&self) -> HANDLE {
        self.child_write
            .as_ref()
            .map_or(std::ptr::null_mut(), OwnedHandle::raw)
    }

    fn close_child_write(&mut self) {
        self.child_write.take();
    }

    fn read_async(&mut self) -> std::thread::JoinHandle<io::Result<Vec<u8>>> {
        let handle = self.read.take().expect("pipe reader already consumed");
        std::thread::spawn(move || {
            let mut file = unsafe { std::fs::File::from_raw_handle(handle.into_raw().cast()) };
            let mut output = Vec::new();
            file.read_to_end(&mut output)?;
            Ok(output)
        })
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
        let mut sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let path = str_to_wide("NUL");
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
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
) -> Vec<u16> {
    let mut values: Vec<(String, String)> = std::env::vars().collect();
    upsert_env_pairs(&mut values, overrides);
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

        let decoded = decode_environment_block(&environment_block(&overrides, &sandbox_overrides));

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

    fn decode_environment_block(block: &[u16]) -> Vec<String> {
        block
            .split(|ch| *ch == 0)
            .filter(|part| !part.is_empty())
            .map(|part| OsString::from_wide(part).to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn implicit_default_presets_do_not_materialize_home_acl_roots() {
        let policy = CapabilityPolicy::default();

        assert!(
            process_sandbox_preset_acl_roots(&policy).is_empty(),
            "Windows should not recursively ACL home-scoped toolchain/cache roots unless presets were explicit"
        );
    }

    #[test]
    fn explicit_empty_presets_do_not_materialize_home_acl_roots() {
        let policy = CapabilityPolicy {
            process_sandbox: ProcessSandboxPolicy {
                presets: Some(Vec::new()),
                ..Default::default()
            },
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
            process_sandbox: ProcessSandboxPolicy {
                presets: Some(vec![
                    ProcessSandboxPreset::DeveloperToolchains,
                    ProcessSandboxPreset::PackageManagerConfig,
                ]),
                ..Default::default()
            },
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
