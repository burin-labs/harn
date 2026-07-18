//! Atomic, machine-global leases for scarce host resources.
//!
//! The store is deliberately independent of project event logs: two Harn
//! processes in different worktrees must still agree on one owner. SQLite's
//! immediate transaction supplies the cross-process compare-and-set. A file
//! watcher on the database directory wakes waiting callers after release or
//! renewal; expiry and caller deadlines remain timer wakeups.

mod execution;

pub use execution::{
    HostLeaseCargoExecutionContext, HostLeaseExecutionContext, HostLeaseOperationKind,
    HostLeasePathIdentity, HostLeaseProcessExit, HostLeaseRunLaunchFailure, HostLeaseRunReceipt,
    HostLeaseRunReleaseOutcome, HostLeaseRunStartFailure, HostLeaseRunState,
};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use notify::{RecursiveMode, Watcher};
use rusqlite::{
    params, Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use sysinfo::System;
use uuid::Uuid;

/// Overrides the machine-global directory containing host lease state.
pub const HOST_LEASE_ROOT_ENV: &str = "HARN_HOST_LEASE_ROOT";
const HARN_HOME_ENV: &str = "HARN_HOME";
const LEASE_DB_FILE: &str = "host-leases.sqlite";
const RUN_RECEIPTS_DIR: &str = "receipts";
const SQLITE_MUTATION_BUSY_TIMEOUT: Duration = Duration::from_secs(1);
const REGISTRY_BUSY_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const PROCESS_LIVENESS_RECHECK_INTERVAL: Duration = Duration::from_secs(5);
const SCHEMA_VERSION: u32 = 2;
const RUN_RECEIPT_SCHEMA_VERSION: u32 = 2;
const WHOLE_MACHINE_RESOURCE_CLASS: &str = "whole-machine";

/// Failures produced while validating or mutating host lease state.
#[derive(Debug, thiserror::Error)]
pub enum HostLeaseError {
    /// The caller supplied an invalid or unsafe contract value.
    #[error("invalid host lease request: {0}")]
    InvalidRequest(String),
    /// The state directory could not be read or written.
    #[error("host lease state I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// SQLite could not complete an atomic lease operation.
    #[error("host lease database failed: {0}")]
    Database(#[from] rusqlite::Error),
    /// Receipt metadata could not be encoded or decoded.
    #[error("host lease metadata serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    /// The cross-process filesystem watcher failed.
    #[error("host lease watcher failed: {0}")]
    Watch(String),
    /// The system clock cannot produce a Unix timestamp.
    #[error("system clock is before the Unix epoch")]
    Clock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProcessObservation {
    Alive { identity: u64 },
    Dead,
    Unknown,
}

trait ProcessInspector: std::fmt::Debug + Send + Sync {
    fn observe(&self, pid: u32) -> ProcessObservation;
}

#[derive(Debug)]
struct SystemProcessInspector;

impl ProcessInspector for SystemProcessInspector {
    fn observe(&self, pid: u32) -> ProcessObservation {
        match crate::process_liveness::process_liveness(pid) {
            crate::process_liveness::ProcessLiveness::Dead => ProcessObservation::Dead,
            crate::process_liveness::ProcessLiveness::Unknown => ProcessObservation::Unknown,
            crate::process_liveness::ProcessLiveness::Alive => {
                crate::process_liveness::process_identity(pid)
                    .map_or(ProcessObservation::Unknown, |identity| {
                        ProcessObservation::Alive { identity }
                    })
            }
        }
    }
}

/// Scheduling class attached to a lease and its receipts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostLeasePriorityClass {
    /// User-facing work that should remain latency-sensitive.
    Interactive,
    /// Authoritative measurement that must never be preempted or expire mid-run.
    Measurement,
    /// Build or verification work that may defer behind interactive/measurement work.
    CiVerify,
    #[default]
    /// Background work that should run only when higher-priority work is absent.
    Deferrable,
}

impl HostLeasePriorityClass {
    /// Stable wire spelling used by SQLite, JSON receipts, and CLI output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Measurement => "measurement",
            Self::CiVerify => "ci-verify",
            Self::Deferrable => "deferrable",
        }
    }

    fn parse(raw: &str) -> Result<Self, HostLeaseError> {
        match raw {
            "interactive" => Ok(Self::Interactive),
            "measurement" => Ok(Self::Measurement),
            "ci-verify" => Ok(Self::CiVerify),
            "deferrable" => Ok(Self::Deferrable),
            other => Err(HostLeaseError::InvalidRequest(format!(
                "unknown priority class `{other}`"
            ))),
        }
    }
}

/// Typed class for a scarce machine resource.
///
/// The initial store is intentionally capacity-one per class. Keeping the
/// class separate from the machine name lets future schedulers add a new
/// resource kind without inventing another registry or encoding policy in a
/// caller-chosen string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostLeaseResourceClass {
    /// Backward-compatible whole-machine lease used by existing callers.
    #[default]
    WholeMachine,
    /// CPU-, linker-, and cache-intensive Rust build or verification work.
    RustHeavy,
}

/// Central capacity and wire-name policy for one machine resource class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostLeaseResourceDefinition {
    /// Stable storage and wire spelling.
    pub name: &'static str,
    /// Maximum simultaneous holders on one machine.
    pub capacity: u16,
}

