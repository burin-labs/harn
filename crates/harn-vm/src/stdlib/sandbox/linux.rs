//! Linux sandbox backend — Landlock LSM filesystem scoping plus
//! seccomp-bpf syscall blocklist installed via `pre_exec`.
//!
//! See `docs/src/sandboxing.md` for the capability → kernel-knob
//! mapping table.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use super::{
    policy_allows_capability, policy_allows_network, process_sandbox_readonly_roots,
    process_sandbox_roots, sandbox_rejection, warn_once, PrepareOutcome, SandboxBackend,
    SandboxFallback,
};
use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::value::VmError;

pub(super) struct Backend;

impl SandboxBackend for Backend {
    fn name() -> &'static str {
        "linux"
    }

    fn available() -> bool {
        // Both seccomp and Landlock are runtime-detected — the syscalls
        // either work or return a documented errno. `available()` is
        // the OR of the two so a kernel without Landlock but with
        // seccomp still passes; the per-mechanism setup below decides
        // independently whether to install each filter.
        true
    }

    fn prepare_std_command(
        _program: &str,
        _args: &[String],
        command: &mut Command,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        let prep = profile_setup(policy, profile)?;
        // SAFETY: `pre_exec` may only call async-signal-safe functions
        // before exec. The raw syscalls here (`prctl`,
        // `landlock_*`, seccomp `prctl`) are async-signal-safe per
        // their man pages; no allocator, locking, or I/O is performed.
        unsafe {
            command.pre_exec(move || apply_profile(&prep));
        }
        Ok(PrepareOutcome::Direct)
    }

    fn prepare_tokio_command(
        _program: &str,
        _args: &[String],
        command: &mut tokio::process::Command,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        let prep = profile_setup(policy, profile)?;
        // SAFETY: see Linux `prepare_std_command` above.
        unsafe {
            command.pre_exec(move || apply_profile(&prep));
        }
        Ok(PrepareOutcome::Direct)
    }
}

struct ProcessProfile {
    landlock: Option<LandlockProfile>,
    denied_syscalls: Vec<libc::c_long>,
}

struct LandlockProfile {
    ruleset_fd: libc::c_int,
    rules: Vec<LandlockRule>,
    handled_access_fs: u64,
}

struct LandlockRule {
    file: std::fs::File,
    allowed_access: u64,
}

impl Drop for LandlockProfile {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.ruleset_fd);
        }
    }
}

fn profile_setup(
    policy: &CapabilityPolicy,
    profile: SandboxProfile,
) -> Result<ProcessProfile, VmError> {
    // landlock_profile() returns Err under OsHardened when Landlock is
    // unavailable (effective_fallback resolves to Enforce), so the
    // OsHardened "must engage" contract is enforced before fork rather
    // than racing the pre_exec callback.
    Ok(ProcessProfile {
        landlock: landlock_profile(policy, profile)?,
        denied_syscalls: denied_syscalls(policy),
    })
}

fn apply_profile(profile: &ProcessProfile) -> io::Result<()> {
    install_seccomp_filter(&profile.denied_syscalls)?;
    if let Some(landlock) = &profile.landlock {
        install_landlock_ruleset(landlock)?;
    }
    Ok(())
}

