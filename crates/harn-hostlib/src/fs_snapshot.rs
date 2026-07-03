//! Per-tool-call filesystem snapshots — Gemini-style `/restore` primitives.
//!
//! Captures the pre-image of paths touched by a mutating tool call so a
//! client can roll the change back surgically without losing untracked
//! work. Snapshot identity is the ACP `toolCallId`, so consumers index
//! into the same id space the rest of the transcript already records.
//!
//! Two capture modes:
//!
//! 1. **Explicit** — the caller passes a `paths` list to
//!    `hostlib_fs_snapshot`; bytes are copied immediately.
//! 2. **Auto-on-write** — calling `hostlib_fs_snapshot` without `paths`
//!    registers an open snapshot. The
//!    [`auto_capture_for_write`] hook fires from inside
//!    `tools/write_file` and `tools/delete_file` and lazy-copies each
//!    pre-image into the active snapshot keyed by the current
//!    [`harn_vm::agent_sessions::current_tool_call_id`].
//!
//! Storage layout (per session):
//!
//! ```text
//! .harn/state/snapshots/<session_id>/
//!   <snapshot_id>/
//!     manifest.json    # path -> { kind, body_hash?, mode? }
//!     bodies/<sha256>  # content-addressed; deduped across snapshots
//! ```
//!
//! Snapshots are session-scoped and ephemeral. They are not persisted
//! across machine reboots; consumers that need durable rollback bundle
//! them into a session via `session/load`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs as stdfs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};

use harn_vm::VmValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability};
use crate::tools::args::{
    build_dict, dict_arg, optional_string, optional_string_list, require_string, str_value,
    to_agent_path,
};

const SNAPSHOT_BUILTIN: &str = "hostlib_fs_snapshot";
const RESTORE_BUILTIN: &str = "hostlib_fs_restore";
const LIST_BUILTIN: &str = "hostlib_fs_list_snapshots";
const DROP_BUILTIN: &str = "hostlib_fs_drop_snapshot";

const MANIFEST_VERSION: u32 = 1;
const STATE_REL: &[&str] = &[".harn", "state", "snapshots"];

/// Default cap on the on-disk footprint of one session's snapshot bundle
/// before the oldest snapshots are evicted. Matches the proposal in
/// [#1720](https://github.com/burin-labs/harn/issues/1720): 1 GiB.
pub const DEFAULT_SESSION_BYTE_CAP: u64 = 1024 * 1024 * 1024;

/// Hostlib filesystem snapshot capability handle.
#[derive(Default)]
pub struct FsSnapshotCapability;

