//! Session-scoped staged filesystem mode.
//!
//! `hostlib_fs_set_mode({session_id, mode: "staged"})` makes hostlib file
//! mutations land in a durable per-session overlay under
//! `.harn/state/staged/<session_id>/`. Reads made by the same session consult
//! that overlay first, so agent loops see their own pending writes without
//! touching the working tree until `hostlib_fs_commit_staged`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs as stdfs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use harn_vm::agent_events::AgentEvent;
use harn_vm::VmValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_string, optional_string_list, require_string,
    str_value,
};

const SET_MODE_BUILTIN: &str = "hostlib_fs_set_mode";
const STATUS_BUILTIN: &str = "hostlib_fs_staged_status";
const COMMIT_BUILTIN: &str = "hostlib_fs_commit_staged";
const DISCARD_BUILTIN: &str = "hostlib_fs_discard_staged";
const SAFE_TEXT_PATCH_BUILTIN: &str = "hostlib_fs_safe_text_patch";
const READ_TEXT_BUILTIN: &str = "hostlib_fs_read_text";

const MANIFEST_VERSION: u32 = 1;
const STATE_REL: &[&str] = &[".harn", "state", "staged"];

/// Hostlib filesystem capability handle.
#[derive(Default)]
pub struct FsCapability;

impl HostlibCapability for FsCapability {
    fn module_name(&self) -> &'static str {
        "fs"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        register(registry, SET_MODE_BUILTIN, "set_mode", set_mode_builtin);
        register(
            registry,
            STATUS_BUILTIN,
            "staged_status",
            staged_status_builtin,
        );
        register(
            registry,
            COMMIT_BUILTIN,
            "commit_staged",
            commit_staged_builtin,
        );
        register(
            registry,
            DISCARD_BUILTIN,
            "discard_staged",
            discard_staged_builtin,
        );
        register(
            registry,
            SAFE_TEXT_PATCH_BUILTIN,
            "safe_text_patch",
            safe_text_patch_builtin,
        );
        register(registry, READ_TEXT_BUILTIN, "read_text", read_text_builtin);
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

/// Filesystem mode for one hostlib session.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FsMode {
    /// Mutations apply to the working tree immediately.
    Immediate,
    /// Mutations are recorded in the staging layer until committed.
    Staged,
}

impl FsMode {
    fn parse(builtin: &'static str, raw: &str) -> Result<Self, HostlibError> {
        match raw {
            "immediate" => Ok(Self::Immediate),
            "staged" => Ok(Self::Staged),
            other => Err(HostlibError::InvalidParameter {
                builtin,
                param: "mode",
                message: format!("expected \"immediate\" or \"staged\", got `{other}`"),
            }),
        }
    }

    /// Wire string used by hostlib schemas.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::Staged => "staged",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    session_id: String,
    mode: FsMode,
    root: String,
    entries: BTreeMap<String, StagedEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StagedEntry {
    Write {
        body_hash: String,
        len: u64,
        created_at_ms: i64,
    },
    Delete {
        recursive: bool,
        created_at_ms: i64,
    },
}

impl StagedEntry {
    fn created_at_ms(&self) -> i64 {
        match self {
            Self::Write { created_at_ms, .. } | Self::Delete { created_at_ms, .. } => {
                *created_at_ms
            }
        }
    }

