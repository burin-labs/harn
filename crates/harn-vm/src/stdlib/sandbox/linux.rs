//! Linux sandbox backend — Landlock LSM filesystem scoping plus
//! seccomp-bpf syscall allowlisting installed via `pre_exec`.
//!
//! See `docs/src/sandboxing.md` for the capability → kernel-knob
//! mapping table.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

use super::{
    policy_allows_capability, policy_allows_network, process_sandbox_presets, sandbox_rejection,
    PrepareOutcome, SandboxBackend,
};
use crate::orchestration::{CapabilityPolicy, ProcessSandboxPreset, SandboxProfile};
use crate::value::VmError;

pub(super) struct Backend;

#[path = "linux_descriptors.rs"]
mod descriptors;
use crate::process_sandbox::DeviceMountFinalization;
pub use descriptors::DescriptorTransfer;
#[path = "linux_bwrap.rs"]
pub(super) mod bwrap;
pub(crate) use bwrap::sealed_filter as sealed_launch_bytes;
#[path = "linux_filesystem.rs"]
mod filesystem;
#[path = "linux_landlock.rs"]
mod landlock_support;
#[path = "linux_nesting.rs"]
mod nesting;
#[path = "linux_self_confinement.rs"]
mod self_confinement;
use filesystem::filesystem_profile;
pub(crate) use filesystem::installed_write_grants;
use landlock_support::{AccessFs, FsAccess, LandlockSupport, ABI};
use nesting::nested_seccomp_filter_rule;

