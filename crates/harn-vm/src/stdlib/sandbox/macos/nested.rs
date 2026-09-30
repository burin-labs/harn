//! A confined Harn that is itself running inside a macOS sandbox.
//!
//! macOS refuses `sandbox_apply` from a process that is already sandboxed
//! whenever the outer profile denies anything at all, so a `harn run` started
//! by an agent's confined command tool cannot wrap its own children in
//! `sandbox-exec`: every spawn exits 71 with "Operation not permitted". The
//! profiles cannot be stacked, so the only choice is between the confinement
//! the process already has and none of its own.
//!
//! The outer sandbox is enforced by the kernel and cannot be widened from
//! inside, so running a child under it never grants more than the outer run
//! already allowed. That is only honest when the outer profile is at least as
//! strict as this policy where it matters, so the outer profile is asked, per
//! operation, about places this policy denies:
//!
//! - writes to existing directories outside every writable root (the home
//!   directory and each workspace root's parent),
//! - reads of the credential directories under home that no read root covers,
//! - reads of the policy's explicit read-deny roots,
//! - outbound network, when the policy denies it.
//!
//! If the outer profile allows any of them, it is weaker than this policy and
//! the spawn is refused with that reason, rather than run under a looser
//! sandbox than the caller asked for. The probes are a sample, not a proof of
//! containment; what the inner policy narrows beyond them is governed by the
//! outer profile, and the receipt says so.

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::orchestration::CapabilityPolicy;

/// Where a spawn under this policy stands relative to the process's own
/// confinement.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Nesting {
    /// This process is not sandboxed; wrap the child in `sandbox-exec`.
    NotNested,
    /// Sandboxed by a profile at least as strict as the policy at every
    /// probe; run the child under it.
    Inherit,
    /// Sandboxed by a profile that allows something the policy denies, so
    /// running under it would silently widen the policy.
    Unenforceable(Narrowing),
}

/// A denial of the policy that the outer sandbox does not make.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Narrowing {
    Network,
    Write(PathBuf),
    Read(PathBuf),
}

impl Narrowing {
    /// The typed kind, as recorded in the refusal receipt.
    pub(super) fn kind(&self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Write(_) => "write",
            Self::Read(_) => "read",
        }
    }

    /// What this run's policy denies and the outer sandbox allows.
    pub(super) fn describe(&self) -> String {
        match self {
            Self::Network => "outbound network".to_string(),
            Self::Write(path) => format!("writes to {}", path.display()),
            Self::Read(path) => format!("reads of {}", path.display()),
        }
    }
}

/// Whether a child spawned under `policy` should inherit this process's
/// sandbox instead of applying its own.
pub(super) fn nesting(policy: &CapabilityPolicy) -> Nesting {
    if !process_is_sandboxed() {
        return Nesting::NotNested;
    }
    if !super::policy_allows_network(policy) && !outer_denies("network-outbound", None) {
        return Nesting::Unenforceable(Narrowing::Network);
    }
    let writable = writable_roots(policy);
    for dir in write_probes(policy) {
        if dir.is_dir() && !covered(&dir, &writable) && !outer_denies("file-write-data", Some(&dir))
        {
            return Nesting::Unenforceable(Narrowing::Write(dir));
        }
    }
    let readable = readable_roots(policy);
    let credential_dirs = read_probes()
        .into_iter()
        .filter(|dir| !covered(dir, &readable));
    for dir in credential_dirs.chain(super::super::process_sandbox_read_deny_roots(policy)) {
        if dir.exists() && !outer_denies("file-read-data", Some(&dir)) {
            return Nesting::Unenforceable(Narrowing::Read(dir));
        }
    }
    Nesting::Inherit
}

/// Record a refusal: the narrowing the outer sandbox would not enforce.
pub(super) fn report_unenforceable(narrowing: &Narrowing) {
    let mut metadata = std::collections::BTreeMap::new();
    metadata.insert("backend".to_string(), serde_json::json!("macos"));
    metadata.insert("confinement".to_string(), serde_json::json!("refused"));
    metadata.insert("narrowing".to_string(), serde_json::json!(narrowing.kind()));
    metadata.insert(
        "detail".to_string(),
        serde_json::json!(narrowing.describe()),
    );
    crate::events::log_warn_meta(
        "process_sandbox_nested",
        "this process is already sandboxed by a profile that does not enforce this run's policy, \
         so the child was not started",
        metadata,
    );
}