impl HostlibCapability for FsSnapshotCapability {
    fn module_name(&self) -> &'static str {
        // Snapshots live under the existing `fs/` schema directory so the
        // contract surface stays consolidated alongside the staging
        // primitives.
        "fs"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        registry.register_fn("fs", SNAPSHOT_BUILTIN, "snapshot", snapshot_builtin);
        registry.register_fn("fs", RESTORE_BUILTIN, "restore", restore_builtin);
        registry.register_fn("fs", LIST_BUILTIN, "list_snapshots", list_snapshots_builtin);
        registry.register_fn("fs", DROP_BUILTIN, "drop_snapshot", drop_snapshot_builtin);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SnapshotEntry {
    File {
        body_hash: String,
        len: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<u32>,
    },
    Absent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    snapshot_id: String,
    scope_id: String,
    session_id: String,
    root: String,
    taken_at_ms: i64,
    entries: BTreeMap<String, SnapshotEntry>,
}

#[derive(Clone, Debug)]
struct SnapshotState {
    snapshot_id: String,
    scope_id: String,
    session_id: String,
    root: PathBuf,
    taken_at_ms: i64,
    /// Logical absolute paths (workspace-relative when storage permits).
    entries: BTreeMap<PathBuf, SnapshotEntry>,
}

/// Per-snapshot summary returned by `list_snapshots`.
#[derive(Clone, Debug)]
pub struct SnapshotSummary {
    /// Stable identifier (canonically the ACP toolCallId).
    pub snapshot_id: String,
    /// Caller-chosen scope id passed when the snapshot was created.
    pub scope_id: String,
    /// Wall-clock capture time, milliseconds since the UNIX epoch.
    pub taken_at_ms: i64,
    /// Logical paths captured at snapshot time.
    pub captured_paths: Vec<String>,
    /// Total bytes captured for `captured_paths`.
    pub byte_count: u64,
}

/// Result returned after capturing a new snapshot.
#[derive(Clone, Debug)]
pub struct SnapshotResult {
    /// Stable identifier (equal to the requested `scope_id`).
    pub snapshot_id: String,
    /// Paths captured into this snapshot.
    pub captured_paths: Vec<String>,
    /// Total bytes captured for `captured_paths`.
    pub byte_count: u64,
}

/// Result returned after restoring a snapshot.
#[derive(Clone, Debug)]
pub struct RestoreResult {
    /// Echoed snapshot id.
    pub snapshot_id: String,
    /// Paths successfully restored.
    pub restored_paths: Vec<String>,
    /// Paths skipped, with human-readable reasons.
    pub skipped_paths_with_reasons: Vec<(String, String)>,
}

/// Result returned after dropping a snapshot.
#[derive(Clone, Debug)]
pub struct DropResult {
    /// Echoed snapshot id.
    pub snapshot_id: String,
    /// True when an existing snapshot was removed.
    pub dropped: bool,
}

#[derive(Debug)]
struct SessionSnapshots {
    /// Snapshots, in insertion order.
    snapshots: Vec<SnapshotState>,
    /// Bytes currently held in this session's snapshot bundle. We track
    /// this rather than recomputing from `bodies/` so eviction stays
    /// O(snapshots) instead of walking the filesystem on every write.
    byte_count: u64,
    /// Per-session byte cap. Defaults to [`DEFAULT_SESSION_BYTE_CAP`] and
    /// can be overridden with [`configure_session_byte_cap`].
    byte_cap: u64,
}

impl Default for SessionSnapshots {
    fn default() -> Self {
        Self {
            snapshots: Vec::new(),
            byte_count: 0,
            byte_cap: DEFAULT_SESSION_BYTE_CAP,
        }
    }
}

static SESSIONS: OnceLock<Mutex<BTreeMap<String, SessionSnapshots>>> = OnceLock::new();

fn sessions() -> &'static Mutex<BTreeMap<String, SessionSnapshots>> {
    SESSIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Override the byte cap for a specific session and immediately enforce
/// it. Returns the previous cap.
///
/// Primarily intended for tests that want to force eviction without
/// writing a gigabyte. Production embedders generally leave the default
/// in place; touching one session never affects another.
pub fn configure_session_byte_cap(session_id: &str, bytes: u64) -> u64 {
    let mut guard = sessions()
        .lock()
        .expect("fs_snapshot session mutex poisoned");
    let bundle = guard.entry(session_id.to_string()).or_default();
    let previous = bundle.byte_cap;
    bundle.byte_cap = bytes.max(1);
    enforce_byte_cap(bundle, session_id, None);
    previous
}

/// Drop every snapshot registered for `session_id`, both in memory and
/// on disk. Returns the number of snapshots removed.
///
/// ACP hosts should call this on session close so the snapshot bundle
/// doesn't outlive the conversation. Tests can also call it on
/// teardown when reusing a session id across cases.
pub fn drop_session_snapshots(session_id: &str) -> usize {
    let mut guard = sessions()
        .lock()
        .expect("fs_snapshot session mutex poisoned");
    let Some(bundle) = guard.remove(session_id) else {
        return 0;
    };
    let count = bundle.snapshots.len();
    for snapshot in &bundle.snapshots {
        remove_snapshot_dir(snapshot);
    }
    count
}

/// Drop every registered session's snapshots, in memory and on disk.
/// Returns the number of sessions removed.
///
/// [`drop_session_snapshots`] handles a single conversation on ACP
/// session close. This drains the entire process-global map and is
/// intended for host reset paths (e.g. the test runner between cases)
/// where the worker is reused and snapshot bundles would otherwise
/// accumulate one session at a time.
pub fn reset_all_sessions() -> usize {
    let mut guard = sessions()
        .lock()
        .expect("fs_snapshot session mutex poisoned");
    let session_count = guard.len();
    for bundle in guard.values() {
        for snapshot in &bundle.snapshots {
            remove_snapshot_dir(snapshot);
        }
    }
    guard.clear();
    session_count
}

/// Number of sessions with registered snapshots. Test-only.
#[cfg(test)]
pub fn session_count() -> usize {
    sessions()
        .lock()
        .expect("fs_snapshot session mutex poisoned")
        .len()
}

/// Take a snapshot. When `paths` is empty the snapshot is "open" — bytes
/// are captured lazily as `auto_capture_for_write` fires from inside
/// the mutating tool builtins.
pub fn snapshot(
    session_id: &str,
    scope_id: &str,
    paths: &[String],
    root: Option<&Path>,
) -> Result<SnapshotResult, HostlibError> {
    validate_session_id(SNAPSHOT_BUILTIN, session_id)?;
    validate_scope_id(SNAPSHOT_BUILTIN, scope_id)?;
    let root = resolve_root(root);
    let mut guard = sessions()
        .lock()
        .expect("fs_snapshot session mutex poisoned");
    let bundle = guard.entry(session_id.to_string()).or_default();
    upsert_snapshot(bundle, session_id, scope_id, &root)?;
    let mut captured_paths = Vec::new();
    let mut byte_count = 0u64;
    for raw in paths {
        let path = normalize_logical(Path::new(raw));
        let added =
            capture_path(bundle, session_id, scope_id, &path, &root).map_err(|message| {
                HostlibError::Backend {
                    builtin: SNAPSHOT_BUILTIN,
                    message,
                }
            })?;
        if let Some(bytes) = added {
            byte_count = byte_count.saturating_add(bytes);
            captured_paths.push(to_agent_path(&path));
        }
    }
    enforce_byte_cap(bundle, session_id, Some(scope_id));
    let state = bundle
        .snapshots
        .iter()
        .find(|snap| snap.snapshot_id == scope_id)
        .expect("snapshot just upserted is protected from byte-cap eviction");
    persist_manifest(state).map_err(|err| HostlibError::Backend {
        builtin: SNAPSHOT_BUILTIN,
        message: err,
    })?;
    Ok(SnapshotResult {
        snapshot_id: state.snapshot_id.clone(),
        captured_paths,
        byte_count,
    })
}

/// Restore a previously-captured snapshot.
pub fn restore(
    session_id: &str,
    snapshot_id: &str,
    paths: &[String],
) -> Result<RestoreResult, HostlibError> {
    validate_session_id(RESTORE_BUILTIN, session_id)?;
    validate_scope_id(RESTORE_BUILTIN, snapshot_id)?;
    let mut guard = sessions()
        .lock()
        .expect("fs_snapshot session mutex poisoned");
    let bundle = guard
        .get_mut(session_id)
        .ok_or_else(|| HostlibError::Backend {
            builtin: RESTORE_BUILTIN,
            message: format!("no snapshots registered for session `{session_id}`"),
        })?;
    let state = bundle
        .snapshots
        .iter()
        .find(|snap| snap.snapshot_id == snapshot_id)
        .cloned()
        .ok_or_else(|| HostlibError::Backend {
            builtin: RESTORE_BUILTIN,
            message: format!("unknown snapshot `{snapshot_id}` for session `{session_id}`"),
        })?;
    let selected = select_paths(&state, paths);
    let mut restored_paths = Vec::new();
    let mut skipped_paths_with_reasons = Vec::new();
    for path in selected {
        let Some(entry) = state.entries.get(&path) else {
            continue;
        };
        let label = to_agent_path(&path);
        match restore_entry(&state, &path, entry) {
            Ok(()) => restored_paths.push(label),
            Err(reason) => skipped_paths_with_reasons.push((label, reason)),
        }
    }
    Ok(RestoreResult {
        snapshot_id: snapshot_id.to_string(),
        restored_paths,
        skipped_paths_with_reasons,
    })
}

/// List snapshots registered for a session, sorted by capture time.
pub fn list_snapshots(session_id: &str) -> Result<Vec<SnapshotSummary>, HostlibError> {
    validate_session_id(LIST_BUILTIN, session_id)?;
    let guard = sessions()
        .lock()
        .expect("fs_snapshot session mutex poisoned");
    let Some(bundle) = guard.get(session_id) else {
        return Ok(Vec::new());
    };
    let mut summaries: Vec<SnapshotSummary> = bundle
        .snapshots
        .iter()
        .map(|state| SnapshotSummary {
            snapshot_id: state.snapshot_id.clone(),
            scope_id: state.scope_id.clone(),
            taken_at_ms: state.taken_at_ms,
            captured_paths: state.entries.keys().map(to_agent_path).collect(),
            byte_count: entry_byte_count(state),
        })
        .collect();
    summaries.sort_by_key(|summary| summary.taken_at_ms);
    Ok(summaries)
}

/// Drop a snapshot's in-memory and on-disk state.
pub fn drop_snapshot(session_id: &str, snapshot_id: &str) -> Result<DropResult, HostlibError> {
    validate_session_id(DROP_BUILTIN, session_id)?;
    validate_scope_id(DROP_BUILTIN, snapshot_id)?;
    let mut guard = sessions()
        .lock()
        .expect("fs_snapshot session mutex poisoned");
    let Some(bundle) = guard.get_mut(session_id) else {
        return Ok(DropResult {
            snapshot_id: snapshot_id.to_string(),
            dropped: false,
        });
    };
    let position = bundle
        .snapshots
        .iter()
        .position(|snap| snap.snapshot_id == snapshot_id);
    let dropped = match position {
        Some(idx) => {
            let removed = bundle.snapshots.remove(idx);
            bundle.byte_count = bundle.byte_count.saturating_sub(entry_byte_count(&removed));
            remove_snapshot_dir(&removed);
            true
        }
        None => false,
    };
    Ok(DropResult {
        snapshot_id: snapshot_id.to_string(),
        dropped,
    })
}

/// Auto-on-write hook called from the mutating tool builtins.
///
/// Captures `path`'s pre-image into the snapshot whose id matches the
/// current [`harn_vm::agent_sessions::current_tool_call_id`]. The first
/// write in a tool call auto-opens that snapshot. The hook silently no-ops
/// when no session is active or no tool-call id is set, which keeps read-only
/// tools and writes outside active tool scopes cheap.
pub(crate) fn auto_capture_for_write(builtin: &'static str, path: &Path) {
    let Some(session_id) = active_session_id() else {
        return;
    };
    // Record the mutated path against the session BEFORE the snapshot/tool-call
    // gate below: this is the single chokepoint every hostlib write reaches, so
    // it is the authoritative source for a session's `files_written` (consumed by
    // the sub-agent receipt). Recorded unconditionally — even when no restore
    // snapshot is open (no active tool call) — because the write still happened.
    harn_vm::agent_sessions::record_session_changed_path(
        &session_id,
        normalize_logical(path).to_string_lossy().as_ref(),
    );
    let Some(snapshot_id) = harn_vm::agent_sessions::current_tool_call_id() else {
        return;
    };
    let mut guard = sessions()
        .lock()
        .expect("fs_snapshot session mutex poisoned");
    let bundle = guard.entry(session_id.clone()).or_default();
    if !bundle
        .snapshots
        .iter()
        .any(|snap| snap.snapshot_id == snapshot_id)
    {
        let root =
            crate::fs::configured_session_root(&session_id).unwrap_or_else(|| resolve_root(None));
        if let Err(error) = upsert_snapshot(bundle, &session_id, &snapshot_id, &root) {
            tracing::warn!(
                "fs_snapshot: failed to auto-open snapshot {snapshot_id} in session {session_id} (builtin={builtin}): {error}"
            );
            return;
        }
    }
    let Some(snapshot) = bundle
        .snapshots
        .iter()
        .find(|snap| snap.snapshot_id == snapshot_id)
    else {
        return;
    };
    let scope_id = snapshot.scope_id.clone();
    let root = snapshot.root.clone();
    let key = normalize_logical(path);
    match capture_path(bundle, &session_id, &snapshot_id, &key, &root) {
        Ok(_added) => {
            if let Some(state) = bundle
                .snapshots
                .iter()
                .find(|snap| snap.snapshot_id == snapshot_id)
            {
                if let Err(err) = persist_manifest(state) {
                    tracing::warn!(
                        "fs_snapshot: failed to persist manifest for snapshot {snapshot_id} in session {session_id} (scope_id={scope_id}, builtin={builtin}): {err}"
                    );
                }
            }
        }
        Err(err) => {
            tracing::warn!(
                "fs_snapshot: failed to auto-capture `{}` for snapshot {snapshot_id} in session {session_id} (scope_id={scope_id}, builtin={builtin}): {err}",
                key.display()
            );
        }
    }
    enforce_byte_cap(bundle, &session_id, Some(&snapshot_id));
}

fn snapshot_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(SNAPSHOT_BUILTIN, args)?;
    let dict = raw.as_ref();
    let session_id = require_string(SNAPSHOT_BUILTIN, dict, "session_id")?;
    let scope_id = require_string(SNAPSHOT_BUILTIN, dict, "scope_id")?;
    let paths = optional_string_list(SNAPSHOT_BUILTIN, dict, "paths")?;
    let root = optional_string(SNAPSHOT_BUILTIN, dict, "root")?.map(PathBuf::from);
    let result = snapshot(&session_id, &scope_id, &paths, root.as_deref())?;
    Ok(build_dict([
        ("snapshot_id", str_value(&result.snapshot_id)),
        (
            "captured_paths",
            VmValue::List(Arc::new(
                result
                    .captured_paths
                    .into_iter()
                    .map(|path| VmValue::String(arcstr::ArcStr::from(path)))
                    .collect(),
            )),
        ),
        ("byte_count", VmValue::Int(result.byte_count as i64)),
    ]))
}