    fn body_len(&self) -> u64 {
        match self {
            Self::Write { len, .. } => *len,
            Self::Delete { .. } => 0,
        }
    }
}

#[derive(Clone, Debug)]
struct SessionState {
    session_id: String,
    mode: FsMode,
    root: PathBuf,
    entries: BTreeMap<PathBuf, StagedEntry>,
}

#[derive(Clone, Debug)]
pub(crate) struct WriteOutcome {
    pub(crate) created: bool,
    pub(crate) bytes_written: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct OverlayDirEntry {
    pub(crate) name: String,
    pub(crate) is_dir: bool,
    pub(crate) is_symlink: bool,
    pub(crate) size: u64,
}

/// Summary of staged filesystem changes for one session.
#[derive(Clone, Debug)]
pub struct StagedStatus {
    /// Pending path changes, sorted by path.
    pub pending_writes: Vec<PendingWrite>,
    /// Bytes stored in staged write bodies.
    pub total_bytes_pending: u64,
    /// Age in milliseconds of the oldest pending change, or 0 when empty.
    pub oldest_pending_age_ms: i64,
}

#[derive(Clone, Debug)]
/// One pending staged filesystem change.
pub struct PendingWrite {
    /// Absolute path affected by this staged change.
    pub path: String,
    /// Change kind (`write`, `delete`, or reserved future `move`).
    pub kind: &'static str,
    /// Bytes the final staged view adds at this path.
    pub bytes_added: u64,
    /// Bytes the final staged view removes at this path.
    pub bytes_removed: u64,
}

/// Result returned after changing a session's filesystem mode.
#[derive(Clone, Debug)]
pub struct SetModeResult {
    /// Mode active before the change.
    pub previous_mode: FsMode,
}

/// Result returned after applying staged changes to disk.
#[derive(Clone, Debug)]
pub struct CommitResult {
    /// Paths successfully applied to disk.
    pub committed_paths: Vec<String>,
    /// Paths that failed to apply, with human-readable reasons.
    pub failed_paths_with_reasons: Vec<(String, String)>,
}

/// Result returned after dropping staged changes.
#[derive(Clone, Debug)]
pub struct DiscardResult {
    /// Paths whose staged entries were removed.
    pub discarded_paths: Vec<String>,
}

static SESSIONS: OnceLock<Mutex<BTreeMap<String, SessionState>>> = OnceLock::new();

fn sessions() -> &'static Mutex<BTreeMap<String, SessionState>> {
    SESSIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Remember the workspace root associated with a live session.
///
/// ACP calls this when a prompt starts so Harn code can call
/// `hostlib_fs_set_mode({session_id, mode})` without also passing a root.
pub fn configure_session_root(session_id: &str, root: &Path) {
    if session_id.trim().is_empty() {
        return;
    }
    let root = normalize_logical(root);
    let mut guard = sessions()
        .lock()
        .expect("hostlib fs session mutex poisoned");
    match guard.get_mut(session_id) {
        Some(state) if state.entries.is_empty() => {
            state.root = root;
        }
        Some(_) => {}
        None => {
            let state = load_state(session_id, Some(root.clone())).unwrap_or(SessionState {
                session_id: session_id.to_string(),
                mode: FsMode::Immediate,
                root,
                entries: BTreeMap::new(),
            });
            guard.insert(session_id.to_string(), state);
        }
    }
}

/// Set a session's filesystem mode.
pub fn set_mode(
    session_id: &str,
    mode: FsMode,
    root: Option<&Path>,
) -> Result<SetModeResult, HostlibError> {
    validate_session_id(SET_MODE_BUILTIN, session_id)?;
    let mut guard = sessions()
        .lock()
        .expect("hostlib fs session mutex poisoned");
    let mut state = state_for_locked(&mut guard, session_id, root.map(normalize_logical))?;
    let previous_mode = state.mode;
    state.mode = mode;
    persist_state(&state, "set_mode", None).map_err(|err| HostlibError::Backend {
        builtin: SET_MODE_BUILTIN,
        message: err,
    })?;
    guard.insert(session_id.to_string(), state);
    Ok(SetModeResult { previous_mode })
}

/// Return the staged status for a session.
pub fn staged_status(session_id: &str) -> Result<StagedStatus, HostlibError> {
    validate_session_id(STATUS_BUILTIN, session_id)?;
    let mut guard = sessions()
        .lock()
        .expect("hostlib fs session mutex poisoned");
    let state = state_for_locked(&mut guard, session_id, None)?;
    let status = status_from_state(&state);
    guard.insert(session_id.to_string(), state);
    Ok(status)
}

/// Commit staged changes for all paths or for a filtered path list.
pub fn commit_staged(session_id: &str, paths: &[String]) -> Result<CommitResult, HostlibError> {
    validate_session_id(COMMIT_BUILTIN, session_id)?;
    let mut guard = sessions()
        .lock()
        .expect("hostlib fs session mutex poisoned");
    let mut state = state_for_locked(&mut guard, session_id, None)?;
    let selected = selected_paths(&state, paths);
    let mut committed_paths = Vec::new();
    let mut failed_paths_with_reasons = Vec::new();

    for path in selected {
        let Some(entry) = state.entries.get(&path).cloned() else {
            continue;
        };
        let path_label = path.to_string_lossy().into_owned();
        match commit_entry(&state, &path, &entry) {
            Ok(()) => {
                state.entries.remove(&path);
                committed_paths.push(path_label);
            }
            Err(reason) => failed_paths_with_reasons.push((path_label, reason)),
        }
    }

    persist_state(&state, "commit_staged", None).map_err(|err| HostlibError::Backend {
        builtin: COMMIT_BUILTIN,
        message: err,
    })?;
    emit_staged_update(&state);
    guard.insert(session_id.to_string(), state);
    Ok(CommitResult {
        committed_paths,
        failed_paths_with_reasons,
    })
}

/// Discard staged changes for all paths or for a filtered path list.
pub fn discard_staged(session_id: &str, paths: &[String]) -> Result<DiscardResult, HostlibError> {
    validate_session_id(DISCARD_BUILTIN, session_id)?;
    let mut guard = sessions()
        .lock()
        .expect("hostlib fs session mutex poisoned");
    let mut state = state_for_locked(&mut guard, session_id, None)?;
    let selected = selected_paths(&state, paths);
    let mut discarded_paths = Vec::new();
    for path in selected {
        if state.entries.remove(&path).is_some() {
            discarded_paths.push(path.to_string_lossy().into_owned());
        }
    }
    persist_state(&state, "discard_staged", None).map_err(|err| HostlibError::Backend {
        builtin: DISCARD_BUILTIN,
        message: err,
    })?;
    emit_staged_update(&state);
    guard.insert(session_id.to_string(), state);
    Ok(DiscardResult { discarded_paths })
}

pub(crate) fn read(
    path: &Path,
    explicit_session_id: Option<&str>,
) -> Option<std::io::Result<Vec<u8>>> {
    let session_id = active_session_id(explicit_session_id)?;
    let mut guard = sessions()
        .lock()
        .expect("hostlib fs session mutex poisoned");
    let state = state_for_locked(&mut guard, &session_id, None).ok()?;
    let result = if state.mode == FsMode::Staged {
        overlay_read(&state, path)
    } else {
        None
    };
    guard.insert(session_id, state);
    result
}

pub(crate) fn read_to_string(
    path: &Path,
    explicit_session_id: Option<&str>,
) -> Option<std::io::Result<String>> {
    read(path, explicit_session_id).map(|result| {
        result.and_then(|bytes| {
            String::from_utf8(bytes).map_err(|err| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string())
            })
        })
    })
}

