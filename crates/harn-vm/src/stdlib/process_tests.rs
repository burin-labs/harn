//! Unit tests for `stdlib::process`.
//!
//! Split out of `process.rs` (via `#[path]`) to keep that file under the
//! source-length ratchet cap; this is the `process::tests` module, so
//! `use super::*` still resolves to the production module's items.

use super::*;

struct CurrentDirGuard(std::path::PathBuf);

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.0).expect("restore current dir");
    }
}

#[test]
fn inherited_process_cwd_prefers_admitted_execution_context_over_ambient_root() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    crate::reset_thread_local_state();
    let roots = tempfile::tempdir().unwrap();
    let ambient = roots.path().join("ambient");
    let execution = roots.path().join("execution");
    std::fs::create_dir_all(&ambient).unwrap();
    std::fs::create_dir_all(&execution).unwrap();
    let original_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&ambient).unwrap();
    let cwd_guard = CurrentDirGuard(original_cwd);
    set_thread_execution_context(Some(crate::orchestration::RunExecutionRecord {
        cwd: Some(execution.to_string_lossy().into_owned()),
        ..Default::default()
    }));
    crate::orchestration::push_execution_policy(crate::orchestration::CapabilityPolicy {
        workspace_roots: vec![
            ambient.to_string_lossy().into_owned(),
            execution.to_string_lossy().into_owned(),
        ],
        sandbox_profile: crate::orchestration::SandboxProfile::Worktree,
        ..Default::default()
    });

    let resolved = inherited_process_cwd().unwrap();

    crate::reset_thread_local_state();
    drop(cwd_guard);
    assert_eq!(resolved, std::fs::canonicalize(&execution).unwrap());
}

#[test]
fn process_error_prefix_preserves_structured_io_fields() {
    let mut fields = crate::value::DictMap::new();
    fields.put_str("error", "io_error");
    fields.put_str("kind", "not_found");
    fields.put_str("category", "environment");
    fields.put_str("message", "process spawn failed: missing");

    let error = prefix_process_error(VmError::Thrown(VmValue::dict(fields)), "exec");
    let VmError::Thrown(VmValue::Dict(fields)) = error else {
        panic!("expected structured thrown I/O error");
    };
    assert_eq!(
        fields.get("error").map(VmValue::display).as_deref(),
        Some("io_error")
    );
    assert_eq!(
        fields.get("kind").map(VmValue::display).as_deref(),
        Some("not_found")
    );
    assert_eq!(
        fields.get("category").map(VmValue::display).as_deref(),
        Some("environment")
    );
    assert_eq!(
        fields.get("message").map(VmValue::display).as_deref(),
        Some("exec failed: process spawn failed: missing")
    );
}

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

