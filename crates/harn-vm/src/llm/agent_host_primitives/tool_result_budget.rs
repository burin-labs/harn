//! Per-result size budget for model-visible tool output.
//!
//! Every dispatched tool result passes through [`apply`] after PostToolUse
//! hooks and before the observation is framed, so no handler can put more than
//! the budget into the conversation. Oversized text is spilled in full to an
//! owner-only file under the sandbox-readable workspace temp dir, and the model
//! sees the shared deterministic microcompact projection plus a notice naming
//! that file. The session's spill directory is removed when the session closes.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::Duration;

/// Default `tool_result_max_chars`: about 16k tokens per tool result.
pub(super) const DEFAULT_TOOL_RESULT_MAX_CHARS: usize = 64_000;
const SPILL_DIR_NAME: &str = "tool-results";
/// Fallback for spill directories a crashed process never closed; matches the
/// command-artifact default retention window.
const SPILL_RETENTION: Duration = Duration::from_hours(168);

/// Spill directory created for each open session, removed on session close.
static SESSION_SPILL_DIRS: LazyLock<Mutex<HashMap<String, PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Room for the excerpt's own snip/diagnostic scaffolding.
const PROJECTION_SCAFFOLD_BYTES: usize = 64;
/// Smallest enforced budget, so the excerpt always has room beside the notice.
const MIN_TOOL_RESULT_MAX_CHARS: usize = 1_024;

pub(super) struct BudgetedText {
    pub text: String,
    pub original_size: usize,
    pub dropped_bytes: usize,
    pub truncated: bool,
    pub spill_path: Option<String>,
}

/// Resolve the budget from dispatch options: absent means the default, zero
/// (or a negative value) disables it, and tiny values are raised to the floor.
pub(super) fn max_chars(configured: Option<i64>) -> usize {
    match configured {
        None => DEFAULT_TOOL_RESULT_MAX_CHARS,
        Some(value) if value <= 0 => 0,
        Some(value) => (value as usize).max(MIN_TOOL_RESULT_MAX_CHARS),
    }
}

/// Bound `text` to `max_chars`, spilling the full text and reporting the cap
/// on the post-tool-output boundary when it fires.
pub(super) fn apply(
    text: String,
    max_chars: usize,
    session_id: &str,
    tool_name: &str,
    tool_call_id: &str,
) -> BudgetedText {
    let original_size = text.len();
    if max_chars == 0 || original_size <= max_chars {
        return BudgetedText {
            text,
            original_size,
            dropped_bytes: 0,
            truncated: false,
            spill_path: None,
        };
    }
    let spill = spill_full_output(&text, session_id, tool_name, tool_call_id);
    let saved = match &spill {
        Ok(path) => format!("full output saved to {}", path.display()),
        Err(error) => format!("the full output could not be saved ({error})"),
    };
    let notice = format!(
        "[tool output truncated: {original_size} chars total, over the {max_chars}-char result budget; {saved}]"
    );
    // microcompact refuses budgets under 200 bytes; never let a long notice disable it.
    let excerpt_budget = max_chars
        .saturating_sub(notice.len() + 1 + PROJECTION_SCAFFOLD_BYTES)
        .max(200);
    let excerpt = crate::orchestration::microcompact_tool_output_result(&text, excerpt_budget);
    crate::boundary::BoundaryFailure::new(
        crate::boundary::BoundaryId::PostToolOutput,
        crate::boundary::BoundaryFailureKind::Capped,
        format!("tool_result_max_chars capped output from tool {tool_name}; {saved}"),
    )
    .with_dropped_bytes(excerpt.dropped_bytes)
    .in_session(session_id)
    .report();
    BudgetedText {
        text: format!("{}\n{notice}", excerpt.text),
        original_size,
        dropped_bytes: excerpt.dropped_bytes,
        truncated: true,
        spill_path: spill.ok().map(|path| path.display().to_string()),
    }
}

fn spill_full_output(
    text: &str,
    session_id: &str,
    tool_name: &str,
    tool_call_id: &str,
) -> std::io::Result<PathBuf> {
    let namespace = crate::stdlib::fs::workspace_temp_root()
        .map_err(|error| std::io::Error::other(error.to_string()))?
        .join(SPILL_DIR_NAME);
    ensure_private_dir(&namespace)?;
    let session_dir = namespace.join(path_component(session_id, "session"));
    if ensure_private_dir(&session_dir)? {
        sweep_expired_sessions(&namespace, &session_dir);
    }
    SESSION_SPILL_DIRS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(session_id.to_string(), session_dir.clone());
    let call = if tool_call_id.is_empty() {
        uuid::Uuid::now_v7().to_string()
    } else {
        path_component(tool_call_id, "call")
    };
    let path = session_dir.join(format!("{}-{call}.txt", path_component(tool_name, "tool")));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&path)?;
    file.write_all(text.as_bytes())?;
    Ok(path)
}

/// Create `dir` owner-only, refusing a symlink or another user's directory.
/// Returns true when this call created it.
fn ensure_private_dir(dir: &Path) -> std::io::Result<bool> {
    let created = match std::fs::create_dir(dir) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error),
    };
    let metadata = std::fs::symlink_metadata(dir)?;
    if !metadata.file_type().is_dir() {
        return Err(std::io::Error::other("spill path is not a real directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::other(
                "spill directory is not owned by the current user",
            ));
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(created)
}

fn sweep_expired_sessions(namespace: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(namespace) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let expired = entry
            .metadata()
            .ok()
            .filter(std::fs::Metadata::is_dir)
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > SPILL_RETENTION);
        if expired && path != keep {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Remove the spill directory a closing session created, if any.
pub(crate) fn release_session(session_id: &str) {
    let dir = SESSION_SPILL_DIRS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(session_id);
    if let Some(dir) = dir {
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn path_component(raw: &str, fallback: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .take(96)
        .collect();
    if cleaned.trim_matches('.').is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}