pub(crate) fn read_dir(
    path: &Path,
    explicit_session_id: Option<&str>,
) -> Option<std::io::Result<Vec<OverlayDirEntry>>> {
    let session_id = active_session_id(explicit_session_id)?;
    let mut guard = sessions()
        .lock()
        .expect("hostlib fs session mutex poisoned");
    let state = state_for_locked(&mut guard, &session_id, None).ok()?;
    let result = if state.mode == FsMode::Staged {
        Some(overlay_read_dir(&state, path))
    } else {
        None
    };
    guard.insert(session_id, state);
    result
}

pub(crate) fn stage_write_or_none(
    builtin: &'static str,
    path: &Path,
    bytes: &[u8],
    create_parents: bool,
    overwrite: bool,
    explicit_session_id: Option<&str>,
) -> Result<Option<WriteOutcome>, HostlibError> {
    let Some(session_id) = active_session_id(explicit_session_id) else {
        return Ok(None);
    };
    let mut guard = sessions()
        .lock()
        .expect("hostlib fs session mutex poisoned");
    let mut state = state_for_locked(&mut guard, &session_id, None)?;
    if state.mode != FsMode::Staged {
        guard.insert(session_id, state);
        return Ok(None);
    }

    let key = normalize_logical(path);
    let existed = overlay_exists(&state, &key);
    if existed && !overwrite {
        guard.insert(session_id, state);
        return Err(HostlibError::Backend {
            builtin,
            message: format!("`{}` exists and overwrite=false", key.display()),
        });
    }
    if !create_parents && !parent_exists(&state, &key) {
        guard.insert(session_id, state);
        return Err(HostlibError::Backend {
            builtin,
            message: format!("parent directory for `{}` does not exist", key.display()),
        });
    }

    let hash = write_body(&state, bytes).map_err(|err| HostlibError::Backend {
        builtin,
        message: err,
    })?;
    state.entries.insert(
        key.clone(),
        StagedEntry::Write {
            body_hash: hash,
            len: bytes.len() as u64,
            created_at_ms: now_ms(),
        },
    );
    persist_state(&state, "write", Some(&key)).map_err(|err| HostlibError::Backend {
        builtin,
        message: err,
    })?;
    emit_staged_update(&state);
    guard.insert(session_id, state);
    Ok(Some(WriteOutcome {
        created: !existed,
        bytes_written: bytes.len(),
    }))
}

pub(crate) fn stage_delete_or_none(
    builtin: &'static str,
    path: &Path,
    recursive: bool,
    explicit_session_id: Option<&str>,
) -> Result<Option<bool>, HostlibError> {
    let Some(session_id) = active_session_id(explicit_session_id) else {
        return Ok(None);
    };
    let mut guard = sessions()
        .lock()
        .expect("hostlib fs session mutex poisoned");
    let mut state = state_for_locked(&mut guard, &session_id, None)?;
    if state.mode != FsMode::Staged {
        guard.insert(session_id, state);
        return Ok(None);
    }

    let key = normalize_logical(path);
    let staged_targets = staged_paths_under(&state, &key);
    let disk_exists = key.exists();
    if !disk_exists && staged_targets.is_empty() {
        guard.insert(session_id, state);
        return Ok(Some(false));
    }

    if !disk_exists {
        for staged in staged_targets {
            state.entries.remove(&staged);
        }
    } else {
        validate_delete_shape(builtin, &key, recursive)?;
        for staged in staged_targets {
            state.entries.remove(&staged);
        }
        state.entries.insert(
            key.clone(),
            StagedEntry::Delete {
                recursive,
                created_at_ms: now_ms(),
            },
        );
    }
    persist_state(&state, "delete", Some(&key)).map_err(|err| HostlibError::Backend {
        builtin,
        message: err,
    })?;
    emit_staged_update(&state);
    guard.insert(session_id, state);
    Ok(Some(true))
}

