//! Turn-boundary workspace checkpoints with per-call attribution and a
//! restore guard.
//!
//! A host rewinds a session to an earlier user message by forking the
//! conversation (`harness.agent.fork_at`) and, optionally, restoring the files
//! the abandoned turns changed. This module owns the file side:
//!
//! 1. **Capture** at each user-message boundary: a stat index of the
//!    non-ignored tree stays in memory as the attribution baseline. Bytes are
//!    stored only for files that differ from the git index (modified and
//!    untracked). Every other file is recovered from the repository's object
//!    store through a tree written at capture time.
//! 2. **Attribute** per tool or shell call: stat the tree before and after the
//!    call and record each changed path with its post-call content hash.
//! 3. **Plan** a restore to boundary N, limited to paths attributed to the
//!    session after N. Each entry is `restore`, `delete`, or `recreate`.
//! 4. **Guard**: a path whose current content differs from its last attributed
//!    post-call hash is refused, never overwritten.
//! 5. **Receipt**: a typed result of restored, refused, and skipped paths plus
//!    the calls whose effects cannot be undone (network and desktop-control
//!    side-effect classes).
//!
//! A directory that is not a git worktree returns an explicit unavailable
//! result from every method, never an empty success.
//!
//! ## Known limits
//!
//! - A scan cannot identify a writer editing during a call. Strict policy
//!   treats the writer as unknown; legacy policy attributes the change to
//!   the call and refuses later divergence from the call's post-call hash.
//! - A same-size edit inside one filesystem mtime tick is invisible to the stat
//!   diff.
//! - Symlinks, files above [`MAX_STORED_FILE_BYTES`], and tracked files matched
//!   by an ignore rule are outside the index. Dirty symlinks and oversize files
//!   are reported as skipped at restore time.
//! - Checkpoint state is in memory; blobs live under
//!   `.harn/state/checkpoints/<session>/blobs/` and die with the session.

mod anchored_fs;
mod builtins;
mod fork;
mod persist;
mod receipts;
mod scan;
mod seed;
mod tape_effects;
use anchored_fs::{open_anchor, restore_one, Step};
#[cfg(all(test, unix))]
use anchored_fs::{Stage, RESTORE_HOOK};
use builtins::{
    call_begin_builtin, call_end_builtin, capture_builtin, drop_builtin, plan_builtin,
    restore_builtin,
};
pub use fork::{fork_session, ForkReceipt};
pub use persist::{resume, unload, ResumeReceipt};
use receipts::unavailable;
pub use receipts::{
    AttributedChange, CallBeginReceipt, CallEndReceipt, CaptureReceipt, ChangeWriter,
    ConfirmedPath, DropReceipt, IrreversibleEffect, ObservedFileState, Outcome, PlanEntry,
    PlanReceipt, RestoreReceipt, Unavailable, WriterPolicy,
};
#[cfg(all(test, unix))]
use scan::scan_tree_with_exclusions;
use scan::{excluded_paths, is_excluded, scan_tree};
pub use seed::{
    ReplayWorkspace, SavedReplayWorkspace, SeedError, SeedLimits, SeedReceipt, SeedSchema,
};
pub use tape_effects::tape_effect_observer;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs as stdfs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use harn_vm::tool_annotations::SideEffectLevel;
use sha2::{Digest, Sha256};

use crate::error::HostlibError;
use crate::fs::atomic_write;
use crate::fs::paths::{normalize_against, sanitize_component, validate_session_id};
use crate::registry::{BuiltinRegistry, HostlibCapability};
use crate::tools::args::to_agent_path;

const CAPTURE_BUILTIN: &str = "hostlib_fs_checkpoint_capture";
const CALL_BEGIN_BUILTIN: &str = "hostlib_fs_checkpoint_call_begin";
const CALL_END_BUILTIN: &str = "hostlib_fs_checkpoint_call_end";
const PLAN_BUILTIN: &str = "hostlib_fs_checkpoint_plan";
const RESTORE_BUILTIN: &str = "hostlib_fs_checkpoint_restore";
const DROP_BUILTIN: &str = "hostlib_fs_checkpoint_drop";

const ALREADY_AT_BOUNDARY: &str = "already matches the boundary";

const STATE_REL: &[&str] = &[
    ".harn",
    harn_vm::runtime_paths::HOSTLIB_STATE_DIR,
    "checkpoints",
];

/// Largest dirty or untracked file a boundary stores. Larger files are listed
/// as unrestorable instead of copied on every turn.
pub const MAX_STORED_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Hostlib workspace checkpoint capability handle.
#[derive(Default)]
pub struct WorkspaceCheckpointCapability;

impl HostlibCapability for WorkspaceCheckpointCapability {
    fn module_name(&self) -> &'static str {
        // Lives beside the per-call snapshot primitives under `fs/`.
        "fs"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        registry.register_fn("fs", CAPTURE_BUILTIN, "checkpoint_capture", capture_builtin);
        registry.register_fn(
            "fs",
            CALL_BEGIN_BUILTIN,
            "checkpoint_call_begin",
            call_begin_builtin,
        );
        registry.register_fn(
            "fs",
            CALL_END_BUILTIN,
            "checkpoint_call_end",
            call_end_builtin,
        );
        registry.register_fn("fs", PLAN_BUILTIN, "checkpoint_plan", plan_builtin);
        registry.register_writer_fn("fs", RESTORE_BUILTIN, "checkpoint_restore", restore_builtin);
        registry.register_fn("fs", DROP_BUILTIN, "checkpoint_drop", drop_builtin);
    }
}

// ---------------------------------------------------------------------------
// Session state
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
struct StatEntry {
    size: u64,
    mtime_ns: i128,
    ino: u64,
}