const HOST_LEASE_RESOURCE_DEFINITIONS: [HostLeaseResourceDefinition; 2] = [
    HostLeaseResourceDefinition {
        name: WHOLE_MACHINE_RESOURCE_CLASS,
        capacity: 1,
    },
    HostLeaseResourceDefinition {
        name: "rust-heavy",
        capacity: 1,
    },
];

impl HostLeaseResourceClass {
    /// Owning resource policy entry.
    pub const fn definition(self) -> &'static HostLeaseResourceDefinition {
        match self {
            Self::WholeMachine => &HOST_LEASE_RESOURCE_DEFINITIONS[0],
            Self::RustHeavy => &HOST_LEASE_RESOURCE_DEFINITIONS[1],
        }
    }

    /// Stable storage and wire spelling.
    pub const fn as_str(self) -> &'static str {
        self.definition().name
    }

    /// Configured capacity for the initial local resource registry.
    ///
    /// Capacity is centralized on the resource definition rather than copied
    /// into callers. The v1 SQLite key remains deliberately capacity-one.
    pub const fn capacity(self) -> u16 {
        self.definition().capacity
    }

    fn parse(raw: &str) -> Result<Self, HostLeaseError> {
        match raw {
            WHOLE_MACHINE_RESOURCE_CLASS => Ok(Self::WholeMachine),
            "rust-heavy" => Ok(Self::RustHeavy),
            other => Err(HostLeaseError::InvalidRequest(format!(
                "unknown resource class `{other}`"
            ))),
        }
    }
}

/// Names one capacity-one resource on a machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeaseResourceKey {
    /// Machine identity. This retains the historic `host` name at public CLI
    /// boundaries for compatibility.
    pub machine: String,
    /// Independent exclusive resource class on that machine.
    pub resource_class: HostLeaseResourceClass,
}

impl HostLeaseResourceKey {
    fn normalize(
        machine: &str,
        resource_class: HostLeaseResourceClass,
    ) -> Result<Self, HostLeaseError> {
        Ok(Self {
            machine: normalize_component("host", machine)?,
            resource_class,
        })
    }
}

/// Request to acquire one exclusive host resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeaseRequest {
    /// Machine resource name, normally the local hostname.
    pub host: String,
    #[serde(default)]
    /// Resource class to acquire. Omitted legacy requests remain
    /// whole-machine leases.
    pub resource_class: HostLeaseResourceClass,
    #[serde(default)]
    /// Typed, redacted workload identity for supervised executions.
    pub execution_context: Option<HostLeaseExecutionContext>,
    /// Stable caller identity shown in contention receipts.
    pub owner: String,
    #[serde(default)]
    /// Scheduling class recorded with the lease.
    pub priority_class: HostLeasePriorityClass,
    /// `None` means no wall-clock expiry. Non-expiring leases require an
    /// owner PID so a later caller can recover after an owner crash without
    /// expiring a healthy measurement mid-run.
    #[serde(default)]
    pub ttl_ms: Option<u64>,
    #[serde(default)]
    /// Process that owns the work, used with its start time for crash recovery.
    pub owner_pid: Option<u32>,
    #[serde(default)]
    /// Human-readable reason for acquiring the machine.
    pub reason: Option<String>,
    #[serde(default)]
    /// Structured caller metadata preserved without interpretation.
    pub metadata: BTreeMap<String, String>,
}

/// Token-bearing authority for an active exclusive host lease.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeaseHandle {
    /// Contract schema version.
    pub schema_version: u32,
    /// Machine resource name.
    pub host: String,
    #[serde(default)]
    /// Resource class held on this host.
    pub resource_class: HostLeaseResourceClass,
    #[serde(default)]
    /// Typed, redacted workload identity. Legacy manual leases omit it.
    pub execution_context: Option<HostLeaseExecutionContext>,
    /// Unforgeable token required to renew or release this lease.
    pub lease_id: String,
    /// Stable caller identity.
    pub owner: String,
    /// Scheduling class attached at acquisition.
    pub priority_class: HostLeasePriorityClass,
    /// Acquisition timestamp in Unix milliseconds.
    pub acquired_at_ms: i64,
    /// Most recent renewal timestamp in Unix milliseconds.
    pub updated_at_ms: i64,
    #[serde(default)]
    /// Optional wall-clock expiry; absent for protected measurement leases.
    pub expires_at_ms: Option<i64>,
    #[serde(default)]
    /// Optional local owner PID.
    pub owner_pid: Option<u32>,
    #[serde(default)]
    /// Native-resolution owner process identity, preventing PID-reuse liveness.
    pub owner_process_identity: Option<u64>,
    #[serde(default)]
    /// Human-readable acquisition reason.
    pub reason: Option<String>,
    #[serde(default)]
    /// Structured caller metadata.
    pub metadata: BTreeMap<String, String>,
}

/// Terminal result of an acquire attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostLeaseAcquireStatus {
    /// The caller atomically became the owner.
    Acquired,
    /// Another live owner still holds the host.
    Deferred,
}

/// Stable reason an acquisition did not become the owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostLeaseDeferReason {
    /// Another live lease owns the same host resource.
    #[serde(rename = "host_lease_contended")]
    Contended,
    /// Another registry transaction briefly owns SQLite's write lock.
    #[serde(rename = "host_lease_registry_busy")]
    RegistryBusy,
}