/// Outcome of one [`safe_text_patch`] call. `applied` says whether the
/// on-disk (or staged-overlay) bytes changed; `result` carries the
/// structured discriminant used by the wire/JSON shape.
#[derive(Clone, Debug)]
pub struct SafeTextPatchOutcome {
    /// Discriminant: `"applied"`, `"stale_base"`, or `"no_op"`.
    pub result: SafeTextPatchResult,
    /// `sha256:HEX` of the pre-image (overlay-aware) the call observed.
    pub current_hash: String,
    /// `sha256:HEX` of the requested post-image.
    pub after_hash: String,
    /// `true` when the file did not exist before the call.
    pub created: bool,
    /// Bytes written; `0` on `stale_base` or `no_op`.
    pub bytes_written: usize,
}

/// Discriminant for a [`safe_text_patch`] outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeTextPatchResult {
    /// Pre-image hash matched (or no expected hash supplied) and the
    /// post-image differs from the pre-image — bytes were written.
    Applied,
    /// `expected_hash` did not match the observed pre-image hash; no
    /// bytes were written. Callers should re-read and retry.
    StaleBase,
    /// Pre-image hash matched and the post-image equals the pre-image —
    /// skipped the write to avoid spurious timestamps and overlay churn.
    NoOp,
}

impl SafeTextPatchResult {
    fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::StaleBase => "stale_base",
            Self::NoOp => "no_op",
        }
    }
}

/// Atomic compare-and-swap-style text write.
///
/// Reads the current bytes at `path` through the staged-fs overlay (when a
/// session is active) so concurrent agent edits see each other's pending
/// writes. If `expected_hash` is supplied and differs from the observed
/// `sha256:HEX`, returns `SafeTextPatchResult::StaleBase` without
/// mutating any state. On a hash match the post-image is written through
/// the same overlay path, keeping the read and the write atomic with
/// respect to other staged-fs consumers in the same process.
pub fn safe_text_patch(
    path: &Path,
    content: &str,
    expected_hash: Option<&str>,
    session_id: Option<&str>,
    create_parents: bool,
    overwrite: bool,
) -> Result<SafeTextPatchOutcome, HostlibError> {
    let (existing_bytes, existed) = read_existing(SAFE_TEXT_PATCH_BUILTIN, path, session_id)?;
    let current_hash = format!("sha256:{}", hex::encode(Sha256::digest(&existing_bytes)));
    let new_bytes = content.as_bytes();
    let after_hash = format!("sha256:{}", hex::encode(Sha256::digest(new_bytes)));

    if let Some(expected) = expected_hash {
        if expected != current_hash {
            return Ok(SafeTextPatchOutcome {
                result: SafeTextPatchResult::StaleBase,
                current_hash,
                after_hash,
                created: false,
                bytes_written: 0,
            });
        }
    }

    if existed && existing_bytes == new_bytes {
        return Ok(SafeTextPatchOutcome {
            result: SafeTextPatchResult::NoOp,
            current_hash,
            after_hash,
            created: false,
            bytes_written: 0,
        });
    }

    if let Some(outcome) = stage_write_or_none(
        SAFE_TEXT_PATCH_BUILTIN,
        path,
        new_bytes,
        create_parents,
        overwrite,
        session_id,
    )? {
        return Ok(SafeTextPatchOutcome {
            result: SafeTextPatchResult::Applied,
            current_hash,
            after_hash,
            created: outcome.created,
            bytes_written: outcome.bytes_written,
        });
    }

    if existed && !overwrite {
        return Err(HostlibError::Backend {
            builtin: SAFE_TEXT_PATCH_BUILTIN,
            message: format!("`{}` exists and overwrite=false", path.display()),
        });
    }

    crate::fs_snapshot::auto_capture_for_write(SAFE_TEXT_PATCH_BUILTIN, path);
    if create_parents {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                stdfs::create_dir_all(parent).map_err(|err| HostlibError::Backend {
                    builtin: SAFE_TEXT_PATCH_BUILTIN,
                    message: format!("mkdir `{}`: {err}", parent.display()),
                })?;
            }
        }
    }
    stdfs::write(path, new_bytes).map_err(|err| HostlibError::Backend {
        builtin: SAFE_TEXT_PATCH_BUILTIN,
        message: format!("write `{}`: {err}", path.display()),
    })?;

    Ok(SafeTextPatchOutcome {
        result: SafeTextPatchResult::Applied,
        current_hash,
        after_hash,
        created: !existed,
        bytes_written: new_bytes.len(),
    })
}

