use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::orchestration::CapabilityPolicy;
use crate::value::{ErrorCategory, VmError};

#[cfg(any(target_os = "linux", target_os = "openbsd"))]
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(any(target_os = "linux", target_os = "openbsd"))]
use std::os::unix::process::CommandExt;

const HANDLER_SANDBOX_ENV: &str = "HARN_HANDLER_SANDBOX";

thread_local! {
    static WARNED_KEYS: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

#[derive(Clone, Copy)]
pub(crate) enum FsAccess {
    Read,
    Write,
    Delete,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SandboxFallback {
    Off,
    Warn,
    Enforce,
}

pub(crate) fn reset_sandbox_state() {
    WARNED_KEYS.with(|keys| keys.borrow_mut().clear());
}

pub(crate) fn enforce_fs_path(builtin: &str, path: &Path, access: FsAccess) -> Result<(), VmError> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(());
    };
    if policy.workspace_roots.is_empty() {
        return Ok(());
    }
    let candidate = normalize_for_policy(path);
    let roots = normalized_workspace_roots(&policy);
    if roots.iter().any(|root| path_is_within(&candidate, root)) {
        return Ok(());
    }
    Err(sandbox_rejection(format!(
        "sandbox violation: builtin '{builtin}' attempted to {} '{}' outside workspace_roots [{}]",
        access.verb(),
        candidate.display(),
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

pub(crate) fn enforce_process_cwd(path: &Path) -> Result<(), VmError> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(());
    };
    if policy.workspace_roots.is_empty() {
        return Ok(());
    }
    let candidate = normalize_for_policy(path);
    let roots = normalized_workspace_roots(&policy);
    if roots.iter().any(|root| path_is_within(&candidate, root)) {
        return Ok(());
    }
    Err(sandbox_rejection(format!(
        "sandbox violation: process cwd '{}' is outside workspace_roots [{}]",
        candidate.display(),
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

pub(crate) fn std_command_for(program: &str, args: &[String]) -> Result<Command, VmError> {
    let policy = active_sandbox_policy();
    match command_wrapper(program, args, policy.as_ref())? {
        CommandWrapper::Direct => {
            let mut command = Command::new(program);
            command.args(args);
            if let Some(policy) = policy {
                platform_configure_std_command(&mut command, &policy)?;
            }
            Ok(command)
        }
        #[cfg(target_os = "macos")]
        CommandWrapper::Sandboxed { wrapper, args } => {
            let mut command = Command::new(wrapper);
            command.args(args);
            Ok(command)
        }
    }
}

pub(crate) fn tokio_command_for(
    program: &str,
    args: &[String],
) -> Result<tokio::process::Command, VmError> {
    let policy = active_sandbox_policy();
    match command_wrapper(program, args, policy.as_ref())? {
        CommandWrapper::Direct => {
            let mut command = tokio::process::Command::new(program);
            command.args(args);
            if let Some(policy) = policy {
                platform_configure_tokio_command(&mut command, &policy)?;
            }
            Ok(command)
        }
        #[cfg(target_os = "macos")]
        CommandWrapper::Sandboxed { wrapper, args } => {
            let mut command = tokio::process::Command::new(wrapper);
            command.args(args);
            Ok(command)
        }
    }
}

pub(crate) fn process_violation_error(output: &std::process::Output) -> Option<VmError> {
    crate::orchestration::current_execution_policy()?;
    if fallback_mode() == SandboxFallback::Off || !platform_sandbox_available() {
        return None;
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    if !output.status.success()
        && (stderr.contains("operation not permitted")
            || stderr.contains("permission denied")
            || stdout.contains("operation not permitted"))
    {
        return Some(sandbox_rejection(format!(
            "sandbox violation: process was denied by the OS sandbox (status {})",
            output.status.code().unwrap_or(-1)
        )));
    }
    if sandbox_signal_status(output) {
        return Some(sandbox_rejection(format!(
            "sandbox violation: process was terminated by the OS sandbox (status {})",
            output.status
        )));
    }
    None
}

pub(crate) fn process_spawn_error(error: &std::io::Error) -> Option<VmError> {
    crate::orchestration::current_execution_policy()?;
    if fallback_mode() == SandboxFallback::Off || !platform_sandbox_available() {
        return None;
    }
    let message = error.to_string().to_ascii_lowercase();
    if error.kind() == std::io::ErrorKind::PermissionDenied
        || message.contains("operation not permitted")
        || message.contains("permission denied")
    {
        return Some(sandbox_rejection(format!(
            "sandbox violation: process was denied by the OS sandbox before exec: {error}"
        )));
    }
    None
}

#[cfg(unix)]
fn sandbox_signal_status(output: &std::process::Output) -> bool {
    use std::os::unix::process::ExitStatusExt;

    matches!(
        output.status.signal(),
        Some(libc::SIGSYS) | Some(libc::SIGABRT) | Some(libc::SIGKILL)
    )
}

#[cfg(not(unix))]
fn sandbox_signal_status(_output: &std::process::Output) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn platform_sandbox_available() -> bool {
    linux_seccomp_available() || linux_landlock_abi_version() > 0
}

#[cfg(target_os = "macos")]
fn platform_sandbox_available() -> bool {
    Path::new("/usr/bin/sandbox-exec").exists()
}

#[cfg(target_os = "openbsd")]
fn platform_sandbox_available() -> bool {
    true
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "openbsd")))]
fn platform_sandbox_available() -> bool {
    false
}

enum CommandWrapper {
    Direct,
    #[cfg(target_os = "macos")]
    Sandboxed {
        wrapper: String,
        args: Vec<String>,
    },
}

fn command_wrapper(
    program: &str,
    args: &[String],
    policy: Option<&CapabilityPolicy>,
) -> Result<CommandWrapper, VmError> {
    let Some(policy) = policy else {
        return Ok(CommandWrapper::Direct);
    };
    platform_command_wrapper(program, args, policy)
}

fn active_sandbox_policy() -> Option<CapabilityPolicy> {
    if fallback_mode() == SandboxFallback::Off {
        return None;
    }
    crate::orchestration::current_execution_policy()
}

#[cfg(any(target_os = "linux", target_os = "openbsd"))]
fn platform_configure_std_command(
    command: &mut Command,
    policy: &CapabilityPolicy,
) -> Result<(), VmError> {
    let profile = platform_process_profile(policy)?;
    unsafe {
        command.pre_exec(move || platform_apply_process_profile(&profile));
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "openbsd"))]
fn platform_configure_tokio_command(
    command: &mut tokio::process::Command,
    policy: &CapabilityPolicy,
) -> Result<(), VmError> {
    let profile = platform_process_profile(policy)?;
    unsafe {
        command.pre_exec(move || platform_apply_process_profile(&profile));
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "openbsd")))]
fn platform_configure_std_command(
    _command: &mut Command,
    _policy: &CapabilityPolicy,
) -> Result<(), VmError> {
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "openbsd")))]
fn platform_configure_tokio_command(
    _command: &mut tokio::process::Command,
    _policy: &CapabilityPolicy,
) -> Result<(), VmError> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn platform_command_wrapper(
    program: &str,
    args: &[String],
    policy: &CapabilityPolicy,
) -> Result<CommandWrapper, VmError> {
    let sandbox_exec = Path::new("/usr/bin/sandbox-exec");
    if !sandbox_exec.exists() {
        return unavailable("macOS sandbox-exec is not available");
    }
    let mut wrapped_args = vec![
        "-p".to_string(),
        macos_sandbox_profile(policy),
        "--".to_string(),
        program.to_string(),
    ];
    wrapped_args.extend(args.iter().cloned());
    Ok(CommandWrapper::Sandboxed {
        wrapper: sandbox_exec.display().to_string(),
        args: wrapped_args,
    })
}

