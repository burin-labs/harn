use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::value::VmError;

/// Persisted snapshot for a single task. Closures themselves are not
/// serialized (they capture host-side state that can't survive a process
/// boundary). Instead, we persist the metadata callers need to resume:
/// status, priority, FIFO seq, idempotency key, timestamps, and (for
/// terminal tasks) a string-form result/error. On reload, surviving
/// metadata is re-hydrated and re-execution is driven from idempotency
/// keys — see `submit_to_pool_entry` for the dedupe path.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct PersistedTask {
    pub(super) id: String,
    pub(super) pool_id: String,
    pub(super) pool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) key: Option<String>,
    pub(super) priority: i64,
    /// Last-recorded status (`queued` / `running` / `completed` / `failed`
    /// / `rejected`). Stale `running` tasks are re-enqueued on reload.
    pub(super) status: String,
    pub(super) submitted_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) rejection_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) rejection_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) idempotency_key: Option<String>,
    /// `result` is stringified (display form) so the JSONL line stays a
    /// stable shape across `VmValue` evolution. Callers that need the
    /// typed result wait on `pool_wait` for the in-memory rerun outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) result_display: Option<String>,
    /// Wall-clock ms snapshot for stale-in-flight detection.
    pub(super) heartbeat_at_ms: i64,
    /// Tiebreaker for priority queues; mirrors the live `seq` so reload
    /// preserves FIFO order among equal priorities.
    pub(super) seq: u64,
}

/// JSONL record kinds the pool durable store appends.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum PoolRecord {
    /// Pool metadata stamped once on create. Compaction rewrites this as
    /// the first line so reloads pick the latest values up.
    Pool {
        id: String,
        name: String,
        scope: String,
        scope_id: String,
        max_concurrent: usize,
        created_at: String,
        submit_counter: u64,
    },
    /// Full task snapshot. Compaction collapses successive `Task` records
    /// for the same id into the most recent one.
    Task { task: PersistedTask },
}

/// File-backed durability store for pipeline-scope pools.
///
/// Concurrency note: pool registry mutations are serialized by the
/// thread-local `POOLS` registry (Harn runs on a single-thread Tokio
/// runtime per session), so this store never sees parallel writes from
/// inside the same process. Cross-process safety relies on the trigger
/// dispatcher's existing single-writer convention for a given pipeline
/// run id — exactly mirroring the channel store's contract.
pub(super) struct PoolDurableStore {
    path: PathBuf,
}

impl PoolDurableStore {
    pub(super) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Append a record to the JSONL log. Crash-safe via line-flush +
    /// fsync at the end of the call. Compaction folds older entries on
    /// next `pool_create` reload, so the file size stays bounded by
    /// `max_concurrent + |queue| + |terminal-since-compaction|`.
    pub(super) fn append(&self, record: &PoolRecord) -> Result<(), VmError> {
        use std::io::Write as _;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    VmError::Runtime(format!(
                        "pool durable store: create dir '{}': {err}",
                        parent.display()
                    ))
                })?;
            }
        }
        let mut line = serde_json::to_vec(record)
            .map_err(|err| VmError::Runtime(format!("pool durable store: encode: {err}")))?;
        line.push(b'\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|err| {
                VmError::Runtime(format!(
                    "pool durable store: open '{}': {err}",
                    self.path.display()
                ))
            })?;
        file.write_all(&line)
            .and_then(|()| file.sync_data())
            .map_err(|err| {
                VmError::Runtime(format!(
                    "pool durable store: write '{}': {err}",
                    self.path.display()
                ))
            })?;
        Ok(())
    }

    /// Read the JSONL log and replay records into a `PersistedPoolState`.
    /// Records for the same `task.id` collapse to the most recent one.
    pub(super) fn load(&self) -> Result<Option<PersistedPoolState>, VmError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(VmError::Runtime(format!(
                    "pool durable store: read '{}': {err}",
                    self.path.display()
                )));
            }
        };
        let mut meta: Option<PersistedPoolMeta> = None;
        let mut tasks: BTreeMap<String, PersistedTask> = BTreeMap::new();
        for (line_no, raw) in bytes.split(|byte| *byte == b'\n').enumerate() {
            if raw.is_empty() {
                continue;
            }
            let record: PoolRecord = serde_json::from_slice(raw).map_err(|err| {
                VmError::Runtime(format!(
                    "pool durable store: decode '{}' line {}: {err}",
                    self.path.display(),
                    line_no + 1
                ))
            })?;
            match record {
                PoolRecord::Pool {
                    id,
                    name,
                    scope,
                    scope_id,
                    max_concurrent,
                    created_at,
                    submit_counter,
                } => {
                    meta = Some(PersistedPoolMeta {
                        id,
                        name,
                        scope,
                        scope_id,
                        max_concurrent,
                        created_at,
                        submit_counter,
                    });
                }
                PoolRecord::Task { task } => {
                    tasks.insert(task.id.clone(), task);
                }
            }
        }
        let Some(meta) = meta else {
            return Ok(None);
        };
        Ok(Some(PersistedPoolState { meta, tasks }))
    }

    /// Compact the JSONL log to its current logical contents: one `Pool`
    /// header line plus one `Task` line per surviving task. Written
    /// crash-safely via the shared atomic-write helper so a crash mid
    /// compaction leaves the previous file intact.
    pub(super) fn compact(
        &self,
        meta: &PersistedPoolMeta,
        tasks: &[PersistedTask],
    ) -> Result<(), VmError> {
        use std::io::Write as _;
        crate::atomic_io::atomic_write_with(&self.path, |writer| {
            let header = PoolRecord::Pool {
                id: meta.id.clone(),
                name: meta.name.clone(),
                scope: meta.scope.clone(),
                scope_id: meta.scope_id.clone(),
                max_concurrent: meta.max_concurrent,
                created_at: meta.created_at.clone(),
                submit_counter: meta.submit_counter,
            };
            let header_line = serde_json::to_vec(&header)
                .map_err(|err| std::io::Error::other(format!("encode header: {err}")))?;
            writer.write_all(&header_line)?;
            writer.write_all(b"\n")?;
            for task in tasks {
                let line = serde_json::to_vec(&PoolRecord::Task { task: task.clone() })
                    .map_err(|err| std::io::Error::other(format!("encode task: {err}")))?;
                writer.write_all(&line)?;
                writer.write_all(b"\n")?;
            }
            Ok(())
        })
        .map_err(|err| {
            VmError::Runtime(format!(
                "pool durable store: compact '{}': {err}",
                self.path.display()
            ))
        })
    }
}

#[derive(Clone, Debug)]
pub(super) struct PersistedPoolMeta {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) scope: String,
    pub(super) scope_id: String,
    pub(super) max_concurrent: usize,
    pub(super) created_at: String,
    pub(super) submit_counter: u64,
}

pub(super) struct PersistedPoolState {
    pub(super) meta: PersistedPoolMeta,
    pub(super) tasks: BTreeMap<String, PersistedTask>,
}
