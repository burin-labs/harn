//! Landlock through the `landlock` crate, which Landlock's maintainer owns.
//!
//! The crate answers what the running kernel supports, creates the ruleset, and
//! adds the rules. One step stays a raw syscall: entering a populated ruleset.
//! It runs on the `pre_exec` side of a fork, or in a helper that received the
//! ruleset as a bare descriptor across an `exec`. The crate guarantees neither
//! async-signal safety nor a way to adopt a descriptor it did not create.
//!
//! # Per-requirement ABI floor
//!
//! Every Landlock ABI from 1 up confines reads, writes, and execution beneath
//! the granted roots, which is the whole filesystem boundary. Later ABIs close
//! narrower gaps, and each gap is either covered another way or refused only
//! for the grant that needs it:
//!
//! * ABI 2 (`REFER`): below it the kernel refuses every cross-directory rename
//!   or link. That is narrower than the policy, never wider, so it costs
//!   compatibility and is reported as a limit.
//! * ABI 3 (`TRUNCATE`): below it truncation by path is outside Landlock. The
//!   seccomp filter refuses the calls that could reach an ungranted file that
//!   way; see [`LandlockSupport::bounds_truncation`].
//! * ABI 5 (`IOCTL_DEV`): below it an `ioctl` on a device opened under a grant
//!   is not scoped. The only devices granted by default are the four standard
//!   character devices, so this is reported, not refused.
//! * ABI 6 (abstract Unix socket scope): needed only by the serve-only Unix
//!   socket grant, which is refused below it. Without that grant seccomp
//!   withholds `socket` below the network ceiling, so no abstract socket can be
//!   created or reached; at the ceiling the policy already grants `connect`.

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};

pub(crate) use landlock::{make_bitflags, AccessFs, BitFlags, ABI};
use landlock::{
    Access, AccessNet, CompatLevel, Compatible, PathBeneath, Ruleset, RulesetAttr,
    RulesetCreatedAttr, Scope,
};

use super::super::enforcement::MechanismLimit;

/// A set of Landlock filesystem rights.
pub(crate) type FsAccess = BitFlags<AccessFs>;

/// Base filesystem rights. Exact SSH agent grants add `ResolveUnix` separately;
/// handling it for every policy would narrow an existing general network grant.
const VOCABULARY: ABI = ABI::V5;

/// The newest ABI the negotiation distinguishes for grants Harn renders.
pub(crate) const NEWEST_DISTINGUISHED: ABI = ABI::V9;

/// What the running kernel's Landlock offers Harn, at most up to the ceiling it
/// was negotiated under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LandlockSupport {
    abi: ABI,
}

impl LandlockSupport {
    /// The newest ABI at or below `ceiling` that this kernel supports, or
    /// `None` when Landlock is absent or disabled.
    ///
    /// A ceiling below the kernel's ABI builds exactly the ruleset an older
    /// kernel would get: an unhandled right behaves the same on every kernel,
    /// which is what lets a test exercise ABI 1 on a current host.
    pub(crate) fn negotiate(ceiling: ABI) -> Option<Self> {
        Self::negotiate_with(ceiling, kernel_creates_ruleset_for)
    }

    /// The newest candidate at or below `ceiling` that `accepts` admits. The
    /// kernel check is a parameter so the selection itself can be tested
    /// against kernels this host is not.
    pub(crate) fn negotiate_with(ceiling: ABI, accepts: impl Fn(ABI) -> bool) -> Option<Self> {
        [
            ABI::V9,
            ABI::V6,
            ABI::V5,
            ABI::V4,
            ABI::V3,
            ABI::V2,
            ABI::V1,
        ]
        .into_iter()
        .filter(|abi| *abi <= ceiling)
        .find(|abi| accepts(*abi))
        .map(|abi| Self { abi })
    }

    /// Support as an ABI would offer it, without asking the kernel, for the
    /// decisions that read only the ABI.
    #[cfg(test)]
    pub(crate) fn assume(abi: ABI) -> Self {
        Self { abi }
    }

    pub(crate) fn abi(self) -> ABI {
        self.abi
    }

