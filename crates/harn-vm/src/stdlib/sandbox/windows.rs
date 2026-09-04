use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
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

// Declared here rather than in the sandbox module index: this backend is its
// only consumer, and the index is a platform-neutral surface.
#[path = "windows_system_roots.rs"]
mod system_roots;

use system_roots::{
    broad_system_root, hosts_an_executable, system_read_roots, tree_entry_count_within,
};

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

static PROFILE_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(super) fn sandboxed_output(
    program: &str,
    args: &[String],
    config: &ProcessCommandConfig,
    policy: &CapabilityPolicy,
) -> io::Result<Output> {
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
    let mut stdin_pipe = match &config.stdin {
        super::ProcessStdin::Null => None,
        super::ProcessStdin::Bytes(_) => Some(InheritableStdinPipe::new()?),
    };
    let stdin_null = if stdin_pipe.is_none() {
        Some(OwnedHandle::nul_read()?)
    } else {
        None
    };
    let stdin_handle = stdin_pipe.as_ref().map_or_else(
        || stdin_null.as_ref().expect("null stdin exists").raw(),
        InheritableStdinPipe::child_read_handle,
    );
    sandbox_trace(&trace_label, "stdio handles prepared");
    let inherited_handles = [
        stdin_handle,
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
    startup.StartupInfo.hStdInput = stdin_handle;
    startup.StartupInfo.hStdOutput = stdout_reader.child_write_handle();
    startup.StartupInfo.hStdError = stderr_reader.child_write_handle();
    startup.lpAttributeList = attributes.as_mut_ptr();

    let mut process_info = PROCESS_INFORMATION::default();
    let mut command_line = command_line(program, args);
    let application = resolve_application_name(program);
    let sandbox_env = profile.environment_overrides(&sid_string)?;
    sandbox_trace(&trace_label, "AppContainer environment prepared");
    let mut environment = environment_block(
        &config.env,
        &sandbox_env,
        config.closed_env,
        &config.env_remove,
    );
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
    if let Some(pipe) = stdin_pipe.as_mut() {
        pipe.close_child_read();
    }
    sandbox_trace(&trace_label, "parent child-write handles closed");

    if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    sandbox_trace(&trace_label, "process resumed");

    let stdin_writer = stdin_pipe.map(|pipe| match &config.stdin {
        super::ProcessStdin::Bytes(input) => pipe.write_async(input.clone()),
        super::ProcessStdin::Null => unreachable!("null stdin does not create a pipe"),
    });

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
    if let Some(stdin_writer) = stdin_writer {
        stdin_writer
            .join()
            .map_err(|_| io::Error::other("stdin writer thread panicked"))??;
    }
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
    /// Only the grants made to this spawn's own container SID. The persistent
    /// system read grants are deliberately absent: see [`Grantee`].
    paths: Vec<PathBuf>,
}

/// The well-known group every AppContainer token carries. Windows itself puts
/// it on `C:\Windows`, `C:\Program Files` and everything that inherits from
/// them, which is why a sandboxed child can already run `cmd.exe`.
const ALL_APPLICATION_PACKAGES_SID: &str = "S-1-15-2-1";

/// Who an ACL grant names, which is also what decides its lifetime.
///
/// The two answers are not interchangeable, and picking the wrong one is what
/// made this backend unusable. An ACL grant on Windows is a recursive rewrite
/// (inheritance is not retroactive, so an inheritable entry placed on a
/// directory does not reach the files already inside it), and a grant named
/// for one spawn has to be taken away again when that spawn ends. Measured on
/// a Windows 11 host, one such rewrite over a Node install of 2449 files costs
/// ~1s, and the matching removal costs the same again — per spawn, forever,
/// for every toolchain the agent might use.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Grantee {
    /// This spawn's own AppContainer SID. No other principal can use the
    /// grant and the SID dies with the spawn, so the grant is removed on
    /// drop. Correct for the workspace, whose contents are this run's alone.
    ThisContainer,
    /// [`ALL_APPLICATION_PACKAGES_SID`]. Correct for a host toolchain
    /// directory, and it is what makes the cost bounded: the grant is the
    /// read-execute entry the rest of `C:\Program Files` already carries, so
    /// it is neither per-spawn nor removed. Every later spawn's cheap
    /// non-recursive probe then sees the entry and skips the rewrite
    /// entirely, which on the same host is a 5ms read in place of a 1s
    /// rewrite.
    ///
    /// Two consequences worth stating plainly, because both outlive the
    /// spawn. The entry is readable by every sandboxed program on the
    /// machine, not only by ours; it is read-execute, and it is the
    /// permission the installer's own prefix already grants, but it is a
    /// widening. And `icacls /grant` clears the directory's protected-DACL
    /// flag, so a directory whose installer had detached it from
    /// `C:\Program Files` starts inheriting from that prefix again. Measured,
    /// not assumed: the Node installer detaches exactly this way, and
    /// granting reattached it.
    EveryAppContainer,
}