/// Read the pre-image through the staged-fs overlay (when active),
/// falling back to disk. Returns `(bytes, existed_on_disk_or_overlay)`.
/// `builtin` is the caller's tag — used so backend errors point at the
/// right builtin name in diagnostics.
fn read_existing(
    builtin: &'static str,
    path: &Path,
    session_id: Option<&str>,
) -> Result<(Vec<u8>, bool), HostlibError> {
    if let Some(result) = read(path, session_id) {
        return match result {
            Ok(bytes) => Ok((bytes, true)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok((Vec::new(), false)),
            Err(err) => Err(HostlibError::Backend {
                builtin,
                message: format!("read `{}`: {err}", path.display()),
            }),
        };
    }
    match stdfs::read(path) {
        Ok(bytes) => Ok((bytes, true)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok((Vec::new(), false)),
        Err(err) => Err(HostlibError::Backend {
            builtin,
            message: format!("read `{}`: {err}", path.display()),
        }),
    }
}

fn read_text_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(READ_TEXT_BUILTIN, args)?;
    let dict = raw.as_ref();
    let path_str = require_string(READ_TEXT_BUILTIN, dict, "path")?;
    let session_id = optional_string(READ_TEXT_BUILTIN, dict, "session_id")?;
    let path = Path::new(&path_str);

    let (bytes, existed) = read_existing(READ_TEXT_BUILTIN, path, session_id.as_deref())?;
    let hash = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    let content = match std::str::from_utf8(&bytes) {
        Ok(s) => s.to_string(),
        Err(err) => {
            return Err(HostlibError::Backend {
                builtin: READ_TEXT_BUILTIN,
                message: format!("`{path_str}` is not valid UTF-8: {err}"),
            });
        }
    };
    let bytes_len = bytes.len() as i64;
    Ok(build_dict([
        ("path", str_value(&path_str)),
        ("content", str_value(&content)),
        ("sha256", str_value(&hash)),
        ("size", VmValue::Int(bytes_len)),
        ("exists", VmValue::Bool(existed)),
    ]))
}

fn safe_text_patch_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(SAFE_TEXT_PATCH_BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_str = require_string(SAFE_TEXT_PATCH_BUILTIN, dict, "path")?;
    let content = require_string(SAFE_TEXT_PATCH_BUILTIN, dict, "content")?;
    let expected_hash = optional_string(SAFE_TEXT_PATCH_BUILTIN, dict, "expected_hash")?;
    let session_id = optional_string(SAFE_TEXT_PATCH_BUILTIN, dict, "session_id")?;
    let create_parents = optional_bool(SAFE_TEXT_PATCH_BUILTIN, dict, "create_parents", true)?;
    let overwrite = optional_bool(SAFE_TEXT_PATCH_BUILTIN, dict, "overwrite", true)?;

    let outcome = safe_text_patch(
        Path::new(&path_str),
        &content,
        expected_hash.as_deref(),
        session_id.as_deref(),
        create_parents,
        overwrite,
    )?;

    let entries: Vec<(&'static str, VmValue)> = vec![
        ("path", str_value(&path_str)),
        ("result", str_value(outcome.result.as_str())),
        (
            "applied",
            VmValue::Bool(outcome.result == SafeTextPatchResult::Applied),
        ),
        (
            "stale_base",
            VmValue::Bool(outcome.result == SafeTextPatchResult::StaleBase),
        ),
        ("current_hash", str_value(&outcome.current_hash)),
        ("before_sha256", str_value(&outcome.current_hash)),
        ("after_sha256", str_value(&outcome.after_hash)),
        ("created", VmValue::Bool(outcome.created)),
        ("bytes_written", VmValue::Int(outcome.bytes_written as i64)),
        (
            "expected_hash",
            match expected_hash.as_deref() {
                Some(hash) => str_value(hash),
                None => VmValue::Nil,
            },
        ),
    ];
    Ok(build_dict(entries))
}

fn set_mode_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(SET_MODE_BUILTIN, args)?;
    let dict = raw.as_ref();
    let session_id = require_string(SET_MODE_BUILTIN, dict, "session_id")?;
    let mode = FsMode::parse(
        SET_MODE_BUILTIN,
        &require_string(SET_MODE_BUILTIN, dict, "mode")?,
    )?;
    let root = optional_string(SET_MODE_BUILTIN, dict, "root")?.map(PathBuf::from);
    let result = set_mode(&session_id, mode, root.as_deref())?;
    Ok(build_dict([(
        "previous_mode",
        str_value(result.previous_mode.as_str()),
    )]))
}

fn staged_status_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(STATUS_BUILTIN, args)?;
    let session_id = require_string(STATUS_BUILTIN, raw.as_ref(), "session_id")?;
    Ok(status_to_value(staged_status(&session_id)?))
}

fn commit_staged_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(COMMIT_BUILTIN, args)?;
    let dict = raw.as_ref();
    let session_id = require_string(COMMIT_BUILTIN, dict, "session_id")?;
    let paths = optional_string_list(COMMIT_BUILTIN, dict, "paths")?;
    Ok(commit_result_to_value(commit_staged(&session_id, &paths)?))
}

fn discard_staged_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(DISCARD_BUILTIN, args)?;
    let dict = raw.as_ref();
    let session_id = require_string(DISCARD_BUILTIN, dict, "session_id")?;
    let paths = optional_string_list(DISCARD_BUILTIN, dict, "paths")?;
    Ok(discard_result_to_value(discard_staged(
        &session_id,
        &paths,
    )?))
}

fn state_for_locked(
    guard: &mut BTreeMap<String, SessionState>,
    session_id: &str,
    root: Option<PathBuf>,
) -> Result<SessionState, HostlibError> {
    if let Some(existing) = guard.get(session_id) {
        let mut state = existing.clone();
        if let Some(root) = root {
            if state.entries.is_empty() {
                state.root = root;
            }
        }
        return Ok(state);
    }
    let state = load_state(session_id, root).map_err(|err| HostlibError::Backend {
        builtin: SET_MODE_BUILTIN,
        message: err,
    })?;
    Ok(state)
}