impl SandboxBackend for Backend {
    fn name() -> &'static str {
        "linux"
    }

    fn filesystem_mechanism() -> super::SandboxMechanism {
        if landlock_available() {
            super::SandboxMechanism::LinuxLandlock
        } else {
            super::SandboxMechanism::LinuxBubblewrap
        }
    }

    fn mechanism_limits() -> Vec<super::enforcement::MechanismLimit> {
        landlock_support::host_support()
            .map(LandlockSupport::limits)
            .unwrap_or_default()
    }

    fn confine_current_process(
        policy: &CapabilityPolicy,
    ) -> Result<super::SandboxMechanism, VmError> {
        self_confinement::confine(policy)?;
        Ok(super::SandboxMechanism::LinuxLandlock)
    }

    fn available() -> bool {
        true
    }

    fn filesystem_available() -> bool {
        // Seccomp may constrain syscalls when Landlock is absent, but it cannot
        // deny an out-of-workspace write or count as filesystem confinement.
        landlock_available() || bwrap::available()
    }

    fn prepare_std_command(
        program: &str,
        args: &[String],
        command: &mut Command,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        if !landlock_available() {
            return bwrap::prepare(program, args, policy, profile);
        }
        let mut prep = profile_setup(program, policy, profile)?;
        if let Some(launcher) = resolve_netns_launcher(policy)? {
            return Ok(namespaced_outcome(launcher, program, args, &mut prep));
        }
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
        args: &[String],
        command: &mut tokio::process::Command,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        if !landlock_available() {
            return bwrap::prepare(program, args, policy, profile);
        }
        let mut prep = profile_setup(program, policy, profile)?;
        if let Some(launcher) = resolve_netns_launcher(policy)? {
            return Ok(namespaced_outcome(launcher, program, args, &mut prep));
        }
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

/// A populated ruleset, ready for the process that will `exec` the payload to
/// enter. Rules are added in the parent, so only the descriptor travels.
struct LandlockProfile {
    ruleset: OwnedFd,
    private_tmp: Option<PathBuf>,
}

struct FilesystemProfile {
    rules: Vec<FilesystemRule>,
    symlinks: std::collections::BTreeMap<PathBuf, PathBuf>,
    handled_access_fs: FsAccess,
    /// Subtrees no grant may cover. Landlock has no deny rule, so this is
    /// enforced by never granting a path that contains one; see
    /// [`expand_around_denied`].
    read_deny_roots: Vec<PathBuf>,
    /// Denials a writable grant contains, which no grant can hold; see
    /// [`filesystem::keep_writable_grant`].
    unheld_read_denials: Vec<filesystem::UnheldReadDenial>,
}

struct FilesystemRule {
    file: std::fs::File,
    path: PathBuf,
    allowed_access: FsAccess,
}

enum ProcessFilesystemScope {
    Host,
    PrivatePidNamespace,
}

impl LandlockProfile {
    /// Hand the populated ruleset over.
    fn take_ruleset(self) -> OwnedFd {
        self.ruleset
    }
}

/// A confinement built in one process and entered in another.
///
/// # Why this exists
///
/// The owner-death guardian re-execs this binary and rebuilds the payload
/// command from a serialized program, args, cwd and environment. A `pre_exec`
/// closure has no representation in that projection, so this backend's
/// confinement was dropped in the handover and the payload ran completely
/// unconfined while the run's receipt still reported the profile as enforced.
/// macOS never showed it because its backend puts the sandbox in argv, which
/// the same projection preserves.
///
/// What does survive being handed to another process is a descriptor and a byte
/// string. A Landlock ruleset is exactly that once its rules are added, and a
/// compiled seccomp program is already a byte string. So the side that has the
/// policy, the open directory descriptors and an allocator does all of that
/// work, and the side that will `exec` the payload is left with two
/// async-signal-safe syscalls it can make from `pre_exec`.
pub struct TransferableConfinement {
    ruleset: Option<OwnedFd>,
    seccomp: BpfProgram,
    /// The session temp dir the namespace helper mounts over `/tmp`, decided
    /// with the ruleset; see [`netns::private_tmp_dir`].
    private_tmp: Option<PathBuf>,
}

/// One `sock_filter` on the wire: code, jt, jf, k, little-endian.
const SECCOMP_INSTRUCTION_BYTES: usize = 8;

impl TransferableConfinement {
    fn take_from(prep: &mut ProcessProfile) -> Self {
        let landlock = prep.landlock.take();
        Self {
            private_tmp: landlock.as_ref().and_then(|l| l.private_tmp.clone()),
            ruleset: landlock.map(LandlockProfile::take_ruleset),
            seccomp: std::mem::take(&mut prep.seccomp),
        }
    }

    /// The ruleset descriptor, or `None` on a host with no Landlock where the
    /// resolved fallback allows the run to proceed without it.
    pub fn ruleset_fd(&self) -> Option<RawFd> {
        self.ruleset.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// The compiled seccomp program, flattened for transport.
    pub fn seccomp_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.seccomp.len() * SECCOMP_INSTRUCTION_BYTES);
        for instruction in &self.seccomp {
            bytes.extend_from_slice(&instruction.code.to_le_bytes());
            bytes.push(instruction.jt);
            bytes.push(instruction.jf);
            bytes.extend_from_slice(&instruction.k.to_le_bytes());
        }
        bytes
    }

    /// Rebuild on the far side of a handover. Takes ownership of `ruleset_fd`.
    ///
    /// Call this BEFORE forking the payload: it allocates, and [`Self::enter`]
    /// must not.
    pub fn from_parts(ruleset_fd: Option<RawFd>, seccomp: &[u8]) -> io::Result<Self> {
        if !seccomp.len().is_multiple_of(SECCOMP_INSTRUCTION_BYTES) {
            return Err(io::Error::other(
                "transferred seccomp program is not a whole number of instructions",
            ));
        }
        let program = seccomp
            .chunks_exact(SECCOMP_INSTRUCTION_BYTES)
            .map(|chunk| seccompiler::sock_filter {
                code: u16::from_le_bytes([chunk[0], chunk[1]]),
                jt: chunk[2],
                jf: chunk[3],
                k: u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]),
            })
            .collect();
        Ok(Self {
            // SAFETY: the caller states this descriptor was inherited for this
            // purpose and is not owned elsewhere in this process.
            ruleset: ruleset_fd.map(|fd| unsafe { OwnedFd::from_raw_fd(fd) }),
            seccomp: program,
            // The helper has already mounted it by the time it rebuilds this.
            private_tmp: None,
        })
    }

    /// Confine the calling process. Async-signal-safe, so it is legal from
    /// `pre_exec`: two raw syscalls for Landlock and one for seccomp, with no
    /// allocation, locking, or I/O.
    pub fn enter(&self) -> io::Result<()> {
        if let Some(ruleset) = self.ruleset.as_ref() {
            landlock_support::enter_ruleset(ruleset.as_raw_fd())?;
            confirm_filesystem_boundary_holds()?;
        }
        seccompiler::apply_filter(&self.seccomp)
            .map_err(|err| io::Error::other(format!("failed to install the seccomp filter: {err}")))
    }

    /// Release the ruleset descriptor so it can be sent across an `exec`.
    pub fn into_ruleset_fd(self) -> Option<RawFd> {
        self.ruleset.map(IntoRawFd::into_raw_fd)
    }
}