type StatIndex = BTreeMap<String, StatEntry>;

struct Scan {
    index: StatIndex,
    errors: usize,
    /// Paths (files or directories, root-relative) whose entries the scan could
    /// not read; anything at or below one is unknown, not absent.
    error_paths: Vec<String>,
    /// True when an error carried no path, so every path is suspect.
    unlocated_error: bool,
    elapsed_ms: u64,
}

#[derive(Clone, Debug)]
struct StoredFile {
    hash: String,
    exec: bool,
}

#[derive(Clone)]
struct Boundary {
    id: String,
    /// Git tree of the index at capture; clean tracked content resolves here.
    tree: String,
    /// Dirty and untracked files, stored by content hash.
    stored: BTreeMap<String, StoredFile>,
    /// Tracked files already missing from disk at capture.
    absent_tracked: BTreeSet<String>,
    /// Dirty files that were not stored, with the reason.
    unrestorable: BTreeMap<String, String>,
}

#[derive(Clone)]
struct CallRecord {
    call_id: String,
    /// The session revision `call_begin` admitted this invocation at.
    invocation: u64,
    boundary_idx: usize,
    side_effect: Option<SideEffectLevel>,
    scan_errors: usize,
    changes: Vec<AttributedChange>,
}

struct PendingCall {
    invocation: u64,
    boundary_idx: usize,
    scan: PendingScan,
}

enum PendingScan {
    Beginning,
    Active(Arc<Scan>),
    Ending,
}

struct Session {
    root: PathBuf,
    writer_policy: WriterPolicy,
    /// Whether ignore files above `root` apply (root is below the git toplevel).
    read_parent_ignores: bool,
    blob_dir: PathBuf,
    boundaries: Vec<Boundary>,
    calls: Vec<CallRecord>,
    pending: HashMap<String, PendingCall>,
    revision: u64,
}

type SharedSession = Arc<Mutex<Session>>;

static SESSIONS: OnceLock<Mutex<BTreeMap<String, SharedSession>>> = OnceLock::new();

#[cfg(all(test, unix))]
type CallEndHook = Box<dyn Fn()>;

#[cfg(all(test, unix))]
thread_local! {
    static CALL_END_HOOK: std::cell::RefCell<Option<CallEndHook>> = const { std::cell::RefCell::new(None) };
    static CALL_BEGIN_HOOK: std::cell::RefCell<Option<CallEndHook>> = const { std::cell::RefCell::new(None) };
}

fn sessions() -> &'static Mutex<BTreeMap<String, SharedSession>> {
    SESSIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn find_session(session_id: &str) -> Option<SharedSession> {
    sessions()
        .lock()
        .expect("workspace checkpoint registry poisoned")
        .get(session_id)
        .cloned()
}

fn lock(session: &SharedSession) -> std::sync::MutexGuard<'_, Session> {
    session
        .lock()
        .expect("workspace checkpoint session poisoned")
}

/// Whether a captured session still belongs to the requested workspace root.
pub fn root_matches(session_id: &str, root: &Path) -> Option<bool> {
    find_session(session_id).map(|session| lock(&session).root == resolve_root(Some(root)))
}

fn backend(builtin: &'static str) -> impl Fn(String) -> HostlibError {
    move |message| HostlibError::Backend { builtin, message }
}

