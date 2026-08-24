use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub const HARN_STATE_DIR_ENV: &str = "HARN_STATE_DIR";
pub const HARN_RUN_DIR_ENV: &str = "HARN_RUN_DIR";
pub const HARN_WORKTREE_DIR_ENV: &str = "HARN_WORKTREE_DIR";
const NEXTEST_ENV: &str = "NEXTEST";
const NEXTEST_RUN_ID_ENV: &str = "NEXTEST_RUN_ID";
const NEXTEST_BINARY_ID_ENV: &str = "NEXTEST_BINARY_ID";
const NEXTEST_TEST_NAME_ENV: &str = "NEXTEST_TEST_NAME";
const NEXTEST_ATTEMPT_ID_ENV: &str = "NEXTEST_ATTEMPT_ID";

#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static std::sync::Mutex<()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn resolve_root_value(base_dir: &Path, env_value: Option<&str>, default_relative: &str) -> PathBuf {
    match env_value {
        Some(value) if !value.trim().is_empty() => {
            let candidate = PathBuf::from(value);
            if candidate.is_absolute() {
                candidate
            } else {
                base_dir.join(candidate)
            }
        }
        _ => base_dir.join(default_relative),
    }
}

pub fn state_root(base_dir: &Path) -> PathBuf {
    if let Some(root) = crate::persistent_state::current_persistent_state_root() {
        return root;
    }
    let state_env_value = std::env::var(HARN_STATE_DIR_ENV).ok();
    state_root_value(
        base_dir,
        state_env_value.as_deref(),
        nextest_attempt_root().as_deref(),
    )
}

/// Resolve the state root for a path stored in a portable record.
pub fn state_root_reference(base_dir: &Path) -> PathBuf {
    if let Some(root) = crate::persistent_state::current_persistent_state_root() {
        return root;
    }
    let state_env_value = std::env::var(HARN_STATE_DIR_ENV).ok();
    root_reference_value(
        base_dir,
        state_env_value.as_deref(),
        ".harn",
        nextest_attempt_root().as_deref(),
        None,
    )
}

pub fn run_root(base_dir: &Path) -> PathBuf {
    let run_env_value = std::env::var(HARN_RUN_DIR_ENV).ok();
    run_root_value(
        base_dir,
        run_env_value.as_deref(),
        nextest_attempt_root().as_deref(),
    )
}

/// Resolve the run root for a path stored in a portable record.
///
/// The ordinary checkout-local default remains the relative `.harn-runs`
/// reference existing records use. An explicit override or an isolated test
/// attempt names a different physical root and therefore remains absolute.
pub fn run_root_reference(base_dir: &Path) -> PathBuf {
    let run_env_value = std::env::var(HARN_RUN_DIR_ENV).ok();
    root_reference_value(
        base_dir,
        run_env_value.as_deref(),
        ".harn-runs",
        nextest_attempt_root().as_deref(),
        Some("runs"),
    )
}

fn worktree_root_value(
    base_dir: &Path,
    state_env_value: Option<&str>,
    worktree_env_value: Option<&str>,
    nextest_root: Option<&Path>,
) -> PathBuf {
    match worktree_env_value {
        Some(value) if !value.trim().is_empty() => {
            let candidate = PathBuf::from(value);
            if candidate.is_absolute() {
                candidate
            } else {
                base_dir.join(candidate)
            }
        }
        _ => state_root_value(base_dir, state_env_value, nextest_root).join("worktrees"),
    }
}

pub fn worktree_root(base_dir: &Path) -> PathBuf {
    let state_env_value = std::env::var(HARN_STATE_DIR_ENV).ok();
    let worktree_env_value = std::env::var(HARN_WORKTREE_DIR_ENV).ok();
    if worktree_env_value
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        if let Some(root) = crate::persistent_state::current_persistent_state_root() {
            return root.join("worktrees");
        }
    }
    worktree_root_value(
        base_dir,
        state_env_value.as_deref(),
        worktree_env_value.as_deref(),
        nextest_attempt_root().as_deref(),
    )
}