fn restore_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(RESTORE_BUILTIN, args)?;
    let dict = raw.as_ref();
    let session_id = require_string(RESTORE_BUILTIN, dict, "session_id")?;
    let snapshot_id = require_string(RESTORE_BUILTIN, dict, "snapshot_id")?;
    let paths = optional_string_list(RESTORE_BUILTIN, dict, "paths")?;
    let result = restore(&session_id, &snapshot_id, &paths)?;
    Ok(build_dict([
        ("snapshot_id", str_value(&result.snapshot_id)),
        (
            "restored_paths",
            VmValue::List(Arc::new(
                result
                    .restored_paths
                    .into_iter()
                    .map(|path| VmValue::String(arcstr::ArcStr::from(path)))
                    .collect(),
            )),
        ),
        (
            "skipped_paths_with_reasons",
            VmValue::List(Arc::new(
                result
                    .skipped_paths_with_reasons
                    .into_iter()
                    .map(|(path, reason)| {
                        build_dict([("path", str_value(&path)), ("reason", str_value(&reason))])
                    })
                    .collect(),
            )),
        ),
    ]))
}

fn list_snapshots_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(LIST_BUILTIN, args)?;
    let dict = raw.as_ref();
    let session_id = require_string(LIST_BUILTIN, dict, "session_id")?;
    let summaries = list_snapshots(&session_id)?;
    Ok(build_dict([(
        "snapshots",
        VmValue::List(Arc::new(
            summaries.into_iter().map(snapshot_summary_value).collect(),
        )),
    )]))
}