#[cfg(any(target_os = "linux", target_os = "openbsd"))]
fn platform_command_wrapper(
    _program: &str,
    _args: &[String],
    _policy: &CapabilityPolicy,
) -> Result<CommandWrapper, VmError> {
    Ok(CommandWrapper::Direct)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "openbsd")))]
fn platform_command_wrapper(
    _program: &str,
    _args: &[String],
    _policy: &CapabilityPolicy,
) -> Result<CommandWrapper, VmError> {
    unavailable(&format!(
        "handler OS sandbox is not implemented for {}",
        std::env::consts::OS
    ))
}

#[cfg(target_os = "macos")]
fn macos_sandbox_profile(policy: &CapabilityPolicy) -> String {
    let roots = process_sandbox_roots(policy);
    let mut profile = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process*)\n\
         (allow sysctl-read)\n\
         (allow mach-lookup)\n\
         (allow file-read*)\n\
         (allow file-write* (subpath \"/dev\") (subpath \"/tmp\") (subpath \"/private/tmp\"))\n",
    );
    if policy_allows_workspace_write(policy) {
        for root in roots {
            profile.push_str(&format!(
                "(allow file-write* (subpath \"{}\"))\n",
                sandbox_profile_escape(&root.display().to_string())
            ));
        }
    }
    if policy_allows_network(policy) {
        profile.push_str("(allow network*)\n");
    }
    profile
}

