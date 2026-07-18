//! Typed, redacted workload identity for machine-resource lease receipts.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::HostLeaseResourceKey;

const EXECUTION_CONTEXT_SCHEMA_VERSION: u32 = 1;

/// One-way, machine-local identity for a filesystem path.
///
/// Receipts can correlate repeated work without publishing a developer's
/// workspace layout. Callers must canonicalize the path before constructing
/// the identity when spelling-independent correlation is required.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeasePathIdentity {
    /// SHA-256 digest of the platform path encoding.
    pub sha256: String,
}

impl HostLeasePathIdentity {
    /// Construct a redacted identity without persisting the source path.
    pub fn from_path(path: &Path) -> Self {
        let digest = Sha256::digest(path.as_os_str().as_encoded_bytes());
        Self {
            sha256: hex::encode(digest),
        }
    }
}

/// Operation kind represented by a supervised lease receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostLeaseOperationKind {
    /// A Cargo build, test, check, or code-generation workload.
    Cargo,
}

/// Cargo-specific context nested beneath a generic execution receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeaseCargoExecutionContext {
    /// Canonical workspace identity supplied to Cargo.
    pub workspace: HostLeasePathIdentity,
    /// Isolated final-artifact directory identity.
    pub target_dir: HostLeasePathIdentity,
    /// Intermediate-artifact directory identity when explicitly selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_dir: Option<HostLeasePathIdentity>,
}

/// Versioned, redacted execution context stored with a supervised lease.
///
/// The tagged enum makes operation-specific fields structurally mandatory;
/// callers cannot claim a Cargo operation while omitting its Cargo context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation_kind", rename_all = "kebab-case")]
pub enum HostLeaseExecutionContext {
    /// Cargo execution context.
    Cargo {
        /// Contract version for this nested receipt.
        schema_version: u32,
        /// Redacted Cargo path identities.
        context: HostLeaseCargoExecutionContext,
    },
}

impl HostLeaseExecutionContext {
    /// Build a Cargo context while keeping raw local paths out of receipts.
    pub fn cargo(workspace: &Path, target_dir: &Path, build_dir: Option<&Path>) -> Self {
        Self::Cargo {
            schema_version: EXECUTION_CONTEXT_SCHEMA_VERSION,
            context: HostLeaseCargoExecutionContext {
                workspace: HostLeasePathIdentity::from_path(workspace),
                target_dir: HostLeasePathIdentity::from_path(target_dir),
                build_dir: build_dir.map(HostLeasePathIdentity::from_path),
            },
        }
    }

    /// Stable operation kind for scheduling and receipt projections.
    pub const fn operation_kind(&self) -> HostLeaseOperationKind {
        match self {
            Self::Cargo { .. } => HostLeaseOperationKind::Cargo,
        }
    }

    /// Cargo context when this receipt represents Cargo work.
    pub const fn cargo_context(&self) -> Option<&HostLeaseCargoExecutionContext> {
        match self {
            Self::Cargo { context, .. } => Some(context),
        }
    }
}

/// Terminal child status recorded by a supervised lease run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeaseProcessExit {
    /// Process exit code when the child exited normally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    /// Terminating Unix signal, absent on normal and non-Unix exits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

/// Outcome of releasing a workload lease after its process exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostLeaseRunReleaseOutcome {
    /// The supervisor deleted the matching active lease.
    Released,
    /// Another transaction had already recovered the now-dead owner.
    AlreadyRecovered,
}

/// Stable category for a failure before workload execution owns a lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostLeaseRunStartFailure {
    /// The supervisor could not encode the worker invocation.
    WorkerArguments,
    /// The supervisor could not create the worker process.
    WorkerSpawn,
    /// The worker exited before recording lease acquisition or deferral.
    WorkerExitedBeforeAcquire,
    /// The worker invocation disagreed with its durable receipt.
    WorkerContextMismatch,
    /// The resource store could not complete acquisition.
    ResourceAcquire,
    /// An acquired receipt omitted required ownership evidence.
    WorkerContract,
    /// The acquired receipt could not advance to running.
    ReceiptTransition,
}

/// Stable category for a failure after acquisition but before execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostLeaseRunLaunchFailure {
    /// A process argument could not be represented by the host process API.
    ArgumentEncoding,
    /// Replacing the worker with the workload failed.
    ProcessReplace,
    /// Creating the workload process failed.
    ProcessSpawn,
    /// The platform process-tree guard could not be established.
    ProcessSupervision,
    /// The platform has no proven supervised-workload implementation.
    UnsupportedPlatform,
}