fn drop_snapshot_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(DROP_BUILTIN, args)?;
    let dict = raw.as_ref();
    let session_id = require_string(DROP_BUILTIN, dict, "session_id")?;
    let snapshot_id = require_string(DROP_BUILTIN, dict, "snapshot_id")?;
    let result = drop_snapshot(&session_id, &snapshot_id)?;
    Ok(build_dict([
        ("snapshot_id", str_value(&result.snapshot_id)),
        ("dropped", VmValue::Bool(result.dropped)),
    ]))
}

fn snapshot_summary_value(summary: SnapshotSummary) -> VmValue {
    build_dict([
        ("snapshot_id", str_value(&summary.snapshot_id)),
        ("scope_id", str_value(&summary.scope_id)),
        ("taken_at_ms", VmValue::Int(summary.taken_at_ms)),
        (
            "captured_paths",
            VmValue::List(Arc::new(
                summary
                    .captured_paths
                    .into_iter()
                    .map(|path| VmValue::String(arcstr::ArcStr::from(path)))
                    .collect(),
            )),
        ),
        ("byte_count", VmValue::Int(summary.byte_count as i64)),
    ])
}

fn upsert_snapshot(
    bundle: &mut SessionSnapshots,
    session_id: &str,
    scope_id: &str,
    root: &Path,
) -> Result<(), HostlibError> {
    if bundle
        .snapshots
        .iter()
        .any(|snap| snap.snapshot_id == scope_id)
    {
        return Ok(());
    }
    let state = SnapshotState {
        snapshot_id: scope_id.to_string(),
        scope_id: scope_id.to_string(),
        session_id: session_id.to_string(),
        root: root.to_path_buf(),
        taken_at_ms: now_ms(),
        entries: BTreeMap::new(),
    };
    let dir = snapshot_dir(&state.root, &state.session_id, &state.snapshot_id);
    stdfs::create_dir_all(dir.join("bodies")).map_err(|err| HostlibError::Backend {
        builtin: SNAPSHOT_BUILTIN,
        message: format!("mkdir {}: {err}", dir.display()),
    })?;
    bundle.snapshots.push(state);
    Ok(())
}

