//! Linux sandbox backend — Landlock LSM filesystem scoping plus
//! seccomp-bpf syscall allowlisting installed via `pre_exec`.
//!
//! See `docs/src/sandboxing.md` for the capability → kernel-knob
//! mapping table.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, TargetArch};

use super::{
    policy_allows_capability, policy_allows_network, policy_allows_workspace_write,
    process_sandbox_developer_toolchain_read_roots,
    process_sandbox_package_manager_config_read_roots, process_sandbox_policy_read_roots,
    process_sandbox_policy_write_roots, process_sandbox_presets, process_sandbox_readonly_roots,
    process_sandbox_roots, sandbox_rejection, warn_once, PrepareOutcome, SandboxBackend,
    SandboxFallback,
};
use crate::orchestration::{CapabilityPolicy, ProcessSandboxPreset, SandboxProfile};
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
        program: &str,
        _args: &[String],
        command: &mut Command,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        let prep = profile_setup(program, policy, profile)?;
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
        program: &str,
        _args: &[String],
        command: &mut tokio::process::Command,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        let prep = profile_setup(program, policy, profile)?;
        // SAFETY: see Linux `prepare_std_command` above.
        unsafe {
            command.pre_exec(move || apply_profile(&prep));
        }
        Ok(PrepareOutcome::Direct)
    }
}

struct ProcessProfile {
    landlock: Option<LandlockProfile>,
    /// Pre-compiled seccomp program. Built before fork so `pre_exec` only has
    /// to install it — see [`compile_seccomp_program`].
    seccomp: BpfProgram,
}

struct LandlockProfile {
    ruleset_fd: libc::c_int,
    rules: Vec<LandlockRule>,
    handled_access_fs: u64,
    /// Subtrees no grant may cover. Landlock has no deny rule, so this is
    /// enforced by never granting a path that contains one; see
    /// [`expand_around_denied`].
    read_deny_roots: Vec<PathBuf>,
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
    program: &str,
    policy: &CapabilityPolicy,
    profile: SandboxProfile,
) -> Result<ProcessProfile, VmError> {
    if policy.process_network_proxy.is_some() {
        return Err(sandbox_rejection(
            "managed child-process egress requires a proxy-only Linux network namespace; this build cannot enforce that boundary"
                .to_string(),
        ));
    }
    if policy.process_sandbox.allow_tcp_loopback {
        return Err(sandbox_rejection(
            "TCP loopback-only child networking requires a private Linux network namespace; this build cannot enforce that boundary"
                .to_string(),
        ));
    }
    // landlock_profile() returns Err under OsHardened when Landlock is
    // unavailable (effective_fallback resolves to Enforce), so the
    // OsHardened "must engage" contract is enforced before fork rather
    // than racing the pre_exec callback.
    Ok(ProcessProfile {
        landlock: landlock_profile(program, policy, profile)?,
        seccomp: compile_seccomp_program(&allowed_syscalls(policy))?,
    })
}

fn apply_profile(profile: &ProcessProfile) -> io::Result<()> {
    if let Some(landlock) = &profile.landlock {
        install_landlock_ruleset(landlock)?;
    }
    // Once seccomp is default-deny, the child should not retain sandbox-setup
    // powers. Install Landlock first, then drop to the runtime syscall ceiling.
    //
    // `apply_filter` sets `PR_SET_NO_NEW_PRIVS` and issues `SYS_seccomp`
    // against the already-compiled program: no allocation, so it is safe on
    // the `pre_exec` side of the fork.
    seccompiler::apply_filter(&profile.seccomp)
        .map_err(|err| io::Error::other(format!("failed to install the seccomp filter: {err}")))?;
    Ok(())
}

