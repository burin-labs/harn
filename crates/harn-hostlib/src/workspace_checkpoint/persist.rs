//! The session's checkpoint index on disk, beside its blobs, so a restarted
//! host can plan and restore against boundaries an earlier process captured.
//!
//! The index is rewritten after every change to boundaries, calls, or the
//! revision. When a write fails the index is removed: a resumed session must
//! never plan from a history that is missing calls.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs as stdfs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use harn_vm::tool_annotations::SideEffectLevel;
use serde::{Deserialize, Serialize};

use super::{
    find_session, lock, resolve_root, session_dir, sessions, AttributedChange, Boundary,
    CallRecord, ChangeWriter, Outcome, Session, StoredFile, WriterPolicy,
};
use crate::error::HostlibError;
use crate::fs::atomic_write;

const INDEX_FILE: &str = "index.json";
const SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Index {
    schema: u32,
    root: PathBuf,
    writer_policy: WriterPolicy,
    read_parent_ignores: bool,
    revision: u64,
    boundaries: Vec<IndexBoundary>,
    calls: Vec<IndexCall>,
}

#[derive(Serialize, Deserialize)]
struct IndexBoundary {
    id: String,
    tree: String,
    stored: BTreeMap<String, (String, bool)>,
    absent_tracked: BTreeSet<String>,
    unrestorable: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct IndexCall {
    call_id: String,
    invocation: u64,
    boundary_idx: usize,
    side_effect: Option<SideEffectLevel>,
    scan_errors: usize,
    changes: Vec<IndexChange>,
}

#[derive(Serialize, Deserialize)]
struct IndexChange {
    path: String,
    kind: String,
    post_hash: Option<String>,
    incomplete: bool,
}

fn index_path(root: &Path, session_id: &str) -> PathBuf {
    session_dir(root, session_id).join(INDEX_FILE)
}

/// Record `state` as `session_id`'s index. Call with the session lock held, so
/// writes land in revision order.
pub(super) fn save(session_id: &str, state: &Session) {
    let path = index_path(&state.root, session_id);
    let index = Index {
        schema: SCHEMA,
        root: state.root.clone(),
        writer_policy: state.writer_policy,
        read_parent_ignores: state.read_parent_ignores,
        revision: state.revision,
        boundaries: state
            .boundaries
            .iter()
            .map(|b| IndexBoundary {
                id: b.id.clone(),
                tree: b.tree.clone(),
                stored: b
                    .stored
                    .iter()
                    .map(|(path, file)| (path.clone(), (file.hash.clone(), file.exec)))
                    .collect(),
                absent_tracked: b.absent_tracked.clone(),
                unrestorable: b.unrestorable.clone(),
            })
            .collect(),
        calls: state
            .calls
            .iter()
            .map(|call| IndexCall {
                call_id: call.call_id.clone(),
                invocation: call.invocation,
                boundary_idx: call.boundary_idx,
                side_effect: call.side_effect,
                scan_errors: call.scan_errors,
                changes: call
                    .changes
                    .iter()
                    .map(|change| IndexChange {
                        path: change.path.clone(),
                        kind: change.kind.to_string(),
                        post_hash: change.post_hash.clone(),
                        incomplete: change.incomplete,
                    })
                    .collect(),
            })
            .collect(),
    };
    let written = serde_json::to_vec(&index)
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            if let Some(parent) = path.parent() {
                stdfs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            atomic_write(&path, &bytes)
        });
    if written.is_err() {
        let _ = stdfs::remove_file(&path);
    }
}

/// Release `session_id`'s in-memory history, keeping the recorded index for a
/// later [`resume`]. Refused while a call is in flight, since its window would
/// be lost. Returns whether a session was released.
pub fn unload(session_id: &str) -> bool {
    let mut registry = sessions()
        .lock()
        .expect("workspace checkpoint registry poisoned");
    let idle = registry
        .get(session_id)
        .is_some_and(|session| lock(session).pending.is_empty());
    idle && registry.remove(session_id).is_some()
}

/// What [`resume`] found.
#[derive(Debug, Serialize)]
pub struct ResumeReceipt {
    /// Always `true`.
    pub available: bool,
    /// False when the session was already loaded in this process.
    pub loaded: bool,
    /// Boundaries the session can plan against.
    pub boundaries: usize,
    /// Their ids, oldest first.
    pub boundary_ids: Vec<String>,
    /// Attributed calls the session holds.
    pub calls: usize,
}