fn capture_path(
    bundle: &mut SessionSnapshots,
    session_id: &str,
    snapshot_id: &str,
    path: &Path,
    root: &Path,
) -> Result<Option<u64>, String> {
    let snap_index = bundle
        .snapshots
        .iter()
        .position(|snap| snap.snapshot_id == snapshot_id)
        .ok_or_else(|| format!("snapshot `{snapshot_id}` is not registered"))?;
    if bundle.snapshots[snap_index].entries.contains_key(path) {
        return Ok(None);
    }
    let metadata = stdfs::symlink_metadata(path);
    let (entry, byte_count) = match metadata {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (SnapshotEntry::Absent, 0u64),
        Err(err) => {
            return Err(format!("stat `{}`: {err}", path.display()));
        }
        Ok(metadata) if metadata.is_dir() => {
            return Err(format!(
                "snapshot of directory `{}` is not supported yet",
                path.display()
            ));
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!(
                "snapshot of symlink `{}` is not supported yet",
                path.display()
            ));
        }
        Ok(metadata) => {
            let bytes = stdfs::read(path)
                .map_err(|err| format!("read `{}` for snapshot: {err}", path.display()))?;
            let body_hash = hex::encode(Sha256::digest(&bytes));
            let len = bytes.len() as u64;
            store_body(root, session_id, snapshot_id, &body_hash, &bytes)?;
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::MetadataExt;
                Some(metadata.mode())
            };
            #[cfg(not(unix))]
            let mode = {
                let _ = &metadata;
                None
            };
            (
                SnapshotEntry::File {
                    body_hash,
                    len,
                    mode,
                },
                len,
            )
        }
    };
    let snap = &mut bundle.snapshots[snap_index];
    snap.entries.insert(path.to_path_buf(), entry);
    bundle.byte_count = bundle.byte_count.saturating_add(byte_count);
    Ok(Some(byte_count))
}

fn store_body(
    root: &Path,
    session_id: &str,
    snapshot_id: &str,
    body_hash: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let bodies = snapshot_dir(root, session_id, snapshot_id).join("bodies");
    stdfs::create_dir_all(&bodies).map_err(|err| format!("mkdir {}: {err}", bodies.display()))?;
    let body_path = bodies.join(body_hash);
    if !body_path.exists() {
        atomic_write(&body_path, bytes)?;
    }
    Ok(())
}