fn advance_revision(state: &mut Session, builtin: &'static str) -> Result<(), HostlibError> {
    state.revision = state
        .revision
        .checked_add(1)
        .ok_or_else(|| backend(builtin)("checkpoint revision exhausted".to_string()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Public operations
// ---------------------------------------------------------------------------

/// Capture a boundary: write a tree of the git index, store dirty and
/// untracked bytes, and take the attribution baseline.
pub fn capture(
    session_id: &str,
    boundary_id: &str,
    root: Option<&Path>,
) -> Result<Outcome<CaptureReceipt>, HostlibError> {
    capture_with_writer_policy(session_id, boundary_id, root, None)
}

/// Capture a boundary with an optional policy fixed for the session lifetime.
pub fn capture_with_writer_policy(
    session_id: &str,
    boundary_id: &str,
    root: Option<&Path>,
    requested_policy: Option<WriterPolicy>,
) -> Result<Outcome<CaptureReceipt>, HostlibError> {
    let fail = backend(CAPTURE_BUILTIN);
    validate_session_id(CAPTURE_BUILTIN, session_id)?;
    require_non_empty(CAPTURE_BUILTIN, "boundary_id", boundary_id)?;
    let root = resolve_root(root);
    let initial_generation = if let Some(session) = find_session(session_id) {
        let state = lock(&session);
        if state.root != root {
            return Err(HostlibError::InvalidParameter {
                builtin: CAPTURE_BUILTIN,
                param: "root",
                message: format!(
                    "session `{session_id}` already checkpoints {}",
                    state.root.display()
                ),
            });
        }
        if requested_policy.is_some_and(|policy| policy != state.writer_policy) {
            return Err(HostlibError::InvalidParameter {
                builtin: CAPTURE_BUILTIN,
                param: "writer_policy",
                message: "cannot change the checkpoint session's writer policy".to_string(),
            });
        }
        if let Some(detail) = pending_calls_detail(&state) {
            return Ok(unavailable("calls_in_flight", detail));
        }
        Some((state.revision, state.boundaries.len(), state.writer_policy))
    } else {
        None
    };

    let prefix = match git(&root, &["rev-parse", "--show-prefix"]) {
        Ok(out) => String::from_utf8_lossy(&out).trim().to_string(),
        Err(error) => {
            return Ok(match error {
                GitError::Missing(detail) => unavailable("git_unavailable", detail),
                GitError::Failed(detail) => unavailable("not_a_git_worktree", detail),
            });
        }
    };
    // Ignore files above the repository must not hide a nested worktree.
    let read_parent_ignores = !prefix.is_empty();
    let tree = match git(&root, &["write-tree"]) {
        Ok(out) => String::from_utf8_lossy(&out).trim().to_string(),
        Err(error) => return Ok(unavailable("index_not_writable_as_tree", error.detail())),
    };
    let scan = scan_tree(&root, read_parent_ignores);
    if scan.errors > 0 {
        let paths = if scan.error_paths.is_empty() {
            "unlocated".to_string()
        } else {
            scan.error_paths.join(", ")
        };
        return Ok(unavailable(
            "incomplete_capture",
            format!("workspace scan errors: {}; paths: {paths}", scan.errors),
        ));
    }

    let dirty = nul_list(&git_text(
        &root,
        &["ls-files", "-z", "-m", "-o", "--exclude-standard"],
    )?)?;
    let absent_tracked: BTreeSet<String> = nul_list(&git_text(&root, &["ls-files", "-z", "-d"])?)?
        .into_iter()
        .collect();

    let blob_dir = blob_dir(&root, session_id);
    let mut stored = BTreeMap::new();
    let mut unrestorable = BTreeMap::new();
    let mut stored_bytes = 0u64;
    let excluded = excluded_paths(&root);
    for path in dirty {
        if path.ends_with('/')
            || absent_tracked.contains(&path)
            || is_excluded(&root, &root.join(&path), &excluded)
        {
            continue; // nested repository, already deleted, or runtime-owned
        }
        let full = root.join(&path);
        let Ok(meta) = stdfs::symlink_metadata(&full) else {
            continue;
        };
        if !meta.is_file() {
            unrestorable.insert(path, "not a regular file".to_string());
            continue;
        }
        if meta.len() > MAX_STORED_FILE_BYTES {
            unrestorable.insert(path, "larger than the checkpoint size cap".to_string());
            continue;
        }
        let bytes =
            stdfs::read(&full).map_err(|e| fail(format!("read {}: {e}", full.display())))?;
        let hash = sha256_hex(&bytes);
        store_blob(&blob_dir, &hash, &bytes).map_err(&fail)?;
        stored_bytes += bytes.len() as u64;
        stored.insert(
            path,
            StoredFile {
                hash,
                exec: is_executable(&meta),
            },
        );
    }

    let receipt = CaptureReceipt {
        available: true,
        boundary_id: boundary_id.to_string(),
        writer_policy: initial_generation
            .map(|(_, _, policy)| policy)
            .or(requested_policy)
            .unwrap_or(WriterPolicy::LegacyScan),
        scanned_files: scan.index.len(),
        stored_files: stored.len(),
        stored_bytes,
        unrestorable_files: unrestorable.len(),
        scan_errors: scan.errors,
        scan_ms: scan.elapsed_ms,
    };
    let boundary = Boundary {
        id: boundary_id.to_string(),
        tree,
        stored,
        absent_tracked,
        unrestorable,
    };

    #[cfg(all(test, unix))]
    tests::capture_hook(tests::CaptureStage::BeforePublish);
    let session = sessions()
        .lock()
        .expect("workspace checkpoint registry poisoned")
        .entry(session_id.to_string())
        .or_insert_with(|| {
            Arc::new(Mutex::new(Session {
                root: root.clone(),
                writer_policy: receipt.writer_policy,
                read_parent_ignores,
                blob_dir: blob_dir.clone(),
                boundaries: Vec::new(),
                calls: Vec::new(),
                pending: HashMap::new(),
                revision: 0,
            }))
        })
        .clone();
    #[cfg(all(test, unix))]
    tests::capture_hook(tests::CaptureStage::AfterPublish);
    let mut state = lock(&session);
    if let Some(detail) = pending_calls_detail(&state) {
        return Ok(unavailable("calls_in_flight", detail));
    }
    if receipt.writer_policy != state.writer_policy
        || initial_generation.map_or(!state.boundaries.is_empty(), |generation| {
            generation != (state.revision, state.boundaries.len(), state.writer_policy)
        })
    {
        return Ok(unavailable(
            "concurrent_calls",
            "the session changed while the workspace boundary was being captured",
        ));
    }
    if state.boundaries.iter().any(|b| b.id == boundary_id) {
        return Err(HostlibError::InvalidParameter {
            builtin: CAPTURE_BUILTIN,
            param: "boundary_id",
            message: format!("boundary `{boundary_id}` already exists"),
        });
    }
    if state.root != root {
        return Err(HostlibError::InvalidParameter {
            builtin: CAPTURE_BUILTIN,
            param: "root",
            message: format!(
                "session `{session_id}` already checkpoints {}",
                state.root.display()
            ),
        });
    }
    advance_revision(&mut state, CAPTURE_BUILTIN)?;
    state.boundaries.push(boundary);
    persist::save(session_id, &state);
    Ok(Outcome::Ready(receipt))
}

/// Stat the tree before a tool or shell call.
pub fn call_begin(
    session_id: &str,
    call_id: &str,
) -> Result<Outcome<CallBeginReceipt>, HostlibError> {
    validate_session_id(CALL_BEGIN_BUILTIN, session_id)?;
    require_non_empty(CALL_BEGIN_BUILTIN, "call_id", call_id)?;
    let Some(session) = find_session(session_id) else {
        return Ok(no_checkpoint(session_id));
    };
    let (root, parents) = {
        let mut state = lock(&session);
        if state.pending.contains_key(call_id) {
            return Err(HostlibError::InvalidParameter {
                builtin: CALL_BEGIN_BUILTIN,
                param: "call_id",
                message: format!("checkpoint call `{call_id}` is already in flight"),
            });
        }
        advance_revision(&mut state, CALL_BEGIN_BUILTIN)?;
        let boundary_idx = state.boundaries.len().saturating_sub(1);
        let invocation = state.revision;
        state.pending.insert(
            call_id.to_string(),
            PendingCall {
                invocation,
                boundary_idx,
                scan: PendingScan::Beginning,
            },
        );
        (state.root.clone(), state.read_parent_ignores)
    };
    let scan = scan_tree(&root, parents);
    #[cfg(all(test, unix))]
    CALL_BEGIN_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow().as_ref() {
            hook();
        }
    });
    let receipt = CallBeginReceipt {
        available: true,
        call_id: call_id.to_string(),
        scanned_files: scan.index.len(),
        scan_errors: scan.errors,
        scan_ms: scan.elapsed_ms,
    };
    lock(&session)
        .pending
        .get_mut(call_id)
        .expect("beginning call remains pending")
        .scan = PendingScan::Active(Arc::new(scan));
    Ok(Outcome::Ready(receipt))
}

/// Stat the tree after a call and attribute every changed path to it.
pub fn call_end(
    session_id: &str,
    call_id: &str,
    side_effect: Option<SideEffectLevel>,
) -> Result<Outcome<CallEndReceipt>, HostlibError> {
    validate_session_id(CALL_END_BUILTIN, session_id)?;
    require_non_empty(CALL_END_BUILTIN, "call_id", call_id)?;
    let Some(session) = find_session(session_id) else {
        return Ok(no_checkpoint(session_id));
    };
    let (root, parents, before, (invocation, boundary_idx)) = {
        let mut state = lock(&session);
        let root = state.root.clone();
        let parents = state.read_parent_ignores;
        let Some(pending) = state.pending.get_mut(call_id) else {
            return Err(HostlibError::InvalidParameter {
                builtin: CALL_END_BUILTIN,
                param: "call_id",
                message: format!("no checkpoint_call_begin for `{call_id}`"),
            });
        };
        let before = match &pending.scan {
            PendingScan::Beginning => {
                return Err(HostlibError::InvalidParameter {
                    builtin: CALL_END_BUILTIN,
                    param: "call_id",
                    message: format!("checkpoint call `{call_id}` is still beginning"),
                });
            }
            PendingScan::Ending => {
                return Err(HostlibError::InvalidParameter {
                    builtin: CALL_END_BUILTIN,
                    param: "call_id",
                    message: format!("checkpoint call `{call_id}` is already ending"),
                });
            }
            PendingScan::Active(before) => before.clone(),
        };
        pending.scan = PendingScan::Ending;
        (
            root,
            parents,
            before,
            (pending.invocation, pending.boundary_idx),
        )
    };
    #[cfg(all(test, unix))]
    CALL_END_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow().as_ref() {
            hook();
        }
    });
    let after = scan_tree(&root, parents);
    let mut changes = Vec::new();
    let mut push = |path: &str, kind: &'static str, post_hash: Option<String>, unreadable: bool| {
        changes.push(AttributedChange {
            path: path.to_string(),
            kind,
            writer: ChangeWriter::Unknown,
            post_hash,
            incomplete: unreadable || scan_blind_to(&before, path) || scan_blind_to(&after, path),
        });
    };
    for (path, entry) in &after.index {
        let kind = match before.index.get(path) {
            None => "created",
            Some(prior) if prior != entry => "modified",
            Some(_) => continue,
        };
        // A file replaced mid-hash reads as gone; record the deletion. A file
        // that cannot be hashed has an unknown post state.
        match sha256_file(&root.join(path)) {
            Ok(Some(hash)) => push(path, kind, Some(hash), false),
            Ok(None) => push(path, "deleted", None, false),
            Err(_) => push(path, kind, None, true),
        }
    }
    for path in before
        .index
        .keys()
        .filter(|p| !after.index.contains_key(*p))
    {
        push(path, "deleted", None, false);
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));

    let mut state = lock(&session);
    state.pending.remove(call_id);
    let scan_errors = after.errors + before.errors;
    state.calls.push(CallRecord {
        call_id: call_id.to_string(),
        invocation,
        boundary_idx,
        side_effect,
        scan_errors,
        changes: changes.clone(),
    });
    persist::save(session_id, &state);
    Ok(Outcome::Ready(CallEndReceipt {
        available: true,
        call_id: call_id.to_string(),
        changes,
        scan_errors,
    }))
}

