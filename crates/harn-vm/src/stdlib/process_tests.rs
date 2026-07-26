//! Unit tests for `stdlib::process`.
//!
//! Split out of `process.rs` (via `#[path]`) to keep that file under the
//! source-length ratchet cap; this is the `process::tests` module, so
//! `use super::*` still resolves to the production module's items.

use super::*;

struct RuntimePathsEnvGuard {
    state: Option<String>,
    run: Option<String>,
    worktree: Option<String>,
}

impl RuntimePathsEnvGuard {
    fn capture() -> Self {
        Self {
            state: std::env::var(crate::runtime_paths::HARN_STATE_DIR_ENV).ok(),
            run: std::env::var(crate::runtime_paths::HARN_RUN_DIR_ENV).ok(),
            worktree: std::env::var(crate::runtime_paths::HARN_WORKTREE_DIR_ENV).ok(),
        }
    }
}

impl Drop for RuntimePathsEnvGuard {
    fn drop(&mut self) {
        match self.state.as_deref() {
            Some(value) => std::env::set_var(crate::runtime_paths::HARN_STATE_DIR_ENV, value),
            None => std::env::remove_var(crate::runtime_paths::HARN_STATE_DIR_ENV),
        }
        match self.run.as_deref() {
            Some(value) => std::env::set_var(crate::runtime_paths::HARN_RUN_DIR_ENV, value),
            None => std::env::remove_var(crate::runtime_paths::HARN_RUN_DIR_ENV),
        }
        match self.worktree.as_deref() {
            Some(value) => {
                std::env::set_var(crate::runtime_paths::HARN_WORKTREE_DIR_ENV, value);
            }
            None => std::env::remove_var(crate::runtime_paths::HARN_WORKTREE_DIR_ENV),
        }
    }
}

#[test]
fn lexically_collapse_resolves_sibling_walk() {
    let path = PathBuf::from("/tmp/project/tests/../fixtures/x.json");
    let collapsed = lexically_collapse(&path).expect("sibling walk");
    assert_eq!(collapsed, PathBuf::from("/tmp/project/fixtures/x.json"));
}

#[test]
fn lexically_collapse_blocks_escape_past_root() {
    // `/app/../etc/passwd` would lexically resolve to `/etc/passwd`,
    // but the pop hits a RootDir which is not Normal — refuse.
    let path = PathBuf::from("/app/../../etc/passwd");
    assert!(lexically_collapse(&path).is_none());
}

#[test]
fn lexically_collapse_strips_curdir() {
    let path = PathBuf::from("/app/./logs/today.txt");
    let collapsed = lexically_collapse(&path).expect("curdir is benign");
    assert_eq!(collapsed, PathBuf::from("/app/logs/today.txt"));
}

