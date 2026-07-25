use std::path::{Path, PathBuf};

pub const HARN_STATE_DIR_ENV: &str = "HARN_STATE_DIR";
pub const HARN_RUN_DIR_ENV: &str = "HARN_RUN_DIR";
pub const HARN_WORKTREE_DIR_ENV: &str = "HARN_WORKTREE_DIR";

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

fn resolve_root(base_dir: &Path, env_key: &str, default_relative: &str) -> PathBuf {
    let env_value = std::env::var(env_key).ok();
    resolve_root_value(base_dir, env_value.as_deref(), default_relative)
}

pub fn state_root(base_dir: &Path) -> PathBuf {
    resolve_root(base_dir, HARN_STATE_DIR_ENV, ".harn")
}

pub fn run_root(base_dir: &Path) -> PathBuf {
    resolve_root(base_dir, HARN_RUN_DIR_ENV, ".harn-runs")
}

fn worktree_root_value(
    base_dir: &Path,
    state_env_value: Option<&str>,
    worktree_env_value: Option<&str>,
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
        _ => resolve_root_value(base_dir, state_env_value, ".harn").join("worktrees"),
    }
}

pub fn worktree_root(base_dir: &Path) -> PathBuf {
    let state_env_value = std::env::var(HARN_STATE_DIR_ENV).ok();
    let worktree_env_value = std::env::var(HARN_WORKTREE_DIR_ENV).ok();
    worktree_root_value(
        base_dir,
        state_env_value.as_deref(),
        worktree_env_value.as_deref(),
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
            worktree_root_value(base, None, None),
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
}