/// Compute the restore plan to a boundary without touching the workspace.
pub fn plan(session_id: &str, boundary_id: &str) -> Result<Outcome<PlanReceipt>, HostlibError> {
    validate_session_id(PLAN_BUILTIN, session_id)?;
    require_non_empty(PLAN_BUILTIN, "boundary_id", boundary_id)?;
    let Some(session) = find_session(session_id) else {
        return Ok(no_checkpoint(session_id));
    };
    let state = lock(&session);
    if let Some(detail) = pending_calls_detail(&state) {
        return Ok(unavailable("calls_in_flight", detail));
    }
    if let Some(detail) = incomplete_attribution(&state, boundary_id) {
        return Ok(unavailable("incomplete_attribution", detail));
    }
    let Some(planned) = compute_plan(&state, boundary_id, PLAN_BUILTIN, &BTreeMap::new())? else {
        return Ok(unknown_boundary(boundary_id));
    };
    Ok(Outcome::Ready(PlanReceipt {
        available: true,
        session_id: session_id.to_string(),
        boundary_id: boundary_id.to_string(),
        entries: planned.actions.into_iter().map(|a| a.entry).collect(),
        irreversible: planned.irreversible,
    }))
}

/// Restore the workspace to a boundary. Only paths attributed to the session
/// after the boundary are touched, and a path that changed since its last
/// attributed write is refused.
pub fn restore(
    session_id: &str,
    boundary_id: &str,
    only_paths: Option<&[String]>,
) -> Result<Outcome<RestoreReceipt>, HostlibError> {
    restore_with_confirmations(session_id, boundary_id, only_paths, &[])
}