/// Record, once per process, that children run under the inherited sandbox.
pub(super) fn report_inherited() {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if REPORTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut metadata = std::collections::BTreeMap::new();
    metadata.insert("backend".to_string(), serde_json::json!("macos"));
    metadata.insert("confinement".to_string(), serde_json::json!("inherited"));
    crate::events::log_warn_meta(
        "process_sandbox_nested",
        "this process is already sandboxed, so its children run under that sandbox; it is at \
         least as strict as this run's policy at every probe, and anything the policy narrows \
         beyond it is not separately enforced",
        metadata,
    );
}

fn process_is_sandboxed() -> bool {
    // A null operation asks only whether the process is sandboxed at all.
    unsafe { sandbox_check(libc::getpid(), std::ptr::null(), SANDBOX_FILTER_NONE) > 0 }
}

/// Whether the process's own profile denies `operation`, on `path` when given.
/// An error reads as "not denied", so a probe that cannot answer never
/// vouches for the outer profile.
fn outer_denies(operation: &str, path: Option<&Path>) -> bool {
    let Ok(operation) = CString::new(operation) else {
        return false;
    };
    let pid = unsafe { libc::getpid() };
    let result = match path {
        None => unsafe { sandbox_check(pid, operation.as_ptr(), SANDBOX_FILTER_NONE) },
        Some(path) => {
            let Ok(path) = CString::new(path.as_os_str().as_encoded_bytes()) else {
                return false;
            };
            unsafe { sandbox_check(pid, operation.as_ptr(), SANDBOX_FILTER_PATH, path.as_ptr()) }
        }
    };
    result == 1
}

const SANDBOX_FILTER_NONE: libc::c_int = 0;
const SANDBOX_FILTER_PATH: libc::c_int = 1;

unsafe extern "C" {
    // 1 when the operation is denied (or, with a null operation, when the
    // process is sandboxed), 0 when allowed, -1 on error. The filter argument
    // is variadic, which is why this is declared rather than called loosely:
    // on arm64 a variadic argument travels on the stack.
    fn sandbox_check(
        pid: libc::pid_t,
        operation: *const libc::c_char,
        filter: libc::c_int,
        ...
    ) -> libc::c_int;
}

/// Existing directories this policy never lets a child write into directly.
fn write_probes(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let mut probes: Vec<PathBuf> = crate::user_dirs::home_dir().into_iter().collect();
    probes.extend(
        super::process_sandbox_roots(policy)
            .iter()
            .filter_map(|root| root.parent().map(Path::to_path_buf)),
    );
    probes
}

/// Credential directories a policy reads only when a read root names them.
fn read_probes() -> Vec<PathBuf> {
    let Some(home) = crate::user_dirs::home_dir() else {
        return Vec::new();
    };
    [".ssh", ".aws", ".gnupg"]
        .iter()
        .map(|name| home.join(name))
        .collect()
}

/// Every root the rendered profile makes writable. A root missing here can
/// only make a probe fail, which refuses the spawn: it was refused before.
fn writable_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if super::policy_allows_workspace_write(policy) {
        roots.extend(super::process_sandbox_roots(policy));
        roots.extend(super::process_sandbox_policy_write_roots(policy));
        roots.extend(super::preset_write_roots(policy).iter().map(PathBuf::from));
        roots.extend(super::super::process_sandbox_developer_toolchain_cache_roots(policy));
    }
    roots
}

/// Every root the rendered profile makes readable, with the same caveat.
fn readable_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let mut roots = writable_roots(policy);
    roots.extend(super::process_sandbox_roots(policy));
    roots.extend(super::preset_read_roots(policy).iter().map(PathBuf::from));
    roots.extend(super::process_sandbox_developer_toolchain_read_roots(
        policy,
    ));
    roots.extend(super::process_sandbox_readonly_roots(policy));
    roots.extend(super::process_sandbox_policy_read_roots(policy));
    roots.extend(super::process_sandbox_package_manager_config_read_roots(
        policy,
    ));
    roots
}

fn covered(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

#[cfg(test)]
#[path = "nested_tests.rs"]
mod tests;
