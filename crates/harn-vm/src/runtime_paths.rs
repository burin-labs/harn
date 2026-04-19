use std::path::{Path, PathBuf};

pub const HARN_STATE_DIR_ENV: &str = "HARN_STATE_DIR";
pub const HARN_RUN_DIR_ENV: &str = "HARN_RUN_DIR";
pub const HARN_WORKTREE_DIR_ENV: &str = "HARN_WORKTREE_DIR";

fn resolve_root(base_dir: &Path, env_key: &str, default_relative: &str) -> PathBuf {
    match std::env::var(env_key) {
        Ok(value) if !value.trim().is_empty() => {
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
    resolve_root(base_dir, HARN_STATE_DIR_ENV, ".harn")
}

pub fn run_root(base_dir: &Path) -> PathBuf {
    resolve_root(base_dir, HARN_RUN_DIR_ENV, ".harn-runs")
}

pub fn worktree_root(base_dir: &Path) -> PathBuf {
    match std::env::var(HARN_WORKTREE_DIR_ENV) {
        Ok(value) if !value.trim().is_empty() => {
            let candidate = PathBuf::from(value);
            if candidate.is_absolute() {
                candidate
            } else {
                base_dir.join(candidate)
            }
        }
        _ => state_root(base_dir).join("worktrees"),
    }
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
    state_root(base_dir).join("events")
}

pub fn event_log_sqlite_path(base_dir: &Path) -> PathBuf {
    state_root(base_dir).join("events.sqlite")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_resolve_under_base_dir() {
        let base = Path::new("/tmp/harn-runtime-paths");
        assert_eq!(state_root(base), base.join(".harn"));
        assert_eq!(run_root(base), base.join(".harn-runs"));
        assert_eq!(worktree_root(base), base.join(".harn").join("worktrees"));
        assert_eq!(event_log_dir(base), base.join(".harn").join("events"));
        assert_eq!(
            event_log_sqlite_path(base),
            base.join(".harn").join("events.sqlite")
        );
    }
}