fn landlock_profile(
    policy: &CapabilityPolicy,
    profile: SandboxProfile,
) -> Result<Option<LandlockProfile>, VmError> {
    let abi = landlock_abi_version();
    if abi == 0 {
        return match super::effective_fallback(profile) {
            SandboxFallback::Enforce => Err(sandbox_rejection(
                "Linux Landlock is not available; OsHardened profile requires it (set HARN_HANDLER_SANDBOX=warn or off, or pick the worktree profile, to run without filesystem isolation)".to_string(),
            )),
            SandboxFallback::Warn => {
                warn_once(
                    "handler_sandbox_linux_landlock_unavailable",
                    "Linux Landlock is not available; process filesystem isolation is disabled",
                );
                Ok(None)
            }
            SandboxFallback::Off => Ok(None),
        };
    }

    let handled_access_fs = landlock_handled_access(abi);
    let ruleset_attr = LandlockRulesetAttr { handled_access_fs };
    let ruleset_fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &raw const ruleset_attr,
            std::mem::size_of::<LandlockRulesetAttr>(),
            0,
        ) as libc::c_int
    };
    if ruleset_fd < 0 {
        return Err(sandbox_rejection(format!(
            "failed to create Linux Landlock ruleset: {}",
            io::Error::last_os_error()
        )));
    }

    let mut profile = LandlockProfile {
        ruleset_fd,
        rules: Vec::new(),
        handled_access_fs,
    };
    for (path, access) in standard_device_rules() {
        push_rule(&mut profile, path, access, true)?;
    }
    for path in system_read_roots() {
        push_rule(
            &mut profile,
            path,
            LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR | LANDLOCK_ACCESS_FS_EXECUTE,
            true,
        )?;
    }
    let workspace_access = workspace_access(policy);
    for root in process_sandbox_roots(policy) {
        push_rule(&mut profile, root, workspace_access, false)?;
    }
    for root in process_sandbox_readonly_roots(policy) {
        push_rule(&mut profile, root, read_only_access(), false)?;
    }
    Ok(Some(profile))
}