fn load_state(session_id: &str, root: Option<PathBuf>) -> Result<SessionState, String> {
    let root = root.unwrap_or_else(default_root);
    let manifest_path = manifest_path(&root, session_id);
    if manifest_path.exists() {
        let text = stdfs::read_to_string(&manifest_path)
            .map_err(|err| format!("read {}: {err}", manifest_path.display()))?;
        let manifest: Manifest = serde_json::from_str(&text)
            .map_err(|err| format!("parse {}: {err}", manifest_path.display()))?;
        if manifest.version != MANIFEST_VERSION {
            return Err(format!(
                "unsupported staged fs manifest version {} in {}",
                manifest.version,
                manifest_path.display()
            ));
        }
        if manifest.session_id != session_id {
            return Err(format!(
                "staged fs manifest session id mismatch in {}",
                manifest_path.display()
            ));
        }
        return Ok(SessionState {
            session_id: manifest.session_id,
            mode: manifest.mode,
            root: normalize_logical(Path::new(&manifest.root)),
            entries: manifest
                .entries
                .into_iter()
                .map(|(path, entry)| (normalize_logical(Path::new(&path)), entry))
                .collect(),
        });
    }
    Ok(SessionState {
        session_id: session_id.to_string(),
        mode: FsMode::Immediate,
        root,
        entries: BTreeMap::new(),
    })
}

fn persist_state(state: &SessionState, op: &str, path: Option<&Path>) -> Result<(), String> {
    let dir = session_dir(&state.root, &state.session_id);
    stdfs::create_dir_all(dir.join("bodies"))
        .map_err(|err| format!("mkdir {}: {err}", dir.display()))?;
    let manifest = Manifest {
        version: MANIFEST_VERSION,
        session_id: state.session_id.clone(),
        mode: state.mode,
        root: state.root.to_string_lossy().into_owned(),
        entries: state
            .entries
            .iter()
            .map(|(path, entry)| (path.to_string_lossy().into_owned(), entry.clone()))
            .collect(),
    };
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|err| format!("serialize staged manifest: {err}"))?;
    atomic_write(&manifest_path(&state.root, &state.session_id), &bytes)?;
    append_journal(state, op, path)?;
    prune_unreferenced_bodies(state);
    Ok(())
}

fn append_journal(state: &SessionState, op: &str, path: Option<&Path>) -> Result<(), String> {
    let dir = session_dir(&state.root, &state.session_id);
    stdfs::create_dir_all(&dir).map_err(|err| format!("mkdir {}: {err}", dir.display()))?;
    let line = serde_json::to_string(&serde_json::json!({
        "ts_ms": now_ms(),
        "op": op,
        "path": path.map(|path| path.to_string_lossy().into_owned()),
        "pending_count": state.entries.len(),
    }))
    .map_err(|err| format!("serialize staged journal: {err}"))?;
    let mut file = stdfs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("journal.jsonl"))
        .map_err(|err| format!("open staged journal: {err}"))?;
    writeln!(file, "{line}").map_err(|err| format!("write staged journal: {err}"))
}

fn write_body(state: &SessionState, bytes: &[u8]) -> Result<String, String> {
    let hash = hex::encode(Sha256::digest(bytes));
    let path = session_dir(&state.root, &state.session_id)
        .join("bodies")
        .join(&hash);
    if !path.exists() {
        atomic_write(&path, bytes)?;
    }
    Ok(hash)
}

fn read_body(state: &SessionState, hash: &str) -> std::io::Result<Vec<u8>> {
    stdfs::read(
        session_dir(&state.root, &state.session_id)
            .join("bodies")
            .join(hash),
    )
}

fn prune_unreferenced_bodies(state: &SessionState) {
    let live: BTreeSet<String> = state
        .entries
        .values()
        .filter_map(|entry| match entry {
            StagedEntry::Write { body_hash, .. } => Some(body_hash.clone()),
            StagedEntry::Delete { .. } => None,
        })
        .collect();
    let body_dir = session_dir(&state.root, &state.session_id).join("bodies");
    let Ok(entries) = stdfs::read_dir(&body_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !live.contains(&name) {
            let _ = stdfs::remove_file(entry.path());
        }
    }
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
        Err(err) => {
            let _ = stdfs::remove_file(path);
            stdfs::rename(&tmp, path).map_err(|retry| {
                format!(
                    "rename {} to {}: {err}; retry: {retry}",
                    tmp.display(),
                    path.display()
                )
            })
        }
    }
}