fn restore_entry(state: &SnapshotState, path: &Path, entry: &SnapshotEntry) -> Result<(), String> {
    match entry {
        SnapshotEntry::Absent => match stdfs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() => stdfs::remove_dir_all(path)
                .map_err(|err| format!("remove_dir_all {}: {err}", path.display())),
            Ok(_) => stdfs::remove_file(path)
                .map_err(|err| format!("remove_file {}: {err}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!("stat {}: {err}", path.display())),
        },
        SnapshotEntry::File {
            body_hash, mode, ..
        } => {
            let body_path = snapshot_dir(&state.root, &state.session_id, &state.snapshot_id)
                .join("bodies")
                .join(body_hash);
            let bytes = stdfs::read(&body_path)
                .map_err(|err| format!("read snapshot body `{}`: {err}", body_path.display()))?;
            atomic_write(path, &bytes)?;
            #[cfg(unix)]
            if let Some(bits) = mode {
                use std::os::unix::fs::PermissionsExt;
                let permissions = stdfs::Permissions::from_mode(*bits);
                stdfs::set_permissions(path, permissions)
                    .map_err(|err| format!("set_permissions `{}`: {err}", path.display()))?;
            }
            #[cfg(not(unix))]
            let _ = mode;
            Ok(())
        }
    }
}

fn persist_manifest(state: &SnapshotState) -> Result<(), String> {
    let dir = snapshot_dir(&state.root, &state.session_id, &state.snapshot_id);
    stdfs::create_dir_all(&dir).map_err(|err| format!("mkdir {}: {err}", dir.display()))?;
    let manifest = Manifest {
        version: MANIFEST_VERSION,
        snapshot_id: state.snapshot_id.clone(),
        scope_id: state.scope_id.clone(),
        session_id: state.session_id.clone(),
        root: state.root.to_string_lossy().into_owned(),
        taken_at_ms: state.taken_at_ms,
        entries: state
            .entries
            .iter()
            .map(|(path, entry)| (path.to_string_lossy().into_owned(), entry.clone()))
            .collect(),
    };
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|err| format!("serialize snapshot manifest: {err}"))?;
    atomic_write(&dir.join("manifest.json"), &bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        stdfs::create_dir_all(parent)
            .map_err(|err| format!("mkdir {}: {err}", parent.display()))?;
    }
    let tmp = path.with_extension(format!("tmp-{}-{}", std::process::id(), now_ms()));
    stdfs::write(&tmp, bytes).map_err(|err| format!("write {}: {err}", tmp.display()))?;
    match stdfs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            let _ = stdfs::remove_file(path);
            stdfs::rename(&tmp, path).map_err(|retry| {
                // Both renames failed; the temp file would otherwise linger
                // and accumulate in the snapshot directory.
                let _ = stdfs::remove_file(&tmp);
                format!(
                    "rename {} to {}: {rename_err}; retry: {retry}",
                    tmp.display(),
                    path.display()
                )
            })
        }
    }
}

/// Evict snapshots oldest-first until the session is back under its byte
/// cap. `protected` names the snapshot currently being written (if any);
/// it is never evicted, even when it alone exceeds the cap — otherwise the
/// caller would lose the very snapshot it just captured (and `snapshot`
/// would panic re-fetching it). A snapshot larger than the whole cap is
/// therefore retained: rollback for an in-flight write takes precedence
/// over the soft budget.
fn enforce_byte_cap(bundle: &mut SessionSnapshots, session_id: &str, protected: Option<&str>) {
    while bundle.byte_count > bundle.byte_cap {
        let Some(idx) = bundle
            .snapshots
            .iter()
            .position(|snap| Some(snap.snapshot_id.as_str()) != protected)
        else {
            break;
        };
        let evicted = bundle.snapshots.remove(idx);
        bundle.byte_count = bundle.byte_count.saturating_sub(entry_byte_count(&evicted));
        tracing::info!(
            "fs_snapshot: evicting snapshot `{}` from session `{session_id}` (over byte cap {})",
            evicted.snapshot_id,
            bundle.byte_cap,
        );
        remove_snapshot_dir(&evicted);
    }
}

fn remove_snapshot_dir(state: &SnapshotState) {
    let dir = snapshot_dir(&state.root, &state.session_id, &state.snapshot_id);
    let _ = stdfs::remove_dir_all(&dir);
}

fn entry_byte_count(state: &SnapshotState) -> u64 {
    state
        .entries
        .values()
        .map(|entry| match entry {
            SnapshotEntry::File { len, .. } => *len,
            SnapshotEntry::Absent => 0,
        })
        .sum()
}

fn select_paths(state: &SnapshotState, paths: &[String]) -> Vec<PathBuf> {
    if paths.is_empty() {
        return state.entries.keys().cloned().collect();
    }
    let requested: BTreeSet<PathBuf> = paths
        .iter()
        .map(|path| normalize_logical(Path::new(path)))
        .collect();
    state
        .entries
        .keys()
        .filter(|path| requested.contains(*path))
        .cloned()
        .collect()
}

fn validate_session_id(builtin: &'static str, session_id: &str) -> Result<(), HostlibError> {
    if session_id.trim().is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: "session_id",
            message: "must not be empty".to_string(),
        });
    }
    Ok(())
}