/// Confirm the boundary is real, in the process that just entered it.
///
/// A syscall returning zero says the kernel accepted the request, not that the
/// request is doing anything. Every layer above reads "enforced" off that
/// acceptance, which is how a confinement that was built and never applied could
/// be reported as holding for as long as it was.
///
/// The filesystem root is the one probe that needs no knowledge of the grants:
/// it always exists, so a denial cannot be confused with an absence, and it can
/// never be granted, because a root that broad is refused when the policy is
/// built. Opening it after `restrict_self` therefore has exactly one meaning.
///
/// Async-signal-safe: one `open` and at most one `close`, no allocation.
///
/// A failed open is the expected answer and needs no distinguishing: any error
/// means the process could not read the root, which is the property being
/// confirmed.
fn confirm_filesystem_boundary_holds() -> io::Result<()> {
    let root = unsafe { libc::open(c"/".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    if root < 0 {
        return Ok(());
    }
    unsafe {
        libc::close(root);
    }
    Err(io::Error::other(
        "the Landlock ruleset was entered but the filesystem boundary is not holding: the root \
         directory is still readable, so this process is not confined",
    ))
}

/// Build a confinement for `program` under the ambient policy, ready to hand to
/// whatever process will actually `exec` it.
///
/// `None` means this run installs no child confinement at all, which is the
/// same answer [`super::std_command_for`] acts on. It is NOT "confinement was
/// requested and could not be built": that is an error, and it is returned as
/// one, so a caller can never mistake a refusal for an absent request.
/// This Landlock-specific handover refuses when Landlock is unavailable;
/// bubblewrap's mount descriptors travel through `command_for_reexec`.
pub fn transferable_confinement(program: &str) -> Result<Option<TransferableConfinement>, VmError> {
    let Some((policy, profile)) = super::active_sandbox_policy() else {
        return Ok(None);
    };
    super::ensure_spawn_enforceable::<Backend>(&policy)?;
    // Resolve exactly as the direct spawn path does. The resolved path is what
    // the ruleset grants read and execute on, so a bare name here would build a
    // ruleset that refuses the very program it was built for.
    let resolved = crate::stdlib::process::resolve_program_path_for_spawn(program);
    let mut prepared = profile_setup(&resolved, &policy, profile)?;
    Ok(Some(TransferableConfinement::take_from(&mut prepared)))
}

fn profile_setup(
    program: &str,
    policy: &CapabilityPolicy,
    profile: SandboxProfile,
) -> Result<ProcessProfile, VmError> {
    let support = host_landlock(profile)?;
    // An older ABI that a child can trip over is named once, so a refused
    // rename or truncation reads as this host's kernel, not as a broken tool.
    let observable: Vec<&str> = support
        .limits()
        .into_iter()
        .filter(|limit| limit.changes_child_behavior())
        .map(|limit| limit.description())
        .collect();
    if !observable.is_empty() {
        super::warn_once(
            "landlock_abi_limits",
            &format!(
                "Linux Landlock ABI {} on this host confines child processes, but {}",
                support.abi(),
                observable.join(", and ")
            ),
        );
    }
    profile_setup_for(program, policy, support)
}

/// The Landlock this host offers, or the typed refusal for its absence.
fn host_landlock(profile: SandboxProfile) -> Result<LandlockSupport, VmError> {
    landlock_support::host_support().ok_or_else(|| {
        super::SandboxMechanismUnavailable::new(
            super::SandboxMechanism::LinuxLandlock,
            super::SandboxMechanismAvailability::AbsentOnHost,
            profile,
        )
        .into_error()
    })
}

fn profile_setup_for(
    program: &str,
    policy: &CapabilityPolicy,
    support: LandlockSupport,
) -> Result<ProcessProfile, VmError> {
    if policy.process_network_proxy.is_some() {
        return Err(sandbox_rejection(
            "managed child-process egress requires a proxy-only Linux network namespace; this build cannot enforce that boundary"
                .to_string(),
        ));
    }
    // Loopback-only networking is rendered by the namespace helper, not here,
    // and it is refused rather than approximated when the helper is missing.
    // `resolve_netns_launcher` owns that decision so the spawn path and this
    // one cannot disagree about whether the grant is available.
    resolve_netns_launcher(policy)?;
    // Socket roots supply serving authority; the separate SSH agent grant
    // scopes its connection by inode before seccomp admits connect.
    // The selected Landlock renderer must attach a filesystem boundary; it
    // cannot return a seccomp-only profile when that mechanism is unavailable.
    let landlock = landlock_profile_for(program, policy, support)?;
    Ok(ProcessProfile {
        landlock: Some(landlock),
        seccomp: compile_seccomp_program(policy, support.bounds_truncation())?,
    })
}

/// Whether this policy asks for, and this backend will render, serve-only
/// local IPC over Unix-domain sockets.
///
/// A policy that already permits general networking is not a local-IPC case:
/// it has `connect` for its own reasons, and re-deciding it here would either
/// take away authority the policy was granted or imply this term protects
/// something it does not.
fn unix_socket_local_ipc_grant(policy: &CapabilityPolicy) -> bool {
    !policy.process_sandbox.unix_socket_roots.is_empty() && !policy_allows_network(policy)
}

pub(super) fn ssh_agent_path_scope_available() -> bool {
    LandlockSupport::negotiate(ABI::V9).is_some_and(|support| support.abi() >= ABI::V9)
}

fn apply_profile(profile: &ProcessProfile) -> io::Result<()> {
    if let Some(landlock) = &profile.landlock {
        landlock_support::enter_ruleset(landlock.ruleset.as_raw_fd())?;
    }
    // Once seccomp is default-deny, the child keeps only the setup calls that
    // narrow it further (stacking Landlock and seccomp, for its own confined
    // children). Install Landlock first, then drop to the runtime syscall
    // ceiling.
    //
    // `apply_filter` sets `PR_SET_NO_NEW_PRIVS` and issues `SYS_seccomp`
    // against the already-compiled program: no allocation, so it is safe on
    // the `pre_exec` side of the fork.
    seccompiler::apply_filter(&profile.seccomp)
        .map_err(|err| io::Error::other(format!("failed to install the seccomp filter: {err}")))?;
    Ok(())
}

/// Build and populate the ruleset `program` runs under, with every right the
/// host's Landlock offers handled.
///
/// Runs in the PARENT, before fork. Adding rules needs the open directory
/// descriptors the profile holds and a `Vec` walk, neither of which belongs on
/// the `pre_exec` side; and a populated ruleset is a self-contained object that
/// a descriptor alone can carry, which is what lets a confinement survive being
/// handed to another process (see [`transferable_confinement`]).
fn landlock_profile_for(
    program: &str,
    policy: &CapabilityPolicy,
    support: LandlockSupport,
) -> Result<LandlockProfile, VmError> {
    // Abstract Unix sockets have no filesystem path, so the path scoping that
    // covers a socket file cannot reach them: without this, a serve-only grant
    // would still let a child speak to any abstract socket on the host. ABI 6
    // added a domain scope that confines them to this sandbox and its
    // descendants, which is the containment the grant claims. An older kernel
    // cannot express it, so the grant is refused there rather than issued
    // half-enforced.
    super::enforcement::validate_ssh_agent(policy)?;
    let scope_unix_paths =
        policy.process_sandbox.ssh_agent_socket.is_some() && !policy_allows_network(policy);
    if scope_unix_paths && support.abi() < ABI::V9 {
        return Err(sandbox_rejection(
            "SSH agent socket connection requires Landlock ABI 9 on this host".into(),
        ));
    }
    let scope_abstract_unix = unix_socket_local_ipc_grant(policy) || scope_unix_paths;
    if scope_abstract_unix && !support.scopes_abstract_unix_sockets() {
        return Err(sandbox_rejection(format!(
            "serve-only Unix-domain sockets need Landlock ABI {} to contain abstract sockets; this host reports ABI {}",
            ABI::V6,
            support.abi()
        )));
    }
    let mut filesystem = filesystem_profile(
        program,
        policy,
        support.handled_access(),
        ProcessFilesystemScope::Host,
    )?;
    if scope_unix_paths {
        use std::os::unix::fs::OpenOptionsExt;
        let path = PathBuf::from(policy.process_sandbox.ssh_agent_socket.as_ref().unwrap());
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|error| sandbox_rejection(format!("cannot pin SSH agent socket: {error}")))?;
        filesystem.rules.push(FilesystemRule {
            file,
            path,
            allowed_access: AccessFs::ResolveUnix.into(),
        });
    }
    let private_tmp = netns::private_tmp_dir(
        policy,
        filesystem.rules.iter().map(|rule| rule.path.as_path()),
    );
    if let Some(tmp) = &private_tmp {
        // Landlock follows the path a file is reached by, and `/tmp/x` reached
        // through the mount never passes the workspace root that grants it.
        push_rule(
            &mut filesystem,
            tmp.clone(),
            workspace_access(policy),
            false,
        )?;
    }
    let ruleset = support
        .populated_ruleset(
            scope_abstract_unix,
            scope_unix_paths,
            filesystem
                .rules
                .iter()
                .map(|rule| (&rule.file, rule.allowed_access)),
        )
        .map_err(|error| {
            sandbox_rejection(format!(
                "failed to build the Linux Landlock ruleset: {error}"
            ))
        })?;
    Ok(LandlockProfile {
        ruleset,
        private_tmp,
    })
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

fn standard_device_rules() -> Vec<(PathBuf, FsAccess)> {
    [
        ("/dev/null", AccessFs::ReadFile | AccessFs::WriteFile),
        ("/dev/zero", AccessFs::ReadFile.into()),
        ("/dev/random", AccessFs::ReadFile.into()),
        ("/dev/urandom", AccessFs::ReadFile.into()),
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
///
/// Only read-only grants are subtracted; see [`filesystem::keep_writable_grant`].
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
                    break;
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
    profile: &mut FilesystemProfile,
    path: PathBuf,
    allowed_access: FsAccess,
    optional: bool,
) -> Result<(), VmError> {
    let mut pending = vec![(path, optional)];
    let mut observed = std::collections::BTreeSet::new();
    while let Some((path, optional)) = pending.pop() {
        // Snapshot aliases for the mount renderer before normalizing. They
        // expose only targets mounted from admitted handles, never host data.
        for ancestor in path.ancestors() {
            if let Ok(target) = std::fs::read_link(ancestor) {
                profile.symlinks.insert(ancestor.to_path_buf(), target);
            }
        }
        let path = super::normalize_for_policy(&path);
        if !observed.insert(path.clone()) {
            continue;
        }
        if observed.len() > MAX_DENY_EXPANSION_RULES {
            return Err(sandbox_rejection(
                "credential subtraction exceeded its grant expansion ceiling".into(),
            ));
        }
        if allowed_access.intersects(filesystem::LANDLOCK_WRITE_ACCESS) {
            if filesystem::keep_writable_grant(profile, &path) {
                push_rule_exact(profile, path, allowed_access, optional)?;
            }
            continue;
        }
        // Expanded siblings can themselves be aliases to denied paths, or
        // back to a previously expanded root. Normalize and subtract every
        // candidate, with a visited set so aliases cannot recurse forever.
        let expanded = expand_around_denied(&path, &profile.read_deny_roots)?;
        if expanded.len() == 1 && expanded[0] == path {
            push_rule_exact(profile, path, allowed_access, optional)?;
        } else {
            pending.extend(expanded.into_iter().map(|path| (path, true)));
        }
    }
    Ok(())
}

fn push_rule_exact(
    profile: &mut FilesystemProfile,
    path: PathBuf,
    allowed_access: FsAccess,
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
    // A symlink or ancestor changed between normalization and open. The
    // descriptor is already pinned, so check its actual target before it can
    // become authority instead of granting a substituted denied inode.
    let opened_path =
        std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).map_err(|error| {
            sandbox_rejection(format!(
                "cannot identify pinned sandbox grant {}: {error}",
                path.display()
            ))
        })?;
    if opened_path != path {
        return Err(sandbox_rejection(format!(
            "sandbox grant {} changed during preparation",
            path.display()
        )));
    }
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
    profile.rules.push(FilesystemRule {
        file,
        path,
        allowed_access: allowed_access & profile.handled_access_fs,
    });
    Ok(())
}

pub(crate) fn proc_runtime_reads_are_contained() -> bool {
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
///
/// `filesystem_bounds_truncation` is false only under a Landlock ABI that
/// cannot scope truncation; the filter then refuses the calls that truncate by
/// path (see [`landlock_support::filter_path_truncation`]).
fn compile_seccomp_program(
    policy: &CapabilityPolicy,
    filesystem_bounds_truncation: bool,
) -> Result<BpfProgram, VmError> {
    // `c_long` is already `i64` on every target `target_arch()` accepts —
    // they are all LP64 — so the syscall numbers need no conversion.
    let mut rules: std::collections::BTreeMap<libc::c_long, Vec<SeccompRule>> =
        allowed_syscalls(policy)
            .iter()
            .map(|syscall| (*syscall, Vec::new()))
            .collect();
    if !filesystem_bounds_truncation {
        landlock_support::filter_path_truncation(&mut rules)?;
    }

    if unix_socket_local_ipc_grant(policy)
        || (policy.process_sandbox.ssh_agent_socket.is_some() && !policy_allows_network(policy))
    {
        // The family filter withholds IP networking. Landlock scopes any
        // admitted connection; only a socket-root grant also admits serving.
        let unix_domain_only = SeccompRule::new(vec![SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Eq,
            libc::AF_UNIX as u64,
        )
        .map_err(|err| {
            sandbox_rejection(format!("failed to build the Unix-domain condition: {err}"))
        })?])
        .map_err(|err| sandbox_rejection(format!("failed to build the socket rule: {err}")))?;
        rules.insert(libc::SYS_socket, vec![unix_domain_only]);
        if policy.process_sandbox.ssh_agent_socket.is_some() {
            rules.entry(libc::SYS_connect).or_default();
        }
        if unix_socket_local_ipc_grant(policy) {
            for syscall in [
                libc::SYS_bind,
                libc::SYS_listen,
                libc::SYS_accept,
                libc::SYS_accept4,
                libc::SYS_getsockname,
            ] {
                rules.entry(syscall).or_default();
            }
        }
    }
    admit_own_identity_changes(&mut rules)?;
    // `seccomp(SECCOMP_SET_MODE_FILTER, 0, prog)` is exactly what a nested
    // Harn issues to install its own ceiling (`seccompiler::apply_filter`).
    // It grants nothing new: `prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER)` is
    // already admitted, and the kernel evaluates every stacked filter and
    // takes the most restrictive action, so an added filter can only narrow.
    // Only that form is admitted. A flag such as `NEW_LISTENER` or `TSYNC`,
    // and the other operations, keep failing with EPERM.
    rules.insert(libc::SYS_seccomp, vec![nested_seccomp_filter_rule()?]);
    let rules: std::collections::BTreeMap<libc::c_long, Vec<SeccompRule>> = rules;

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

/// Admit the uid and gid setters only for calls that leave every credential slot
/// as it is: each argument is `-1` (unchanged) or the value its own slot already
/// holds (real, effective or saved, by position).
///
/// GNU make's `posix_spawn` child calls `setresuid(-1, euid, -1)` before it
/// execs a recipe, so withholding the setters made every recipe fail with
/// `Operation not permitted`. Matching by slot matters: `setresuid(-1, saved,
/// -1)` would move the effective id to a different held id, which regains a
/// saved privileged identity. `setuid` and `setgid` set several slots at once,
/// so they are admitted only when real, effective and saved already agree.
fn admit_own_identity_changes(
    rules: &mut std::collections::BTreeMap<libc::c_long, Vec<SeccompRule>>,
) -> Result<(), VmError> {
    let (mut real, mut effective, mut saved) = (0, 0, 0);
    let (mut real_group, mut effective_group, mut saved_group) = (0, 0, 0);
    // SAFETY: each call writes only to the three integers it is given.
    let read = unsafe {
        libc::getresuid(&raw mut real, &raw mut effective, &raw mut saved) == 0
            && libc::getresgid(
                &raw mut real_group,
                &raw mut effective_group,
                &raw mut saved_group,
            ) == 0
    };
    if !read {
        return Err(sandbox_rejection(
            "failed to read the process's own ids for the identity-setter rules".to_string(),
        ));
    }
    let unchanged = u64::from(u32::MAX);
    let slot = |held: u32| vec![unchanged, u64::from(held)];
    let users = [slot(real), slot(effective), slot(saved)];
    let groups = [slot(real_group), slot(effective_group), slot(saved_group)];
    // (syscall, admitted values per argument position)
    let mut setters: Vec<(libc::c_long, Vec<Vec<u64>>)> = vec![
        (libc::SYS_setreuid, users[..2].to_vec()),
        (libc::SYS_setresuid, users.to_vec()),
        (libc::SYS_setregid, groups[..2].to_vec()),
        (libc::SYS_setresgid, groups.to_vec()),
    ];
    if real == effective && effective == saved {
        setters.push((libc::SYS_setuid, vec![vec![u64::from(effective)]]));
    }
    if real_group == effective_group && effective_group == saved_group {
        setters.push((libc::SYS_setgid, vec![vec![u64::from(effective_group)]]));
    }
    for (syscall, positions) in setters {
        let combinations = admitted_combinations(&positions);
        let admitted = combinations
            .into_iter()
            .map(|values| {
                let conditions = values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        SeccompCondition::new(
                            index as u8,
                            SeccompCmpArgLen::Dword,
                            SeccompCmpOp::Eq,
                            *value,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|err| {
                        sandbox_rejection(format!(
                            "failed to build an identity-setter condition: {err}"
                        ))
                    })?;
                SeccompRule::new(conditions).map_err(|err| {
                    sandbox_rejection(format!("failed to build an identity-setter rule: {err}"))
                })
            })
            .collect::<Result<Vec<_>, VmError>>()?;
        rules.insert(syscall, admitted);
    }
    Ok(())
}

/// Rules for one syscall are alternatives, so the cross product of the values
/// admitted at each argument position is the set of admitted calls.
pub(super) fn admitted_combinations(positions: &[Vec<u64>]) -> Vec<Vec<u64>> {
    let mut combinations: Vec<Vec<u64>> = vec![Vec::new()];
    for values in positions {
        combinations = combinations
            .into_iter()
            .flat_map(|prefix| {
                values.iter().map(move |value| {
                    let mut next = prefix.clone();
                    next.push(*value);
                    next
                })
            })
            .collect();
    }
    combinations
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

#[cfg(target_arch = "x86_64")]
const SYS_FCHMODAT2: libc::c_long = libc::SYS_fchmodat2;

// aarch64 and riscv64 use Linux's asm-generic syscall table. libc does not
// currently expose SYS_fchmodat2 for those targets even though their kernels
// assign it the same generic number.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
const SYS_FCHMODAT2: libc::c_long = 452;

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
        SYS_FCHMODAT2,
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
        // A confined child may confine its own children further. Landlock
        // domains stack: a nested `landlock_restrict_self` intersects with the
        // inherited domain and can never widen it. Without these, a confined
        // Harn sees no Landlock, falls back to Bubblewrap, and Bubblewrap needs
        // a user namespace this ceiling withholds, so every nested confined
        // command fails (harn#9454). `seccomp` is admitted separately, with
        // its arguments filtered, in `compile_seccomp_program`.
        libc::SYS_landlock_create_ruleset,
        libc::SYS_landlock_add_rule,
        libc::SYS_landlock_restrict_self,
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

    if policy_allows_network(policy) || namespaced_loopback_grant(policy) {
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
fn read_only_access() -> FsAccess {
    AccessFs::from_read(ABI::V1)
}

fn workspace_access(policy: &CapabilityPolicy) -> FsAccess {
    let read_access = read_only_access();
    // Every right that changes a file, through truncation. `IOCTL_DEV` is
    // left out: no workspace grant needs to drive a device.
    let write_access = AccessFs::from_write(ABI::V3);
    // The child write grant is the whole write set, whatever subset of it the
    // policy's own `workspace` capability names: the grant exists for roles
    // whose capability names none of it.
    if !policy.capabilities_are_restricted() || policy.process_sandbox.allow_child_workspace_write {
        return read_access | write_access;
    }
    let mut access = FsAccess::empty();
    if policy_allows_capability(policy, "workspace", &["read_text", "list", "exists"]) {
        access |= read_access;
    }
    if policy_allows_capability(policy, "workspace", &["write_text"]) {
        access |= write_access;
    }
    if policy_allows_capability(policy, "workspace", &["delete"]) {
        access |= AccessFs::RemoveDir | AccessFs::RemoveFile;
    }
    if access.is_empty() {
        read_access
    } else {
        access
    }
}

/// Whether this host's Landlock holds a child restriction. Any ABI qualifies;
/// a grant that needs a newer one refuses on its own.
pub(super) fn landlock_available() -> bool {
    landlock_support::host_support().is_some()
}

/// Access rights that Landlock only permits on a *directory* handle. A
/// PATH_BENEATH rule that pairs any of these with a non-directory
/// `parent_fd` is rejected with EINVAL, so [`push_rule`] strips them when
/// the resolved path is a regular file. (`READ_FILE`, `WRITE_FILE`,
/// `EXECUTE`, `TRUNCATE`, and `IOCTL_DEV` are valid on file handles and
/// are intentionally absent here.)
const DIRECTORY_ONLY_ACCESS_FS: FsAccess = landlock_support::make_bitflags!(AccessFs::{
    ReadDir
        | RemoveDir
        | RemoveFile
        | MakeChar
        | MakeDir
        | MakeReg
        | MakeSock
        | MakeFifo
        | MakeBlock
        | MakeSym
        | Refer
});

#[path = "netns.rs"]
mod netns;

pub use super::command_for::command_for_reexec;
pub use netns::decode_seccomp_hex;
pub use netns::keep_ruleset_across_exec;
pub(crate) use netns::keep_ruleset_across_exec_tokio;
pub use netns::ReexecConfinement;
pub(super) use netns::{launcher_argv_with_ruleset, resolve_netns_launcher};
use netns::{namespaced_loopback_grant, namespaced_outcome};

#[cfg(test)]
#[path = "linux_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "linux_path_grant_tests.rs"]
mod path_grant_tests;

#[cfg(test)]
#[path = "netns_tests.rs"]
mod netns_tests;

#[cfg(test)]
#[path = "linux_socket_root_tests.rs"]
mod socket_root_tests;

#[cfg(test)]
#[path = "linux_credential_denylist_tests.rs"]
mod credential_denylist_tests;

#[cfg(test)]
#[path = "linux_writable_denial_tests.rs"]
mod writable_denial_tests;
