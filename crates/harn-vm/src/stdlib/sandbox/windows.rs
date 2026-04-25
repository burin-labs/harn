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
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
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
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ResumeThread, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use super::{policy_allows_workspace_write, process_sandbox_roots, ProcessCommandConfig};
use crate::orchestration::CapabilityPolicy;

static PROFILE_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(super) fn sandboxed_output(
    program: &str,
    args: &[String],
    config: &ProcessCommandConfig,
    policy: &CapabilityPolicy,
) -> io::Result<Output> {
    let profile = AppContainerProfile::create()?;
    let sid_string = profile.sid_string()?;
    let grants = WorkspaceAclGrants::grant(&sid_string, policy)?;
    let _grants = grants;

    let stdout_pipe = InheritablePipe::new()?;
    let stderr_pipe = InheritablePipe::new()?;
    let stdin = OwnedHandle::nul_read()?;
    let inherited_handles = [
        stdin.raw(),
        stdout_pipe.write.raw(),
        stderr_pipe.write.raw(),
    ];
    let mut security_capabilities = profile.security_capabilities();
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
    let mut environment = environment_block(&config.env);
    let cwd = config.cwd.as_ref().map(|path| path_to_wide(path));
    let job = JobObject::create()?;

    let created = unsafe {
        CreateProcessW(
            application
                .as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr()),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
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
    if let Err(error) = job.assign(process.raw()) {
        unsafe {
            TerminateProcess(process.raw(), 1);
        }
        return Err(error);
    }
    stdout_reader.close_child_write();
    stderr_reader.close_child_write();

    if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }

    let stdout = stdout_reader.read_async();
    let stderr = stderr_reader.read_async();
    let wait = unsafe { WaitForSingleObject(process.raw(), INFINITE) };
    if wait == WAIT_FAILED {
        return Err(io::Error::last_os_error());
    }

    let mut code = 1u32;
    if unsafe { GetExitCodeProcess(process.raw(), &mut code) } == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(Output {
        status: ExitStatus::from_raw(code),
        stdout: join_reader(stdout)?,
        stderr: join_reader(stderr)?,
    })
}

struct AppContainerProfile {
    name: Vec<u16>,
    sid: PSID,
}

impl AppContainerProfile {
    fn create() -> io::Result<Self> {
        let id = PROFILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("harn.sandbox.{}.{}", std::process::id(), id);
        let wide_name = str_to_wide(&name);
        let display = str_to_wide("Harn Sandbox");
        let description = str_to_wide("Harn per-process no-capability sandbox");
        let mut sid = std::ptr::null_mut();
        let hr = unsafe {
            CreateAppContainerProfile(
                wide_name.as_ptr(),
                display.as_ptr(),
                description.as_ptr(),
                std::ptr::null(),
                0,
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
            sid,
        })
    }

    fn security_capabilities(&self) -> SECURITY_CAPABILITIES {
        SECURITY_CAPABILITIES {
            AppContainerSid: self.sid,
            Capabilities: std::ptr::null_mut(),
            CapabilityCount: 0,
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
    sid: String,
    paths: Vec<PathBuf>,
}

impl WorkspaceAclGrants {
    fn grant(sid: &str, policy: &CapabilityPolicy) -> io::Result<Self> {
        let permission = if policy_allows_workspace_write(policy) {
            "(OI)(CI)M"
        } else {
            "(OI)(CI)RX"
        };
        let mut paths = Vec::new();
        for root in process_sandbox_roots(policy) {
            if !root.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("sandbox workspace root '{}' does not exist", root.display()),
                ));
            }
            run_icacls(
                &root,
                ["/grant", &format!("*{sid}:{permission}"), "/T", "/C"],
            )?;
            paths.push(root);
        }
        Ok(Self {
            sid: sid.to_string(),
            paths,
        })
    }
}

impl Drop for WorkspaceAclGrants {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = run_icacls(path, ["/remove:g", &format!("*{}", self.sid), "/T", "/C"]);
        }
    }
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

        let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT_HANDLES
                | JOB_OBJECT_UILIMIT_READCLIPBOARD
                | JOB_OBJECT_UILIMIT_WRITECLIPBOARD
                | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                | JOB_OBJECT_UILIMIT_GLOBALATOMS
                | JOB_OBJECT_UILIMIT_DESKTOP
                | JOB_OBJECT_UILIMIT_EXITWINDOWS,
        };
        set_job_info(handle.raw(), JobObjectBasicUIRestrictions, &ui)?;
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

fn environment_block(overrides: &[(String, String)]) -> Vec<u16> {
    let mut values: Vec<(String, String)> = std::env::vars().collect();
    for (key, value) in overrides {
        if let Some(existing) = values
            .iter_mut()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        {
            existing.1 = value.clone();
        } else {
            values.push((key.clone(), value.clone()));
        }
    }
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