fn commit_entry(state: &SessionState, path: &Path, entry: &StagedEntry) -> Result<(), String> {
    match entry {
        StagedEntry::Write { body_hash, .. } => {
            let bytes = read_body(state, body_hash)
                .map_err(|err| format!("read staged body for {}: {err}", path.display()))?;
            atomic_write(path, &bytes)
        }
        StagedEntry::Delete { recursive, .. } => match stdfs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() => {
                if *recursive {
                    stdfs::remove_dir_all(path)
                        .map_err(|err| format!("remove_dir_all {}: {err}", path.display()))
                } else {
                    stdfs::remove_dir(path)
                        .map_err(|err| format!("remove_dir {}: {err}", path.display()))
                }
            }
            Ok(_) => stdfs::remove_file(path)
                .map_err(|err| format!("remove_file {}: {err}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!("stat {}: {err}", path.display())),
        },
    }
}

fn overlay_read(state: &SessionState, path: &Path) -> Option<std::io::Result<Vec<u8>>> {
    let key = normalize_logical(path);
    if let Some(entry) = state.entries.get(&key) {
        return Some(match entry {
            StagedEntry::Write { body_hash, .. } => read_body(state, body_hash),
            StagedEntry::Delete { .. } => Err(not_found(&key)),
        });
    }
    if deleted_ancestor(state, &key) {
        return Some(Err(not_found(&key)));
    }
    None
}

fn overlay_read_dir(state: &SessionState, path: &Path) -> std::io::Result<Vec<OverlayDirEntry>> {
    let dir_key = normalize_logical(path);
    if matches!(state.entries.get(&dir_key), Some(StagedEntry::Write { .. }))
        || deleted_ancestor(state, &dir_key)
        || matches!(
            state.entries.get(&dir_key),
            Some(StagedEntry::Delete { .. })
        )
    {
        return Err(not_found(&dir_key));
    }
    if !path.exists() && !has_staged_descendant(state, &dir_key) {
        return Err(not_found(&dir_key));
    }

    let mut entries: BTreeMap<String, OverlayDirEntry> = BTreeMap::new();
    if path.exists() {
        for entry in stdfs::read_dir(path)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = entry.file_type().ok();
            let metadata = entry.metadata().ok();
            entries.insert(
                name.clone(),
                OverlayDirEntry {
                    name,
                    is_dir: file_type.is_some_and(|ty| ty.is_dir()),
                    is_symlink: file_type.is_some_and(|ty| ty.is_symlink()),
                    size: metadata.map(|m| m.len()).unwrap_or(0),
                },
            );
        }
    }

    for (path, entry) in &state.entries {
        let Some(name) = overlay_child_name(path, &dir_key) else {
            continue;
        };
        match entry {
            StagedEntry::Write { len, .. } => {
                let is_dir = path.parent() != Some(dir_key.as_path());
                entries.insert(
                    name.clone(),
                    OverlayDirEntry {
                        name,
                        is_dir,
                        is_symlink: false,
                        size: if is_dir { 0 } else { *len },
                    },
                );
            }
            StagedEntry::Delete { .. } => {
                if path.parent() == Some(dir_key.as_path()) {
                    entries.remove(&name);
                }
            }
        }
    }

    Ok(entries.into_values().collect())
}

fn overlay_child_name(path: &Path, dir: &Path) -> Option<String> {
    let suffix = path.strip_prefix(dir).ok()?;
    let mut components = suffix.components();
    let first = components.next()?;
    match first {
        Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
        _ => None,
    }
}

fn overlay_exists(state: &SessionState, path: &Path) -> bool {
    if let Some(entry) = state.entries.get(path) {
        return matches!(entry, StagedEntry::Write { .. });
    }
    if deleted_ancestor(state, path) {
        return false;
    }
    if has_staged_descendant(state, path) {
        return true;
    }
    path.exists()
}

fn parent_exists(state: &SessionState, path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return true;
    };
    if parent.as_os_str().is_empty() {
        return true;
    }
    if let Some(entry) = state.entries.get(parent) {
        return !matches!(entry, StagedEntry::Delete { .. });
    }
    if deleted_ancestor(state, parent) {
        return false;
    }
    if has_staged_descendant(state, parent) {
        return true;
    }
    parent.is_dir()
}

fn deleted_ancestor(state: &SessionState, path: &Path) -> bool {
    state.entries.iter().any(|(candidate, entry)| {
        matches!(entry, StagedEntry::Delete { .. })
            && path != candidate.as_path()
            && path.starts_with(candidate)
    })
}

fn has_staged_descendant(state: &SessionState, path: &Path) -> bool {
    state.entries.iter().any(|(candidate, entry)| {
        matches!(entry, StagedEntry::Write { .. })
            && candidate != path
            && candidate.starts_with(path)
    })
}

fn staged_paths_under(state: &SessionState, path: &Path) -> Vec<PathBuf> {
    state
        .entries
        .keys()
        .filter(|candidate| *candidate == path || candidate.starts_with(path))
        .cloned()
        .collect()
}

fn validate_delete_shape(
    builtin: &'static str,
    path: &Path,
    recursive: bool,
) -> Result<(), HostlibError> {
    let Ok(metadata) = stdfs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.is_dir() && !recursive {
        let mut entries = stdfs::read_dir(path).map_err(|err| HostlibError::Backend {
            builtin,
            message: format!("read_dir `{}`: {err}", path.display()),
        })?;
        if entries.next().is_some() {
            return Err(HostlibError::Backend {
                builtin,
                message: format!(
                    "remove_dir `{}` (pass recursive=true to delete non-empty dirs): directory not empty",
                    path.display()
                ),
            });
        }
    }
    Ok(())
}