#[cfg(target_os = "macos")]
fn sandbox_profile_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "linux")]
struct PlatformProcessProfile {
    landlock: Option<LinuxLandlockProfile>,
    denied_syscalls: Vec<libc::c_long>,
}

#[cfg(target_os = "linux")]
struct LinuxLandlockProfile {
    ruleset_fd: libc::c_int,
    rules: Vec<LinuxLandlockRule>,
    handled_access_fs: u64,
}

#[cfg(target_os = "linux")]
struct LinuxLandlockRule {
    file: std::fs::File,
    allowed_access: u64,
}

#[cfg(target_os = "linux")]
impl Drop for LinuxLandlockProfile {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.ruleset_fd);
        }
    }
}

#[cfg(target_os = "linux")]
fn platform_process_profile(policy: &CapabilityPolicy) -> Result<PlatformProcessProfile, VmError> {
    Ok(PlatformProcessProfile {
        landlock: linux_landlock_profile(policy)?,
        denied_syscalls: linux_denied_syscalls(policy),
    })
}

#[cfg(target_os = "linux")]
fn platform_apply_process_profile(profile: &PlatformProcessProfile) -> io::Result<()> {
    install_seccomp_filter(&profile.denied_syscalls)?;
    if let Some(landlock) = &profile.landlock {
        install_landlock_ruleset(landlock)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_seccomp_available() -> bool {
    true
}

#[cfg(target_os = "linux")]
fn linux_landlock_profile(
    policy: &CapabilityPolicy,
) -> Result<Option<LinuxLandlockProfile>, VmError> {
    let abi = linux_landlock_abi_version();
    if abi == 0 {
        match fallback_mode() {
            SandboxFallback::Enforce => {
                return Err(sandbox_rejection(
                    "Linux Landlock is not available; set HARN_HANDLER_SANDBOX=warn or off to run without filesystem isolation".to_string(),
                ));
            }
            SandboxFallback::Warn => warn_once(
                "handler_sandbox_linux_landlock_unavailable",
                "Linux Landlock is not available; process filesystem isolation is disabled",
            ),
            SandboxFallback::Off => {}
        }
        return Ok(None);
    }

    let handled_access_fs = linux_landlock_handled_access(abi);
    let ruleset_attr = LinuxLandlockRulesetAttr { handled_access_fs };
    let ruleset_fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &ruleset_attr as *const LinuxLandlockRulesetAttr,
            std::mem::size_of::<LinuxLandlockRulesetAttr>(),
            0,
        ) as libc::c_int
    };
    if ruleset_fd < 0 {
        return Err(sandbox_rejection(format!(
            "failed to create Linux Landlock ruleset: {}",
            io::Error::last_os_error()
        )));
    }

    let mut profile = LinuxLandlockProfile {
        ruleset_fd,
        rules: Vec::new(),
        handled_access_fs,
    };
    for path in linux_system_read_roots() {
        push_linux_landlock_rule(
            &mut profile,
            path,
            LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR | LANDLOCK_ACCESS_FS_EXECUTE,
            true,
        )?;
    }
    let workspace_access = linux_workspace_access(policy);
    for root in process_sandbox_roots(policy) {
        push_linux_landlock_rule(&mut profile, root, workspace_access, false)?;
    }
    Ok(Some(profile))
}

