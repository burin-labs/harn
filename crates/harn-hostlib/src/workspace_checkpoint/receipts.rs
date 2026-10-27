//! Wire contracts for workspace checkpoints.

use harn_vm::tool_annotations::SideEffectLevel;
use serde::{Deserialize, Serialize};

/// Either a method's result or an explicit statement that checkpoints cannot
/// run here. `available: false` is never an empty success.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Outcome<T> {
    /// The method ran; the payload carries `available: true`.
    Ready(T),
    /// Checkpoints cannot run for this request.
    Unavailable(Unavailable),
}

/// Why checkpoints are unavailable.
#[derive(Debug, Serialize)]
pub struct Unavailable {
    /// Always `false`.
    pub available: bool,
    /// Stable classifier: `not_a_git_worktree`, `git_unavailable`,
    /// `index_not_writable_as_tree`, `no_checkpoint`, or `unknown_boundary`.
    pub reason: &'static str,
    /// Human-readable detail.
    pub detail: String,
}

/// Result of [`super::capture`].
#[derive(Debug, Serialize)]
pub struct CaptureReceipt {
    /// Always `true`.
    pub available: bool,
    /// Echoed boundary id.
    pub boundary_id: String,
    /// Writer policy enforced by this checkpoint session.
    pub writer_policy: WriterPolicy,
    /// Files in the stat index.
    pub scanned_files: usize,
    /// Dirty or untracked files whose bytes were stored.
    pub stored_files: usize,
    /// Bytes stored for those files.
    pub stored_bytes: u64,
    /// Dirty files that cannot be restored (symlink or over the size cap).
    pub unrestorable_files: usize,
    /// Directory entries the scan could not read.
    pub scan_errors: usize,
    /// Wall time of the stat scan.
    pub scan_ms: u64,
}

/// How checkpoint restores handle changes without verified writer evidence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum WriterPolicy {
    /// Preserve the historical scan-based attribution behavior.
    LegacyScan,
    /// Refuse to overwrite a path whose writer is not known.
    RequireKnown,
}

/// Writer evidence for a workspace change.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[non_exhaustive]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangeWriter {
    /// A scan detected a change but cannot identify its writer.
    Unknown,
}

/// The exact file state shown to a person before a restore is confirmed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservedFileState {
    /// No file exists at the path.
    Absent,
    /// A file exists with this SHA-256 hash.
    Present {
        /// Lowercase SHA-256 digest of the file contents.
        sha256: String,
    },
}

/// A path and file state explicitly confirmed by the host's user.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmedPath {
    /// Session shown when the host obtained confirmation.
    pub session_id: String,
    /// Boundary shown when the host obtained confirmation.
    pub boundary_id: String,
    /// Path relative to the workspace root.
    pub path: String,
    /// State shown when the person confirmed the restore.
    pub observed: ObservedFileState,
}

/// Result of [`super::call_begin`].
#[derive(Debug, Serialize)]
pub struct CallBeginReceipt {
    /// Always `true`.
    pub available: bool,
    /// Echoed call id.
    pub call_id: String,
    /// Files in the pre-call stat index.
    pub scanned_files: usize,
    /// Directory entries the scan could not read.
    pub scan_errors: usize,
    /// Wall time of the stat scan.
    pub scan_ms: u64,
}

/// One path attributed to a call.
#[derive(Clone, Debug, Serialize)]
pub struct AttributedChange {
    /// Path relative to the workspace root, with `/` separators.
    pub path: String,
    /// `created`, `modified`, or `deleted`.
    pub kind: &'static str,
    /// Writer evidence for the change.
    pub writer: ChangeWriter,
    /// SHA-256 of the content after the call; absent when deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_hash: Option<String>,
    /// True when a scan error means this path's attribution may be wrong. A
    /// restore refuses incomplete paths.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub incomplete: bool,
}

/// Result of [`super::call_end`].
#[derive(Debug, Serialize)]
pub struct CallEndReceipt {
    /// Always `true`.
    pub available: bool,
    /// Echoed call id.
    pub call_id: String,
    /// Paths the call changed.
    pub changes: Vec<AttributedChange>,
    /// Directory entries the post-call scan could not read.
    pub scan_errors: usize,
}

/// One planned or executed path action.
#[derive(Clone, Debug, Serialize)]
pub struct PlanEntry {
    /// Path relative to the workspace root.
    pub path: String,
    /// `restore`, `delete`, `recreate`, `refuse`, `skip`, or `fail`.
    pub action: &'static str,
    /// Writer evidence for this path since the boundary.
    pub writers: Vec<ChangeWriter>,
    /// The file state used for a possible user confirmation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed: Option<ObservedFileState>,
    /// Why the path is refused or skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The session's last attributed content hash; absent when it deleted the path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_hash: Option<String>,
    /// The content hash found on disk now; absent when the path is missing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found_hash: Option<String>,
}

/// A call whose effects a file restore cannot undo.
#[derive(Clone, Debug, Serialize)]
pub struct IrreversibleEffect {
    /// Tool call id.
    pub call_id: String,
    /// Side-effect class from the tool annotation.
    pub side_effect: SideEffectLevel,
}

/// Result of [`super::plan`].
#[derive(Debug, Serialize)]
pub struct PlanReceipt {
    /// Always `true`.
    pub available: bool,
    /// Session the plan belongs to.
    pub session_id: String,
    /// Boundary the plan restores to.
    pub boundary_id: String,
    /// One entry per attributed path.
    pub entries: Vec<PlanEntry>,
    /// Calls after the boundary whose effects cannot be undone.
    pub irreversible: Vec<IrreversibleEffect>,
}

/// Result of [`super::restore`].
#[derive(Debug, Serialize)]
pub struct RestoreReceipt {
    /// Always `true`.
    pub available: bool,
    /// Boundary restored to.
    pub boundary_id: String,
    /// Paths written or removed.
    pub restored: Vec<PlanEntry>,
    /// Paths left alone because they changed outside the session.
    pub refused: Vec<PlanEntry>,
    /// Paths that needed no action or could not be restored.
    pub skipped: Vec<PlanEntry>,
    /// Paths whose read or write failed; the rest of the restore still ran.
    pub failed: Vec<PlanEntry>,
    /// Calls after the boundary whose effects cannot be undone.
    pub irreversible: Vec<IrreversibleEffect>,
}

/// Result of [`super::drop_session`].
#[derive(Debug, Serialize)]
pub struct DropReceipt {
    /// True when the session's state and stored files are gone.
    pub dropped: bool,
    /// Why stored files remain, when state existed but cleanup failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