pub fn store_path(base_dir: &Path) -> PathBuf {
    state_root(base_dir).join("store.json")
}

pub fn checkpoint_dir(base_dir: &Path) -> PathBuf {
    state_root(base_dir).join("checkpoints")
}

pub fn metadata_dir(base_dir: &Path) -> PathBuf {
    state_root(base_dir).join("metadata")
}

pub fn event_log_dir(base_dir: &Path) -> PathBuf {
    event_log_dir_at_state_root(&state_root(base_dir))
}

pub fn event_log_sqlite_path(base_dir: &Path) -> PathBuf {
    event_log_sqlite_path_at_state_root(&state_root(base_dir))
}

/// Event-log directory under an already-resolved state root.
///
/// Callers that own their state root exactly — an orchestrator told where to
/// keep its state, an embedder running concurrent isolated VMs — use this and
/// the sibling sqlite helper instead of the `base_dir` forms, which route
/// through [`state_root`] and therefore let an absolute `HARN_STATE_DIR`
/// discard the caller's path entirely.
pub fn event_log_dir_at_state_root(state_root: &Path) -> PathBuf {
    state_root.join("events")
}

/// Sqlite event-log path under an already-resolved state root. See
/// [`event_log_dir_at_state_root`].
pub fn event_log_sqlite_path_at_state_root(state_root: &Path) -> PathBuf {
    state_root.join("events.sqlite")
}

pub fn workflow_dir(base_dir: &Path) -> PathBuf {
    state_root(base_dir).join("workflows")
}

fn state_root_value(
    base_dir: &Path,
    state_env_value: Option<&str>,
    nextest_root: Option<&Path>,
) -> PathBuf {
    attempt_scoped_root_value(base_dir, state_env_value, ".harn", nextest_root, None)
}

fn run_root_value(
    base_dir: &Path,
    run_env_value: Option<&str>,
    nextest_root: Option<&Path>,
) -> PathBuf {
    attempt_scoped_root_value(
        base_dir,
        run_env_value,
        ".harn-runs",
        nextest_root,
        Some("runs"),
    )
}

fn root_reference_value(
    base_dir: &Path,
    explicit_value: Option<&str>,
    default_relative: &str,
    nextest_root: Option<&Path>,
    nextest_child: Option<&str>,
) -> PathBuf {
    if explicit_value.is_none_or(|value| value.trim().is_empty()) && nextest_root.is_none() {
        PathBuf::from(default_relative)
    } else {
        attempt_scoped_root_value(
            base_dir,
            explicit_value,
            default_relative,
            nextest_root,
            nextest_child,
        )
    }
}

fn attempt_scoped_root_value(
    base_dir: &Path,
    explicit_value: Option<&str>,
    default_relative: &str,
    nextest_root: Option<&Path>,
    nextest_child: Option<&str>,
) -> PathBuf {
    if explicit_value.is_some_and(|value| !value.trim().is_empty()) {
        return resolve_root_value(base_dir, explicit_value, default_relative);
    }
    nextest_root
        .map(|root| nextest_child.map_or_else(|| root.to_path_buf(), |child| root.join(child)))
        .unwrap_or_else(|| base_dir.join(default_relative))
}

/// Nextest runs each test attempt in its own process and identifies that
/// attempt in the environment. Keep every default runtime root inside the
/// attempt instead of letting concurrent tests persist state, runs, or
/// worktrees into the checkout. Explicit root variables still win, and child
/// processes inherit the same Nextest identity.
fn nextest_attempt_root() -> Option<PathBuf> {
    std::env::var_os(NEXTEST_ENV)?;
    let identity = [
        std::env::var(NEXTEST_RUN_ID_ENV).ok()?,
        std::env::var(NEXTEST_BINARY_ID_ENV).ok()?,
        std::env::var(NEXTEST_TEST_NAME_ENV).ok()?,
        std::env::var(NEXTEST_ATTEMPT_ID_ENV).ok()?,
    ];
    Some(nextest_attempt_root_for_identity(
        &std::env::temp_dir(),
        identity.iter().map(String::as_str),
    ))
}