impl HostLeaseDeferReason {
    /// Stable wire spelling used by CLI error envelopes.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Contended => "host_lease_contended",
            Self::RegistryBusy => "host_lease_registry_busy",
        }
    }
}

/// Typed evidence explaining why an acquire attempt deferred.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeaseDeferReceipt {
    /// Contended machine resource.
    pub host: String,
    #[serde(default)]
    /// Resource class that remains contended.
    pub resource_class: HostLeaseResourceClass,
    /// Stable machine-readable reason.
    pub deferred_reason: HostLeaseDeferReason,
    /// Observation timestamp in Unix milliseconds.
    pub observed_at_ms: i64,
    #[serde(default)]
    /// Earliest known state transition, normally the current lease expiry.
    pub next_wake_at_ms: Option<i64>,
    #[serde(default)]
    /// Caller-supplied wait deadline when acquisition is bounded.
    pub deadline_at_ms: Option<i64>,
    /// Authority describing the current owner. Absent only when SQLite's
    /// short write lock prevented the registry from reading lease state.
    #[serde(default)]
    pub active: Option<HostLeaseHandle>,
}

/// Versioned result of an immediate or waiting acquire operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeaseAcquireReceipt {
    /// Contract schema version.
    pub schema_version: u32,
    /// Whether acquisition succeeded or deferred.
    pub status: HostLeaseAcquireStatus,
    /// Final observation timestamp in Unix milliseconds.
    pub observed_at_ms: i64,
    /// Time spent waiting on filesystem notifications or expiry.
    pub waited_ms: u64,
    #[serde(default)]
    /// Token-bearing authority, present only after acquisition.
    pub handle: Option<HostLeaseHandle>,
    #[serde(default)]
    /// Contention authority, present only after deferral.
    pub defer: Option<HostLeaseDeferReceipt>,
    /// True when acquisition first removed an expired or dead-owner row.
    pub recovered_stale_lease: bool,
}

/// Current authoritative lease state for one host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeaseState {
    /// Contract schema version.
    pub schema_version: u32,
    /// Machine resource name.
    pub host: String,
    #[serde(default)]
    /// Resource class inspected on this host.
    pub resource_class: HostLeaseResourceClass,
    /// Observation timestamp in Unix milliseconds.
    pub observed_at_ms: i64,
    #[serde(default)]
    /// Current owner, or `None` when the host is available.
    pub active: Option<HostLeaseHandle>,
    /// True when this read removed an expired or dead-owner row.
    pub recovered_stale_lease: bool,
}

/// Versioned result of a token-scoped lease renewal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeaseRenewReceipt {
    /// Contract schema version.
    pub schema_version: u32,
    /// True only when the supplied token owned the active lease.
    pub renewed: bool,
    /// Observation timestamp in Unix milliseconds.
    pub observed_at_ms: i64,
    #[serde(default)]
    /// Updated handle when renewal succeeds.
    pub handle: Option<HostLeaseHandle>,
}

/// Versioned result of a token-scoped lease release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeaseReleaseReceipt {
    /// Contract schema version.
    pub schema_version: u32,
    /// True only when the supplied token owned and removed the active lease.
    pub released: bool,
    /// Machine resource name.
    pub host: String,
    #[serde(default)]
    /// Resource class released on this host.
    pub resource_class: HostLeaseResourceClass,
    /// Token supplied by the caller.
    pub lease_id: String,
    /// Observation timestamp in Unix milliseconds.
    pub observed_at_ms: i64,
}

/// Atomic machine-global lease store for CLI and runtime adapters.
#[derive(Clone, Debug)]
pub struct HostLeaseStore {
    root: PathBuf,
    db_path: PathBuf,
    process_inspector: Arc<dyn ProcessInspector>,
}

impl HostLeaseStore {
    /// Resolve state from `HARN_HOST_LEASE_ROOT`, `HARN_HOME`, or the user home.
    pub fn from_env() -> Result<Self, HostLeaseError> {
        let root = if let Some(path) = std::env::var_os(HOST_LEASE_ROOT_ENV) {
            PathBuf::from(path)
        } else if let Some(path) = std::env::var_os(HARN_HOME_ENV) {
            PathBuf::from(path).join("host-leases")
        } else {
            harn_vm::user_dirs::home_dir()
                .ok_or_else(|| {
                    HostLeaseError::InvalidRequest(
                        "cannot resolve a home directory; set HARN_HOST_LEASE_ROOT".to_string(),
                    )
                })?
                .join(".harn/host-leases")
        };
        Self::for_root(root)
    }

    /// Create a store at an explicit root, primarily for hermetic hosts and tests.
    pub fn for_root(root: impl Into<PathBuf>) -> Result<Self, HostLeaseError> {
        Self::for_root_with_inspector(root, Arc::new(SystemProcessInspector))
    }