fn validate_scope_id(builtin: &'static str, scope_id: &str) -> Result<(), HostlibError> {
    if scope_id.trim().is_empty() {
        let param = match builtin {
            SNAPSHOT_BUILTIN => "scope_id",
            _ => "snapshot_id",
        };
        return Err(HostlibError::InvalidParameter {
            builtin,
            param,
            message: "must not be empty".to_string(),
        });
    }
    Ok(())
}

fn active_session_id() -> Option<String> {
    harn_vm::agent_sessions::current_session_id().filter(|id| !id.trim().is_empty())
}

fn resolve_root(root: Option<&Path>) -> PathBuf {
    match root {
        Some(path) => normalize_logical(path),
        None => normalize_logical(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
    }
}

fn snapshot_dir(root: &Path, session_id: &str, snapshot_id: &str) -> PathBuf {
    let mut dir = root.to_path_buf();
    for component in STATE_REL {
        dir.push(component);
    }
    dir.push(sanitize_component(session_id));
    dir.push(sanitize_component(snapshot_id));
    dir
}

fn sanitize_component(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '_',
        })
        .collect();
    if sanitized == input {
        sanitized
    } else {
        let hash = hex::encode(Sha256::digest(input.as_bytes()));
        format!("{sanitized}-{}", &hash[..12])
    }
}