/// Load `session_id`'s checkpoint history that an earlier process recorded
/// under `root`. A session already in memory is left as it is.
pub fn resume(session_id: &str, root: &Path) -> Result<Outcome<ResumeReceipt>, HostlibError> {
    const BUILTIN: &str = "hostlib_fs_checkpoint_resume";
    crate::fs::paths::validate_session_id(BUILTIN, session_id)?;
    let root = resolve_root(Some(root));
    if let Some(session) = find_session(session_id) {
        let state = lock(&session);
        if state.root != root {
            return Ok(super::unavailable(
                "workspace_root_changed",
                "the checkpoint belongs to a different workspace root",
            ));
        }
        return Ok(Outcome::Ready(ResumeReceipt {
            available: true,
            loaded: false,
            boundaries: state.boundaries.len(),
            boundary_ids: state.boundaries.iter().map(|b| b.id.clone()).collect(),
            calls: state.calls.len(),
        }));
    }
    let bytes = match stdfs::read(index_path(&root, session_id)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(super::no_checkpoint(session_id));
        }
        Err(error) => {
            return Err(super::backend(BUILTIN)(format!(
                "read checkpoint index: {error}"
            )))
        }
    };
    let index: Index = serde_json::from_slice(&bytes)
        .map_err(|error| super::backend(BUILTIN)(format!("parse checkpoint index: {error}")))?;
    if index.schema != SCHEMA || index.root != root {
        return Ok(super::unavailable(
            "workspace_root_changed",
            "the recorded checkpoint does not belong to this workspace root",
        ));
    }
    let calls = index
        .calls
        .into_iter()
        .map(|call| {
            let changes = call
                .changes
                .into_iter()
                .map(|change| {
                    let kind = match change.kind.as_str() {
                        "created" => "created",
                        "modified" => "modified",
                        "deleted" => "deleted",
                        other => return Err(format!("unknown change kind `{other}`")),
                    };
                    Ok(AttributedChange {
                        path: change.path,
                        kind,
                        writer: ChangeWriter::Unknown,
                        post_hash: change.post_hash,
                        incomplete: change.incomplete,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(CallRecord {
                call_id: call.call_id,
                invocation: call.invocation,
                boundary_idx: call.boundary_idx,
                side_effect: call.side_effect,
                scan_errors: call.scan_errors,
                changes,
            })
        })
        .collect::<Result<Vec<_>, String>>()
        .map_err(super::backend(BUILTIN))?;
    let boundaries: Vec<Boundary> = index
        .boundaries
        .into_iter()
        .map(|b| Boundary {
            id: b.id,
            tree: b.tree,
            stored: b
                .stored
                .into_iter()
                .map(|(path, (hash, exec))| (path, StoredFile { hash, exec }))
                .collect(),
            absent_tracked: b.absent_tracked,
            unrestorable: b.unrestorable,
        })
        .collect();
    if calls
        .iter()
        .any(|call| call.boundary_idx >= boundaries.len())
    {
        return Err(super::backend(BUILTIN)(
            "checkpoint index names a boundary it does not hold".to_string(),
        ));
    }
    let boundary_ids: Vec<String> = boundaries.iter().map(|b| b.id.clone()).collect();
    let call_count = calls.len();
    let session = Session {
        blob_dir: super::blob_dir(&root, session_id),
        root,
        writer_policy: index.writer_policy,
        read_parent_ignores: index.read_parent_ignores,
        boundaries,
        calls,
        pending: HashMap::new(),
        revision: index.revision,
    };
    let mut registry = sessions()
        .lock()
        .expect("workspace checkpoint registry poisoned");
    // Another caller may have resumed or captured meanwhile; keep theirs.
    let mut loaded = false;
    registry.entry(session_id.to_string()).or_insert_with(|| {
        loaded = true;
        Arc::new(Mutex::new(session))
    });
    Ok(Outcome::Ready(ResumeReceipt {
        available: true,
        loaded,
        boundaries: boundary_ids.len(),
        boundary_ids,
        calls: call_count,
    }))
}