/// Restore with host-provided confirmations bound to exact observed file states.
pub fn restore_with_confirmations(
    session_id: &str,
    boundary_id: &str,
    only_paths: Option<&[String]>,
    confirmed_paths: &[ConfirmedPath],
) -> Result<Outcome<RestoreReceipt>, HostlibError> {
    validate_session_id(RESTORE_BUILTIN, session_id)?;
    require_non_empty(RESTORE_BUILTIN, "boundary_id", boundary_id)?;
    if only_paths.is_some_and(<[String]>::is_empty) {
        return Err(HostlibError::InvalidParameter {
            builtin: RESTORE_BUILTIN,
            param: "paths",
            message: "must not be empty; omit it to restore every attributed path".to_string(),
        });
    }
    let mut confirmations = BTreeMap::new();
    for confirmed in confirmed_paths {
        if confirmed.session_id != session_id || confirmed.boundary_id != boundary_id {
            return Err(HostlibError::InvalidParameter {
                builtin: RESTORE_BUILTIN,
                param: "confirmed_paths",
                message: "confirmation must match the requested session and boundary".to_string(),
            });
        }
        require_non_empty(RESTORE_BUILTIN, "confirmed_paths.path", &confirmed.path)?;
        if let ObservedFileState::Present { sha256 } = &confirmed.observed {
            if sha256.len() != 64
                || !sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(HostlibError::InvalidParameter {
                    builtin: RESTORE_BUILTIN,
                    param: "confirmed_paths.observed.sha256",
                    message: "must be a lowercase 64-character SHA-256 hex digest".to_string(),
                });
            }
        }
        if confirmations
            .insert(confirmed.path.as_str(), &confirmed.observed)
            .is_some()
        {
            return Err(HostlibError::InvalidParameter {
                builtin: RESTORE_BUILTIN,
                param: "confirmed_paths",
                message: format!("duplicate path `{}`", confirmed.path),
            });
        }
    }
    let Some(session) = find_session(session_id) else {
        return Ok(no_checkpoint(session_id));
    };
    let mut state = lock(&session);
    if let Some(detail) = pending_calls_detail(&state) {
        return Ok(unavailable("calls_in_flight", detail));
    }
    if let Some(detail) = incomplete_attribution(&state, boundary_id) {
        return Ok(unavailable("incomplete_attribution", detail));
    }
    let Some(planned) = compute_plan(&state, boundary_id, RESTORE_BUILTIN, &confirmations)? else {
        return Ok(unknown_boundary(boundary_id));
    };
    advance_revision(&mut state, RESTORE_BUILTIN)?;
    persist::save(session_id, &state);
    let root = state.root.clone();
    let require_no_replace = state.writer_policy == WriterPolicy::RequireKnown;
    let anchor = open_anchor(&root);
    let selected: Option<BTreeSet<&str>> =
        only_paths.map(|paths| paths.iter().map(String::as_str).collect());

    let mut restored = Vec::new();
    let mut refused = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = Vec::new();
    let mut unselected = false;
    for action in planned.actions {
        if selected
            .as_ref()
            .is_some_and(|chosen| !chosen.contains(action.entry.path.as_str()))
        {
            unselected = true;
            continue;
        }
        let mut entry = action.entry;
        match entry.action {
            "refuse" => refused.push(entry),
            "skip" => skipped.push(entry),
            "fail" => failed.push(entry),
            _ => {
                // The guard is re-checked through the anchored descriptors
                // immediately before writing: the plan may be moments old and
                // another writer may have moved the file.
                let step = match &anchor {
                    Ok(anchor) => restore_one(
                        anchor,
                        &entry,
                        &action.guard_hash,
                        action.write.as_ref(),
                        require_no_replace,
                    ),
                    Err(error) => Err(error.clone()),
                };
                match step {
                    Ok(Step::Restored) => restored.push(entry),
                    Ok(Step::Refused(found)) => {
                        entry.action = "refuse";
                        entry.reason = Some("changed outside the session".to_string());
                        entry.expected_hash = action.guard_hash;
                        entry.found_hash = found;
                        refused.push(entry);
                    }
                    Err(error) => {
                        entry.action = "fail";
                        entry.reason = Some(error);
                        failed.push(entry);
                    }
                }
            }
        }
    }

    // Paths now at the boundary no longer need attribution. When every
    // attributed path was handled, the later timeline is gone; otherwise keep
    // the boundaries and the attribution of everything left (refused, failed,
    // unrestorable, or not selected) so a retry still has its guard.
    let idx = state
        .boundaries
        .iter()
        .position(|b| b.id == boundary_id)
        .expect("compute_plan found the boundary");
    let settled: BTreeSet<&str> = restored
        .iter()
        .chain(
            skipped
                .iter()
                .filter(|e| e.reason.as_deref() == Some(ALREADY_AT_BOUNDARY)),
        )
        .map(|e| e.path.as_str())
        .collect();
    let everything_settled = !unselected
        && refused.is_empty()
        && failed.is_empty()
        && skipped
            .iter()
            .all(|e| e.reason.as_deref() == Some(ALREADY_AT_BOUNDARY));
    if everything_settled {
        state.boundaries.truncate(idx + 1);
        state.calls.retain(|c| c.boundary_idx < idx);
    } else {
        for call in state.calls.iter_mut().filter(|c| c.boundary_idx >= idx) {
            call.changes.retain(|c| !settled.contains(c.path.as_str()));
        }
    }
    state.pending.clear();

    Ok(Outcome::Ready(RestoreReceipt {
        available: true,
        boundary_id: boundary_id.to_string(),
        restored,
        refused,
        skipped,
        failed,
        irreversible: planned.irreversible,
    }))
}