    fn for_root_with_inspector(
        root: impl Into<PathBuf>,
        process_inspector: Arc<dyn ProcessInspector>,
    ) -> Result<Self, HostLeaseError> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        let store = Self {
            db_path: root.join(LEASE_DB_FILE),
            root,
            process_inspector,
        };
        store.initialize()?;
        Ok(store)
    }

    /// Directory containing the lease database and watcher events.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Persist intent for one supervised execution before its worker starts.
    pub fn begin_run(
        &self,
        owner: &str,
        priority_class: HostLeasePriorityClass,
        resource: HostLeaseResourceKey,
        execution_context: HostLeaseExecutionContext,
        wait_limit_ms: u64,
    ) -> Result<HostLeaseRunReceipt, HostLeaseError> {
        let resource = HostLeaseResourceKey::normalize(&resource.machine, resource.resource_class)?;
        let receipt = HostLeaseRunReceipt {
            schema_version: RUN_RECEIPT_SCHEMA_VERSION,
            run_id: Uuid::now_v7().to_string(),
            owner: normalize_component("owner", owner)?,
            priority_class,
            wait_limit_ms,
            resource,
            execution_context,
            status: HostLeaseRunState::Pending {
                requested_at_ms: unix_now_ms()?,
            },
        };
        let path = self.run_receipt_path(&receipt.run_id)?;
        let bytes = serde_json::to_vec_pretty(&receipt)?;
        harn_vm::atomic_io::atomic_write(&path, &bytes)?;
        Ok(receipt)
    }

    /// Load one durable supervised-execution receipt.
    pub fn load_run(&self, run_id: &str) -> Result<HostLeaseRunReceipt, HostLeaseError> {
        let path = self.run_receipt_path(run_id)?;
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }

    /// Advance one supervised execution through a validated lifecycle edge.
    pub fn transition_run(
        &self,
        run_id: &str,
        status: HostLeaseRunState,
    ) -> Result<HostLeaseRunReceipt, HostLeaseError> {
        let mut receipt = self.load_run(run_id)?;
        if !receipt.status.may_transition_to(&status) {
            return Err(HostLeaseError::InvalidRequest(format!(
                "invalid run receipt transition from {:?} to {:?}",
                receipt.status, status
            )));
        }
        receipt.status = status;
        let path = self.run_receipt_path(run_id)?;
        let bytes = serde_json::to_vec_pretty(&receipt)?;
        harn_vm::atomic_io::atomic_write(&path, &bytes)?;
        Ok(receipt)
    }

    /// Stable path containing one run receipt.
    pub fn run_receipt_path(&self, run_id: &str) -> Result<PathBuf, HostLeaseError> {
        let run_id = normalize_component("run_id", run_id)?;
        Ok(self
            .root
            .join(RUN_RECEIPTS_DIR)
            .join(format!("{run_id}.json")))
    }

    /// Return the local hostname used when callers omit `--host`.
    pub fn default_host() -> String {
        System::host_name()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "local".to_string())
    }

    /// Attempt one immediate atomic acquisition.
    pub fn try_acquire(
        &self,
        request: HostLeaseRequest,
    ) -> Result<HostLeaseAcquireReceipt, HostLeaseError> {
        self.try_acquire_once(request, None, None)
    }

    /// Wait on cross-process notifications and expiry, then retry atomically.
    pub fn acquire_wait(
        &self,
        request: HostLeaseRequest,
        wait_timeout: Duration,
    ) -> Result<HostLeaseAcquireReceipt, HostLeaseError> {
        if wait_timeout.is_zero() {
            return self.try_acquire(request);
        }
        let started_at_ms = unix_now_ms()?;
        let started_at = Instant::now();
        let deadline = started_at.checked_add(wait_timeout).ok_or_else(|| {
            HostLeaseError::InvalidRequest("wait timeout exceeds the monotonic clock".to_string())
        })?;
        let deadline_at_ms = started_at_ms.saturating_add(duration_ms_i64(wait_timeout));
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = tx.send(event);
        })
        .map_err(|error| HostLeaseError::Watch(error.to_string()))?;
        watcher
            .watch(&self.root, RecursiveMode::NonRecursive)
            .map_err(|error| HostLeaseError::Watch(error.to_string()))?;

        loop {
            let receipt =
                self.try_acquire_once(request.clone(), Some(started_at), Some(deadline_at_ms))?;
            if receipt.status == HostLeaseAcquireStatus::Acquired || Instant::now() >= deadline {
                return Ok(receipt);
            }
            let wake_at = receipt
                .defer
                .as_ref()
                .and_then(|defer| defer.next_wake_at_ms)
                .map(|wake| wake.min(deadline_at_ms))
                .unwrap_or(deadline_at_ms);
            let wake_duration =
                Duration::from_millis(wake_at.saturating_sub(receipt.observed_at_ms).max(1) as u64);
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(receipt);
            }
            match rx.recv_timeout(wake_duration.min(remaining)) {
                Ok(Ok(_)) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Ok(Err(error)) => return Err(HostLeaseError::Watch(error.to_string())),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(HostLeaseError::Watch(
                        "host lease watcher disconnected".to_string(),
                    ));
                }
            }
        }
    }

    /// Inspect one host, recovering expired or dead-owner state transactionally.
    pub fn status(&self, host: &str) -> Result<HostLeaseState, HostLeaseError> {
        self.status_for_resource(host, HostLeaseResourceClass::WholeMachine)
    }

    /// Inspect a specific resource class, recovering stale state transactionally.
    pub fn status_for_resource(
        &self,
        host: &str,
        resource_class: HostLeaseResourceClass,
    ) -> Result<HostLeaseState, HostLeaseError> {
        let resource = HostLeaseResourceKey::normalize(host, resource_class)?;
        let mut conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = unix_now_ms()?;
        self.status_in_transaction(tx, &resource.machine, resource.resource_class, now)
    }

    /// Renew the active lease only when the token matches.
    pub fn renew(
        &self,
        host: &str,
        lease_id: &str,
        ttl_ms: u64,
    ) -> Result<HostLeaseRenewReceipt, HostLeaseError> {
        self.renew_for_resource(host, HostLeaseResourceClass::WholeMachine, lease_id, ttl_ms)
    }

    /// Renew a lease for one resource class only when its token matches.
    pub fn renew_for_resource(
        &self,
        host: &str,
        resource_class: HostLeaseResourceClass,
        lease_id: &str,
        ttl_ms: u64,
    ) -> Result<HostLeaseRenewReceipt, HostLeaseError> {
        let resource = HostLeaseResourceKey::normalize(host, resource_class)?;
        let lease_id = normalize_component("lease_id", lease_id)?;
        validate_ttl(Some(ttl_ms))?;
        let mut conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = unix_now_ms()?;
        let (active, _) = active_handle(
            &tx,
            &resource.machine,
            resource.resource_class,
            now,
            self.process_inspector.as_ref(),
        )?;
        let Some(mut handle) = active.filter(|handle| handle.lease_id == lease_id) else {
            tx.commit()?;
            return Ok(HostLeaseRenewReceipt {
                schema_version: SCHEMA_VERSION,
                renewed: false,
                observed_at_ms: now,
                handle: None,
            });
        };
        handle.updated_at_ms = now;
        handle.expires_at_ms = Some(now.saturating_add(u64_ms_i64(ttl_ms)));
        write_handle(&tx, &handle)?;
        tx.commit()?;
        Ok(HostLeaseRenewReceipt {
            schema_version: SCHEMA_VERSION,
            renewed: true,
            observed_at_ms: now,
            handle: Some(handle),
        })
    }

    /// Release the active lease only when the token matches.
    pub fn release(
        &self,
        host: &str,
        lease_id: &str,
    ) -> Result<HostLeaseReleaseReceipt, HostLeaseError> {
        self.release_for_resource(host, HostLeaseResourceClass::WholeMachine, lease_id)
    }

    /// Release a lease for one resource class only when its token matches.
    pub fn release_for_resource(
        &self,
        host: &str,
        resource_class: HostLeaseResourceClass,
        lease_id: &str,
    ) -> Result<HostLeaseReleaseReceipt, HostLeaseError> {
        let resource = HostLeaseResourceKey::normalize(host, resource_class)?;
        let lease_id = normalize_component("lease_id", lease_id)?;
        let conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        let released = conn.execute(
            "DELETE FROM host_leases
             WHERE host = ?1 AND resource_class = ?2 AND lease_id = ?3",
            params![
                &resource.machine,
                resource.resource_class.as_str(),
                &lease_id
            ],
        )? == 1;
        let now = unix_now_ms()?;
        Ok(HostLeaseReleaseReceipt {
            schema_version: SCHEMA_VERSION,
            released,
            host: resource.machine,
            resource_class: resource.resource_class,
            lease_id,
            observed_at_ms: now,
        })
    }

    fn initialize(&self) -> Result<(), HostLeaseError> {
        let mut conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match lease_table_layout(&tx)? {
            LeaseTableLayout::Missing => create_current_lease_table(&tx)?,
            LeaseTableLayout::LegacyWholeMachine => migrate_legacy_lease_table(&tx)?,
            LeaseTableLayout::ResourceClassWithoutExecutionContext => {
                add_execution_context_column(&tx)?;
            }
            LeaseTableLayout::Current => {}
        }
        tx.commit()?;
        Ok(())
    }

    fn connection(&self, busy_timeout: Duration) -> Result<Connection, HostLeaseError> {
        let conn = Connection::open(&self.db_path)?;
        conn.busy_timeout(busy_timeout)?;
        Ok(conn)
    }

    fn try_acquire_once(
        &self,
        request: HostLeaseRequest,
        started_at: Option<Instant>,
        deadline_at_ms: Option<i64>,
    ) -> Result<HostLeaseAcquireReceipt, HostLeaseError> {
        let request = normalize_request(request)?;
        let mut conn = self.connection(Duration::ZERO)?;
        let tx = match conn.transaction_with_behavior(TransactionBehavior::Immediate) {
            Ok(tx) => tx,
            Err(error) if sqlite_is_busy(&error) => {
                let now = unix_now_ms()?;
                return Ok(registry_busy_receipt(
                    request.host,
                    request.resource_class,
                    now,
                    started_at.map(|started| duration_ms_u64(started.elapsed())),
                    deadline_at_ms,
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let now = unix_now_ms()?;
        let waited_ms = started_at
            .map(|started| duration_ms_u64(started.elapsed()))
            .unwrap_or(0);
        let host = request.host.clone();
        let resource_class = request.resource_class;
        match self.acquire_in_transaction(tx, request, now, deadline_at_ms, waited_ms) {
            Err(HostLeaseError::Database(error)) if sqlite_is_busy(&error) => Ok(
                registry_busy_receipt(host, resource_class, now, Some(waited_ms), deadline_at_ms),
            ),
            result => result,
        }
    }

    #[cfg(test)]
    fn try_acquire_at(
        &self,
        request: HostLeaseRequest,
        now: i64,
        deadline_at_ms: Option<i64>,
        waited_ms: u64,
    ) -> Result<HostLeaseAcquireReceipt, HostLeaseError> {
        let request = normalize_request(request)?;
        let mut conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.acquire_in_transaction(tx, request, now, deadline_at_ms, waited_ms)
    }

    fn acquire_in_transaction(
        &self,
        tx: Transaction<'_>,
        request: HostLeaseRequest,
        now: i64,
        deadline_at_ms: Option<i64>,
        waited_ms: u64,
    ) -> Result<HostLeaseAcquireReceipt, HostLeaseError> {
        let owner_process_identity = request
            .owner_pid
            .map(|pid| match self.process_inspector.observe(pid) {
                ProcessObservation::Alive { identity } => Ok(identity),
                ProcessObservation::Dead => Err(HostLeaseError::InvalidRequest(
                    "owner_pid is not a live local process".to_string(),
                )),
                ProcessObservation::Unknown => Err(HostLeaseError::InvalidRequest(
                    "owner_pid liveness could not be verified".to_string(),
                )),
            })
            .transpose()?;
        let (active, recovered_stale_lease) = active_handle(
            &tx,
            &request.host,
            request.resource_class,
            now,
            self.process_inspector.as_ref(),
        )?;
        if let Some(active) = active {
            let defer = HostLeaseDeferReceipt {
                host: request.host,
                resource_class: request.resource_class,
                deferred_reason: HostLeaseDeferReason::Contended,
                observed_at_ms: now,
                next_wake_at_ms: Some(next_lease_wake_at(&active, now, deadline_at_ms)),
                deadline_at_ms,
                active: Some(active),
            };
            tx.commit()?;
            return Ok(HostLeaseAcquireReceipt {
                schema_version: SCHEMA_VERSION,
                status: HostLeaseAcquireStatus::Deferred,
                observed_at_ms: now,
                waited_ms,
                handle: None,
                defer: Some(defer),
                recovered_stale_lease,
            });
        }

        let handle = HostLeaseHandle {
            schema_version: SCHEMA_VERSION,
            host: request.host,
            resource_class: request.resource_class,
            execution_context: request.execution_context,
            lease_id: Uuid::now_v7().to_string(),
            owner: request.owner,
            priority_class: request.priority_class,
            acquired_at_ms: now,
            updated_at_ms: now,
            expires_at_ms: request
                .ttl_ms
                .map(|ttl| now.saturating_add(u64_ms_i64(ttl))),
            owner_pid: request.owner_pid,
            owner_process_identity,
            reason: request.reason,
            metadata: request.metadata,
        };
        write_handle(&tx, &handle)?;
        tx.commit()?;
        Ok(HostLeaseAcquireReceipt {
            schema_version: SCHEMA_VERSION,
            status: HostLeaseAcquireStatus::Acquired,
            observed_at_ms: now,
            waited_ms,
            handle: Some(handle),
            defer: None,
            recovered_stale_lease,
        })
    }

    #[cfg(test)]
    fn status_at(
        &self,
        host: &str,
        resource_class: HostLeaseResourceClass,
        now: i64,
    ) -> Result<HostLeaseState, HostLeaseError> {
        let mut conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.status_in_transaction(tx, host, resource_class, now)
    }

    fn status_in_transaction(
        &self,
        tx: Transaction<'_>,
        host: &str,
        resource_class: HostLeaseResourceClass,
        now: i64,
    ) -> Result<HostLeaseState, HostLeaseError> {
        let (active, recovered_stale_lease) = active_handle(
            &tx,
            host,
            resource_class,
            now,
            self.process_inspector.as_ref(),
        )?;
        tx.commit()?;
        Ok(HostLeaseState {
            schema_version: SCHEMA_VERSION,
            host: host.to_string(),
            resource_class,
            observed_at_ms: now,
            active,
            recovered_stale_lease,
        })
    }
}

fn normalize_request(mut request: HostLeaseRequest) -> Result<HostLeaseRequest, HostLeaseError> {
    let resource = HostLeaseResourceKey::normalize(&request.host, request.resource_class)?;
    request.host = resource.machine;
    request.resource_class = resource.resource_class;
    request.owner = normalize_component("owner", &request.owner)?;
    validate_ttl(request.ttl_ms)?;
    if request.ttl_ms.is_none() && request.owner_pid.is_none() {
        return Err(HostLeaseError::InvalidRequest(
            "a non-expiring lease requires owner_pid for crash recovery".to_string(),
        ));
    }
    request.reason = request.reason.and_then(|reason| {
        let trimmed = reason.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    });
    Ok(request)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeaseTableLayout {
    Missing,
    LegacyWholeMachine,
    ResourceClassWithoutExecutionContext,
    Current,
}

fn lease_table_layout(tx: &Transaction<'_>) -> Result<LeaseTableLayout, HostLeaseError> {
    let mut statement = tx.prepare("PRAGMA table_info(host_leases)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if columns.is_empty() {
        return Ok(LeaseTableLayout::Missing);
    }
    let has_resource_class = columns.iter().any(|column| column == "resource_class");
    let has_execution_context = columns
        .iter()
        .any(|column| column == "execution_context_json");
    if has_resource_class && has_execution_context {
        return Ok(LeaseTableLayout::Current);
    }
    if has_resource_class {
        return Ok(LeaseTableLayout::ResourceClassWithoutExecutionContext);
    }
    Ok(LeaseTableLayout::LegacyWholeMachine)
}

fn create_current_lease_table(tx: &Transaction<'_>) -> Result<(), HostLeaseError> {
    tx.execute_batch(
        "CREATE TABLE host_leases (
            host TEXT NOT NULL,
            resource_class TEXT NOT NULL,
            lease_id TEXT NOT NULL,
            owner TEXT NOT NULL,
            priority_class TEXT NOT NULL,
            acquired_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            expires_at_ms INTEGER,
            owner_pid INTEGER,
            owner_process_identity INTEGER,
            reason TEXT,
            metadata_json TEXT NOT NULL,
            execution_context_json TEXT,
            PRIMARY KEY (host, resource_class)
        );",
    )?;
    Ok(())
}

fn migrate_legacy_lease_table(tx: &Transaction<'_>) -> Result<(), HostLeaseError> {
    tx.execute_batch(
        "ALTER TABLE host_leases RENAME TO host_leases_v1;
         CREATE TABLE host_leases (
            host TEXT NOT NULL,
            resource_class TEXT NOT NULL,
            lease_id TEXT NOT NULL,
            owner TEXT NOT NULL,
            priority_class TEXT NOT NULL,
            acquired_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            expires_at_ms INTEGER,
            owner_pid INTEGER,
            owner_process_identity INTEGER,
            reason TEXT,
            metadata_json TEXT NOT NULL,
            execution_context_json TEXT,
            PRIMARY KEY (host, resource_class)
         );
         INSERT INTO host_leases (
            host, resource_class, lease_id, owner, priority_class, acquired_at_ms,
            updated_at_ms, expires_at_ms, owner_pid, owner_process_identity, reason, metadata_json,
            execution_context_json
         )
         SELECT host, 'whole-machine', lease_id, owner, priority_class, acquired_at_ms,
            updated_at_ms, expires_at_ms, owner_pid, owner_process_identity, reason, metadata_json,
            NULL
         FROM host_leases_v1;
         DROP TABLE host_leases_v1;",
    )?;
    Ok(())
}

fn add_execution_context_column(tx: &Transaction<'_>) -> Result<(), HostLeaseError> {
    tx.execute_batch("ALTER TABLE host_leases ADD COLUMN execution_context_json TEXT;")?;
    Ok(())
}

fn normalize_component(name: &str, value: &str) -> Result<String, HostLeaseError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(HostLeaseError::InvalidRequest(format!(
            "{name} cannot be empty"
        )));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return Err(HostLeaseError::InvalidRequest(format!(
            "{name} may contain only ASCII letters, digits, '.', '_' and '-'"
        )));
    }
    Ok(value.to_string())
}

fn validate_ttl(ttl_ms: Option<u64>) -> Result<(), HostLeaseError> {
    if ttl_ms == Some(0) {
        return Err(HostLeaseError::InvalidRequest(
            "ttl_ms must be greater than zero when supplied".to_string(),
        ));
    }
    Ok(())
}

fn active_handle(
    tx: &Transaction<'_>,
    host: &str,
    resource_class: HostLeaseResourceClass,
    now: i64,
    process_inspector: &dyn ProcessInspector,
) -> Result<(Option<HostLeaseHandle>, bool), HostLeaseError> {
    let handle = read_handle(tx, host, resource_class)?;
    let Some(handle) = handle else {
        return Ok((None, false));
    };
    let expired = handle.expires_at_ms.is_some_and(|expiry| expiry <= now);
    let owner_dead = match (handle.owner_pid, handle.owner_process_identity) {
        (Some(pid), Some(expected_identity)) => match process_inspector.observe(pid) {
            ProcessObservation::Alive { identity } => identity != expected_identity,
            ProcessObservation::Dead => true,
            ProcessObservation::Unknown => false,
        },
        _ => false,
    };
    if expired || owner_dead {
        tx.execute(
            "DELETE FROM host_leases
             WHERE host = ?1 AND resource_class = ?2 AND lease_id = ?3",
            params![host, resource_class.as_str(), handle.lease_id],
        )?;
        return Ok((None, true));
    }
    Ok((Some(handle), false))
}

fn next_lease_wake_at(active: &HostLeaseHandle, now: i64, deadline_at_ms: Option<i64>) -> i64 {
    let mut wake_at = deadline_at_ms.unwrap_or(i64::MAX);
    if let Some(expiry) = active.expires_at_ms {
        wake_at = wake_at.min(expiry);
    }
    if active.owner_pid.is_some() {
        wake_at =
            wake_at.min(now.saturating_add(duration_ms_i64(PROCESS_LIVENESS_RECHECK_INTERVAL)));
    }
    wake_at
}

fn registry_busy_receipt(
    host: String,
    resource_class: HostLeaseResourceClass,
    now: i64,
    waited_ms: Option<u64>,
    deadline_at_ms: Option<i64>,
) -> HostLeaseAcquireReceipt {
    let next_wake_at_ms = now
        .saturating_add(duration_ms_i64(REGISTRY_BUSY_RETRY_INTERVAL))
        .min(deadline_at_ms.unwrap_or(i64::MAX));
    HostLeaseAcquireReceipt {
        schema_version: SCHEMA_VERSION,
        status: HostLeaseAcquireStatus::Deferred,
        observed_at_ms: now,
        waited_ms: waited_ms.unwrap_or(0),
        handle: None,
        defer: Some(HostLeaseDeferReceipt {
            host,
            resource_class,
            deferred_reason: HostLeaseDeferReason::RegistryBusy,
            observed_at_ms: now,
            next_wake_at_ms: Some(next_wake_at_ms),
            deadline_at_ms,
            active: None,
        }),
        recovered_stale_lease: false,
    }
}

fn sqlite_is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(inner, _)
            if matches!(inner.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

fn read_handle(
    tx: &Transaction<'_>,
    host: &str,
    resource_class: HostLeaseResourceClass,
) -> Result<Option<HostLeaseHandle>, HostLeaseError> {
    tx.query_row(
        "SELECT resource_class, lease_id, owner, priority_class, acquired_at_ms, updated_at_ms,
                expires_at_ms, owner_pid, owner_process_identity, reason, metadata_json,
                execution_context_json
         FROM host_leases WHERE host = ?1 AND resource_class = ?2",
        params![host, resource_class.as_str()],
        |row| {
            let priority: String = row.get(3)?;
            let metadata_json: String = row.get(10)?;
            let execution_context_json: Option<String> = row.get(11)?;
            let owner_pid_i64: Option<i64> = row.get(7)?;
            let owner_identity_i64: Option<i64> = row.get(8)?;
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                priority,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?,
                owner_pid_i64,
                owner_identity_i64,
                row.get::<_, Option<String>>(9)?,
                metadata_json,
                row.get::<_, String>(0)?,
                execution_context_json,
            ))
        },
    )
    .optional()?
    .map(
        |(
            lease_id,
            owner,
            priority,
            acquired_at_ms,
            updated_at_ms,
            expires_at_ms,
            owner_pid,
            owner_process_identity,
            reason,
            metadata_json,
            stored_resource_class,
            execution_context_json,
        )| {
            let owner_pid = owner_pid
                .map(|pid| {
                    u32::try_from(pid).map_err(|_| {
                        HostLeaseError::InvalidRequest(
                            "persisted owner_pid is outside the u32 range".to_string(),
                        )
                    })
                })
                .transpose()?;
            let owner_process_identity = owner_process_identity
                .map(|identity| {
                    u64::try_from(identity).map_err(|_| {
                        HostLeaseError::InvalidRequest(
                            "persisted process identity is negative".to_string(),
                        )
                    })
                })
                .transpose()?;
            Ok(HostLeaseHandle {
                schema_version: SCHEMA_VERSION,
                host: host.to_string(),
                resource_class: HostLeaseResourceClass::parse(&stored_resource_class)?,
                execution_context: execution_context_json
                    .map(|encoded| serde_json::from_str(&encoded))
                    .transpose()?,
                lease_id,
                owner,
                priority_class: HostLeasePriorityClass::parse(&priority)?,
                acquired_at_ms,
                updated_at_ms,
                expires_at_ms,
                owner_pid,
                owner_process_identity,
                reason,
                metadata: serde_json::from_str(&metadata_json)?,
            })
        },
    )
    .transpose()
}