    /// The filesystem rights a ruleset handles, so every right outside a grant
    /// is denied.
    pub(crate) fn handled_access(self) -> FsAccess {
        AccessFs::from_all(self.abi.min(VOCABULARY))
    }

    /// Whether Landlock refuses truncation outside the grants. Below ABI 3 it
    /// cannot, and the syscall filter has to refuse the calls that truncate by
    /// path instead.
    pub(crate) fn bounds_truncation(self) -> bool {
        self.abi >= ABI::V3
    }

    /// Whether a ruleset can confine abstract Unix sockets to its own domain.
    pub(crate) fn scopes_abstract_unix_sockets(self) -> bool {
        self.abi >= ABI::V6
    }

    /// Every way this ABI falls short of the newest one Harn distinguishes.
    pub(crate) fn limits(self) -> Vec<MechanismLimit> {
        let mut limits = Vec::new();
        if self.abi < ABI::V2 {
            limits.push(MechanismLimit::CrossDirectoryRenameRefused);
        }
        if !self.bounds_truncation() {
            limits.push(MechanismLimit::TruncationFilteredBySyscall);
        }
        if self.abi < ABI::V5 {
            limits.push(MechanismLimit::DeviceIoctlUnscoped);
        }
        if !self.scopes_abstract_unix_sockets() {
            limits.push(MechanismLimit::UnixSocketGrantUnavailable);
        }
        limits
    }

    /// Create a ruleset handling every right this ABI offers, populated with
    /// `rules`, and hand it over as the descriptor a process enters.
    ///
    /// Every request is a hard requirement. The ABI was negotiated first, so
    /// nothing asked for here is beyond the kernel; a failure is a real error,
    /// never a silently weaker ruleset.
    pub(crate) fn populated_ruleset<'a>(
        self,
        scope_abstract_unix_sockets: bool,
        scope_unix_paths: bool,
        rules: impl IntoIterator<Item = (&'a File, FsAccess)>,
    ) -> Result<OwnedFd, String> {
        let mut ruleset = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(self.handled_access())
            .map_err(|error| error.to_string())?;
        if scope_unix_paths {
            ruleset = ruleset
                .handle_access(AccessFs::ResolveUnix)
                .map_err(|error| error.to_string())?;
        }
        if scope_abstract_unix_sockets {
            ruleset = ruleset
                .scope(Scope::AbstractUnixSocket)
                .map_err(|error| error.to_string())?;
        }
        let mut created = ruleset.create().map_err(|error| error.to_string())?;
        for (file, access) in rules {
            created = created
                .add_rule(PathBeneath::new(file.as_fd(), access))
                .map_err(|error| error.to_string())?;
        }
        Option::<OwnedFd>::from(created)
            .ok_or_else(|| "the kernel created no Landlock ruleset".to_string())
    }
}

/// Whether the kernel creates a ruleset that needs the feature `abi`
/// introduced. Every request is a hard requirement and the ruleset is really
/// created, so the answer is the kernel's, not the builder's reading of it.
fn kernel_creates_ruleset_for(abi: ABI) -> bool {
    let ruleset = Ruleset::default().set_compatibility(CompatLevel::HardRequirement);
    let ruleset = match abi {
        ABI::V1 => ruleset.handle_access(AccessFs::from_all(ABI::V1)),
        ABI::V2 => ruleset.handle_access(AccessFs::Refer),
        ABI::V3 => ruleset.handle_access(AccessFs::Truncate),
        ABI::V4 => ruleset.handle_access(AccessNet::BindTcp),
        ABI::V5 => ruleset.handle_access(AccessFs::IoctlDev),
        ABI::V6 => ruleset
            .handle_access(AccessFs::Execute)
            .and_then(|ruleset| ruleset.scope(Scope::AbstractUnixSocket)),
        ABI::V9 => ruleset.handle_access(AccessFs::ResolveUnix),
        _ => return false,
    };
    ruleset
        .and_then(Ruleset::create)
        .is_ok_and(|created| Option::<OwnedFd>::from(created).is_some())
}