fn landlock_profile(
    program: &str,
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
        read_deny_roots: super::process_sandbox_read_deny_roots(policy),
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
    for path in network_name_service_read_roots(policy) {
        // `/etc/resolv.conf` is commonly a symlink into `/run` on hosted
        // Linux. Landlock checks the resolved inode, so the broad `/etc` rule
        // above does not cover that target. Open each exact host file before
        // confinement and grant its canonical inode without exposing `/run`.
        push_rule(&mut profile, path, LANDLOCK_ACCESS_FS_READ_FILE, true)?;
    }
    if proc_runtime_reads_are_contained() {
        // Some language runtimes (notably Swift on Linux) discover argv by
        // reading their own memory map. A rule for `/proc/self/maps` cannot
        // cover grandchildren: Landlock resolves it to the immediate child's
        // PID-specific inode, while compiler drivers and shell scripts spawn
        // fresh processes. Grant file reads below procfs only when Yama keeps
        // a sandboxed descendant from reading its parent or sibling process
        // state. READ_DIR remains denied, so procfs cannot be enumerated.
        push_rule(
            &mut profile,
            PathBuf::from("/proc"),
            LANDLOCK_ACCESS_FS_READ_FILE,
            true,
        )?;
    }
    // Naming an absolute executable is explicit authority to read and execute
    // that file, even when it lives outside the workspace and standard system
    // roots. This is common for verified CI/release artifacts under
    // `$RUNNER_TEMP`. Grant only the selected file, not its parent directory.
    let program_path = std::path::Path::new(program);
    if program_path.is_absolute() {
        push_rule(
            &mut profile,
            program_path.to_path_buf(),
            LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_EXECUTE,
            true,
        )?;
    }
    for root in process_sandbox_developer_toolchain_read_roots(policy) {
        push_rule(&mut profile, root, read_only_access(), true)?;
    }
    for root in developer_toolchain_system_read_roots(policy) {
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

fn network_name_service_read_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    if !policy_allows_network(policy) {
        return Vec::new();
    }
    [
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/nsswitch.conf",
        "/etc/gai.conf",
        "/etc/host.conf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

fn developer_toolchain_system_read_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    if process_sandbox_presets(policy).contains(&ProcessSandboxPreset::DeveloperToolchains) {
        // Linux vendor toolchains commonly live below /opt. In particular,
        // hosted runners may expose /usr/bin/go as a symlink into /opt; Landlock
        // evaluates the resolved target and therefore needs this read/execute
        // root even though PATH reports a system-runtime path.
        vec![PathBuf::from("/opt")]
    } else {
        Vec::new()
    }
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

/// Landlock's ruleset is allow-only: `landlock_add_rule` grants, and there is no
/// deny counterpart. A denial is therefore expressed by NOT granting — which
/// means a grant that contains a denied subtree has to be replaced by the set of
/// its children that do not lead to that subtree, recursively, down to the
/// denied path's parent.
///
/// Returns the paths to grant in place of `root`. Fails closed: if a directory
/// on the path to a denial cannot be enumerated, or the expansion exceeds
/// [`MAX_DENY_EXPANSION_RULES`], the caller refuses the spawn rather than
/// granting a root that would include the denied subtree.
///
/// Cost is one `read_dir` per level between `root` and each denial inside it,
/// which for the default credential denylist is a handful of home-directory
/// listings. A denial that is not inside `root` costs nothing.
///
/// Newly created siblings are NOT granted: a directory added after expansion is
/// invisible to the child. That is the safe direction, and it is why this is
/// done per spawn rather than cached.
fn expand_around_denied(root: &Path, denied: &[PathBuf]) -> Result<Vec<PathBuf>, VmError> {
    let inside: Vec<&PathBuf> = denied
        .iter()
        .filter(|deny| deny.starts_with(root) && deny.as_path() != root)
        .collect();
    if denied.iter().any(|deny| root.starts_with(deny)) {
        // The root IS denied, or sits inside a denial. Grant nothing.
        return Ok(Vec::new());
    }
    if inside.is_empty() {
        return Ok(vec![root.to_path_buf()]);
    }
    let mut granted: Vec<PathBuf> = Vec::new();
    for deny in &inside {
        let relative = deny.strip_prefix(root).map_err(|_| {
            sandbox_rejection(format!(
                "cannot subtract '{}' from sandbox root '{}'",
                deny.display(),
                root.display()
            ))
        })?;
        let mut cursor = root.to_path_buf();
        for component in relative.components() {
            // An ancestor we cannot enumerate ends the walk, and the walk
            // ending grants NOTHING below that directory. That is the safe
            // direction on an allow-only backend: the denial is expressed by
            // the absence of a grant, so stopping early is strictly narrower
            // than continuing, never wider.
            //
            // Two shapes reach here, and both must end the walk rather than
            // refuse the spawn:
            //
            // * NOT FOUND. Nothing below a missing directory exists, so there
            //   is nothing to subtract. Refusing here blocked every spawn on
            //   any host that simply had no `~/.kube` (measured on a downstream
            //   host's CI fleet, where it would have taken every run down).
            // * PERMISSION DENIED. We cannot list it, so we cannot grant its
            //   children; granting nothing is exactly right. Refusing here
            //   broke every run whose `$HOME` was not readable by the runtime
            //   (`/root` under the hardened conformance profile), turning an
            //   unreadable home into a total outage for no authority gained.
            //
            // The failure mode this must never become is granting `cursor`
            // itself, which would expose the denied path. Ending the walk does
            // not do that: nothing is pushed to `granted` on this iteration.
            let entries = match std::fs::read_dir(&cursor) {
                Ok(entries) => entries,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                    ) =>
                {
                    break
                }
                Err(error) => {
                    return Err(sandbox_rejection(format!(
                        "cannot enumerate '{}' to exclude denied path '{}': {error}; refusing \
                         the spawn rather than granting a root that would expose it",
                        cursor.display(),
                        deny.display()
                    )));
                }
            };
            let step = cursor.join(component.as_os_str());
            for entry in entries.flatten() {
                let sibling = entry.path();
                if sibling == step {
                    continue;
                }
                if denied
                    .iter()
                    .any(|d| sibling.starts_with(d) || d.starts_with(&sibling))
                {
                    // Another denial lives here; it is handled by its own pass.
                    continue;
                }
                if !granted.contains(&sibling) {
                    granted.push(sibling);
                }
            }
            cursor = step;
            if granted.len() > MAX_DENY_EXPANSION_RULES {
                return Err(sandbox_rejection(format!(
                    "excluding denied paths from sandbox root '{}' produced {} rules, over the \
                     {} cap; the directory that expanded was '{}' while excluding '{}'. Refusing \
                     the spawn rather than granting the root unsubtracted.",
                    root.display(),
                    granted.len(),
                    MAX_DENY_EXPANSION_RULES,
                    cursor.display(),
                    deny.display(),
                )));
            }
        }
    }
    Ok(granted)
}

/// Ceiling on complement expansion.
///
/// Landlock itself has no small documented rule limit — rules are added one
/// `landlock_add_rule` syscall at a time and bounded by memory — so this is a
/// guard against a pathological directory, not a kernel constraint. It is set
/// well above anything a real home produces: measured on the two Linux eval
/// hosts, the product-default denylist expands to well under a hundred rules
/// (see `report_default_denylist_expansion_cost`, which prints the live count).
///
/// Set high enough that a real machine cannot hit it, because the failure mode
/// is a refused spawn: a cap that trips on a busy home turns a security feature
/// into an outage.
const MAX_DENY_EXPANSION_RULES: usize = 4096;

fn push_rule(
    profile: &mut LandlockProfile,
    path: PathBuf,
    allowed_access: u64,
    optional: bool,
) -> Result<(), VmError> {
    let path = super::normalize_for_policy(&path);
    // Every Landlock grant funnels through here, so the subtraction lives here
    // too: no call site can add a root and forget to exclude the denylist.
    if !profile.read_deny_roots.is_empty() {
        let deny_roots = profile.read_deny_roots.clone();
        let expanded = expand_around_denied(&path, &deny_roots)?;
        if expanded.len() != 1 || expanded[0] != path {
            for replacement in expanded {
                push_rule_exact(profile, replacement, allowed_access, true)?;
            }
            return Ok(());
        }
    }
    push_rule_exact(profile, path, allowed_access, optional)
}

fn push_rule_exact(
    profile: &mut LandlockProfile,
    path: PathBuf,
    allowed_access: u64,
    optional: bool,
) -> Result<(), VmError> {
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        // An OPTIONAL root we cannot open is skipped, not fatal. Missing and
        // unreadable are the same answer here: we cannot grant it, and not
        // granting it is strictly narrower than the alternative.
        //
        // Only NotFound was tolerated before, and that turned an unreadable
        // preset root into a refusal of the entire spawn. It is reachable
        // whenever the runtime's `$HOME` is not its own: with `HOME=/root`
        // under a non-root uid, the `~/.asdf` preset root exists, cannot be
        // opened, and killed every confined command.
        //
        // A NON-optional root still fails closed: something explicitly asked
        // for it, so silently dropping it would be a grant the caller believes
        // it has.
        Err(error)
            if optional
                && matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) =>
        {
            // A root that EXISTS but cannot be opened is dropped silently
            // otherwise, and a silently narrower sandbox is the shape that
            // reads as success: the agent loses reach with nothing in the
            // transcript to explain it. Missing roots stay quiet, because an
            // absent optional root is the normal case and saying so every time
            // would bury this.
            if error.kind() == io::ErrorKind::PermissionDenied {
                super::warn_once(
                    &format!("sandbox_unreadable_root:{}", path.display()),
                    &format!(
                        "sandbox root '{}' exists but could not be opened ({error}); it was NOT \
                         granted. The child runs with less reach, not more.",
                        path.display()
                    ),
                );
            }
            return Ok(());
        }
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

fn proc_runtime_reads_are_contained() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
        .ok()
        .is_some_and(|value| yama_scope_contains_process_reads(&value))
}

fn yama_scope_contains_process_reads(value: &str) -> bool {
    value.trim().parse::<u8>().is_ok_and(|scope| scope >= 1)
}

/// Compile the syscall allowlist into a BPF program.
///
/// Runs before fork, in `profile_setup` — deliberately, on two counts:
///
/// * Compilation allocates, and `pre_exec` may only call async-signal-safe
///   functions. Building here leaves the child with nothing but two
///   allocation-free syscalls to make.
/// * A malformed policy surfaces as an ordinary `VmError` at spawn time
///   rather than as an opaque `pre_exec` failure.
///
/// `seccompiler` prefixes every program with an architecture check: it loads
/// `seccomp_data.arch` and returns `SECCOMP_RET_KILL_PROCESS` unless the
/// caller's ABI matches `target_arch`. That prologue is what makes the
/// allowlist sound.
///
/// Without it, a filter that matches on the syscall number alone is only as
/// strong as the ABI the caller chooses. An x86-64 process can re-enter the
/// kernel through the i386 compat gate with `int $0x80`, where the same
/// numbers name different syscalls — so every allowlisted number silently
/// grants whatever i386 assigns it. Concretely, for the allowlist in
/// [`allowed_syscalls`]: number 26 is `msync`, which we permit, and i386
/// number 26 is `ptrace`, which we deliberately withhold (see
/// `allowlist_excludes_process_introspection_and_io_uring`). The exclusion
/// held only for callers that agreed to use the ABI we expected.
fn compile_seccomp_program(allowed_syscalls: &[libc::c_long]) -> Result<BpfProgram, VmError> {
    // `c_long` is already `i64` on every target `target_arch()` accepts —
    // they are all LP64 — so the syscall numbers need no conversion.
    let rules = allowed_syscalls
        .iter()
        .map(|syscall| (*syscall, Vec::new()))
        .collect();

    // Denials return EPERM rather than killing: a child that trips the
    // ceiling should fail the individual call the way a permission error
    // would, not die without explanation. Architecture mismatches are the
    // exception and seccompiler always kills on those — a foreign ABI is
    // evidence of evasion, not of an ordinary denied operation.
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Errno(libc::EPERM as u32),
        SeccompAction::Allow,
        target_arch(),
    )
    .map_err(|err| sandbox_rejection(format!("failed to build the seccomp filter: {err}")))?;

    BpfProgram::try_from(filter)
        .map_err(|err| sandbox_rejection(format!("failed to compile the seccomp filter: {err}")))
}

/// The ABI this binary was built for, and therefore the only one the child is
/// permitted to enter the kernel through.
///
/// Deliberately exhaustive over what `seccompiler` supports: a Linux target it
/// does not know is a target we cannot confine, and failing the build says so
/// far more usefully than silently shipping an unfiltered sandbox.
const fn target_arch() -> TargetArch {
    #[cfg(target_arch = "x86_64")]
    {
        TargetArch::x86_64
    }
    #[cfg(target_arch = "aarch64")]
    {
        TargetArch::aarch64
    }
    #[cfg(target_arch = "riscv64")]
    {
        TargetArch::riscv64
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )))]
    {
        compile_error!(
            "the Linux sandbox backend has no seccomp target architecture for this platform; \
             seccompiler supports x86_64, aarch64, and riscv64"
        )
    }
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
#[path = "linux_tests.rs"]
mod tests;
