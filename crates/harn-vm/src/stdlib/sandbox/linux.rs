//! Linux sandbox backend — Landlock LSM filesystem scoping plus
//! seccomp-bpf syscall allowlisting installed via `pre_exec`.
//!
//! See `docs/src/sandboxing.md` for the capability → kernel-knob
//! mapping table.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use super::{
    policy_allows_capability, policy_allows_network, policy_allows_workspace_write,
    process_sandbox_developer_toolchain_read_roots,
    process_sandbox_package_manager_config_read_roots, process_sandbox_policy_read_roots,
    process_sandbox_policy_write_roots, process_sandbox_readonly_roots, process_sandbox_roots,
    sandbox_rejection, warn_once, PrepareOutcome, SandboxBackend, SandboxFallback,
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
    allowed_syscalls: Vec<libc::c_long>,
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
        allowed_syscalls: allowed_syscalls(policy),
    })
}

fn apply_profile(profile: &ProcessProfile) -> io::Result<()> {
    if let Some(landlock) = &profile.landlock {
        install_landlock_ruleset(landlock)?;
    }
    // Once seccomp is default-deny, the child should not retain sandbox-setup
    // powers. Install Landlock first, then drop to the runtime syscall ceiling.
    install_seccomp_filter(&profile.allowed_syscalls)?;
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
    for root in process_sandbox_developer_toolchain_read_roots(policy) {
        push_rule(&mut profile, root, read_only_access(), true)?;
    }
    let workspace_access = workspace_access(policy);
    for root in process_sandbox_roots(policy) {
        push_rule(&mut profile, root, workspace_access, false)?;
    }
    for root in process_sandbox_readonly_roots(policy) {
        push_rule(&mut profile, root, read_only_access(), false)?;
    }
    for root in process_sandbox_policy_read_roots(policy) {
        push_rule(&mut profile, root, read_only_access(), false)?;
    }
    for root in process_sandbox_package_manager_config_read_roots(policy) {
        push_rule(&mut profile, root, read_only_access(), true)?;
    }
    // JVM/iOS toolchain caches (Gradle/Maven/Kotlin-Native/Xcode/CocoaPods).
    // Grant write when the policy allows workspace writes so a sandboxed build
    // can populate its caches; otherwise read-only so dependency resolution
    // still works. These roots are optional — they are skipped when absent.
    let toolchain_cache_roots = super::process_sandbox_developer_toolchain_cache_roots(policy);
    let toolchain_cache_access = if policy_allows_workspace_write(policy) {
        workspace_access
    } else {
        read_only_access()
    };
    for root in toolchain_cache_roots {
        push_rule(&mut profile, root, toolchain_cache_access, true)?;
    }
    if policy_allows_workspace_write(policy) {
        for root in process_sandbox_policy_write_roots(policy) {
            push_rule(&mut profile, root, workspace_access, false)?;
        }
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
    // Landlock rejects (EINVAL) a PATH_BENEATH rule whose `parent_fd`
    // points at a non-directory file but whose `allowed_access` carries
    // directory-only rights (READ_DIR, the MAKE_*/REMOVE_* family,
    // REFER). Read-root presets routinely resolve to *files* (e.g.
    // `~/.gitconfig`, `~/.cargo/config.toml`), so mask the directory-only
    // bits off when the handle is not a directory. The remaining
    // file-applicable rights (READ_FILE/EXECUTE/WRITE_FILE/…) still apply.
    let is_dir = file
        .metadata()
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false);
    let allowed_access = if is_dir {
        allowed_access
    } else {
        allowed_access & !DIRECTORY_ONLY_ACCESS_FS
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

fn install_seccomp_filter(allowed_syscalls: &[libc::c_long]) -> io::Result<()> {
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let mut filter = seccomp_allowlist_filter(allowed_syscalls);
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

fn seccomp_allowlist_filter(allowed_syscalls: &[libc::c_long]) -> Vec<libc::sock_filter> {
    let mut filter = Vec::with_capacity(allowed_syscalls.len() * 2 + 2);
    filter.push(bpf_stmt(
        (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
        0,
    ));
    for syscall in allowed_syscalls {
        filter.push(bpf_jump(
            (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            *syscall as u32,
            0,
            1,
        ));
        filter.push(bpf_stmt(
            (libc::BPF_RET | libc::BPF_K) as u16,
            libc::SECCOMP_RET_ALLOW,
        ));
    }
    filter.push(bpf_stmt(
        (libc::BPF_RET | libc::BPF_K) as u16,
        libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
    ));
    filter
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

fn allowed_syscalls(policy: &CapabilityPolicy) -> Vec<libc::c_long> {
    let mut syscalls = vec![
        libc::SYS_brk,
        libc::SYS_capget,
        libc::SYS_chdir,
        libc::SYS_clock_getres,
        libc::SYS_clock_gettime,
        libc::SYS_clock_nanosleep,
        libc::SYS_clone,
        libc::SYS_clone3,
        libc::SYS_close,
        libc::SYS_close_range,
        libc::SYS_copy_file_range,
        libc::SYS_dup,
        libc::SYS_dup3,
        libc::SYS_epoll_create1,
        libc::SYS_epoll_ctl,
        libc::SYS_epoll_pwait,
        libc::SYS_epoll_pwait2,
        libc::SYS_eventfd2,
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_faccessat,
        libc::SYS_faccessat2,
        libc::SYS_fallocate,
        libc::SYS_fchdir,
        libc::SYS_fchmod,
        libc::SYS_fchmodat,
        libc::SYS_fchown,
        libc::SYS_fchownat,
        libc::SYS_fcntl,
        libc::SYS_fdatasync,
        libc::SYS_fgetxattr,
        libc::SYS_flistxattr,
        libc::SYS_flock,
        libc::SYS_fremovexattr,
        libc::SYS_fsetxattr,
        libc::SYS_fstat,
        libc::SYS_fstatfs,
        libc::SYS_fsync,
        libc::SYS_ftruncate,
        libc::SYS_futex,
        libc::SYS_futex_waitv,
        libc::SYS_get_robust_list,
        libc::SYS_getcpu,
        libc::SYS_getcwd,
        libc::SYS_getdents64,
        libc::SYS_getegid,
        libc::SYS_geteuid,
        libc::SYS_getgid,
        libc::SYS_getgroups,
        libc::SYS_getitimer,
        libc::SYS_getpeername,
        libc::SYS_getpgid,
        libc::SYS_getpid,
        libc::SYS_getppid,
        libc::SYS_getpriority,
        libc::SYS_getrandom,
        libc::SYS_getresgid,
        libc::SYS_getresuid,
        libc::SYS_getrusage,
        libc::SYS_getsid,
        libc::SYS_getsockname,
        libc::SYS_getsockopt,
        libc::SYS_gettid,
        libc::SYS_gettimeofday,
        libc::SYS_getuid,
        libc::SYS_getxattr,
        libc::SYS_inotify_add_watch,
        libc::SYS_inotify_init1,
        libc::SYS_inotify_rm_watch,
        libc::SYS_ioctl,
        libc::SYS_kill,
        libc::SYS_linkat,
        libc::SYS_listxattr,
        libc::SYS_llistxattr,
        libc::SYS_lremovexattr,
        libc::SYS_lseek,
        libc::SYS_lsetxattr,
        libc::SYS_lgetxattr,
        libc::SYS_madvise,
        libc::SYS_membarrier,
        libc::SYS_memfd_create,
        libc::SYS_mincore,
        libc::SYS_mkdirat,
        libc::SYS_mknodat,
        libc::SYS_mlock,
        libc::SYS_mlock2,
        libc::SYS_mmap,
        libc::SYS_mprotect,
        libc::SYS_mremap,
        libc::SYS_msync,
        libc::SYS_munlock,
        libc::SYS_munmap,
        libc::SYS_nanosleep,
        libc::SYS_newfstatat,
        libc::SYS_openat,
        libc::SYS_openat2,
        libc::SYS_pipe2,
        libc::SYS_pidfd_open,
        libc::SYS_pidfd_send_signal,
        libc::SYS_ppoll,
        libc::SYS_prctl,
        libc::SYS_pread64,
        libc::SYS_preadv,
        libc::SYS_preadv2,
        libc::SYS_prlimit64,
        libc::SYS_pselect6,
        libc::SYS_pwrite64,
        libc::SYS_pwritev,
        libc::SYS_pwritev2,
        libc::SYS_read,
        libc::SYS_readahead,
        libc::SYS_readlinkat,
        libc::SYS_readv,
        libc::SYS_recvfrom,
        libc::SYS_recvmmsg,
        libc::SYS_recvmsg,
        libc::SYS_removexattr,
        libc::SYS_renameat2,
        libc::SYS_restart_syscall,
        libc::SYS_rseq,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigpending,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigqueueinfo,
        libc::SYS_rt_sigreturn,
        libc::SYS_rt_sigsuspend,
        libc::SYS_rt_sigtimedwait,
        libc::SYS_sched_get_priority_max,
        libc::SYS_sched_get_priority_min,
        libc::SYS_sched_getaffinity,
        libc::SYS_sched_getparam,
        libc::SYS_sched_getscheduler,
        libc::SYS_sched_rr_get_interval,
        libc::SYS_sched_setaffinity,
        libc::SYS_sched_setparam,
        libc::SYS_sched_setscheduler,
        libc::SYS_sched_yield,
        libc::SYS_sendmmsg,
        libc::SYS_sendmsg,
        libc::SYS_sendto,
        libc::SYS_set_robust_list,
        libc::SYS_set_tid_address,
        libc::SYS_setitimer,
        libc::SYS_setpgid,
        libc::SYS_setsockopt,
        libc::SYS_setpriority,
        libc::SYS_setsid,
        libc::SYS_setxattr,
        libc::SYS_shutdown,
        libc::SYS_sigaltstack,
        libc::SYS_signalfd4,
        libc::SYS_socketpair,
        libc::SYS_splice,
        libc::SYS_statfs,
        libc::SYS_statx,
        libc::SYS_symlinkat,
        libc::SYS_sync,
        libc::SYS_tee,
        libc::SYS_tgkill,
        libc::SYS_timer_create,
        libc::SYS_timer_delete,
        libc::SYS_timer_getoverrun,
        libc::SYS_timer_gettime,
        libc::SYS_timer_settime,
        libc::SYS_timerfd_create,
        libc::SYS_timerfd_gettime,
        libc::SYS_timerfd_settime,
        libc::SYS_times,
        libc::SYS_tkill,
        libc::SYS_umask,
        libc::SYS_uname,
        libc::SYS_unlinkat,
        libc::SYS_utimensat,
        libc::SYS_vmsplice,
        libc::SYS_wait4,
        libc::SYS_waitid,
        libc::SYS_write,
        libc::SYS_writev,
    ];

    #[cfg(target_arch = "x86_64")]
    syscalls.extend([
        libc::SYS_access,
        libc::SYS_alarm,
        libc::SYS_arch_prctl,
        libc::SYS_chmod,
        libc::SYS_chown,
        libc::SYS_creat,
        libc::SYS_dup2,
        libc::SYS_epoll_create,
        libc::SYS_epoll_wait,
        libc::SYS_eventfd,
        libc::SYS_fadvise64,
        libc::SYS_fork,
        libc::SYS_futimesat,
        libc::SYS_getdents,
        libc::SYS_getpgrp,
        libc::SYS_getrlimit,
        libc::SYS_link,
        libc::SYS_lchown,
        libc::SYS_lstat,
        libc::SYS_mkdir,
        libc::SYS_open,
        libc::SYS_pause,
        libc::SYS_pipe,
        libc::SYS_poll,
        libc::SYS_readlink,
        libc::SYS_rename,
        libc::SYS_renameat,
        libc::SYS_rmdir,
        libc::SYS_select,
        libc::SYS_sendfile,
        libc::SYS_setrlimit,
        libc::SYS_stat,
        libc::SYS_symlink,
        libc::SYS_sync_file_range,
        libc::SYS_time,
        libc::SYS_truncate,
        libc::SYS_unlink,
        libc::SYS_utime,
        libc::SYS_vfork,
    ]);

    if policy_allows_network(policy) {
        syscalls.extend([
            libc::SYS_accept,
            libc::SYS_accept4,
            libc::SYS_bind,
            libc::SYS_connect,
            libc::SYS_listen,
            libc::SYS_socket,
        ]);
    } else {
        // Allow only the socket operations that work on existing local FDs.
        // With socket/connect/bind/listen/accept absent from the allowlist, the
        // child cannot create or address an off-host endpoint, while Cargo's
        // socketpair-backed jobserver and other local IPC can still exchange
        // tokens over inherited anonymous sockets.
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
    if !policy.capabilities_are_restricted() {
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

/// Access rights that Landlock only permits on a *directory* handle. A
/// PATH_BENEATH rule that pairs any of these with a non-directory
/// `parent_fd` is rejected with EINVAL, so [`push_rule`] strips them when
/// the resolved path is a regular file. (`READ_FILE`, `WRITE_FILE`,
/// `EXECUTE`, `TRUNCATE`, and `IOCTL_DEV` are valid on file handles and
/// are intentionally absent here.)
const DIRECTORY_ONLY_ACCESS_FS: u64 = LANDLOCK_ACCESS_FS_READ_DIR
    | LANDLOCK_ACCESS_FS_REMOVE_DIR
    | LANDLOCK_ACCESS_FS_REMOVE_FILE
    | LANDLOCK_ACCESS_FS_MAKE_CHAR
    | LANDLOCK_ACCESS_FS_MAKE_DIR
    | LANDLOCK_ACCESS_FS_MAKE_REG
    | LANDLOCK_ACCESS_FS_MAKE_SOCK
    | LANDLOCK_ACCESS_FS_MAKE_FIFO
    | LANDLOCK_ACCESS_FS_MAKE_BLOCK
    | LANDLOCK_ACCESS_FS_MAKE_SYM
    | LANDLOCK_ACCESS_FS_REFER;

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
            process_sandbox: Default::default(),
        }
    }

    #[test]
    fn no_network_excludes_addressable_sockets_but_allows_local_socketpair() {
        // At a sub-network ceiling, the egress-capable socket syscalls are
        // not allowlisted, but `socketpair` (anonymous, unaddressable local IPC) stays
        // allowed so Cargo's socketpair-backed jobserver can spawn rustc.
        let policy = linux_policy_with_workspace_ops(&["read_text"]);
        assert_eq!(
            policy.side_effect_level.as_deref(),
            Some("read_only"),
            "fixture must be below the network ceiling",
        );
        let allowed = allowed_syscalls(&policy);

        assert!(
            !allowed.contains(&libc::SYS_socket),
            "addressable socket() must not be allowlisted without network",
        );
        assert!(
            !allowed.contains(&libc::SYS_connect),
            "connect() must not be allowlisted without network",
        );
        assert!(
            allowed.contains(&libc::SYS_socketpair),
            "socketpair() (local IPC) must be allowlisted — Cargo's jobserver needs it",
        );
        // The socketpair-backed jobserver also drives its pair with the
        // send/recv family. They open no egress while socket/connect/bind
        // stay absent from the allowlist.
        for call in [
            libc::SYS_recvfrom,
            libc::SYS_recvmsg,
            libc::SYS_sendmsg,
            libc::SYS_sendto,
        ] {
            assert!(
                allowed.contains(&call),
                "send/recv syscall {call} must be allowlisted — local socketpair IPC (Cargo jobserver) needs it",
            );
        }
        // The egress-capable openers stay absent: no addressable socket can be
        // created or routed, so the inherited-fd send/recv calls cannot reach the network.
        for call in [
            libc::SYS_socket,
            libc::SYS_connect,
            libc::SYS_bind,
            libc::SYS_listen,
            libc::SYS_accept,
            libc::SYS_accept4,
        ] {
            assert!(
                !allowed.contains(&call),
                "egress opener {call} must stay absent without network",
            );
        }
    }

    #[test]
    fn network_ceiling_allows_all_socket_syscalls() {
        // When network side effects are permitted, none of the socket family
        // is removed from the allowlist (socketpair included).
        let mut policy = linux_policy_with_workspace_ops(&["read_text"]);
        policy.side_effect_level = Some("network".to_string());
        let allowed = allowed_syscalls(&policy);
        for call in [
            libc::SYS_socket,
            libc::SYS_socketpair,
            libc::SYS_connect,
            libc::SYS_bind,
        ] {
            assert!(
                allowed.contains(&call),
                "network ceiling must allowlist socket-family syscall {call}",
            );
        }
    }

    #[test]
    fn process_network_ceiling_controls_real_child_socket() {
        let workspace = tempfile::tempdir().expect("workspace");
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("loopback listener");
        let address = listener.local_addr().expect("listener address");
        let args = vec![
            "-c".to_string(),
            format!("exec 3<>/dev/tcp/127.0.0.1/{}", address.port()),
        ];

        let run_probe = |policy: &CapabilityPolicy| {
            let mut command = Command::new("/bin/bash");
            command.args(&args).current_dir(workspace.path());
            let preparation = Backend::prepare_std_command(
                "/bin/bash",
                &args,
                &mut command,
                policy,
                SandboxProfile::Worktree,
            )
            .expect("prepare sandboxed child");
            assert!(matches!(preparation, PrepareOutcome::Direct));
            command.output().expect("run sandboxed child")
        };

        let mut denied = linux_policy_with_workspace_ops(&["read_text"]);
        denied.workspace_roots = vec![workspace.path().display().to_string()];
        denied.side_effect_level = Some("process_exec".to_string());
        let denied_output = run_probe(&denied);
        assert!(
            !denied_output.status.success(),
            "the default process-exec ceiling must deny an addressable child socket",
        );

        let mut allowed = denied;
        allowed.side_effect_level = Some("network".to_string());
        let allowed_output = run_probe(&allowed);
        assert!(
            allowed_output.status.success(),
            "the network ceiling must permit the child loopback socket: {}",
            String::from_utf8_lossy(&allowed_output.stderr),
        );
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        listener
            .accept()
            .expect("the listener must observe the allowed child connection");
    }

    #[test]
    fn seccomp_filter_is_default_deny_allowlist() {
        let filter = seccomp_allowlist_filter(&[libc::SYS_read, libc::SYS_write]);
        assert_eq!(
            filter.last().map(|entry| entry.k),
            Some(libc::SECCOMP_RET_ERRNO | libc::EPERM as u32),
            "seccomp fallthrough must deny unknown syscalls",
        );
        assert!(
            filter
                .iter()
                .any(|entry| entry.k == libc::SECCOMP_RET_ALLOW),
            "allowlisted syscalls must jump to an allow action",
        );
    }

    #[test]
    fn allowlist_excludes_process_introspection_and_io_uring() {
        let policy = linux_policy_with_workspace_ops(&["read_text", "write_text"]);
        let allowed = allowed_syscalls(&policy);
        for call in [
            libc::SYS_ptrace,
            libc::SYS_process_vm_readv,
            libc::SYS_process_vm_writev,
            libc::SYS_io_uring_setup,
            libc::SYS_io_uring_enter,
            libc::SYS_io_uring_register,
        ] {
            assert!(
                !allowed.contains(&call),
                "dangerous syscall {call} must stay outside the seccomp allowlist",
            );
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
    fn package_manager_config_roots_are_read_only() {
        let temp_home = tempfile::tempdir().expect("temp home");
        std::fs::write(
            temp_home.path().join(".npmrc"),
            "registry=https://registry.example\n",
        )
        .expect("write npmrc");
        let roots = super::super::package_manager_config_read_roots_for_home(temp_home.path());

        assert!(
            roots.iter().any(|path| path.ends_with(".npmrc")),
            "npmrc should be part of the package-manager preset"
        );
        assert!(
            roots
                .iter()
                .any(|path| path.ends_with(".cargo/config.toml")),
            "cargo config should be part of the package-manager preset"
        );
        assert!(
            roots.iter().all(|path| path.starts_with(temp_home.path())),
            "package-manager roots must stay under HOME"
        );
        assert_eq!(
            read_only_access() & WRITE_BITS,
            0,
            "package-manager Landlock rules use read-only access bits"
        );
    }

    #[test]
    fn developer_toolchain_roots_are_read_only() {
        let temp_home = tempfile::tempdir().expect("temp home");
        let roots = super::super::developer_toolchain_read_roots_for_home(temp_home.path());

        assert!(
            roots.iter().any(|path| path.ends_with(".local/share/uv")),
            "uv runtimes should be part of the developer-toolchain preset"
        );
        assert!(
            roots.iter().any(|path| path.ends_with(".rustup")),
            "rustup should be part of the developer-toolchain preset"
        );
        assert!(
            roots.iter().all(|path| path.starts_with(temp_home.path())),
            "developer-toolchain roots must stay under HOME"
        );
        assert_eq!(
            read_only_access() & WRITE_BITS,
            0,
            "developer-toolchain Landlock rules use read-only access bits"
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
    fn directory_only_access_excludes_file_applicable_rights() {
        // The file-applicable rights must never be classified as
        // directory-only, otherwise `push_rule` would strip a read/exec
        // grant from a regular-file rule and silently under-scope it.
        for right in [
            LANDLOCK_ACCESS_FS_READ_FILE,
            LANDLOCK_ACCESS_FS_WRITE_FILE,
            LANDLOCK_ACCESS_FS_EXECUTE,
            LANDLOCK_ACCESS_FS_TRUNCATE,
            LANDLOCK_ACCESS_FS_IOCTL_DEV,
        ] {
            assert_eq!(
                DIRECTORY_ONLY_ACCESS_FS & right,
                0,
                "file-applicable right {right:#x} must not be directory-only",
            );
        }
        // READ_DIR is the right that triggers the EINVAL on regular files.
        assert_ne!(
            DIRECTORY_ONLY_ACCESS_FS & LANDLOCK_ACCESS_FS_READ_DIR,
            0,
            "READ_DIR must be classified as directory-only",
        );
    }

    #[test]
    fn read_only_access_on_a_regular_file_drops_directory_only_bits() {
        // A read-only preset root that resolves to a *file* (e.g.
        // `~/.gitconfig`) must end up with only file-applicable rights;
        // the `READ_DIR` bit in `read_only_access()` would otherwise make
        // `landlock_add_rule` return EINVAL.
        let masked = read_only_access() & !DIRECTORY_ONLY_ACCESS_FS;
        assert_eq!(
            masked & LANDLOCK_ACCESS_FS_READ_DIR,
            0,
            "READ_DIR must be stripped for non-directory rules",
        );
        assert_ne!(
            masked & LANDLOCK_ACCESS_FS_READ_FILE,
            0,
            "READ_FILE must survive for non-directory rules",
        );
        assert_ne!(
            masked & LANDLOCK_ACCESS_FS_EXECUTE,
            0,
            "EXECUTE must survive for non-directory rules",
        );
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