fn nextest_attempt_root_for_identity<'a>(
    temp_dir: &Path,
    identity: impl IntoIterator<Item = &'a str>,
) -> PathBuf {
    let mut hasher = Sha256::new();
    for part in identity {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    temp_dir
        .join("harn-nextest-state")
        .join(hex::encode(&digest[..16]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_resolve_under_base_dir() {
        let base = Path::new("/tmp/harn-runtime-paths");
        assert_eq!(resolve_root_value(base, None, ".harn"), base.join(".harn"));
        assert_eq!(
            resolve_root_value(base, None, ".harn-runs"),
            base.join(".harn-runs")
        );
        assert_eq!(
            worktree_root_value(base, None, None, None),
            base.join(".harn").join("worktrees")
        );
        assert_eq!(
            resolve_root_value(base, None, ".harn").join("events"),
            base.join(".harn").join("events")
        );
        assert_eq!(
            resolve_root_value(base, None, ".harn").join("workflows"),
            base.join(".harn").join("workflows")
        );
        assert_eq!(
            resolve_root_value(base, None, ".harn").join("events.sqlite"),
            base.join(".harn").join("events.sqlite")
        );
    }

    #[test]
    fn nextest_default_roots_are_attempt_scoped_but_explicit_roots_still_win() {
        let base = Path::new("/workspace");
        let temp = Path::new("/tmp");
        let first =
            nextest_attempt_root_for_identity(temp, ["run-1", "harn-vm", "test-a", "attempt-1"]);
        let same =
            nextest_attempt_root_for_identity(temp, ["run-1", "harn-vm", "test-a", "attempt-1"]);
        let other =
            nextest_attempt_root_for_identity(temp, ["run-1", "harn-vm", "test-b", "attempt-1"]);

        assert_eq!(first, same);
        assert_ne!(first, other);
        assert_eq!(state_root_value(base, None, Some(&first)), first);
        assert_eq!(run_root_value(base, None, Some(&first)), first.join("runs"));
        assert_eq!(
            root_reference_value(base, None, ".harn-runs", Some(&first), Some("runs")),
            first.join("runs")
        );
        assert_eq!(
            root_reference_value(base, None, ".harn", Some(&first), None),
            first
        );
        assert_eq!(
            worktree_root_value(base, None, None, Some(&first)),
            first.join("worktrees")
        );
        assert_eq!(
            state_root_value(base, Some("/operator/state"), Some(&other)),
            PathBuf::from("/operator/state")
        );
        assert_eq!(
            state_root_value(base, Some("relative-state"), Some(&other)),
            base.join("relative-state")
        );
        assert_eq!(
            run_root_value(base, Some("/operator/runs"), Some(&other)),
            PathBuf::from("/operator/runs")
        );
        assert_eq!(
            run_root_value(base, Some("relative-runs"), Some(&other)),
            base.join("relative-runs")
        );
        assert_eq!(
            root_reference_value(base, None, ".harn-runs", None, Some("runs")),
            PathBuf::from(".harn-runs")
        );
        assert_eq!(
            root_reference_value(
                base,
                Some("relative-runs"),
                ".harn-runs",
                Some(&other),
                Some("runs"),
            ),
            base.join("relative-runs")
        );
        assert_eq!(
            worktree_root_value(base, Some("/operator/state"), None, Some(&other)),
            PathBuf::from("/operator/state/worktrees")
        );
        assert_eq!(
            worktree_root_value(base, None, Some("relative-worktrees"), Some(&other)),
            base.join("relative-worktrees")
        );
    }
}