/// Drop a session's checkpoint state and stored blobs.
pub fn drop_session(session_id: &str) -> DropReceipt {
    let removed = sessions()
        .lock()
        .expect("workspace checkpoint registry poisoned")
        .remove(session_id);
    let Some(session) = removed else {
        return DropReceipt {
            dropped: false,
            error: None,
        };
    };
    let root = lock(&session).root.clone();
    match stdfs::remove_dir_all(session_dir(&root, session_id)) {
        Ok(()) => DropReceipt {
            dropped: true,
            error: None,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DropReceipt {
            dropped: true,
            error: None,
        },
        Err(e) => DropReceipt {
            dropped: false,
            error: Some(format!("stored files were not removed: {e}")),
        },
    }
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

struct PlannedAction {
    entry: PlanEntry,
    /// Hash the path must still have on disk for the action to proceed.
    guard_hash: Option<String>,
    /// Content to write for `restore` and `recreate`.
    write: Option<WritePayload>,
}

struct WritePayload {
    bytes: Vec<u8>,
    exec: bool,
}

struct Planned {
    actions: Vec<PlannedAction>,
    irreversible: Vec<IrreversibleEffect>,
}

fn pending_calls_detail(state: &Session) -> Option<String> {
    if state.pending.is_empty() {
        return None;
    }
    let mut calls: Vec<&str> = state.pending.keys().map(String::as_str).collect();
    calls.sort_unstable();
    Some(format!(
        "pending calls: {}; ids: {}",
        calls.len(),
        calls.join(", ")
    ))
}

fn incomplete_attribution(state: &Session, boundary_id: &str) -> Option<String> {
    let idx = state
        .boundaries
        .iter()
        .position(|boundary| boundary.id == boundary_id)?;
    let failed: Vec<&CallRecord> = state
        .calls
        .iter()
        .filter(|call| call.boundary_idx >= idx && call.scan_errors > 0)
        .collect();
    if failed.is_empty() {
        return None;
    }
    let count: usize = failed.iter().map(|call| call.scan_errors).sum();
    let names: Vec<&str> = failed.iter().map(|call| call.call_id.as_str()).collect();
    Some(format!(
        "workspace scan errors: {count}; calls: {}",
        names.join(", ")
    ))
}

enum Target {
    Present {
        hash: String,
        bytes: Vec<u8>,
        exec: bool,
    },
    Absent,
    Unrestorable(String),
}

fn compute_plan(
    state: &Session,
    boundary_id: &str,
    builtin: &'static str,
    confirmations: &BTreeMap<&str, &ObservedFileState>,
) -> Result<Option<Planned>, HostlibError> {
    let fail = backend(builtin);
    let Some(idx) = state.boundaries.iter().position(|b| b.id == boundary_id) else {
        return Ok(None);
    };
    let boundary = &state.boundaries[idx];

    // Last attributed state per path across every call at or after the boundary.
    let mut last: BTreeMap<&str, (Option<&str>, bool)> = BTreeMap::new();
    let mut writers: BTreeMap<&str, BTreeSet<ChangeWriter>> = BTreeMap::new();
    let mut incomplete_paths: BTreeSet<&str> = BTreeSet::new();
    let mut irreversible = Vec::new();
    for call in state.calls.iter().filter(|c| c.boundary_idx >= idx) {
        for change in &call.changes {
            writers
                .entry(change.path.as_str())
                .or_default()
                .insert(change.writer.clone());
            last.insert(
                change.path.as_str(),
                (change.post_hash.as_deref(), change.incomplete),
            );
            if change.incomplete {
                incomplete_paths.insert(change.path.as_str());
            }
        }
        if let Some(level) = call.side_effect {
            if matches!(
                level,
                SideEffectLevel::Network | SideEffectLevel::DesktopControl
            ) {
                irreversible.push(IrreversibleEffect {
                    call_id: call.call_id.clone(),
                    invocation: call.invocation,
                    side_effect: level,
                });
            }
        }
    }

    let mut actions = Vec::new();
    let mut candidates = Vec::new();
    for (path, (expected, _)) in last {
        let mut path_writers: Vec<ChangeWriter> = writers
            .remove(path)
            .unwrap_or_default()
            .into_iter()
            .collect();
        if path_writers.is_empty() {
            path_writers.push(ChangeWriter::Unknown);
        }
        let loaded = current_hash(&state.root.join(path)).and_then(|found| {
            boundary_target(&state.root, &state.blob_dir, boundary, path)
                .map(|target| (found, target))
        });
        match loaded {
            Ok((found, target)) => candidates.push((
                path.to_string(),
                expected.map(str::to_string),
                found,
                target,
                path_writers,
            )),
            Err(error) => actions.push(PlannedAction {
                entry: PlanEntry {
                    path: path.to_string(),
                    action: "fail",
                    writers: path_writers,
                    observed: None,
                    reason: Some(error),
                    expected_hash: expected.map(str::to_string),
                    found_hash: None,
                },
                guard_hash: None,
                write: None,
            }),
        }
    }
    let to_delete: Vec<&str> = candidates
        .iter()
        .filter(|(_, _, found, target, _)| found.is_some() && matches!(target, Target::Absent))
        .map(|(path, ..)| path.as_str())
        .collect();
    let ignored = ignored_paths(&state.root, &to_delete).map_err(&fail)?;

    for (path, expected, found, target, path_writers) in candidates {
        let observed = match &found {
            Some(hash) => ObservedFileState::Present {
                sha256: hash.clone(),
            },
            None => ObservedFileState::Absent,
        };
        let confirmed = state.writer_policy == WriterPolicy::RequireKnown
            && confirmations.get(path.as_str()).copied() == Some(&observed);
        let entry = |action: &'static str, reason: Option<&str>| PlanEntry {
            path: path.clone(),
            action,
            writers: path_writers.clone(),
            observed: Some(observed.clone()),
            reason: reason.map(str::to_string),
            expected_hash: expected.clone(),
            found_hash: found.clone(),
        };
        let skip = |reason: &str| PlannedAction {
            entry: entry("skip", Some(reason)),
            guard_hash: None,
            write: None,
        };
        let planned = match target {
            Target::Unrestorable(reason) => skip(&reason),
            Target::Absent if found.is_none() => skip(ALREADY_AT_BOUNDARY),
            Target::Present { ref hash, .. } if found.as_deref() == Some(hash.as_str()) => {
                skip(ALREADY_AT_BOUNDARY)
            }
            _ if incomplete_paths.contains(path.as_str()) => PlannedAction {
                entry: entry(
                    "refuse",
                    Some("attribution incomplete: a directory was unreadable during the call"),
                ),
                guard_hash: None,
                write: None,
            },
            _ if found != expected && !confirmed => PlannedAction {
                entry: entry("refuse", Some("changed outside the session")),
                guard_hash: None,
                write: None,
            },
            Target::Absent if ignored.contains(&path) => {
                skip("ignored by git, not part of the checkpoint tree")
            }
            _ if state.writer_policy == WriterPolicy::RequireKnown
                && (path_writers.is_empty() || path_writers.contains(&ChangeWriter::Unknown))
                && !confirmed =>
            {
                PlannedAction {
                    entry: entry("refuse", Some("writer is unknown")),
                    guard_hash: None,
                    write: None,
                }
            }
            _ if state.writer_policy == WriterPolicy::RequireKnown && found.is_some() => {
                PlannedAction {
                    entry: entry("refuse", Some("concurrent-safe replacement is unavailable")),
                    guard_hash: None,
                    write: None,
                }
            }
            Target::Absent => PlannedAction {
                entry: entry("delete", None),
                guard_hash: if confirmed {
                    found.clone()
                } else {
                    expected.clone()
                },
                write: None,
            },
            _ if state.writer_policy == WriterPolicy::RequireKnown && !cfg!(unix) => {
                PlannedAction {
                    entry: entry("refuse", Some("concurrent-safe recreation is unavailable")),
                    guard_hash: None,
                    write: None,
                }
            }
            Target::Present { bytes, exec, .. } => PlannedAction {
                entry: entry(
                    if found.is_none() {
                        "recreate"
                    } else {
                        "restore"
                    },
                    None,
                ),
                guard_hash: if confirmed {
                    found.clone()
                } else {
                    expected.clone()
                },
                write: Some(WritePayload { bytes, exec }),
            },
        };
        actions.push(planned);
    }
    Ok(Some(Planned {
        actions,
        irreversible,
    }))
}

fn boundary_target(
    root: &Path,
    blob_dir: &Path,
    boundary: &Boundary,
    path: &str,
) -> Result<Target, String> {
    if let Some(reason) = boundary.unrestorable.get(path) {
        return Ok(Target::Unrestorable(format!("not checkpointed: {reason}")));
    }
    if let Some(file) = boundary.stored.get(path) {
        let blob = blob_dir.join(&file.hash);
        let bytes =
            stdfs::read(&blob).map_err(|e| format!("read stored blob {}: {e}", blob.display()))?;
        if sha256_hex(&bytes) != file.hash {
            return Err(format!(
                "stored checkpoint blob {} has a hash mismatch",
                blob.display()
            ));
        }
        return Ok(Target::Present {
            hash: file.hash.clone(),
            bytes,
            exec: file.exec,
        });
    }
    if boundary.absent_tracked.contains(path) {
        return Ok(Target::Absent);
    }
    let listing = git_text(root, &["ls-tree", "-z", &boundary.tree, "--", path])
        .map_err(|e| e.to_string())?;
    let Some(record) = listing.split('\0').find(|r| !r.is_empty()) else {
        return Ok(Target::Absent);
    };
    // "<mode> <type> <oid>\t<path>"
    let meta = record.split('\t').next().unwrap_or_default();
    let mut fields = meta.split(' ');
    let (mode, kind, oid) = (
        fields.next().unwrap_or_default(),
        fields.next().unwrap_or_default(),
        fields.next().unwrap_or_default(),
    );
    if kind != "blob" || mode == "120000" {
        return Ok(Target::Unrestorable(
            "checkpoint content is a symlink or submodule".to_string(),
        ));
    }
    let bytes = git(root, &["cat-file", "blob", oid]).map_err(|e| e.detail())?;
    Ok(Target::Present {
        hash: sha256_hex(&bytes),
        bytes,
        exec: mode == "100755",
    })
}

// ---------------------------------------------------------------------------
// Filesystem effects
// ---------------------------------------------------------------------------

/// SHA-256 of the file at `path`; `None` when missing. A directory or symlink
/// hashes as a fixed marker so it can never equal an attributed content hash.
fn current_hash(path: &Path) -> Result<Option<String>, String> {
    match stdfs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("stat {}: {e}", path.display())),
        Ok(meta) if meta.is_file() => sha256_file(path),
        Ok(_) => Ok(Some("not-a-regular-file".to_string())),
    }
}