/// The seccomp half of confinement under a Landlock that cannot scope
/// truncation (below ABI 3).
///
/// Landlock still governs every open for writing there, so `ftruncate` and
/// `fallocate`, which need a descriptor opened for writing, stay bounded. What
/// escapes it is truncation by path: `truncate(2)` reaches any file the uid
/// owns, granted or not, and so does an `open` that asks for read access and
/// `O_TRUNC`, because Landlock checks only the read. This refuses both, and
/// `openat2(2)`, whose flags sit behind a pointer the filter cannot read.
/// `creat(2)` opens for writing, so Landlock already bounds it.
pub(crate) fn filter_path_truncation(
    rules: &mut std::collections::BTreeMap<libc::c_long, Vec<seccompiler::SeccompRule>>,
) -> Result<(), crate::value::VmError> {
    use seccompiler::{SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompRule};

    rules.remove(&libc::SYS_truncate);
    rules.remove(&libc::SYS_openat2);
    // An open is admitted when it does not truncate, or when it asks for
    // write access, which Landlock then checks against the grants.
    let flag_rules = |flags_index: u8| -> Result<Vec<SeccompRule>, String> {
        let flags = |mask: u64, value: u64| {
            SeccompCondition::new(
                flags_index,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::MaskedEq(mask),
                value,
            )
            .and_then(|condition| SeccompRule::new(vec![condition]))
            .map_err(|error| error.to_string())
        };
        let access_mode = libc::O_ACCMODE as u64;
        Ok(vec![
            flags(libc::O_TRUNC as u64, 0)?,
            flags(access_mode, libc::O_WRONLY as u64)?,
            flags(access_mode, libc::O_RDWR as u64)?,
        ])
    };
    let opens = [
        (libc::SYS_openat, 2),
        #[cfg(target_arch = "x86_64")]
        (libc::SYS_open, 1),
    ];
    for (syscall, flags_index) in opens {
        let filtered = flag_rules(flags_index).map_err(|error| {
            super::sandbox_rejection(format!(
                "failed to build the truncation filter for Landlock below ABI 3: {error}"
            ))
        })?;
        rules.insert(syscall, filtered);
    }
    Ok(())
}

/// Enter an already-populated ruleset. Async-signal-safe: two raw syscalls,
/// no allocation, no locking, so it is legal on the `pre_exec` side of a fork.
///
/// This is the step that actually confines, and it must run in the process that
/// will `exec` the payload. Nothing before it restricts anything: a ruleset that
/// is created, populated, and never entered leaves the child completely
/// unconfined while every earlier step reports success.
pub(crate) fn enter_ruleset(ruleset: RawFd) -> io::Result<()> {
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::syscall(libc::SYS_landlock_restrict_self, ruleset, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// What this host's Landlock supports, once a child restriction has been seen
/// to hold. ABI presence alone is not enforcement.
pub(crate) fn host_support() -> Option<LandlockSupport> {
    static SUPPORT: std::sync::OnceLock<Option<LandlockSupport>> = std::sync::OnceLock::new();
    *SUPPORT.get_or_init(|| {
        let support = LandlockSupport::negotiate(NEWEST_DISTINGUISHED)?;
        restriction_holds(support).then_some(support)
    })
}

/// Enter an empty ruleset in a forked child and confirm a directory it could
/// open before is closed to it afterwards.
fn restriction_holds(support: LandlockSupport) -> bool {
    let Some(witness) = super::nesting::landlock_probe_witness() else {
        return false;
    };
    let Ok(ruleset) = support.populated_ruleset(false, false, []) else {
        return false;
    };
    // The child runs only raw async-signal-safe syscalls and _exit. No
    // Rust allocator, destructor, callback, or ambient policy is touched.
    let child = unsafe { libc::fork() };
    if child == 0 {
        let confined = enter_ruleset(ruleset.as_raw_fd()).is_ok();
        let opened = if confined {
            unsafe { libc::open(witness.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) }
        } else {
            0
        };
        unsafe { libc::_exit(i32::from(!(confined && opened < 0))) };
    }
    if child < 0 {
        return false;
    }
    let mut status = 0;
    loop {
        let waited = unsafe { libc::waitpid(child, &raw mut status, 0) };
        if waited == child {
            return libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0;
        }
        if waited < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return false;
    }
}