#[cfg(target_os = "linux")]
fn linux_system_read_roots() -> Vec<PathBuf> {
    [
        "/bin",
        "/lib",
        "/lib64",
        "/usr",
        "/etc",
        "/nix/store",
        "/System",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(target_os = "linux")]
fn push_linux_landlock_rule(
    profile: &mut LinuxLandlockProfile,
    path: PathBuf,
    allowed_access: u64,
    optional: bool,
) -> Result<(), VmError> {
    let path = normalize_for_policy(&path);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if optional && error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(sandbox_rejection(format!(
                "failed to open sandbox path '{}': {error}",
                path.display()
            )));
        }
    };
    profile.rules.push(LinuxLandlockRule {
        file,
        allowed_access: allowed_access & profile.handled_access_fs,
    });
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_landlock_ruleset(profile: &LinuxLandlockProfile) -> io::Result<()> {
    for rule in &profile.rules {
        let path_beneath = LinuxLandlockPathBeneathAttr {
            allowed_access: rule.allowed_access,
            parent_fd: rule.file.as_raw_fd(),
        };
        let result = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                profile.ruleset_fd,
                LANDLOCK_RULE_PATH_BENEATH,
                &path_beneath as *const LinuxLandlockPathBeneathAttr,
                0,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(io::Error::last_os_error());
        }
        let result = libc::syscall(libc::SYS_landlock_restrict_self, profile.ruleset_fd, 0);
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_seccomp_filter(denied_syscalls: &[libc::c_long]) -> io::Result<()> {
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let mut filter = Vec::with_capacity(denied_syscalls.len() * 2 + 1);
    filter.push(bpf_stmt(
        (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
        0,
    ));
    for syscall in denied_syscalls {
        filter.push(bpf_jump(
            (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            *syscall as u32,
            0,
            1,
        ));
        filter.push(bpf_stmt(
            (libc::BPF_RET | libc::BPF_K) as u16,
            libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
        ));
    }
    filter.push(bpf_stmt(
        (libc::BPF_RET | libc::BPF_K) as u16,
        libc::SECCOMP_RET_ALLOW,
    ));
    let mut program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_mut_ptr(),
    };
    unsafe {
        if libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &mut program as *mut libc::sock_fprog,
            0,
            0,
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn bpf_stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

#[cfg(target_os = "linux")]
fn bpf_jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

#[cfg(target_os = "linux")]
fn linux_denied_syscalls(policy: &CapabilityPolicy) -> Vec<libc::c_long> {
    let mut syscalls = vec![
        libc::SYS_bpf,
        libc::SYS_delete_module,
        libc::SYS_fanotify_init,
        libc::SYS_finit_module,
        libc::SYS_init_module,
        libc::SYS_kexec_file_load,
        libc::SYS_kexec_load,
        libc::SYS_mount,
        libc::SYS_open_by_handle_at,
        libc::SYS_perf_event_open,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_ptrace,
        libc::SYS_reboot,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_umount2,
        libc::SYS_userfaultfd,
    ];
    if !policy_allows_network(policy) {
        syscalls.extend([
            libc::SYS_accept,
            libc::SYS_accept4,
            libc::SYS_bind,
            libc::SYS_connect,
            libc::SYS_listen,
            libc::SYS_recvfrom,
            libc::SYS_recvmsg,
            libc::SYS_sendmsg,
            libc::SYS_sendto,
            libc::SYS_socket,
            libc::SYS_socketpair,
        ]);
    }
    syscalls.sort_unstable();
    syscalls.dedup();
    syscalls
}

#[cfg(target_os = "linux")]
fn linux_workspace_access(policy: &CapabilityPolicy) -> u64 {
    let read_access =
        LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR | LANDLOCK_ACCESS_FS_EXECUTE;
    let write_access = LANDLOCK_ACCESS_FS_WRITE_FILE
        | LANDLOCK_ACCESS_FS_REMOVE_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_FILE
        | LANDLOCK_ACCESS_FS_MAKE_CHAR
        | LANDLOCK_ACCESS_FS_MAKE_DIR
        | LANDLOCK_ACCESS_FS_MAKE_REG
        | LANDLOCK_ACCESS_FS_MAKE_SOCK
        | LANDLOCK_ACCESS_FS_MAKE_FIFO
        | LANDLOCK_ACCESS_FS_MAKE_BLOCK
        | LANDLOCK_ACCESS_FS_MAKE_SYM
        | LANDLOCK_ACCESS_FS_REFER
        | LANDLOCK_ACCESS_FS_TRUNCATE;
    if policy.capabilities.is_empty() {
        return read_access | write_access;
    }
    let mut access = 0;
    if policy_allows_capability(policy, "workspace", &["read_text", "list", "exists"]) {
        access |= read_access;
    }
    if policy_allows_capability(policy, "workspace", &["write_text"]) {
        access |= write_access;
    }
    if policy_allows_capability(policy, "workspace", &["delete"]) {
        access |= LANDLOCK_ACCESS_FS_REMOVE_DIR | LANDLOCK_ACCESS_FS_REMOVE_FILE;
    }
    if access == 0 {
        read_access
    } else {
        access
    }
}

#[cfg(target_os = "linux")]
fn linux_landlock_abi_version() -> u32 {
    let result = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if result <= 0 {
        0
    } else {
        result as u32
    }
}

#[cfg(target_os = "linux")]
fn linux_landlock_handled_access(abi: u32) -> u64 {
    let mut access = LANDLOCK_ACCESS_FS_EXECUTE
        | LANDLOCK_ACCESS_FS_WRITE_FILE
        | LANDLOCK_ACCESS_FS_READ_FILE
        | LANDLOCK_ACCESS_FS_READ_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_FILE
        | LANDLOCK_ACCESS_FS_MAKE_CHAR
        | LANDLOCK_ACCESS_FS_MAKE_DIR
        | LANDLOCK_ACCESS_FS_MAKE_REG
        | LANDLOCK_ACCESS_FS_MAKE_SOCK
        | LANDLOCK_ACCESS_FS_MAKE_FIFO
        | LANDLOCK_ACCESS_FS_MAKE_BLOCK
        | LANDLOCK_ACCESS_FS_MAKE_SYM;
    if abi >= 2 {
        access |= LANDLOCK_ACCESS_FS_REFER;
    }
    if abi >= 3 {
        access |= LANDLOCK_ACCESS_FS_TRUNCATE;
    }
    access
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct LinuxLandlockRulesetAttr {
    handled_access_fs: u64,
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct LinuxLandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: libc::c_int,
}

#[cfg(target_os = "linux")]
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
#[cfg(target_os = "linux")]
const LANDLOCK_RULE_PATH_BENEATH: libc::c_int = 1;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1 << 0;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_READ_FILE: u64 = 1 << 2;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_READ_DIR: u64 = 1 << 3;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_MAKE_REG: u64 = 1 << 8;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_REFER: u64 = 1 << 13;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 14;

#[cfg(target_os = "openbsd")]
struct PlatformProcessProfile {
    unveil_rules: Vec<(String, String)>,
    promises: String,
}

#[cfg(target_os = "openbsd")]
fn platform_process_profile(policy: &CapabilityPolicy) -> Result<PlatformProcessProfile, VmError> {
    let workspace_permissions = if policy_allows_workspace_write(policy) {
        "rwcx"
    } else {
        "rx"
    };
    let mut unveil_rules = vec![
        ("/bin".to_string(), "rx".to_string()),
        ("/usr".to_string(), "rx".to_string()),
        ("/lib".to_string(), "rx".to_string()),
        ("/etc".to_string(), "r".to_string()),
        ("/dev".to_string(), "rw".to_string()),
    ];
    for root in process_sandbox_roots(policy) {
        unveil_rules.push((
            root.display().to_string(),
            workspace_permissions.to_string(),
        ));
    }

    let mut promises = vec!["stdio", "rpath", "proc", "exec"];
    if policy_allows_workspace_write(policy) {
        promises.extend(["wpath", "cpath", "dpath"]);
    }
    if policy_allows_network(policy) {
        promises.extend(["inet", "dns"]);
    }
    Ok(PlatformProcessProfile {
        unveil_rules,
        promises: promises.join(" "),
    })
}

#[cfg(target_os = "openbsd")]
fn platform_apply_process_profile(profile: &PlatformProcessProfile) -> io::Result<()> {
    for (path, permissions) in &profile.unveil_rules {
        let path = std::ffi::CString::new(path.as_str())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "unveil path contains NUL"))?;
        let permissions = std::ffi::CString::new(permissions.as_str()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "unveil permissions contain NUL",
            )
        })?;
        unsafe {
            if unveil(path.as_ptr(), permissions.as_ptr()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    unsafe {
        if unveil(std::ptr::null(), std::ptr::null()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let promises = std::ffi::CString::new(profile.promises.as_str())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pledge promises contain NUL"))?;
    unsafe {
        if pledge(promises.as_ptr(), std::ptr::null()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(target_os = "openbsd")]
extern "C" {
    fn pledge(promises: *const libc::c_char, execpromises: *const libc::c_char) -> libc::c_int;
    fn unveil(path: *const libc::c_char, permissions: *const libc::c_char) -> libc::c_int;
}

#[cfg(not(any(target_os = "linux", target_os = "openbsd")))]
fn unavailable(message: &str) -> Result<CommandWrapper, VmError> {
    match fallback_mode() {
        SandboxFallback::Off | SandboxFallback::Warn => {
            warn_once("handler_sandbox_unavailable", message);
            Ok(CommandWrapper::Direct)
        }
        SandboxFallback::Enforce => Err(sandbox_rejection(format!(
            "{message}; set {HANDLER_SANDBOX_ENV}=warn or off to run unsandboxed"
        ))),
    }
}

fn fallback_mode() -> SandboxFallback {
    match std::env::var(HANDLER_SANDBOX_ENV)
        .unwrap_or_else(|_| "warn".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "0" | "false" | "off" | "none" => SandboxFallback::Off,
        "1" | "true" | "enforce" | "required" => SandboxFallback::Enforce,
        _ => SandboxFallback::Warn,
    }
}

fn warn_once(key: &str, message: &str) {
    let inserted = WARNED_KEYS.with(|keys| keys.borrow_mut().insert(key.to_string()));
    if inserted {
        crate::events::log_warn("handler_sandbox", message);
    }
}

fn sandbox_rejection(message: String) -> VmError {
    VmError::CategorizedError {
        message,
        category: ErrorCategory::ToolRejected,
    }
}

fn normalized_workspace_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    policy
        .workspace_roots
        .iter()
        .map(|root| normalize_for_policy(&resolve_policy_path(root)))
        .collect()
}

fn process_sandbox_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let roots = if policy.workspace_roots.is_empty() {
        vec![crate::stdlib::process::execution_root_path()]
    } else {
        normalized_workspace_roots(policy)
    };
    roots
        .into_iter()
        .map(|root| normalize_for_policy(&root))
        .collect()
}

fn resolve_policy_path(path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        crate::stdlib::process::execution_root_path().join(candidate)
    }
}

fn normalize_for_policy(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::stdlib::process::execution_root_path().join(path)
    };
    let absolute = normalize_lexically(&absolute);
    if let Ok(canonical) = absolute.canonicalize() {
        return canonical;
    }

    let mut existing = absolute.as_path();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let Some(parent) = existing.parent() else {
            return normalize_lexically(&absolute);
        };
        if let Some(name) = existing.file_name() {
            suffix.push(name.to_os_string());
        }
        existing = parent;
    }

    let mut normalized = existing
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexically(existing));
    for component in suffix.iter().rev() {
        normalized.push(component);
    }
    normalize_lexically(&normalized)
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn policy_allows_network(policy: &CapabilityPolicy) -> bool {
    fn rank(value: &str) -> usize {
        match value {
            "none" => 0,
            "read_only" => 1,
            "workspace_write" => 2,
            "process_exec" => 3,
            "network" => 4,
            _ => 5,
        }
    }
    policy
        .side_effect_level
        .as_ref()
        .map(|level| rank(level) >= rank("network"))
        .unwrap_or(true)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "openbsd"))]
fn policy_allows_workspace_write(policy: &CapabilityPolicy) -> bool {
    policy.capabilities.is_empty()
        || policy_allows_capability(policy, "workspace", &["write_text", "delete"])
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "openbsd"))]
fn policy_allows_capability(policy: &CapabilityPolicy, capability: &str, ops: &[&str]) -> bool {
    policy
        .capabilities
        .get(capability)
        .map(|allowed| {
            ops.iter()
                .any(|op| allowed.iter().any(|candidate| candidate == op))
        })
        .unwrap_or(false)
}

impl FsAccess {
    fn verb(self) -> &'static str {
        match self {
            FsAccess::Read => "read",
            FsAccess::Write => "write",
            FsAccess::Delete => "delete",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_create_path_normalizes_against_existing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/../new.txt");
        let normalized = normalize_for_policy(&nested);
        assert_eq!(
            normalized,
            normalize_for_policy(&dir.path().join("new.txt"))
        );
    }

    #[test]
    fn path_within_root_accepts_root_and_children() {
        let root = Path::new("/tmp/harn-root");
        assert!(path_is_within(root, root));
        assert!(path_is_within(Path::new("/tmp/harn-root/file"), root));
        assert!(!path_is_within(
            Path::new("/tmp/harn-root-other/file"),
            root
        ));
    }
}
