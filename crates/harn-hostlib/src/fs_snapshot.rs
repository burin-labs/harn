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
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use harn_vm::VmValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};
use crate::tools::args::{
    build_dict, dict_arg, optional_string, optional_string_list, require_string, str_value,
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
        register(registry, SNAPSHOT_BUILTIN, "snapshot", snapshot_builtin);
        register(registry, RESTORE_BUILTIN, "restore", restore_builtin);
        register(
            registry,
            LIST_BUILTIN,
            "list_snapshots",
            list_snapshots_builtin,
        );
        register(
            registry,
            DROP_BUILTIN,
            "drop_snapshot",
            drop_snapshot_builtin,
        );
    }
}

fn register(
    registry: &mut BuiltinRegistry,
    name: &'static str,
    method: &'static str,
    runner: fn(&[VmValue]) -> Result<VmValue, HostlibError>,
) {
    let handler: SyncHandler = std::sync::Arc::new(runner);
    registry.register(RegisteredBuiltin {
        name,
        module: "fs",
        method,
        handler,
    });
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

#[derive(Default, Debug)]
struct SessionSnapshots {
    /// Snapshots, in insertion order.
    snapshots: Vec<SnapshotState>,
    /// Bytes currently held in this session's snapshot bundle. We track
    /// this rather than recomputing from `bodies/` so eviction stays
    /// O(snapshots) instead of walking the filesystem on every write.
    byte_count: u64,
}

static SESSIONS: OnceLock<Mutex<BTreeMap<String, SessionSnapshots>>> = OnceLock::new();
static SESSION_BYTE_CAP: OnceLock<Mutex<u64>> = OnceLock::new();

fn sessions() -> &'static Mutex<BTreeMap<String, SessionSnapshots>> {
    SESSIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn session_byte_cap() -> u64 {
    *SESSION_BYTE_CAP
        .get_or_init(|| Mutex::new(DEFAULT_SESSION_BYTE_CAP))
        .lock()
        .expect("fs_snapshot byte cap mutex poisoned")
}

/// Override the per-session byte cap. Returns the previous value.
///
/// Primarily for tests that want to force eviction without writing a
/// gigabyte. Production embedders should leave the default in place
/// unless they have evidence it is too large.
pub fn set_session_byte_cap(bytes: u64) -> u64 {
    let mutex = SESSION_BYTE_CAP.get_or_init(|| Mutex::new(DEFAULT_SESSION_BYTE_CAP));
    let mut guard = mutex.lock().expect("fs_snapshot byte cap mutex poisoned");
    let previous = *guard;
    *guard = bytes.max(1);
    previous
}

/// Test-only helper that clears the in-memory snapshot store. We deliberately
/// leave on-disk state alone — tests that need a clean filesystem use a
/// `TempDir` and reseat the workspace root.
#[doc(hidden)]
pub fn reset_for_test() {
    let mut guard = sessions()
        .lock()
        .expect("fs_snapshot session mutex poisoned");
    guard.clear();
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
            captured_paths.push(path.to_string_lossy().into_owned());
        }
    }
    enforce_byte_cap(bundle, session_id);
    let state = bundle
        .snapshots
        .iter()
        .find(|snap| snap.snapshot_id == scope_id)
        .expect("snapshot just upserted");
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
        let label = path.to_string_lossy().into_owned();
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
            captured_paths: state
                .entries
                .keys()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
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
/// current [`harn_vm::agent_sessions::current_tool_call_id`]. Silently
/// no-ops when no session is active, no tool-call id is set, or no
/// snapshot is registered under that id — this is the zero-cost path
/// for read-only tools and immediate-mode writes outside an active
/// snapshot scope.
pub(crate) fn auto_capture_for_write(builtin: &'static str, path: &Path) {
    let Some(session_id) = active_session_id() else {
        return;
    };
    let Some(snapshot_id) = harn_vm::agent_sessions::current_tool_call_id() else {
        return;
    };
    let mut guard = sessions()
        .lock()
        .expect("fs_snapshot session mutex poisoned");
    let Some(bundle) = guard.get_mut(&session_id) else {
        return;
    };
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
    enforce_byte_cap(bundle, &session_id);
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
            VmValue::List(Rc::new(
                result
                    .captured_paths
                    .into_iter()
                    .map(|path| VmValue::String(Rc::from(path)))
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
            VmValue::List(Rc::new(
                result
                    .restored_paths
                    .into_iter()
                    .map(|path| VmValue::String(Rc::from(path)))
                    .collect(),
            )),
        ),
        (
            "skipped_paths_with_reasons",
            VmValue::List(Rc::new(
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
        VmValue::List(Rc::new(
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
            VmValue::List(Rc::new(
                summary
                    .captured_paths
                    .into_iter()
                    .map(|path| VmValue::String(Rc::from(path)))
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
                format!(
                    "rename {} to {}: {rename_err}; retry: {retry}",
                    tmp.display(),
                    path.display()
                )
            })
        }
    }
}

fn enforce_byte_cap(bundle: &mut SessionSnapshots, session_id: &str) {
    let cap = session_byte_cap();
    while bundle.byte_count > cap && !bundle.snapshots.is_empty() {
        let evicted = bundle.snapshots.remove(0);
        bundle.byte_count = bundle.byte_count.saturating_sub(entry_byte_count(&evicted));
        tracing::info!(
            "fs_snapshot: evicting snapshot `{}` from session `{session_id}` (over byte cap {cap})",
            evicted.snapshot_id
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
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;

    /// Tests in this module mutate process-wide snapshot state and the
    /// thread-local session/tool-call stacks. Serialize them so reset
    /// calls from one test don't race with another's in-flight setup.
    fn test_guard() -> MutexGuard<'static, ()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn enter_session(id: &'static str) -> harn_vm::agent_sessions::CurrentSessionGuard {
        harn_vm::agent_sessions::reset_session_store();
        harn_vm::agent_sessions::open_or_create(Some(id.to_string()));
        harn_vm::agent_sessions::enter_current_session(id)
    }

    #[test]
    fn explicit_snapshot_then_restore_round_trips_file_bytes() {
        let _guard = test_guard();
        reset_for_test();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("note.txt");
        stdfs::write(&file, b"v1").unwrap();
        let _session = enter_session("snap-roundtrip");

        let result = snapshot(
            "snap-roundtrip",
            "tc-1",
            &[file.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();
        assert_eq!(result.snapshot_id, "tc-1");
        assert_eq!(result.captured_paths.len(), 1);
        assert_eq!(result.byte_count, 2);

        stdfs::write(&file, b"clobbered").unwrap();
        let restored = restore("snap-roundtrip", "tc-1", &[]).unwrap();
        assert_eq!(restored.restored_paths.len(), 1);
        assert!(restored.skipped_paths_with_reasons.is_empty());
        assert_eq!(stdfs::read(&file).unwrap(), b"v1");
    }

    #[test]
    fn restore_reinstates_deleted_file() {
        let _guard = test_guard();
        reset_for_test();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("doomed.txt");
        stdfs::write(&file, b"alive").unwrap();
        let _session = enter_session("snap-reinstate");

        snapshot(
            "snap-reinstate",
            "tc-2",
            &[file.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();
        stdfs::remove_file(&file).unwrap();
        assert!(!file.exists());
        let restored = restore("snap-reinstate", "tc-2", &[]).unwrap();
        assert_eq!(restored.restored_paths.len(), 1);
        assert_eq!(stdfs::read(&file).unwrap(), b"alive");
    }

    #[test]
    fn absent_snapshot_means_restore_deletes_paths_created_during_the_call() {
        let _guard = test_guard();
        reset_for_test();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("new.txt");
        assert!(!file.exists());
        let _session = enter_session("snap-absent");

        snapshot(
            "snap-absent",
            "tc-3",
            &[file.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();
        stdfs::write(&file, b"created during call").unwrap();
        let restored = restore("snap-absent", "tc-3", &[]).unwrap();
        assert_eq!(restored.restored_paths.len(), 1);
        assert!(
            !file.exists(),
            "restore must delete files that the snapshot saw as absent"
        );
    }

    #[test]
    fn list_and_drop_round_trip_through_metadata() {
        let _guard = test_guard();
        reset_for_test();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("listed.txt");
        stdfs::write(&file, b"abc").unwrap();
        let _session = enter_session("snap-list");

        snapshot(
            "snap-list",
            "tc-4",
            &[file.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();
        let summaries = list_snapshots("snap-list").unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].snapshot_id, "tc-4");
        assert_eq!(summaries[0].byte_count, 3);

        let dropped = drop_snapshot("snap-list", "tc-4").unwrap();
        assert!(dropped.dropped);
        assert!(list_snapshots("snap-list").unwrap().is_empty());

        let again = drop_snapshot("snap-list", "tc-4").unwrap();
        assert!(!again.dropped, "second drop must be idempotent");
    }

    #[test]
    fn auto_capture_records_pre_image_keyed_by_current_tool_call_id() {
        let _guard = test_guard();
        reset_for_test();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("auto.txt");
        stdfs::write(&file, b"pre").unwrap();
        let _session = enter_session("snap-auto");
        let _tool = harn_vm::agent_sessions::enter_current_tool_call("tc-auto");

        snapshot("snap-auto", "tc-auto", &[], Some(dir.path())).unwrap();
        // Pretend a mutating tool fired:
        auto_capture_for_write("hostlib_tools_write_file", &file);
        stdfs::write(&file, b"post").unwrap();

        let restored = restore("snap-auto", "tc-auto", &[]).unwrap();
        assert_eq!(restored.restored_paths.len(), 1);
        assert_eq!(stdfs::read(&file).unwrap(), b"pre");
    }

    #[test]
    fn byte_cap_evicts_oldest_snapshot_when_exceeded() {
        let _guard = test_guard();
        reset_for_test();
        let prev_cap = set_session_byte_cap(8);
        let dir = TempDir::new().unwrap();
        let _session = enter_session("snap-evict");

        let mk = |name: &str| {
            let path = dir.path().join(name);
            stdfs::write(&path, b"12345").unwrap();
            path
        };

        let a = mk("a.txt");
        snapshot(
            "snap-evict",
            "tc-a",
            &[a.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();
        let b = mk("b.txt");
        snapshot(
            "snap-evict",
            "tc-b",
            &[b.to_string_lossy().into_owned()],
            Some(dir.path()),
        )
        .unwrap();
        let snapshots = list_snapshots("snap-evict").unwrap();
        let ids: Vec<&str> = snapshots
            .iter()
            .map(|summary| summary.snapshot_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["tc-b"],
            "older snapshot must be evicted when the per-session byte cap is exceeded"
        );

        set_session_byte_cap(prev_cap);
    }
}