#[test]
fn child_cwd_drops_the_verbatim_prefix_a_shell_cannot_start_in() {
    // The positive case: this is the exact shape `canonicalize` hands back on
    // Windows, and the exact shape `cmd.exe` answered with "UNC paths are not
    // supported" before starting in the Windows directory instead.
    assert_eq!(
        super::child_process_cwd(std::path::PathBuf::from(r"\\?\C:\work\repo")),
        std::path::PathBuf::from(r"C:\work\repo"),
    );
    // A bare drive root still has a drive letter and still gets stripped.
    assert_eq!(
        super::child_process_cwd(std::path::PathBuf::from(r"\\?\C:\")),
        std::path::PathBuf::from(r"C:\"),
    );
}

#[test]
fn child_cwd_leaves_every_path_that_is_not_a_verbatim_disk_path_alone() {
    // The controls. Without these the test above would also pass for an
    // implementation that stripped four characters off anything.
    for path in [
        // A true UNC verbatim path has no drive-letter form to fall back to.
        r"\\?\UNC\server\share",
        // Already startable.
        r"C:\work\repo",
        // POSIX, where the prefix never appears. This is the case that makes
        // applying the rule unconditionally safe.
        "/work/repo",
        "relative/path",
        // Prefix present but not followed by a drive letter.
        r"\\?\Volume{9c8f1a}\work",
    ] {
        assert_eq!(
            super::child_process_cwd(std::path::PathBuf::from(path)),
            std::path::PathBuf::from(path),
            "{path} must be handed to the child unchanged",
        );
    }
}

#[test]
fn child_cwd_drops_the_forward_slash_spelling_a_harn_script_hands_back() {
    // Every `std/path` helper normalizes its output to forward slashes
    // (`stdlib/path.rs::to_posix`), so a canonicalized Windows path that
    // passes through a Harn script — as `workspace_root(fs)` does in
    // `experiments/burin-mini` — comes back `//?/C:/...`, not `\\?\C:\...`.
    // Windows treats `/` and `\` as interchangeable path separators, so this
    // is the same prefix and must be stripped the same way. This is the
    // shape that reproduced the "UNC paths are not supported" / "'node' is
    // not recognized" failure on a real Windows job even after the
    // backslash form above was fixed (harn#7974 fixed the backslash
    // spelling only).
    assert_eq!(
        super::child_process_cwd(std::path::PathBuf::from("//?/C:/work/repo")),
        std::path::PathBuf::from("C:/work/repo"),
    );
    // Controls: a forward-slash UNC verbatim path still has no drive-letter
    // form to fall back to, and an ordinary POSIX-looking path with no `?`
    // marker must not be mistaken for the prefix.
    for path in ["//?/UNC/server/share", "/work/repo", "relative/path"] {
        assert_eq!(
            super::child_process_cwd(std::path::PathBuf::from(path)),
            std::path::PathBuf::from(path),
            "{path} must be handed to the child unchanged",
        );
    }
}

/// A fake `PATH` directory holding one dummy "executable" (an empty file;
/// `resolve_program_path` only checks `is_file`, matching what `Command`'s
/// own OS-level PATH search does before it gets to permission bits).
fn fake_path_dir(program_name: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let candidate = dir.path().join(program_name);
    std::fs::write(&candidate, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path_value = dir.path().to_string_lossy().into_owned();
    (dir, path_value)
}

#[test]
fn resolve_program_path_finds_a_bare_name_on_the_resolved_environments_path() {
    let (dir, path_value) = fake_path_dir("myprog");
    let resolved_environment = Some(vec![("PATH".to_string(), path_value)]);
    let resolved = super::resolve_program_path("myprog", &resolved_environment, false, &[]);
    assert_eq!(resolved, dir.path().join("myprog").to_string_lossy());
}

#[test]
fn resolve_program_path_leaves_a_path_shaped_name_unchanged() {
    // Contains a separator either way, so no PATH search should even be
    // attempted — an empty env proves it, since a search would find nothing.
    for program in ["/usr/bin/node", "./node", r"C:\nodejs\node.exe"] {
        assert_eq!(
            super::resolve_program_path(program, &None, false, &[]),
            program,
        );
    }
}

#[test]
fn resolve_program_path_falls_through_unchanged_when_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let resolved_environment = Some(vec![(
        "PATH".to_string(),
        dir.path().to_string_lossy().into_owned(),
    )]);
    assert_eq!(
        super::resolve_program_path("nonexistent-program", &resolved_environment, false, &[]),
        "nonexistent-program",
    );
}

#[test]
fn resolve_program_path_uses_the_env_clear_overlay_when_env_clear_is_set() {
    let (dir, path_value) = fake_path_dir("myprog");
    let overlay = vec![("PATH".to_string(), path_value)];
    let resolved = super::resolve_program_path("myprog", &None, true, &overlay);
    assert_eq!(resolved, dir.path().join("myprog").to_string_lossy());
}

#[test]
fn resolve_program_path_falls_back_to_the_parent_process_env_in_merge_mode() {
    // Merge mode with no closed session environment and no overlay override:
    // the child inherits this process's own environment wholesale (`Command`
    // never calls `env_clear` on that path), so resolution must search THIS
    // process's real `PATH`, not the overlay (empty here) or a closed map
    // (`None` here).
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let original_path = std::env::var_os("PATH");
    let (dir, path_value) = fake_path_dir("myprog");
    std::env::set_var("PATH", &path_value);
    let resolved = super::resolve_program_path("myprog", &None, false, &[]);
    match original_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    assert_eq!(resolved, dir.path().join("myprog").to_string_lossy());
}

#[test]
fn resolve_program_path_resolved_environment_wins_over_overlay_and_process_env() {
    // A closed `resolved_environment` (the session-governed common case) is
    // authoritative: neither the overlay nor this process's own `PATH`
    // should be consulted once it is `Some`, so a program that exists only
    // in the overlay's directory must NOT resolve.
    let (_missing_dir, overlay_path_value) = fake_path_dir("myprog");
    let empty_dir = tempfile::tempdir().unwrap();
    let resolved_environment = Some(vec![(
        "PATH".to_string(),
        empty_dir.path().to_string_lossy().into_owned(),
    )]);
    let overlay = vec![("PATH".to_string(), overlay_path_value)];
    let resolved = super::resolve_program_path("myprog", &resolved_environment, false, &overlay);
    assert_eq!(resolved, "myprog", "overlay's PATH must not be consulted");
}
