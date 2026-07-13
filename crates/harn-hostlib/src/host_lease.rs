//! Atomic, machine-global leases for scarce host resources.
//!
//! The store is deliberately independent of project event logs: two Harn
//! processes in different worktrees must still agree on one owner. SQLite's
//! immediate transaction supplies the cross-process compare-and-set. A file
//! watcher on the database directory wakes waiting callers after release or
//! renewal; expiry and caller deadlines remain timer wakeups.

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
const SQLITE_MUTATION_BUSY_TIMEOUT: Duration = Duration::from_secs(1);
const REGISTRY_BUSY_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const PROCESS_LIVENESS_RECHECK_INTERVAL: Duration = Duration::from_secs(5);
const SCHEMA_VERSION: u32 = 1;

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

/// Request to acquire one exclusive host resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeaseRequest {
    /// Machine resource name, normally the local hostname.
    pub host: String,
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
        let host = normalize_component("host", host)?;
        let mut conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = unix_now_ms()?;
        self.status_in_transaction(tx, &host, now)
    }

    /// Renew the active lease only when the token matches.
    pub fn renew(
        &self,
        host: &str,
        lease_id: &str,
        ttl_ms: u64,
    ) -> Result<HostLeaseRenewReceipt, HostLeaseError> {
        let host = normalize_component("host", host)?;
        let lease_id = normalize_component("lease_id", lease_id)?;
        validate_ttl(Some(ttl_ms))?;
        let mut conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = unix_now_ms()?;
        let (active, _) = active_handle(&tx, &host, now, self.process_inspector.as_ref())?;
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
        let host = normalize_component("host", host)?;
        let lease_id = normalize_component("lease_id", lease_id)?;
        let conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        let released = conn.execute(
            "DELETE FROM host_leases WHERE host = ?1 AND lease_id = ?2",
            params![host, lease_id],
        )? == 1;
        let now = unix_now_ms()?;
        Ok(HostLeaseReleaseReceipt {
            schema_version: SCHEMA_VERSION,
            released,
            host,
            lease_id,
            observed_at_ms: now,
        })
    }

    fn initialize(&self) -> Result<(), HostLeaseError> {
        let conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS host_leases (
                host TEXT PRIMARY KEY NOT NULL,
                lease_id TEXT NOT NULL,
                owner TEXT NOT NULL,
                priority_class TEXT NOT NULL,
                acquired_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                expires_at_ms INTEGER,
                owner_pid INTEGER,
                owner_process_identity INTEGER,
                reason TEXT,
                metadata_json TEXT NOT NULL
            );",
        )?;
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
        match self.acquire_in_transaction(tx, request, now, deadline_at_ms, waited_ms) {
            Err(HostLeaseError::Database(error)) if sqlite_is_busy(&error) => Ok(
                registry_busy_receipt(host, now, Some(waited_ms), deadline_at_ms),
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
        let (active, recovered_stale_lease) =
            active_handle(&tx, &request.host, now, self.process_inspector.as_ref())?;
        if let Some(active) = active {
            let defer = HostLeaseDeferReceipt {
                host: request.host,
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
    fn status_at(&self, host: &str, now: i64) -> Result<HostLeaseState, HostLeaseError> {
        let mut conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.status_in_transaction(tx, host, now)
    }

    fn status_in_transaction(
        &self,
        tx: Transaction<'_>,
        host: &str,
        now: i64,
    ) -> Result<HostLeaseState, HostLeaseError> {
        let (active, recovered_stale_lease) =
            active_handle(&tx, host, now, self.process_inspector.as_ref())?;
        tx.commit()?;
        Ok(HostLeaseState {
            schema_version: SCHEMA_VERSION,
            host: host.to_string(),
            observed_at_ms: now,
            active,
            recovered_stale_lease,
        })
    }
}

fn normalize_request(mut request: HostLeaseRequest) -> Result<HostLeaseRequest, HostLeaseError> {
    request.host = normalize_component("host", &request.host)?;
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
    now: i64,
    process_inspector: &dyn ProcessInspector,
) -> Result<(Option<HostLeaseHandle>, bool), HostLeaseError> {
    let handle = read_handle(tx, host)?;
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
            "DELETE FROM host_leases WHERE host = ?1 AND lease_id = ?2",
            params![host, handle.lease_id],
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
) -> Result<Option<HostLeaseHandle>, HostLeaseError> {
    tx.query_row(
        "SELECT lease_id, owner, priority_class, acquired_at_ms, updated_at_ms,
                expires_at_ms, owner_pid, owner_process_identity, reason, metadata_json
         FROM host_leases WHERE host = ?1",
        [host],
        |row| {
            let priority: String = row.get(2)?;
            let metadata_json: String = row.get(9)?;
            let owner_pid_i64: Option<i64> = row.get(6)?;
            let owner_identity_i64: Option<i64> = row.get(7)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                priority,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<i64>>(5)?,
                owner_pid_i64,
                owner_identity_i64,
                row.get::<_, Option<String>>(8)?,
                metadata_json,
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
            host, lease_id, owner, priority_class, acquired_at_ms, updated_at_ms,
            expires_at_ms, owner_pid, owner_process_identity, reason, metadata_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(host) DO UPDATE SET
            lease_id = excluded.lease_id,
            owner = excluded.owner,
            priority_class = excluded.priority_class,
            acquired_at_ms = excluded.acquired_at_ms,
            updated_at_ms = excluded.updated_at_ms,
            expires_at_ms = excluded.expires_at_ms,
            owner_pid = excluded.owner_pid,
            owner_process_identity = excluded.owner_process_identity,
            reason = excluded.reason,
            metadata_json = excluded.metadata_json",
        params![
            handle.host,
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
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    use tempfile::TempDir;

    use super::*;

    fn store(temp: &TempDir) -> HostLeaseStore {
        HostLeaseStore::for_root(temp.path()).unwrap()
    }

    #[derive(Debug)]
    struct ScriptedProcessInspector {
        observation: AtomicU64,
    }

    impl ScriptedProcessInspector {
        fn alive(identity: u64) -> Self {
            Self {
                observation: AtomicU64::new(identity.saturating_add(2)),
            }
        }

        fn set(&self, observation: ProcessObservation) {
            let value = match observation {
                ProcessObservation::Unknown => 0,
                ProcessObservation::Dead => 1,
                ProcessObservation::Alive { identity } => identity.saturating_add(2),
            };
            self.observation.store(value, Ordering::SeqCst);
        }
    }

    impl ProcessInspector for ScriptedProcessInspector {
        fn observe(&self, _pid: u32) -> ProcessObservation {
            match self.observation.load(Ordering::SeqCst) {
                0 => ProcessObservation::Unknown,
                1 => ProcessObservation::Dead,
                value => ProcessObservation::Alive {
                    identity: value - 2,
                },
            }
        }
    }

    fn request(owner: &str) -> HostLeaseRequest {
        HostLeaseRequest {
            host: "mac-local".to_string(),
            owner: owner.to_string(),
            priority_class: HostLeasePriorityClass::Measurement,
            ttl_ms: Some(60_000),
            owner_pid: None,
            reason: Some("meter run".to_string()),
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn acquire_blocks_second_owner_until_release() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        let first = store
            .try_acquire_at(request("codex-0"), 1_000, None, 0)
            .unwrap();
        assert_eq!(first.status, HostLeaseAcquireStatus::Acquired);
        let second = store
            .try_acquire_at(request("codex-1"), 1_001, None, 0)
            .unwrap();
        assert_eq!(second.status, HostLeaseAcquireStatus::Deferred);
        assert_eq!(
            second.defer.as_ref().unwrap().deferred_reason,
            HostLeaseDeferReason::Contended
        );

        let handle = first.handle.unwrap();
        assert!(
            store
                .release(&handle.host, &handle.lease_id)
                .unwrap()
                .released
        );
        let third = store
            .try_acquire_at(request("codex-1"), 1_002, None, 0)
            .unwrap();
        assert_eq!(third.status, HostLeaseAcquireStatus::Acquired);
    }

    #[test]
    fn immediate_transaction_allows_exactly_one_race_winner() {
        let temp = TempDir::new().unwrap();
        let store = Arc::new(store(&temp));
        let worker_count = 12;
        let barrier = Arc::new(Barrier::new(worker_count));
        let workers = (0..worker_count)
            .map(|index| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    store
                        .try_acquire_at(request(&format!("worker-{index}")), 1_000, None, 0)
                        .unwrap()
                        .status
                })
            })
            .collect::<Vec<_>>();
        let statuses = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == HostLeaseAcquireStatus::Acquired)
                .count(),
            1
        );
    }

    #[test]
    fn expiry_is_recovered_inside_the_acquire_transaction() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        let mut short = request("codex-0");
        short.ttl_ms = Some(10);
        store.try_acquire_at(short, 1_000, None, 0).unwrap();
        let next = store
            .try_acquire_at(request("codex-1"), 1_011, None, 0)
            .unwrap();
        assert_eq!(next.status, HostLeaseAcquireStatus::Acquired);
        assert!(next.recovered_stale_lease);
    }

    #[test]
    fn wrong_token_cannot_release_an_active_lease() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        let first = store
            .try_acquire_at(request("codex-0"), 1_000, None, 0)
            .unwrap();
        let handle = first.handle.unwrap();
        assert!(!store.release(&handle.host, "wrong-token").unwrap().released);
        assert_eq!(
            store.status_at(&handle.host, 1_001).unwrap().active,
            Some(handle)
        );
    }

    #[test]
    fn renew_requires_the_active_token() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        let first = store.try_acquire(request("codex-0")).unwrap();
        let handle = first.handle.unwrap();

        assert!(
            !store
                .renew(&handle.host, "wrong-token", 120_000)
                .unwrap()
                .renewed
        );
        let renewed = store
            .renew(&handle.host, &handle.lease_id, 120_000)
            .unwrap();
        assert!(renewed.renewed);
        let renewed_handle = renewed.handle.unwrap();
        assert_eq!(renewed_handle.lease_id, handle.lease_id);
        assert!(renewed_handle.updated_at_ms >= handle.updated_at_ms);
        assert!(renewed_handle.expires_at_ms > Some(renewed_handle.updated_at_ms));
    }

    #[test]
    fn non_expiring_lease_requires_a_live_owner_pid() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        let mut missing = request("codex-0");
        missing.ttl_ms = None;
        let error = store.try_acquire_at(missing, 1_000, None, 0).unwrap_err();
        assert!(error.to_string().contains("requires owner_pid"));

        let mut live = request("codex-0");
        live.ttl_ms = None;
        live.owner_pid = Some(std::process::id());
        let acquired = store.try_acquire_at(live, 1_000, None, 0).unwrap();
        assert_eq!(acquired.status, HostLeaseAcquireStatus::Acquired);
        assert_eq!(acquired.handle.unwrap().expires_at_ms, None);
    }

    #[test]
    fn unknown_process_liveness_preserves_the_active_lease() {
        let temp = TempDir::new().unwrap();
        let inspector = Arc::new(ScriptedProcessInspector::alive(42));
        let store = HostLeaseStore::for_root_with_inspector(
            temp.path(),
            Arc::clone(&inspector) as Arc<dyn ProcessInspector>,
        )
        .unwrap();
        let mut owner = request("codex-0");
        owner.ttl_ms = None;
        owner.owner_pid = Some(1234);
        store.try_acquire_at(owner, 1_000, None, 0).unwrap();

        inspector.set(ProcessObservation::Unknown);
        let deferred = store
            .try_acquire_at(request("codex-1"), 1_001, None, 0)
            .unwrap();
        assert_eq!(deferred.status, HostLeaseAcquireStatus::Deferred);
        assert!(!deferred.recovered_stale_lease);

        inspector.set(ProcessObservation::Dead);
        let recovered = store
            .try_acquire_at(request("codex-1"), 1_002, None, 0)
            .unwrap();
        assert_eq!(recovered.status, HostLeaseAcquireStatus::Acquired);
        assert!(recovered.recovered_stale_lease);
    }

    #[test]
    fn non_expiring_owner_gets_a_bounded_liveness_wake() {
        let mut active = request("codex-0");
        active.ttl_ms = None;
        active.owner_pid = Some(1234);
        let handle = HostLeaseHandle {
            schema_version: SCHEMA_VERSION,
            host: active.host,
            lease_id: "lease-1".to_string(),
            owner: active.owner,
            priority_class: active.priority_class,
            acquired_at_ms: 1_000,
            updated_at_ms: 1_000,
            expires_at_ms: None,
            owner_pid: active.owner_pid,
            owner_process_identity: Some(42),
            reason: active.reason,
            metadata: active.metadata,
        };
        assert_eq!(next_lease_wake_at(&handle, 1_000, Some(60_000)), 6_000);
    }

    #[test]
    fn registry_write_contention_returns_a_typed_defer_receipt() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        let mut conn = store.connection(SQLITE_MUTATION_BUSY_TIMEOUT).unwrap();
        let _tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();

        let receipt = store.try_acquire(request("codex-0")).unwrap();
        assert_eq!(receipt.status, HostLeaseAcquireStatus::Deferred);
        let defer = receipt.defer.unwrap();
        assert_eq!(defer.deferred_reason, HostLeaseDeferReason::RegistryBusy);
        assert!(defer.active.is_none());
    }

    #[test]
    fn wait_rechecks_after_cross_thread_release_without_polling() {
        let temp = TempDir::new().unwrap();
        let store = Arc::new(store(&temp));
        let first = store.try_acquire(request("codex-0")).unwrap();
        let handle = first.handle.unwrap();
        let waiter = {
            let store = Arc::clone(&store);
            thread::spawn(move || {
                store
                    .acquire_wait(request("codex-1"), Duration::from_secs(5))
                    .unwrap()
            })
        };
        assert!(
            store
                .release(&handle.host, &handle.lease_id)
                .unwrap()
                .released
        );
        let receipt = waiter.join().unwrap();
        assert_eq!(receipt.status, HostLeaseAcquireStatus::Acquired);
    }
}