fn status_from_state(state: &SessionState) -> StagedStatus {
    let now = now_ms();
    let mut pending_writes = Vec::new();
    let mut total_bytes_pending = 0u64;
    let mut oldest = None;
    for (path, entry) in &state.entries {
        total_bytes_pending = total_bytes_pending.saturating_add(entry.body_len());
        oldest = Some(oldest.map_or(entry.created_at_ms(), |old: i64| {
            old.min(entry.created_at_ms())
        }));
        let (kind, bytes_added, bytes_removed) = match entry {
            StagedEntry::Write { len, .. } => ("write", *len, disk_size(path).unwrap_or(0)),
            StagedEntry::Delete { .. } => ("delete", 0, disk_size(path).unwrap_or(0)),
        };
        pending_writes.push(PendingWrite {
            path: path.to_string_lossy().into_owned(),
            kind,
            bytes_added,
            bytes_removed,
        });
    }
    StagedStatus {
        pending_writes,
        total_bytes_pending,
        oldest_pending_age_ms: oldest.map(|old| now.saturating_sub(old)).unwrap_or(0),
    }
}

fn disk_size(path: &Path) -> Option<u64> {
    let metadata = stdfs::symlink_metadata(path).ok()?;
    if metadata.is_file() {
        return Some(metadata.len());
    }
    if metadata.is_dir() {
        let mut total = 0u64;
        for entry in walkdir::WalkDir::new(path)
            .into_iter()
            .filter_map(Result::ok)
        {
            if let Ok(metadata) = entry.metadata() {
                if metadata.is_file() {
                    total = total.saturating_add(metadata.len());
                }
            }
        }
        return Some(total);
    }
    Some(metadata.len())
}

fn selected_paths(state: &SessionState, paths: &[String]) -> Vec<PathBuf> {
    if paths.is_empty() {
        return state.entries.keys().cloned().collect();
    }
    let selected: BTreeSet<PathBuf> = paths
        .iter()
        .map(|path| normalize_logical(Path::new(path)))
        .collect();
    state
        .entries
        .keys()
        .filter(|path| selected.contains(*path))
        .cloned()
        .collect()
}

fn active_session_id(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::to_string)
        .or_else(harn_vm::agent_sessions::current_session_id)
        .filter(|id| !id.trim().is_empty())
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

fn default_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn session_dir(root: &Path, session_id: &str) -> PathBuf {
    let mut dir = root.to_path_buf();
    for component in STATE_REL {
        dir.push(component);
    }
    dir.push(sanitize_component(session_id));
    dir
}

fn manifest_path(root: &Path, session_id: &str) -> PathBuf {
    session_dir(root, session_id).join("manifest.json")
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
        default_root().join(path)
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

fn not_found(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("staged fs: {} is deleted or absent", path.display()),
    )
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn emit_staged_update(state: &SessionState) {
    let status = status_from_state(state);
    harn_vm::agent_events::emit_event(&AgentEvent::StagedWritesPending {
        session_id: state.session_id.clone(),
        pending_count: status.pending_writes.len(),
        total_bytes: status.total_bytes_pending,
    });
}

fn pending_write_to_value(write: PendingWrite) -> VmValue {
    build_dict([
        ("path", str_value(&write.path)),
        ("kind", str_value(write.kind)),
        ("bytes_added", VmValue::Int(write.bytes_added as i64)),
        ("bytes_removed", VmValue::Int(write.bytes_removed as i64)),
    ])
}

fn status_to_value(status: StagedStatus) -> VmValue {
    build_dict([
        (
            "pending_writes",
            VmValue::List(Rc::new(
                status
                    .pending_writes
                    .into_iter()
                    .map(pending_write_to_value)
                    .collect(),
            )),
        ),
        (
            "total_bytes_pending",
            VmValue::Int(status.total_bytes_pending as i64),
        ),
        (
            "oldest_pending_age_ms",
            VmValue::Int(status.oldest_pending_age_ms),
        ),
    ])
}

fn commit_result_to_value(result: CommitResult) -> VmValue {
    build_dict([
        (
            "committed_paths",
            VmValue::List(Rc::new(
                result
                    .committed_paths
                    .into_iter()
                    .map(|path| VmValue::String(Rc::from(path)))
                    .collect(),
            )),
        ),
        (
            "failed_paths_with_reasons",
            VmValue::List(Rc::new(
                result
                    .failed_paths_with_reasons
                    .into_iter()
                    .map(|(path, reason)| {
                        build_dict([("path", str_value(&path)), ("reason", str_value(&reason))])
                    })
                    .collect(),
            )),
        ),
    ])
}

fn discard_result_to_value(result: DiscardResult) -> VmValue {
    build_dict([(
        "discarded_paths",
        VmValue::List(Rc::new(
            result
                .discarded_paths
                .into_iter()
                .map(|path| VmValue::String(Rc::from(path)))
                .collect(),
        )),
    )])
}