/// Whether a root has to be on disk for the spawn to be well-formed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MustExist {
    Yes,
    No,
}

/// What a failed grant costs, which is what decides whether the spawn can
/// continue without it. This is the one place the distinction lives, so a new
/// root source has to answer the question rather than inherit an answer.
#[derive(Clone, Copy)]
enum GrantIs {
    /// The child cannot do its job without this grant.
    LoadBearing,
    /// The grant only widens what the child can read.
    BestEffort,
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
        let writable = process_sandbox_roots(policy).into_iter().map(|root| {
            (
                root,
                workspace_permission,
                MustExist::Yes,
                GrantIs::LoadBearing,
                Grantee::ThisContainer,
            )
        });
        let read_only = process_sandbox_readonly_roots(policy)
            .into_iter()
            .map(|root| {
                (
                    root,
                    "(OI)(CI)RX",
                    MustExist::Yes,
                    GrantIs::BestEffort,
                    Grantee::ThisContainer,
                )
            });
        let process_read = process_sandbox_policy_read_roots(policy)
            .into_iter()
            .map(|root| {
                (
                    root,
                    "(OI)(CI)RX",
                    MustExist::Yes,
                    GrantIs::BestEffort,
                    Grantee::ThisContainer,
                )
            });
        let preset_roots = process_sandbox_preset_acl_roots(policy)
            .into_iter()
            .map(|root| {
                (
                    root,
                    "(OI)(CI)RX",
                    MustExist::No,
                    GrantIs::BestEffort,
                    Grantee::ThisContainer,
                )
            });
        // Host toolchains on PATH that the container cannot already read.
        // Unlike every other entry here this set is not preset-gated: the
        // product contract is reads-open on every profile, and a child that
        // cannot read the interpreter its command names fails with a message
        // that blames PATH rather than the sandbox.
        //
        // The write roots are what a PATH root can already be covered by
        // without being granted itself. The read roots are deliberately NOT
        // listed here: whether each of those is really granted is decided
        // under the cost budget, so the selection tracks them as it accepts
        // them rather than assuming them.
        let write_roots: Vec<PathBuf> = process_sandbox_roots(policy)
            .into_iter()
            .chain(process_sandbox_policy_write_roots(policy))
            .collect();
        // One cost discipline for every read-only grant this spawn would make,
        // computed in the same order the loop below grants them.
        let unaffordable = unaffordable_read_roots(policy, &write_roots);
        let system_read = system_read_roots().into_iter().map(|root| {
            (
                root,
                "(OI)(CI)RX",
                MustExist::No,
                GrantIs::BestEffort,
                Grantee::EveryAppContainer,
            )
        });
        let process_write = if policy_allows_workspace_write(policy) {
            process_sandbox_policy_write_roots(policy)
                .into_iter()
                .map(|root| {
                    (
                        root,
                        workspace_permission,
                        MustExist::Yes,
                        GrantIs::LoadBearing,
                        Grantee::ThisContainer,
                    )
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for (root, permission, must_exist, grant_is, grantee) in writable
            .chain(read_only)
            .chain(process_read)
            .chain(preset_roots)
            .chain(system_read)
            .chain(process_write)
        {
            if !root.exists() {
                if must_exist == MustExist::No {
                    continue;
                }
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("sandbox workspace root '{}' does not exist", root.display()),
                ));
            }
            // Read grants the cost discipline ruled out. Checked after the
            // existence handling above so a missing load-bearing root still
            // fails the spawn rather than being quietly skipped.
            if unaffordable.contains(&root) {
                continue;
            }
            let grantee_sid = match grantee {
                Grantee::ThisContainer => sid,
                Grantee::EveryAppContainer => ALL_APPLICATION_PACKAGES_SID,
            };
            sandbox_trace(
                label,
                format!(
                    "icacls grant begin path={} grantee={}",
                    root.display(),
                    match grantee {
                        Grantee::ThisContainer => "this-container",
                        Grantee::EveryAppContainer => "every-app-container",
                    }
                ),
            );
            let granted = run_icacls(
                &root,
                [
                    "/grant",
                    &format!("*{grantee_sid}:{permission}"),
                    "/T",
                    "/C",
                ],
            );
            match (granted, grant_is) {
                (Ok(()), _) => {
                    sandbox_trace(label, "icacls grant ok");
                    // Only a grant named for this spawn's own container SID is
                    // recorded for removal. A read-execute entry for every
                    // AppContainer is shared state that outlives this spawn by
                    // design, and taking it away again would both restore the
                    // per-spawn cost and race any concurrent spawn relying on
                    // it.
                    if grantee == Grantee::ThisContainer {
                        paths.push(root);
                    }
                }
                // A write grant the child depends on. Without it the child
                // cannot write its own workspace, which is not a usable
                // sandbox, so the spawn fails rather than running crippled.
                (Err(error), GrantIs::LoadBearing) => return Err(error),
                // A read grant. Failing it leaves the child seeing that
                // directory as read-closed, which is exactly the behavior
                // before this root was ever attempted — a narrower sandbox,
                // not a broken one. An unelevated caller cannot rewrite a
                // system directory's ACL, and that must not take every
                // command on the machine down with it.
                (Err(error), GrantIs::BestEffort) => sandbox_trace(
                    label,
                    format!(
                        "icacls grant failed, continuing read-closed for this root: path={} error={error}",
                        root.display()
                    ),
                ),
            }
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

/// Ceiling on how many read roots one spawn grants. Each grant
/// is a recursive ACL rewrite, so the set has to stay small even on a host
/// whose PATH is mostly closed; a host that needs more than this many is
/// telling us per-directory grants are the wrong mechanism, not that the
/// limit should rise.
///
/// The ceiling binds only on a host's first spawn. The grants name
/// [`Grantee::EveryAppContainer`] and are not removed, so every later spawn's
/// probe finds the entry already there and skips the rewrite.
const SYSTEM_READ_GRANT_LIMIT: usize = 24;

/// Ceiling on the total number of filesystem entries one spawn will rewrite
/// permissions on, across every read root it grants.
///
/// A count of directories is the wrong unit, because directories differ by
/// orders of magnitude: a Node install is ~2,400 entries and a cargo target
/// directory is tens of thousands. Measured on a Windows 11 host the rewrite
/// runs at roughly 2,500 entries per second, so this budget bounds the work
/// to a few seconds — paid once on a host, because the grants persist and
/// every later spawn's probe skips them.
const SYSTEM_READ_GRANT_ENTRY_BUDGET: usize = 32768;

/// Every read-only root this spawn should NOT grant, decided once for all
/// four read sources under one budget.
///
/// Read grants are the only ones with a cost problem, and they all have the
/// same one: each is a recursive ACL rewrite whose price is the size of the
/// tree. Deciding that per source is how the backend ended up unusable, so the
/// decision lives here and nowhere else. Load-bearing workspace write grants
/// are deliberately not routed through this: the spawn needs them, so their
/// cost is not optional and skipping one would produce a sandbox that cannot
/// do its job.
///
/// The order matters, because the budget is finite and spent in order. It
/// matches the order the grant loop uses, so what this rules out is exactly
/// what that loop would otherwise have paid for.
///
/// Two measurements from a Windows 11 developer machine shaped this:
///
/// * The home toolchain roots (`.cargo`, `.rustup`, `.cache`) are enormous on
///   any machine that has actually built something. Granting them per spawn
///   and removing them again afterwards is what made every sandboxed command
///   time out at two minutes — not the `PATH` roots this module was written
///   for (harn#7993, harn#8004).
/// * `PATH` on a build host is mostly cargo build output directories, which
///   hold object files and no executable. [`hosts_an_executable`] is what
///   stops them crowding out the Node install they were hiding.
///
/// A root ruled out here is simply not opened to the child, which is a
/// narrower sandbox rather than a broken one. A root that does not exist is
/// never ruled out, so the caller's own existence handling still decides
/// whether a missing load-bearing root fails the spawn.
fn unaffordable_read_roots(
    policy: &CapabilityPolicy,
    write_roots: &[PathBuf],
) -> BTreeSet<PathBuf> {
    let mut skip = BTreeSet::new();
    let mut remaining_entry_budget = SYSTEM_READ_GRANT_ENTRY_BUDGET;
    let mut granted = 0usize;
    // What a `PATH` root can be covered by, grown as roots are accepted. A
    // proposed root is not a granted one: `~/.cargo` is a read root on every
    // developer machine and is far too large to grant, so treating it as
    // covering `~/.cargo\bin` would leave the child unable to read either.
    let mut covered_by: Vec<PathBuf> = write_roots.to_vec();

    // Same order as the grant loop: the policy's own read roots first, then
    // the roots discovered from `PATH`.
    let candidates = process_sandbox_readonly_roots(policy)
        .into_iter()
        .chain(process_sandbox_policy_read_roots(policy))
        .chain(process_sandbox_preset_acl_roots(policy))
        .map(|root| (root, false))
        .chain(system_read_roots().into_iter().map(|root| (root, true)));

    for (root, from_path) in candidates {
        if skip.contains(&root) || !root.exists() {
            continue;
        }
        if granted == SYSTEM_READ_GRANT_LIMIT {
            read_root_decision(
                &root,
                &format!(
                    "probe=unprobed action=skipped reason=grant-limit-{SYSTEM_READ_GRANT_LIMIT}-reached elapsed_ms=0"
                ),
            );
            skip.insert(root);
            continue;
        }
        // The next three rules apply only to roots discovered from `PATH`. A
        // root the policy names is there because an embedder asked for it, so
        // it is not ours to second-guess on shape — only on cost.
        if from_path {
            if broad_system_root(&root) {
                read_root_decision(
                    &root,
                    "probe=skipped action=skipped reason=broad-system-prefix elapsed_ms=0",
                );
                skip.insert(root);
                continue;
            }
            if covered_by.iter().any(|already| root.starts_with(already)) {
                read_root_decision(
                    &root,
                    "probe=skipped action=skipped reason=already-granted-by-another-root elapsed_ms=0",
                );
                skip.insert(root);
                continue;
            }
            if !hosts_an_executable(&root) {
                read_root_decision(
                    &root,
                    "probe=skipped action=skipped reason=no-executable-in-directory elapsed_ms=0",
                );
                skip.insert(root);
                continue;
            }
        }
        let started = std::time::Instant::now();
        let readable = app_container_can_already_read(&root);
        let elapsed_ms = started.elapsed().as_millis();
        if readable {
            read_root_decision(
                &root,
                &format!(
                    "probe=already-open action=skipped reason=admits-all-application-packages elapsed_ms={elapsed_ms}"
                ),
            );
            skip.insert(root);
            continue;
        }
        // The rewrite costs one unit per entry, so the remaining budget is
        // what decides whether this root is affordable. Asking is bounded by
        // the budget, so the question stays cheap even when the tree is vast.
        let Some(entries) = tree_entry_count_within(&root, remaining_entry_budget) else {
            read_root_decision(
                &root,
                &format!(
                    "probe=closed action=skipped reason=tree-exceeds-remaining-entry-budget-{remaining_entry_budget} elapsed_ms={elapsed_ms}"
                ),
            );
            skip.insert(root);
            continue;
        };
        remaining_entry_budget -= entries;
        granted += 1;
        covered_by.push(root.clone());
        read_root_decision(
            &root,
            &format!(
                "probe=closed action=will-grant reason=no-all-application-packages-entry entries={entries} remaining_budget={remaining_entry_budget} elapsed_ms={elapsed_ms}"
            ),
        );
    }
    skip
}

/// One line per read root, naming what was decided and why. A spawn that
/// takes too long or a toolchain the child cannot see is unreadable without
/// it, and both were diagnosed from exactly these lines.
fn read_root_decision(root: &Path, outcome: &str) {
    sandbox_trace(
        "read-roots",
        format!("decision path={} {outcome}", root.display()),
    );
}

/// Whether `path`'s DACL already admits every AppContainer, i.e. carries an
/// `ALL APPLICATION PACKAGES` (`S-1-15-2-1`) entry. Read through `icacls`
/// rather than `GetNamedSecurityInfoW` so the check uses the same mechanism
/// the grants do and stays free of hand-rolled ACL walking. This read is
/// non-recursive and costs milliseconds, unlike the `/T` grant it avoids.
///
/// Cached per path: host permissions do not change under us, and the same
/// roots are re-examined on every spawn.
///
/// A DACL that cannot be read counts as already-open. That is the cheap
/// direction and it matches what this backend did before the probe existed:
/// an unreadable DACL never adds a recursive ACL rewrite.
fn app_container_can_already_read(path: &Path) -> bool {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, bool>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Ok(map) = cache.lock() {
        if let Some(known) = map.get(path) {
            return *known;
        }
    }
    let Ok(dacl) = read_icacls(path) else {
        sandbox_trace(
            "system-read-roots",
            format!("DACL unreadable, treated as open path={}", path.display()),
        );
        return true;
    };
    let dacl = dacl.to_ascii_uppercase();
    // The friendly name is localized, so match the raw SID too. `icacls`
    // renders an unresolved SID as `*S-1-15-2-1:(...)`; the trailing colon
    // keeps `S-1-15-2-1` from matching a longer sibling SID.
    let readable = dacl.contains("ALL APPLICATION PACKAGES") || dacl.contains("S-1-15-2-1:");
    if let Ok(mut map) = cache.lock() {
        map.insert(path.to_path_buf(), readable);
    }
    readable
}

fn read_icacls(path: &Path) -> io::Result<String> {
    let output = std::process::Command::new("icacls").arg(path).output()?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("icacls read failed for '{}'", path.display()),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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

    fn child_read_handle(&self) -> HANDLE {
        self.child_read
            .as_ref()
            .map_or(std::ptr::null_mut(), OwnedHandle::raw)
    }

    fn close_child_read(&mut self) {
        self.child_read.take();
    }

    fn write_async(mut self, input: Vec<u8>) -> std::thread::JoinHandle<io::Result<()>> {
        let handle = self.write.take().expect("stdin writer already consumed");
        std::thread::spawn(move || {
            let mut file = unsafe { std::fs::File::from_raw_handle(handle.into_raw().cast()) };
            file.write_all(&input)
        })
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

    /// The inverse of what this asserted before harn#7993. `presets: None`
    /// means "use the runtime defaults", and those defaults include
    /// `DeveloperToolchains`, so a policy that never customized presets must
    /// get the home-relative toolchain roots. The old assertion encoded the
    /// bug: it passed only because the function short-circuited on the raw
    /// `None` field and disagreed with `effective_presets()`.
    #[test]
    fn implicit_default_presets_materialize_home_acl_roots() {
        if crate::user_dirs::home_dir().is_none() {
            return;
        }
        let policy = CapabilityPolicy::default();

        let roots = process_sandbox_preset_acl_roots(&policy);
        assert!(
            roots.iter().any(|path| path.ends_with(".cargo")),
            "a default policy resolves to the default presets, so its home-relative \
             toolchain roots must be granted: {roots:?}"
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