fn normalize_logical(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tempfile::TempDir;

    /// Hand each test its own session id so the process-wide `SESSIONS`
    /// map isolates them by key — no serialization or process-wide
    /// reset required.
    fn unique_session(prefix: &str) -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{n}-{}", std::process::id())
    }

    fn unique_scope() -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        format!("tc-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    fn enter_session(id: &str) -> harn_vm::agent_sessions::CurrentSessionGuard {
        harn_vm::agent_sessions::open_or_create(Some(id.to_string()));
        harn_vm::agent_sessions::enter_current_session(id.to_string())
    }

    #[test]
    fn explicit_snapshot_then_restore_round_trips_file_bytes() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("note.txt");
        stdfs::write(&file, b"v1").unwrap();
        let session = unique_session("snap-roundtrip");
        let scope = unique_scope();
        let _session_guard = enter_session(&session);

        let result = snapshot(
            &session,
            &scope,
            &[file.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();
        assert_eq!(result.snapshot_id, scope);
        assert_eq!(result.captured_paths.len(), 1);
        assert_eq!(result.byte_count, 2);

        stdfs::write(&file, b"clobbered").unwrap();
        let restored = restore(&session, &scope, &[]).unwrap();
        assert_eq!(restored.restored_paths.len(), 1);
        assert!(restored.skipped_paths_with_reasons.is_empty());
        assert_eq!(stdfs::read(&file).unwrap(), b"v1");
    }

    #[test]
    fn restore_reinstates_deleted_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("doomed.txt");
        stdfs::write(&file, b"alive").unwrap();
        let session = unique_session("snap-reinstate");
        let scope = unique_scope();
        let _session_guard = enter_session(&session);

        snapshot(
            &session,
            &scope,
            &[file.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();
        stdfs::remove_file(&file).unwrap();
        assert!(!file.exists());
        let restored = restore(&session, &scope, &[]).unwrap();
        assert_eq!(restored.restored_paths.len(), 1);
        assert_eq!(stdfs::read(&file).unwrap(), b"alive");
    }

    #[test]
    fn absent_snapshot_means_restore_deletes_paths_created_during_the_call() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("new.txt");
        assert!(!file.exists());
        let session = unique_session("snap-absent");
        let scope = unique_scope();
        let _session_guard = enter_session(&session);

        snapshot(
            &session,
            &scope,
            &[file.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();
        stdfs::write(&file, b"created during call").unwrap();
        let restored = restore(&session, &scope, &[]).unwrap();
        assert_eq!(restored.restored_paths.len(), 1);
        assert!(
            !file.exists(),
            "restore must delete files that the snapshot saw as absent"
        );
    }

    #[test]
    fn list_and_drop_round_trip_through_metadata() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("listed.txt");
        stdfs::write(&file, b"abc").unwrap();
        let session = unique_session("snap-list");
        let scope = unique_scope();
        let _session_guard = enter_session(&session);

        snapshot(
            &session,
            &scope,
            &[file.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();
        let summaries = list_snapshots(&session).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].snapshot_id, scope);
        assert_eq!(summaries[0].byte_count, 3);

        let dropped = drop_snapshot(&session, &scope).unwrap();
        assert!(dropped.dropped);
        assert!(list_snapshots(&session).unwrap().is_empty());

        let again = drop_snapshot(&session, &scope).unwrap();
        assert!(!again.dropped, "second drop must be idempotent");
    }

    #[test]
    fn auto_capture_records_pre_image_keyed_by_current_tool_call_id() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("auto.txt");
        stdfs::write(&file, b"pre").unwrap();
        let session = unique_session("snap-auto");
        let scope = unique_scope();
        let _session_guard = enter_session(&session);
        let _tool_guard = harn_vm::agent_sessions::enter_current_tool_call(scope.clone());

        snapshot(&session, &scope, &[], Some(dir.path())).unwrap();
        auto_capture_for_write("hostlib_tools_write_file", &file);
        stdfs::write(&file, b"post").unwrap();

        let restored = restore(&session, &scope, &[]).unwrap();
        assert_eq!(restored.restored_paths.len(), 1);
        assert_eq!(stdfs::read(&file).unwrap(), b"pre");
    }

    #[test]
    fn auto_capture_records_session_changed_path_for_files_written_receipt() {
        let dir = TempDir::new().unwrap();
        let one = dir.path().join("a.txt");
        let two = dir.path().join("b.txt");
        let session = unique_session("snap-changed");
        harn_vm::agent_sessions::clear_session_changed_paths(&session);
        let _session_guard = enter_session(&session);

        // No active tool call / open snapshot: the write still happened, so the
        // path must be recorded for the receipt regardless.
        auto_capture_for_write("hostlib_tools_write_file", &one);
        auto_capture_for_write("hostlib_tools_write_file", &two);
        // A duplicate write of the same path must dedupe.
        auto_capture_for_write("hostlib_tools_write_file", &one);

        let changed = harn_vm::agent_sessions::session_changed_paths(&session);
        assert_eq!(changed.len(), 2, "two distinct paths recorded (deduped)");
        let expect_one = normalize_logical(&one).to_string_lossy().into_owned();
        let expect_two = normalize_logical(&two).to_string_lossy().into_owned();
        assert!(
            changed.contains(&expect_one),
            "path a recorded: {changed:?}"
        );
        assert!(
            changed.contains(&expect_two),
            "path b recorded: {changed:?}"
        );

        // `take` drains so the receipt captures the set exactly once.
        let drained = harn_vm::agent_sessions::take_session_changed_paths(&session);
        assert_eq!(drained.len(), 2);
        assert!(
            harn_vm::agent_sessions::session_changed_paths(&session).is_empty(),
            "take drains the session's recorded paths"
        );
    }

    #[test]
    fn byte_cap_evicts_oldest_snapshot_when_exceeded() {
        let dir = TempDir::new().unwrap();
        let session = unique_session("snap-evict");
        let _session_guard = enter_session(&session);

        // Per-session cap: only affects this test's session, so other
        // tests can run in parallel without seeing the squeeze.
        configure_session_byte_cap(&session, 8);

        let mk = |name: &str| {
            let path = dir.path().join(name);
            stdfs::write(&path, b"12345").unwrap();
            path
        };

        let scope_a = unique_scope();
        let scope_b = unique_scope();
        let a = mk("a.txt");
        snapshot(
            &session,
            &scope_a,
            &[a.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();
        let b = mk("b.txt");
        snapshot(
            &session,
            &scope_b,
            &[b.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();

        let ids: Vec<String> = list_snapshots(&session)
            .unwrap()
            .into_iter()
            .map(|summary| summary.snapshot_id)
            .collect();
        assert_eq!(
            ids,
            vec![scope_b],
            "older snapshot must be evicted when the per-session byte cap is exceeded"
        );
    }

    #[test]
    fn snapshot_larger_than_cap_is_retained_not_evicted() {
        // A single snapshot whose captured bytes exceed the whole cap must
        // survive — evicting the snapshot we just took would lose rollback
        // for the in-flight write (and previously panicked re-fetching it).
        let dir = TempDir::new().unwrap();
        let session = unique_session("snap-oversized");
        let _session_guard = enter_session(&session);
        configure_session_byte_cap(&session, 4);

        let scope = unique_scope();
        let file = dir.path().join("big.txt");
        stdfs::write(&file, b"0123456789").unwrap();
        let result = snapshot(
            &session,
            &scope,
            &[file.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();
        assert_eq!(result.byte_count, 10);

        let ids: Vec<String> = list_snapshots(&session)
            .unwrap()
            .into_iter()
            .map(|summary| summary.snapshot_id)
            .collect();
        assert_eq!(
            ids,
            vec![scope],
            "an oversized snapshot must be retained rather than evicting itself"
        );
    }

    #[test]
    fn drop_session_snapshots_removes_every_snapshot_for_a_session() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("retained.txt");
        stdfs::write(&file, b"x").unwrap();
        let session = unique_session("snap-drop-session");
        let scope_a = unique_scope();
        let scope_b = unique_scope();
        let _session_guard = enter_session(&session);

        snapshot(
            &session,
            &scope_a,
            &[file.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();
        snapshot(
            &session,
            &scope_b,
            &[file.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();
        assert_eq!(list_snapshots(&session).unwrap().len(), 2);

        assert_eq!(drop_session_snapshots(&session), 2);
        assert!(list_snapshots(&session).unwrap().is_empty());
        assert_eq!(drop_session_snapshots(&session), 0, "idempotent");
    }
}
