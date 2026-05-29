//! The runtime arm of the permission primitive: pluggable sandbox
//! backends that *enforce* a declared policy rather than merely gating
//! tool dispatch.
//!
//! A sandbox is the runtime answer to a permission policy. The
//! authoritative policy model lives in `harn-serve`'s `permissions`
//! module (the `policy { read, write, exec, net }` block); this module
//! is where that policy becomes true at execution time. `harn-serve`
//! lowers a `PermissionPolicy` into a [`SandboxSpec`] and a backend
//! makes the spec real:
//!
//! - **filesystem** — mounts scope what the spawned process can touch;
//!   reads and writes outside the declared roots are rejected by the
//!   underlying OS sandbox.
//! - **process** — every command runs through `harn-vm`'s process
//!   sandbox, which maps the policy onto Landlock/seccomp (Linux),
//!   `sandbox-exec` (macOS), Job Objects (Windows), and `pledge`/
//!   `unveil` (OpenBSD).
//! - **network** — egress is governed by [`NetworkPolicy`]; a backend
//!   advertises whether it can honour a per-host allowlist via
//!   [`SandboxCapabilities::network_policy`].
//!
//! The [`LocalSandbox`] backend ships here because the process/fs
//! enforcement it relies on already lives in `harn-vm`; remote backends
//! (Fly Machines, Modal, E2B, …) implement the same [`SandboxBackend`]
//! contract from wherever they run.

mod local;

pub use local::{LocalSandbox, LocalSandboxConfig};

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Canonical guest mount for durable agent memory, read-only by
/// default. Backends expose its host path through the `HARN_MEMORY_DIR`
/// environment variable.
pub const MEMORY_MOUNT: &str = "/mnt/memory";

/// Canonical guest mount for a session's writable scratch/output
/// directory. Backends expose its host path through the
/// `HARN_OUTPUTS_DIR` environment variable.
pub const OUTPUTS_MOUNT: &str = "/mnt/session/outputs";

/// Errors surfaced by a [`SandboxBackend`].
#[derive(Debug, Error)]
pub enum SandboxError {
    /// No live session matches the supplied id.
    #[error("sandbox session `{0}` was not found")]
    SessionNotFound(String),
    /// The backend cannot honour the requested operation (e.g. a local
    /// backend asked for a per-host egress allowlist).
    #[error("backend `{backend}` does not support {operation}")]
    Unsupported {
        /// The backend that rejected the operation.
        backend: &'static str,
        /// A human-readable name for the unsupported operation.
        operation: &'static str,
    },
    /// The request was malformed (empty command, relative mount, …).
    #[error("sandbox request was invalid: {0}")]
    InvalidRequest(String),
    /// A provision/suspend/resume/terminate step failed.
    #[error("sandbox lifecycle operation failed: {0}")]
    Lifecycle(String),
    /// Executing the requested command failed.
    #[error("sandbox exec failed: {0}")]
    Exec(String),
    /// Applying or enforcing a network policy failed.
    #[error("sandbox network policy failed: {0}")]
    NetworkPolicy(String),
    /// An underlying I/O operation failed.
    #[error("sandbox I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialisation failed.
    #[error("sandbox JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    /// A spawned async task failed to join.
    #[error("sandbox task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

/// Result alias for sandbox operations.
pub type SandboxResult<T> = Result<T, SandboxError>;

/// Stable identifier for a provisioned sandbox session.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct SandboxSessionId(pub String);

impl SandboxSessionId {
    /// Construct a session id, rejecting blank values.
    pub fn new(value: impl Into<String>) -> SandboxResult<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(SandboxError::InvalidRequest(
                "session id cannot be empty".to_string(),
            ));
        }
        Ok(Self(value))
    }
}

impl std::fmt::Display for SandboxSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Egress policy for a sandbox session. The wire shape matches the
/// Anthropic sandbox network-policy contract so cloud backends can
/// forward it verbatim.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum NetworkPolicy {
    /// No egress restrictions.
    #[default]
    Unrestricted,
    /// Egress restricted to the listed hosts. An empty list denies all
    /// network access.
    Limited {
        /// Host allowlist; empty means deny-all.
        allowed_hosts: Vec<String>,
    },
}

/// Whether a mount is writable by the guest.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemAccess {
    /// The guest may read but not write.
    ReadOnly,
    /// The guest may read and write.
    ReadWrite,
}

/// A requested mount: a host `source` exposed to the guest at `target`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FilesystemMount {
    /// Host path to expose. Empty means "allocate a fresh directory
    /// under the session root".
    pub source: PathBuf,
    /// Absolute guest path the source is mounted at.
    pub target: String,
    /// Read-only or read-write.
    pub access: FilesystemAccess,
}

/// Resource ceilings applied to a session.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceLimits {
    /// Maximum wall-clock duration for any single exec.
    pub wall_time: Option<Duration>,
    /// CPU count hint for backends that can allocate it.
    pub cpu_count: Option<u32>,
    /// Memory ceiling in megabytes.
    pub memory_mb: Option<u32>,
    /// Idle timeout before a backend may suspend the session.
    pub idle_timeout: Option<Duration>,
}

/// The full request to provision a session: the runtime lowering of a
/// declared permission policy.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxSpec {
    /// Optional caller-chosen id; backends mint one when absent.
    pub session_id: Option<SandboxSessionId>,
    /// Free-form labels propagated to the backend (tenant, persona, …).
    pub labels: BTreeMap<String, String>,
    /// Egress policy.
    pub network_policy: NetworkPolicy,
    /// Mounts beyond the canonical memory/outputs pair.
    pub mounts: Vec<FilesystemMount>,
    /// Resource ceilings.
    pub limits: ResourceLimits,
}