fn sha256_file(path: &Path) -> Result<Option<String>, String> {
    let mut file = match stdfs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("open {}: {e}", path.display())),
    };
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(Some(hex::encode(hasher.finalize())))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(unix)]
fn is_executable(meta: &stdfs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &stdfs::Metadata) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Stat scan
// ---------------------------------------------------------------------------

/// True when the scan could not see `path`: it, or a directory above it, failed.
fn scan_blind_to(scan: &Scan, path: &str) -> bool {
    scan.unlocated_error
        || scan.error_paths.iter().any(|bad| {
            path == bad
                || (path.len() > bad.len()
                    && path.starts_with(bad.as_str())
                    && path.as_bytes()[bad.len()] == b'/')
        })
}

#[cfg(unix)]
fn stat_entry(meta: &stdfs::Metadata) -> StatEntry {
    use std::os::unix::fs::MetadataExt;
    StatEntry {
        size: meta.len(),
        mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
        ino: meta.ino(),
    }
}

#[cfg(not(unix))]
fn stat_entry(meta: &stdfs::Metadata) -> StatEntry {
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128);
    StatEntry {
        size: meta.len(),
        mtime_ns,
        ino: 0,
    }
}

// ---------------------------------------------------------------------------
// Git and storage helpers
// ---------------------------------------------------------------------------