fn write_handle(tx: &Transaction<'_>, handle: &HostLeaseHandle) -> Result<(), HostLeaseError> {
    let metadata_json = serde_json::to_string(&handle.metadata)?;
    let execution_context_json = handle
        .execution_context
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let owner_process_identity = handle
        .owner_process_identity
        .map(|value| {
            i64::try_from(value).map_err(|_| {
                HostLeaseError::InvalidRequest(
                    "owner process identity is outside the SQLite integer range".to_string(),
                )
            })
        })
        .transpose()?;
    tx.execute(
        "INSERT INTO host_leases (
            host, resource_class, lease_id, owner, priority_class, acquired_at_ms, updated_at_ms,
            expires_at_ms, owner_pid, owner_process_identity, reason, metadata_json,
            execution_context_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(host, resource_class) DO UPDATE SET
            lease_id = excluded.lease_id,
            owner = excluded.owner,
            priority_class = excluded.priority_class,
            acquired_at_ms = excluded.acquired_at_ms,
            updated_at_ms = excluded.updated_at_ms,
            expires_at_ms = excluded.expires_at_ms,
            owner_pid = excluded.owner_pid,
            owner_process_identity = excluded.owner_process_identity,
            reason = excluded.reason,
            metadata_json = excluded.metadata_json,
            execution_context_json = excluded.execution_context_json",
        params![
            handle.host,
            handle.resource_class.as_str(),
            handle.lease_id,
            handle.owner,
            handle.priority_class.as_str(),
            handle.acquired_at_ms,
            handle.updated_at_ms,
            handle.expires_at_ms,
            handle.owner_pid.map(i64::from),
            owner_process_identity,
            handle.reason,
            metadata_json,
            execution_context_json,
        ],
    )?;
    Ok(())
}

fn unix_now_ms() -> Result<i64, HostLeaseError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| HostLeaseError::Clock)?
        .as_millis();
    Ok(millis.min(i64::MAX as u128) as i64)
}

fn duration_ms_i64(duration: Duration) -> i64 {
    duration.as_millis().min(i64::MAX as u128) as i64
}

fn duration_ms_u64(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

fn u64_ms_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

#[cfg(test)]
#[path = "host_lease/tests.rs"]
mod tests;