/// Lifecycle state of a session.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SandboxState {
    /// Provisioned but not yet running.
    Provisioned,
    /// Live and accepting exec requests.
    Running,
    /// Suspended; resumes on next exec.
    Suspended,
    /// Torn down.
    Terminated,
}

/// A provisioned session as seen by callers.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxSession {
    /// Session id.
    pub id: SandboxSessionId,
    /// Name of the backend that owns the session.
    pub backend: String,
    /// Current lifecycle state.
    pub state: SandboxState,
    /// Mounts resolved to their host/guest paths.
    pub mounts: Vec<ResolvedMount>,
    /// Backend-specific metadata (e.g. the session root path).
    pub metadata: BTreeMap<String, String>,
}

/// A mount resolved to concrete host/guest paths.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedMount {
    /// Absolute guest path.
    pub target: String,
    /// Read-only or read-write.
    pub access: FilesystemAccess,
    /// Host path, when the backend exposes one (remote guests may not).
    pub host_path: Option<PathBuf>,
}

/// A command to run inside a session.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecRequest {
    /// Executable or shell builtin to run.
    pub command: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Working directory; resolved against mounts then the session root.
    pub cwd: Option<String>,
    /// Extra environment variables.
    pub env: BTreeMap<String, String>,
    /// Data piped to the command's stdin.
    pub stdin: Option<String>,
    /// Per-exec timeout; falls back to [`ResourceLimits::wall_time`].
    pub timeout: Option<Duration>,
}

/// The outcome of an [`ExecRequest`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecResult {
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// Process exit code.
    pub exit_code: i32,
    /// Whether the exec hit its timeout.
    pub timed_out: bool,
}

impl ExecResult {
    /// True when the command exited zero and did not time out.
    pub fn success(&self) -> bool {
        self.exit_code == 0 && !self.timed_out
    }
}

/// A point-in-time snapshot handle for a session.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxSnapshot {
    /// Session the snapshot belongs to.
    pub session_id: SandboxSessionId,
    /// Backend that produced it.
    pub backend: String,
    /// Backend-specific snapshot identifier.
    pub snapshot_id: String,
    /// Snapshot metadata.
    pub metadata: BTreeMap<String, String>,
}

/// What a backend can do, so callers can degrade gracefully.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxCapabilities {
    /// Enforces an OS-level process sandbox locally.
    pub local_process_sandbox: bool,
    /// Honours a per-host network allowlist.
    pub network_policy: bool,
    /// Supports snapshots.
    pub snapshot: bool,
    /// Supports resuming a suspended session.
    pub resume: bool,
    /// Suspends sessions after an idle timeout.
    pub suspend_on_idle: bool,
}

/// Pluggable enforcement backend. Implementations make a [`SandboxSpec`]
/// (the runtime lowering of a permission policy) real and run commands
/// under it.
#[async_trait]
pub trait SandboxBackend: Send + Sync {
    /// Stable backend name (used in [`SandboxSession::backend`]).
    fn name(&self) -> &'static str;

    /// What this backend can enforce.
    fn capabilities(&self) -> SandboxCapabilities;

    /// Provision a session from a spec.
    async fn provision(&self, spec: SandboxSpec) -> SandboxResult<SandboxSession>;

    /// Attach an additional mount to a live session.
    async fn attach_filesystem(
        &self,
        session_id: &SandboxSessionId,
        mount: FilesystemMount,
    ) -> SandboxResult<SandboxSession>;

    /// Apply (or update) the egress policy on a live session.
    async fn apply_network_policy(
        &self,
        session_id: &SandboxSessionId,
        policy: NetworkPolicy,
    ) -> SandboxResult<SandboxSession>;

    /// Run a command inside a session.
    async fn exec(
        &self,
        session_id: &SandboxSessionId,
        request: ExecRequest,
    ) -> SandboxResult<ExecResult>;

    /// Snapshot a session.
    async fn snapshot(&self, session_id: &SandboxSessionId) -> SandboxResult<SandboxSnapshot>;

    /// Resume a suspended session.
    async fn resume(&self, session_id: &SandboxSessionId) -> SandboxResult<SandboxSession>;

    /// Tear a session down.
    async fn terminate(&self, session_id: &SandboxSessionId) -> SandboxResult<()>;
}

/// Normalise a guest mount target: trim trailing slashes and require an
/// absolute path.
pub(crate) fn normalized_mount_target(target: &str) -> SandboxResult<String> {
    let trimmed = target.trim().trim_end_matches('/');
    if !trimmed.starts_with('/') {
        return Err(SandboxError::InvalidRequest(format!(
            "mount target `{target}` must be absolute"
        )));
    }
    Ok(trimmed.to_string())
}

/// POSIX-shell-quote a value for safe inclusion in a generated command.
pub(crate) fn sh_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

/// Quote a value as a Harn string literal.
pub(crate) fn harn_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Whole seconds for a duration, clamped to at least one so `timeout(1)`
/// never receives a zero argument.
pub(crate) fn duration_secs(duration: Duration) -> u64 {
    duration.as_secs().max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_policy_uses_anthropic_compatible_shape() {
        let json = serde_json::to_value(NetworkPolicy::Limited {
            allowed_hosts: vec!["api.github.com".to_string()],
        })
        .unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "mode": "limited",
                "allowed_hosts": ["api.github.com"]
            })
        );
    }

    #[test]
    fn quotes_shell_values() {
        assert_eq!(sh_quote("a'b"), "'a'\"'\"'b'");
        assert_eq!(sh_quote(""), "''");
    }
}