/// Lifecycle state for one supervised workload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum HostLeaseRunState {
    /// The supervisor persisted intent before creating the worker.
    Pending {
        /// Receipt creation time in Unix milliseconds.
        requested_at_ms: i64,
    },
    /// The worker owns the lease and is about to advance the workload.
    Running {
        /// Opaque lease token.
        lease_id: String,
        /// Lease acquisition time in Unix milliseconds.
        acquired_at_ms: i64,
        /// Time spent waiting for the resource.
        acquire_wait_ms: u64,
        /// PID whose liveness owns the lease.
        worker_pid: u32,
    },
    /// The resource remained held through the requested wait deadline.
    Deferred {
        /// Observation time in Unix milliseconds.
        observed_at_ms: i64,
        /// Time spent waiting before deferral.
        waited_ms: u64,
    },
    /// The worker could not reach lease ownership.
    StartFailed {
        /// Observation time in Unix milliseconds.
        observed_at_ms: i64,
        /// Stable failure category without raw command or path text.
        error: HostLeaseRunStartFailure,
    },
    /// The supervisor cancelled the worker before it acquired the resource.
    CancelledBeforeStart {
        /// Terminal observation time in Unix milliseconds.
        finished_at_ms: i64,
    },
    /// Command preparation failed after acquisition but before execution.
    LaunchFailed {
        /// Opaque lease token acquired for the failed launch.
        lease_id: String,
        /// Lease cleanup result after the launch was abandoned.
        release: HostLeaseRunReleaseOutcome,
        /// Observation time in Unix milliseconds.
        observed_at_ms: i64,
        /// Stable failure category without raw command or path text.
        error: HostLeaseRunLaunchFailure,
    },
    /// The workload process reached a terminal status.
    Completed {
        /// Opaque lease token retained for correlation.
        lease_id: String,
        /// Time spent waiting for the resource.
        acquire_wait_ms: u64,
        /// Time from acquisition until workload exit.
        hold_ms: u64,
        /// PID whose liveness owned the lease.
        worker_pid: u32,
        /// Workload exit status.
        exit: HostLeaseProcessExit,
        /// Lease cleanup result after the process was confirmed dead.
        release: HostLeaseRunReleaseOutcome,
        /// Terminal observation time in Unix milliseconds.
        finished_at_ms: i64,
    },
    /// The supervisor interrupted and reaped the workload process group.
    Cancelled {
        /// Opaque lease token retained for correlation.
        lease_id: String,
        /// Time spent waiting for the resource.
        acquire_wait_ms: u64,
        /// Time from acquisition until cancellation completed.
        hold_ms: u64,
        /// PID whose liveness owned the lease.
        worker_pid: u32,
        /// Lease cleanup result after process-tree cleanup.
        release: HostLeaseRunReleaseOutcome,
        /// Terminal observation time in Unix milliseconds.
        finished_at_ms: i64,
    },
}

impl HostLeaseRunState {
    pub(super) fn may_transition_to(&self, next: &Self) -> bool {
        match (self, next) {
            (
                Self::Pending { .. },
                Self::Running { .. }
                | Self::Deferred { .. }
                | Self::StartFailed { .. }
                | Self::CancelledBeforeStart { .. },
            ) => true,
            (
                Self::Running {
                    lease_id,
                    acquire_wait_ms,
                    worker_pid,
                    ..
                },
                Self::Completed {
                    lease_id: next_lease_id,
                    acquire_wait_ms: next_wait_ms,
                    worker_pid: next_worker_pid,
                    ..
                }
                | Self::Cancelled {
                    lease_id: next_lease_id,
                    acquire_wait_ms: next_wait_ms,
                    worker_pid: next_worker_pid,
                    ..
                },
            ) => {
                lease_id == next_lease_id
                    && acquire_wait_ms == next_wait_ms
                    && worker_pid == next_worker_pid
            }
            (
                Self::Running { lease_id, .. },
                Self::LaunchFailed {
                    lease_id: next_lease_id,
                    ..
                },
            ) => lease_id == next_lease_id,
            _ => false,
        }
    }
}

/// Durable lifecycle evidence for one supervised workload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeaseRunReceipt {
    /// Contract version for this receipt.
    pub schema_version: u32,
    /// Stable run identifier and receipt filename stem.
    pub run_id: String,
    /// Validated operator or automation identity that requested the run.
    pub owner: String,
    /// Maximum resource wait requested by the public supervisor.
    pub wait_limit_ms: u64,
    /// Capacity-one machine resource used by the workload.
    pub resource: HostLeaseResourceKey,
    /// Typed, redacted workload identity.
    pub execution_context: HostLeaseExecutionContext,
    /// Current durable lifecycle state.
    pub status: HostLeaseRunState,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn cargo_context_keeps_only_stable_path_identities() {
        let context = HostLeaseExecutionContext::cargo(
            Path::new("/workspace/project"),
            Path::new("/tmp/target"),
            Some(Path::new("/tmp/build")),
        );

        assert_eq!(context.operation_kind(), HostLeaseOperationKind::Cargo);
        let cargo = context.cargo_context().expect("Cargo context is present");
        assert_ne!(cargo.target_dir, cargo.build_dir.clone().unwrap());
        assert_eq!(cargo.workspace.sha256.len(), 64);
    }

    #[test]
    fn omitted_build_directory_is_not_invented() {
        let context = HostLeaseExecutionContext::cargo(
            Path::new("/workspace/project"),
            Path::new("/tmp/target"),
            None,
        );

        assert!(context
            .cargo_context()
            .expect("Cargo context is present")
            .build_dir
            .is_none());
    }
}