#[test]
fn resolve_source_relative_path_blocks_obvious_escape() {
    let dir = std::env::temp_dir().join(format!("harn-process-escape-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    set_thread_source_dir(&dir);
    set_thread_execution_context(Some(crate::orchestration::RunExecutionRecord {
        cwd: Some(dir.to_string_lossy().into_owned()),
        project_root: None,
        source_dir: Some(dir.to_string_lossy().into_owned()),
        env: BTreeMap::new(),
        adapter: None,
        repo_path: None,
        worktree_path: None,
        branch: None,
        base_ref: None,
        cleanup: None,
        environment_policy: Default::default(),
        grants: Vec::new(),
    }));
    // A long string of `..` should escape the temp-root and trip
    // the rejection sentinel, so the file read fails NotFound
    // instead of escaping to a different filesystem location.
    let resolved = resolve_source_relative_path("../../../../../../../../etc/passwd");
    assert!(
        resolved
            .to_string_lossy()
            .contains("__harn_rejected_parent_dir_traversal__"),
        "expected rejection sentinel, got {resolved:?}"
    );
    reset_process_state();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_source_relative_path_ignores_thread_source_dir_without_execution_context() {
    let dir = std::env::temp_dir().join(format!("harn-process-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let current_dir = std::env::current_dir().unwrap();
    set_thread_source_dir(&dir);
    let resolved = resolve_source_relative_path("templates/prompt.txt");
    assert_eq!(resolved, current_dir.join("templates/prompt.txt"));
    reset_process_state();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_source_relative_path_prefers_execution_cwd_over_source_dir() {
    let cwd = std::env::temp_dir().join(format!("harn-process-cwd-{}", uuid::Uuid::now_v7()));
    let source_dir =
        std::env::temp_dir().join(format!("harn-process-source-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&source_dir).unwrap();
    set_thread_source_dir(&source_dir);
    set_thread_execution_context(Some(crate::orchestration::RunExecutionRecord {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        project_root: None,
        source_dir: Some(source_dir.to_string_lossy().into_owned()),
        env: BTreeMap::new(),
        adapter: None,
        repo_path: None,
        worktree_path: None,
        branch: None,
        base_ref: None,
        cleanup: None,
        environment_policy: Default::default(),
        grants: Vec::new(),
    }));
    let resolved = resolve_source_relative_path("templates/prompt.txt");
    assert_eq!(resolved, cwd.join("templates/prompt.txt"));
    reset_process_state();
    let _ = std::fs::remove_dir_all(&cwd);
    let _ = std::fs::remove_dir_all(&source_dir);
}

#[test]
fn resolve_source_asset_path_prefers_execution_source_dir_over_cwd() {
    let cwd = std::env::temp_dir().join(format!("harn-asset-cwd-{}", uuid::Uuid::now_v7()));
    let source_dir =
        std::env::temp_dir().join(format!("harn-asset-source-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&source_dir).unwrap();
    set_thread_source_dir(&source_dir);
    set_thread_execution_context(Some(crate::orchestration::RunExecutionRecord {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        project_root: None,
        source_dir: Some(source_dir.to_string_lossy().into_owned()),
        env: BTreeMap::new(),
        adapter: None,
        repo_path: None,
        worktree_path: None,
        branch: None,
        base_ref: None,
        cleanup: None,
        environment_policy: Default::default(),
        grants: Vec::new(),
    }));
    let resolved = resolve_source_asset_path("templates/prompt.txt");
    assert_eq!(resolved, source_dir.join("templates/prompt.txt"));
    reset_process_state();
    let _ = std::fs::remove_dir_all(&cwd);
    let _ = std::fs::remove_dir_all(&source_dir);
}

#[test]
fn set_thread_source_dir_absolutizes_relative_paths() {
    reset_process_state();
    let current_dir = std::env::current_dir().unwrap();
    set_thread_source_dir(std::path::Path::new("scripts"));
    assert_eq!(source_root_path(), current_dir.join("scripts"));
    reset_process_state();
}

#[test]
fn project_root_builtin_prefers_explicit_execution_project_root() {
    let cwd = std::env::temp_dir().join(format!("harn-process-cwd-{}", uuid::Uuid::now_v7()));
    let project_root =
        std::env::temp_dir().join(format!("harn-process-root-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&project_root).unwrap();
    set_thread_execution_context(Some(RunExecutionRecord {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        project_root: Some(project_root.to_string_lossy().into_owned()),
        ..Default::default()
    }));

    let mut out = String::new();
    let value = project_root_impl(&[], &mut out).unwrap();
    assert_eq!(value.display(), project_root.display().to_string());

    reset_process_state();
    let _ = std::fs::remove_dir_all(&cwd);
    let _ = std::fs::remove_dir_all(&project_root);
}

#[test]
fn runtime_paths_uses_configurable_state_roots() {
    let _runtime_paths_env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env_guard = RuntimePathsEnvGuard::capture();
    let base = std::env::temp_dir().join(format!("harn-process-runtime-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&base).unwrap();
    std::env::set_var(crate::runtime_paths::HARN_STATE_DIR_ENV, ".custom-harn");
    std::env::set_var(crate::runtime_paths::HARN_RUN_DIR_ENV, ".custom-runs");
    std::env::set_var(
        crate::runtime_paths::HARN_WORKTREE_DIR_ENV,
        ".custom-worktrees",
    );
    set_thread_execution_context(Some(RunExecutionRecord {
        cwd: Some(base.to_string_lossy().into_owned()),
        ..Default::default()
    }));

    let mut vm = crate::vm::Vm::new();
    register_process_builtins(&mut vm);
    let mut out = String::new();
    let builtin = vm
        .builtins
        .get("runtime_paths")
        .expect("runtime_paths builtin");
    let paths = match builtin(&[], &mut out).unwrap() {
        VmValue::Dict(map) => map,
        other => panic!("expected dict, got {other:?}"),
    };
    assert_eq!(
        paths.get("state_root").unwrap().display(),
        base.join(".custom-harn").display().to_string()
    );
    assert_eq!(
        paths.get("run_root").unwrap().display(),
        base.join(".custom-runs").display().to_string()
    );
    assert_eq!(
        paths.get("worktree_root").unwrap().display(),
        base.join(".custom-worktrees").display().to_string()
    );

    reset_process_state();
    let _ = std::fs::remove_dir_all(&base);
}