enum GitError {
    Missing(String),
    Failed(String),
}

impl GitError {
    fn detail(self) -> String {
        match self {
            Self::Missing(d) | Self::Failed(d) => d,
        }
    }
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(d) | Self::Failed(d) => f.write_str(d),
        }
    }
}

/// A `git -C <root>` command built through the session environment funnel,
/// with ambient `GIT_*` variables (set while a hook runs) removed.
fn git_command(root: &Path) -> Result<Command, GitError> {
    let mut cmd = harn_vm::process_sandbox::session_std_command("git")
        .map_err(|e| GitError::Missing(format!("git session environment: {e:?}")))?;
    crate::scanner::strip_ambient_git_env(&mut cmd);
    cmd.arg("-C").arg(root).env("GIT_OPTIONAL_LOCKS", "0");
    Ok(cmd)
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    let mut cmd = git_command(root)?;
    let output = cmd
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| GitError::Missing(format!("cannot run git: {e}")))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(GitError::Failed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

fn git_text(root: &Path, args: &[&str]) -> Result<String, HostlibError> {
    git(root, args)
        .map(|out| String::from_utf8_lossy(&out).into_owned())
        .map_err(|e| HostlibError::Backend {
            builtin: CAPTURE_BUILTIN,
            message: format!("git {}: {e}", args.first().copied().unwrap_or_default()),
        })
}

fn nul_list(text: &str) -> Result<Vec<String>, HostlibError> {
    Ok(text
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

/// Which of `paths` git ignores (used so a delete never targets an ignored
/// path the stat scan could not have seen at capture).
fn ignored_paths(root: &Path, paths: &[&str]) -> Result<BTreeSet<String>, String> {
    if paths.is_empty() {
        return Ok(BTreeSet::new());
    }
    // One process for every path: stdin comes from a temp file, so no helper
    // thread is needed to avoid a pipe deadlock.
    let mut input = tempfile::tempfile().map_err(|e| format!("temp file: {e}"))?;
    std::io::Write::write_all(&mut input, paths.join("\0").as_bytes())
        .map_err(|e| format!("write check-ignore input: {e}"))?;
    std::io::Seek::rewind(&mut input).map_err(|e| format!("rewind check-ignore input: {e}"))?;
    let output = git_command(root)
        .map_err(|e| e.detail())?
        .args(["check-ignore", "-z", "--stdin"])
        .stdin(Stdio::from(input))
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("git check-ignore: {e}"))?;
    // Exit 1 means "none ignored"; anything above is a real failure.
    if output.status.code().is_some_and(|code| code > 1) {
        return Err("git check-ignore failed".to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

fn session_dir(root: &Path, session_id: &str) -> PathBuf {
    let mut dir = root.to_path_buf();
    for component in STATE_REL {
        dir.push(component);
    }
    dir.push(sanitize_component(session_id));
    dir
}

fn blob_dir(root: &Path, session_id: &str) -> PathBuf {
    session_dir(root, session_id).join("blobs")
}

fn store_blob(dir: &Path, hash: &str, bytes: &[u8]) -> Result<(), String> {
    let path = dir.join(hash);
    if path.exists() {
        return Ok(());
    }
    stdfs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    atomic_write(&path, bytes)
}

fn resolve_root(root: Option<&Path>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let resolved = normalize_against(&cwd, root.unwrap_or_else(|| Path::new(".")));
    harn_vm::workspace_path::canonicalize_existing_ancestor(&resolved).unwrap_or(resolved)
}

fn require_non_empty(
    builtin: &'static str,
    param: &'static str,
    value: &str,
) -> Result<(), HostlibError> {
    if value.trim().is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param,
            message: "must not be empty".to_string(),
        });
    }
    Ok(())
}

fn no_checkpoint<T>(session_id: &str) -> Outcome<T> {
    unavailable(
        "no_checkpoint",
        format!("session `{session_id}` has no captured boundary"),
    )
}

fn unknown_boundary<T>(boundary_id: &str) -> Outcome<T> {
    unavailable(
        "unknown_boundary",
        format!("boundary `{boundary_id}` was not captured for this session"),
    )
}

#[cfg(all(test, unix))]
mod tests;