fn system_read_roots() -> Vec<PathBuf> {
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

fn standard_device_rules() -> Vec<(PathBuf, u64)> {
    [
        (
            "/dev/null",
            LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_WRITE_FILE,
        ),
        ("/dev/zero", LANDLOCK_ACCESS_FS_READ_FILE),
        ("/dev/random", LANDLOCK_ACCESS_FS_READ_FILE),
        ("/dev/urandom", LANDLOCK_ACCESS_FS_READ_FILE),
    ]
    .into_iter()
    .map(|(path, access)| (PathBuf::from(path), access))
    .collect()
}

fn push_rule(
    profile: &mut LandlockProfile,
    path: PathBuf,
    allowed_access: u64,
    optional: bool,
) -> Result<(), VmError> {
    let path = super::normalize_for_policy(&path);
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
    profile.rules.push(LandlockRule {
        file,
        allowed_access: allowed_access & profile.handled_access_fs,
    });
    Ok(())
}

fn install_landlock_ruleset(profile: &LandlockProfile) -> io::Result<()> {
    for rule in &profile.rules {
        let path_beneath = LandlockPathBeneathAttr {
            allowed_access: rule.allowed_access,
            parent_fd: rule.file.as_raw_fd(),
        };
        let result = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                profile.ruleset_fd,
                LANDLOCK_RULE_PATH_BENEATH,
                &raw const path_beneath,
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
            &raw mut program,
            0,
            0,
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn bpf_stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

fn bpf_jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

fn denied_syscalls(policy: &CapabilityPolicy) -> Vec<libc::c_long> {
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

/// Access rights granted to every `read_only_roots` entry: read +
/// directory-read + execute, and never any write/create/remove right —
/// regardless of the policy's `workspace.*` capabilities. Landlock rules
/// are additive (there is no deny), so a read-only root nested under a
/// writable workspace root still inherits the parent's write grant; the
/// two lists are intended to be disjoint (see `docs/src/sandboxing.md`).
fn read_only_access() -> u64 {
    LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR | LANDLOCK_ACCESS_FS_EXECUTE
}

fn workspace_access(policy: &CapabilityPolicy) -> u64 {
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

fn landlock_abi_version() -> u32 {
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

fn landlock_handled_access(abi: u32) -> u64 {
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
    if abi >= 5 {
        access |= LANDLOCK_ACCESS_FS_IOCTL_DEV;
    }
    access
}

#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
}

#[repr(C)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: libc::c_int,
}

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: libc::c_int = 1;
const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1 << 0;
const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const LANDLOCK_ACCESS_FS_READ_FILE: u64 = 1 << 2;
const LANDLOCK_ACCESS_FS_READ_DIR: u64 = 1 << 3;
const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const LANDLOCK_ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const LANDLOCK_ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
const LANDLOCK_ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
const LANDLOCK_ACCESS_FS_REFER: u64 = 1 << 13;
const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 14;
const LANDLOCK_ACCESS_FS_IOCTL_DEV: u64 = 1 << 15;

#[cfg(test)]
mod tests {
    use super::*;

    const WRITE_BITS: u64 = LANDLOCK_ACCESS_FS_WRITE_FILE
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

    fn linux_policy_with_workspace_ops(ops: &[&str]) -> CapabilityPolicy {
        CapabilityPolicy {
            tools: Vec::new(),
            capabilities: std::collections::BTreeMap::from([(
                "workspace".to_string(),
                ops.iter().map(|op| op.to_string()).collect(),
            )]),
            workspace_roots: vec!["/ws".to_string()],
            read_only_roots: Vec::new(),
            side_effect_level: Some("read_only".to_string()),
            recursion_limit: None,
            tool_arg_constraints: Vec::new(),
            tool_annotations: std::collections::BTreeMap::new(),
            sandbox_profile: SandboxProfile::Worktree,
        }
    }

    #[test]
    fn read_only_access_grants_read_and_execute_but_never_write() {
        let access = read_only_access();
        assert_ne!(access & LANDLOCK_ACCESS_FS_READ_FILE, 0, "read file");
        assert_ne!(access & LANDLOCK_ACCESS_FS_READ_DIR, 0, "read dir");
        assert_ne!(access & LANDLOCK_ACCESS_FS_EXECUTE, 0, "execute");
        assert_eq!(
            access & WRITE_BITS,
            0,
            "read-only access must not carry any write/create/remove right",
        );
    }

    #[test]
    fn read_only_access_is_independent_of_workspace_write_capability() {
        // Even when the policy otherwise allows workspace writes, the
        // read-only access bits are unchanged: a read-only root gets
        // read+execute only.
        let writable = linux_policy_with_workspace_ops(&["read_text", "write_text", "delete"]);
        assert_ne!(
            workspace_access(&writable) & LANDLOCK_ACCESS_FS_WRITE_FILE,
            0,
            "writable workspace root should carry write",
        );
        assert_eq!(
            read_only_access() & WRITE_BITS,
            0,
            "read-only roots stay unwritable regardless of workspace write capability",
        );
    }

    #[test]
    fn standard_device_rules_allow_common_device_files_only() {
        let rules = standard_device_rules();
        assert_eq!(rules.len(), 4);
        assert!(rules.iter().any(|(path, access)| path.as_path()
            == std::path::Path::new("/dev/null")
            && access & LANDLOCK_ACCESS_FS_READ_FILE != 0
            && access & LANDLOCK_ACCESS_FS_WRITE_FILE != 0
            && access & LANDLOCK_ACCESS_FS_IOCTL_DEV == 0));
        for device in ["/dev/zero", "/dev/random", "/dev/urandom"] {
            let Some((_, access)) = rules
                .iter()
                .find(|(path, _)| path.as_path() == std::path::Path::new(device))
            else {
                panic!("missing standard device rule for {device}");
            };
            assert_ne!(
                *access & LANDLOCK_ACCESS_FS_READ_FILE,
                0,
                "{device} should be readable"
            );
            assert_eq!(
                *access & LANDLOCK_ACCESS_FS_WRITE_FILE,
                0,
                "{device} must not be writable"
            );
            assert_eq!(
                *access & LANDLOCK_ACCESS_FS_IOCTL_DEV,
                0,
                "{device} must not receive device ioctl access"
            );
        }
    }

    #[test]
    fn landlock_handled_access_tracks_device_ioctl_abi() {
        assert_eq!(
            landlock_handled_access(4) & LANDLOCK_ACCESS_FS_IOCTL_DEV,
            0,
            "ABI 4 kernels do not support device ioctl mediation",
        );
        assert_ne!(
            landlock_handled_access(5) & LANDLOCK_ACCESS_FS_IOCTL_DEV,
            0,
            "ABI 5+ kernels should explicitly mediate device ioctls",
        );
    }
}
