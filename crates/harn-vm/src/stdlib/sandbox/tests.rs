use super::*;
use crate::orchestration::{pop_execution_policy, push_execution_policy};

#[test]
fn missing_create_path_normalizes_against_existing_parent() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("a/../new.txt");
    let normalized = normalize_for_policy(&nested);
    assert_eq!(
        normalized,
        normalize_for_policy(&dir.path().join("new.txt"))
    );
}

#[test]
fn empty_workspace_roots_default_to_execution_root_for_fs_paths() {
    // Serialize env mutation and clear HARN_PROJECT_ROOT so this asserts the
    // pure execution-root fallback (the project-root-env preference is
    // covered by the next test).
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::env::remove_var("HARN_PROJECT_ROOT");
    let dir = tempfile::tempdir().unwrap();
    crate::stdlib::process::set_thread_execution_context(Some(
        crate::orchestration::RunExecutionRecord {
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            project_root: None,
            source_dir: None,
            env: Default::default(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
            grants: Vec::new(),
        },
    ));
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    });

    assert!(enforce_fs_path("read_file", &dir.path().join("inside.txt"), FsAccess::Read).is_ok());
    let outside = tempfile::tempdir().unwrap();
    assert!(enforce_fs_path(
        "read_file",
        &outside.path().join("outside.txt"),
        FsAccess::Read
    )
    .is_err());

    pop_execution_policy();
    crate::stdlib::process::set_thread_execution_context(None);
}

/// Regression for burin-labs/burin-code#4266. When a restricted policy has
/// no explicit `workspace_roots`, the write jail must follow the typed
/// execution `project_root` before env/cwd fallbacks.
#[test]
fn empty_workspace_roots_prefer_execution_project_root_over_env_and_execution_root() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let project = tempfile::tempdir().unwrap();
    let env_project = tempfile::tempdir().unwrap();
    let execution_cwd = tempfile::tempdir().unwrap();
    std::env::set_var("HARN_PROJECT_ROOT", env_project.path());
    crate::stdlib::process::set_thread_execution_context(Some(
        crate::orchestration::RunExecutionRecord {
            cwd: Some(execution_cwd.path().to_string_lossy().into_owned()),
            project_root: Some(project.path().to_string_lossy().into_owned()),
            source_dir: None,
            env: Default::default(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
            grants: Vec::new(),
        },
    ));
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    });

    assert!(
        enforce_fs_path(
            "write_file",
            &project.path().join("test/created.ts"),
            FsAccess::Write,
        )
        .is_ok(),
        "write into typed project_root must be allowed"
    );
    assert!(
        enforce_fs_path(
            "write_file",
            &env_project.path().join("escape.ts"),
            FsAccess::Write,
        )
        .is_err(),
        "legacy HARN_PROJECT_ROOT must not widen an explicit execution project_root"
    );
    assert!(
        enforce_fs_path(
            "write_file",
            &execution_cwd.path().join("escape.ts"),
            FsAccess::Write,
        )
        .is_err(),
        "execution cwd outside the project must be rejected"
    );

    pop_execution_policy();
    crate::stdlib::process::set_thread_execution_context(None);
    std::env::remove_var("HARN_PROJECT_ROOT");
}

/// Regression for burin-labs/burin-code#3288. When a restricted policy has
/// no explicit `workspace_roots`, the write jail must follow the
/// host-declared `HARN_PROJECT_ROOT` project — NOT the process/execution
/// cwd. This is the eval/dispatch pattern: `burin-headless` runs from the
/// repo (`execution cwd = repo`) with `--project <fixture>` + matching
/// `HARN_PROJECT_ROOT`, and a dispatched sub-agent worker's writes resolve
/// into the fixture. Before the fix the empty-roots fallback used the
/// execution cwd (the repo), so the in-project write was rejected
/// (HARN-CAP-201) and the dispatched child wrote nothing.
#[test]
fn empty_workspace_roots_prefer_project_root_env_over_execution_root() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let project = tempfile::tempdir().unwrap();
    let execution_cwd = tempfile::tempdir().unwrap();
    std::env::set_var("HARN_PROJECT_ROOT", project.path());
    crate::stdlib::process::set_thread_execution_context(Some(
        crate::orchestration::RunExecutionRecord {
            cwd: Some(execution_cwd.path().to_string_lossy().into_owned()),
            project_root: None,
            source_dir: None,
            env: Default::default(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
            grants: Vec::new(),
        },
    ));
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    });

    // A write that resolves INTO the project is allowed even though the
    // process/execution cwd is elsewhere.
    assert!(
        enforce_fs_path(
            "write_file",
            &project.path().join("test/created.ts"),
            FsAccess::Write,
        )
        .is_ok(),
        "write into HARN_PROJECT_ROOT must be allowed"
    );
    // A write under the execution cwd (the repo, in the eval pattern) is NOT
    // the project and must still be rejected — the jail moved to the
    // project, it did not widen to both.
    assert!(
        enforce_fs_path(
            "write_file",
            &execution_cwd.path().join("escape.ts"),
            FsAccess::Write,
        )
        .is_err(),
        "write under the execution cwd (outside the project) must be rejected"
    );

    pop_execution_policy();
    crate::stdlib::process::set_thread_execution_context(None);
    std::env::remove_var("HARN_PROJECT_ROOT");
}

#[test]
fn empty_workspace_roots_default_to_execution_root_for_process_cwd() {
    let dir = tempfile::tempdir().unwrap();
    crate::stdlib::process::set_thread_execution_context(Some(
        crate::orchestration::RunExecutionRecord {
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            project_root: None,
            source_dir: None,
            env: Default::default(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
            grants: Vec::new(),
        },
    ));
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    });

    assert!(enforce_process_cwd(dir.path()).is_ok());
    let outside = tempfile::tempdir().unwrap();
    assert!(enforce_process_cwd(outside.path()).is_err());

    pop_execution_policy();
    crate::stdlib::process::set_thread_execution_context(None);
}

#[test]
fn scoped_process_sandbox_roots_concretize_empty_policy_for_command_cwd() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::env::remove_var("HARN_PROJECT_ROOT");
    let execution_root = tempfile::tempdir().unwrap();
    let command_root = tempfile::tempdir().unwrap();
    crate::stdlib::process::set_thread_execution_context(Some(
        crate::orchestration::RunExecutionRecord {
            cwd: Some(execution_root.path().to_string_lossy().into_owned()),
            project_root: None,
            source_dir: None,
            env: Default::default(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
            grants: Vec::new(),
        },
    ));
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    });

    assert!(
        enforce_process_cwd(command_root.path()).is_err(),
        "before the scoped overlay the command temp root is outside the execution-root fallback",
    );
    {
        let _guard = push_process_sandbox_scope(ProcessSandboxScope {
            workspace_roots: vec![command_root.path().to_string_lossy().into_owned()],
        })
        .unwrap();
        assert!(
            enforce_process_cwd(command_root.path()).is_ok(),
            "scoped command root must be usable as the process cwd"
        );
        assert!(
            enforce_process_cwd(execution_root.path()).is_err(),
            "the scoped root must narrow the concrete spawn jail instead of widening it"
        );
    }
    assert!(
        enforce_process_cwd(command_root.path()).is_err(),
        "the scoped command root must pop after the command spawn"
    );

    pop_execution_policy();
    crate::stdlib::process::set_thread_execution_context(None);
}

#[test]
fn scoped_process_sandbox_roots_cannot_widen_explicit_workspace_roots() {
    let workspace = tempfile::tempdir().unwrap();
    let inside = workspace.path().join("subdir");
    std::fs::create_dir(&inside).unwrap();
    let outside = tempfile::tempdir().unwrap();
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });

    assert!(
        push_process_sandbox_scope(ProcessSandboxScope {
            workspace_roots: vec![inside.to_string_lossy().into_owned()],
        })
        .is_ok(),
        "a command subroot inside the explicit workspace ceiling is allowed"
    );
    assert!(
        push_process_sandbox_scope(ProcessSandboxScope {
            workspace_roots: vec![outside.path().to_string_lossy().into_owned()],
        })
        .is_err(),
        "a command root outside the explicit workspace ceiling must be rejected"
    );

    pop_execution_policy();
}

#[test]
fn git_worktree_topology_extends_scope_for_external_git_dirs() {
    // A linked worktree's `.git` points at `<main>/.git/worktrees/<name>`
    // and its `commondir` back at `<main>/.git`, both outside the working
    // tree. Under a restricted profile the sandbox scope must reach those
    // read-write (git writes locks/index/refs there) or every git
    // subprocess fails inside an ordinary worktree checkout.
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    let git_common = main.join(".git");
    let worktree_gitdir = git_common.join("worktrees/feature");
    std::fs::create_dir_all(git_common.join("objects/info")).unwrap();
    std::fs::create_dir_all(&worktree_gitdir).unwrap();
    std::fs::write(worktree_gitdir.join("commondir"), "../..\n").unwrap();

    // A `git clone --shared`-style borrowed object store, external and
    // read-only.
    let shared_objects = tmp.path().join("source/.git/objects");
    std::fs::create_dir_all(&shared_objects).unwrap();
    std::fs::write(
        git_common.join("objects/info/alternates"),
        format!("{}\n", shared_objects.display()),
    )
    .unwrap();

    let worktree = tmp.path().join("feature");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", worktree_gitdir.display()),
    )
    .unwrap();

    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![worktree.to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });

    // The external worktree git dir and shared common dir are writable.
    assert!(
        check_fs_path_scope(&worktree_gitdir.join("index"), FsAccess::Write).is_ok(),
        "git subprocess must be able to write the worktree index"
    );
    assert!(
        check_fs_path_scope(&git_common.join("HEAD"), FsAccess::Write).is_ok(),
        "git subprocess must be able to write refs in the shared common dir"
    );
    // Borrowed alternate objects are readable but not writable.
    assert!(
        check_fs_path_scope(&shared_objects.join("pack"), FsAccess::Read).is_ok(),
        "git must be able to read borrowed alternate objects"
    );
    assert!(
        check_fs_path_scope(&shared_objects.join("pack"), FsAccess::Write).is_err(),
        "alternate object stores are read-only scope, not writable"
    );
    // A genuinely unrelated path stays out of scope.
    let outside = tmp.path().join("elsewhere/secret");
    assert!(
        check_fs_path_scope(&outside, FsAccess::Read).is_err(),
        "the git-topology extension must not widen scope beyond git's own dirs"
    );

    // The first (real) workspace root is preserved for temp-dir anchoring.
    let policy = crate::orchestration::current_execution_policy().unwrap();
    assert_eq!(
        normalized_workspace_roots(&policy).first(),
        Some(&normalize_for_policy(&worktree)),
        "the real workspace root must remain the primary anchor"
    );

    pop_execution_policy();
}

#[cfg(unix)]
#[test]
fn scoped_atomic_write_rejects_parent_swapped_to_symlink_after_policy_match() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let safe_parent = workspace.path().join("safe");
    std::fs::create_dir(&safe_parent).unwrap();
    let path = safe_parent.join("state.json");
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });
    let target = scoped_mutation_target("write_file", &path, FsAccess::Write)
        .unwrap()
        .expect("restricted policy yields scoped target");

    std::fs::remove_dir(&safe_parent).unwrap();
    std::os::unix::fs::symlink(outside.path(), &safe_parent).unwrap();
    let error = atomic_write_scoped_target(&target, b"escape").unwrap_err();
    pop_execution_policy();

    assert!(
        !outside.path().join("state.json").exists(),
        "scoped write must not follow swapped parent symlink; error={error}"
    );
}

#[cfg(unix)]
#[test]
fn scoped_write_creates_missing_parent_dirs() {
    // Regression guard for #4147: the scoped-write hardening dropped the
    // `mkdir -p` contract that `write_file` / `write_text` / `http_download`
    // relied on, surfacing downstream as "No such file or directory" when a
    // tool wrote into a not-yet-created state dir. A write to a path whose
    // ancestors are missing must recreate them (scoped) and succeed.
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("a/b/c/plan.json");
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });
    let target = scoped_mutation_target("write_file", &path, FsAccess::Write)
        .unwrap()
        .expect("restricted policy yields scoped target");
    let result = atomic_write_scoped_target(&target, b"{\"plan\":\"Redis-backed\"}");
    pop_execution_policy();

    assert!(
        result.is_ok(),
        "write must create missing parents: {result:?}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"{\"plan\":\"Redis-backed\"}".to_vec()
    );
}

#[cfg(unix)]
#[test]
fn scoped_parent_autocreate_refuses_preexisting_symlink_component() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("a")).unwrap();
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("a/b")).unwrap();
    let target = ScopedMutationTarget {
        root: workspace.path().to_path_buf(),
        relative: PathBuf::from("a/b/c/plan.json"),
    };

    let error = ensure_parent_dirs_scoped(&target).unwrap_err();

    assert!(
        !outside.path().join("c/plan.json").exists(),
        "parent creation must not follow a symlinked component; error={error}"
    );
    assert!(
        !workspace.path().join("a/b/c").exists(),
        "symlinked components must not be treated as satisfied parents"
    );
}

#[test]
fn scoped_read_check_does_not_create_missing_parent_dirs() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("a/b/c/plan.json");
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });
    let result = enforce_fs_path("read_file", &path, FsAccess::Read);
    pop_execution_policy();

    assert!(
        result.is_ok(),
        "read path inside workspace should be in scope"
    );
    assert!(
        !workspace.path().join("a").exists(),
        "read/list/stat/delete scope checks must not create ancestors"
    );
}

#[cfg(unix)]
#[test]
fn scoped_paths_refuse_excessive_component_depth() {
    let mut relative = PathBuf::new();
    for index in 0..=MAX_SCOPED_PATH_COMPONENTS {
        relative.push(format!("d{index}"));
    }

    let error = clean_relative_components(&relative).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(
        error
            .to_string()
            .contains("sandbox scoped path exceeds 256 components"),
        "unexpected error: {error}"
    );
}

// ----------------------------------------------------------------------
// (a) Windows junctions: a junction IS a directory (non-admin creatable),
// so it bypasses every leaf-only symlink check. The scoped walk must refuse
// it as an intermediate component. Junctions (mount points) need no admin
// rights, so `mklink /J` works in CI; NTFS symlinks would need elevation.
// ----------------------------------------------------------------------
#[cfg(windows)]
#[test]
fn scoped_walk_refuses_junction_intermediate_component() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("a")).unwrap();
    let link = workspace.path().join("a").join("b");
    let status = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&link)
        .arg(outside.path())
        .status()
        .expect("spawn mklink /J");
    assert!(status.success(), "mklink /J failed to plant a junction");

    let target = ScopedMutationTarget {
        root: workspace.path().to_path_buf(),
        relative: PathBuf::from("a/b/c/plan.json"),
    };
    let error = win_scoped_parent(&target, true).unwrap_err();

    assert_eq!(
        error.kind(),
        io::ErrorKind::PermissionDenied,
        "junction intermediate must be refused, got {error}"
    );
    assert!(
        !outside.path().join("c").exists(),
        "walk must not create through a junction; error={error}"
    );
}

// ----------------------------------------------------------------------
// (c) Recurrence-guard lint. runc added a linter after this bug class
// recurred; we scan this module's own source so a future edit cannot
// silently reintroduce a path-based mutation inside the scoped walk, and so
// every scoped leaf open keeps `O_NOFOLLOW`. Two invariants are guarded:
//   1. The fd-walk helpers AND the content-open fns (write/append/copy/
//      rename/mkdir) never round-trip through a path-based `std::fs`/`libc`
//      call — they must stay on the *at primitives so the parent fd carried
//      out of the walk (#4210's no-re-resolution contract) is the one the
//      write uses. A path-re-resolving write in any of those fns trips this.
//   2. Every scoped leaf open, and the directory-descent primitives, pass
//      `O_NOFOLLOW`; the Windows walk rejects reparse-point components.
//
// A source scan (not a `clippy.toml` `disallowed-methods` entry) is used
// deliberately: those raw APIs are legitimate elsewhere in this module (the
// *unscoped* fallbacks used when no sandbox is active, and the non-unix
// fallbacks) and across harn-vm, so a crate-wide clippy ban would be all
// false positives; the risky call sites are exactly the functions named
// below, which a targeted scan pins precisely.
//
// Coverage limits (deliberate): the scan is lexical, so it (a) only guards
// the *first* (unix) definition of each dual-cfg fn — the unix fd-walk is
// where the contract lives; the Windows fallbacks legitimately use `std::fs`
// after their own reparse-point validation and are checked structurally via
// the `win_walk_components` assertion below — and (b) matches call spellings,
// not semantics, so a novel escape hatch (e.g. a freshly `use`-aliased fs fn
// under a new name) would need its spelling added here.
// ----------------------------------------------------------------------
#[test]
fn scoped_walk_forbids_raw_path_filesystem_calls() {
    let src = include_str!("mod.rs");
    let locked_append_src = include_str!("locked_append.rs");
    // The tests live in a sibling module, so `mod.rs` is production-only.
    let production = src;

    // Return the `{ ... }` body of the first function whose signature line
    // starts with `sig` (the unix definitions precede the fallbacks, so the
    // fd-walk / fd-carried versions are the ones scanned).
    fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src
            .find(sig)
            .unwrap_or_else(|| panic!("scoped-walk fn not found: {sig}"));
        let open = start
            + src[start..]
                .find('{')
                .unwrap_or_else(|| panic!("no body brace for {sig}"));
        let bytes = src.as_bytes();
        let mut depth = 0usize;
        for (offset, byte) in bytes[open..].iter().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &src[open..=open + offset];
                    }
                }
                _ => {}
            }
        }
        panic!("unbalanced braces scanning {sig}");
    }

    // Path-based mutations / resolver shortcuts that must never appear in a
    // scoped fn: each re-resolves a full path string (re-introducing the
    // TOCTOU the fd-walk closes) or shortcuts symlink resolution. Bare
    // `create_dir(`/`rename(` catch `use`-imported (unqualified) calls; the
    // `std::fs::`-qualified spellings catch the fully-pathed forms not
    // subsumed by a bare match. `file.write(`/`io::copy(` (fd-based) are NOT
    // forbidden — only the path-taking `std::fs::write(`/`std::fs::copy(`.
    const FORBIDDEN: [&str; 10] = [
        "create_dir_all(",
        "create_dir(",
        "File::create(",
        "OpenOptions",
        "canonicalize(",
        "rename(",
        "remove_dir_all(",
        "std::fs::write(",
        "std::fs::copy(",
        "libc::open(", // a full-path open; the walk uses openat/open_dir_at
    ];
    for sig in [
        // fd-walk helpers.
        "fn ensure_parent_dirs_scoped(",
        "fn open_parent_dir_scoped(",
        "fn create_dir_all_scoped_target(",
        "fn create_dir_scoped_target(",
        // content-open fns: this is where #4210's "carry the parent fd into
        // the write, never re-resolve the path" contract must hold.
        "fn atomic_write_scoped_target(",
        "fn append_scoped_target(",
        "fn copy_scoped_target(",
        "fn rename_scoped_targets(",
    ] {
        let body = fn_body(production, sig);
        for needle in FORBIDDEN {
            assert!(
                !body.contains(needle),
                "{sig} must not use raw `{needle}`; stay on the fd-carried *at path"
            );
        }
    }
    let body = fn_body(
        locked_append_src,
        "pub(super) fn append_locked_scoped_target(",
    );
    for needle in FORBIDDEN {
        assert!(
            !body.contains(needle),
            "append_locked_scoped_target must not use raw `{needle}`; stay on the fd-carried *at path"
        );
    }

    // Every scoped *leaf* open must carry O_NOFOLLOW so it cannot follow a
    // swapped-in leaf symlink. Skip the `openat_file` wrapper definition
    // (its flags arrive as a parameter); only the call sites carry literals.
    for haystack in [production, locked_append_src] {
        for (idx, _) in haystack.match_indices("openat_file(") {
            if haystack[..idx].ends_with("fn ") {
                continue;
            }
            let window = &haystack[idx..(idx + 300).min(haystack.len())];
            assert!(
                window.contains("O_NOFOLLOW"),
                "openat_file call site near byte {idx} must pass O_NOFOLLOW"
            );
        }
    }
    // The directory-descent primitives must open O_NOFOLLOW too.
    for sig in ["fn open_dir_at(", "fn open_dir_absolute("] {
        assert!(
            fn_body(production, sig).contains("O_NOFOLLOW"),
            "{sig} must open directories with O_NOFOLLOW"
        );
    }

    // The Windows walk is the platform's O_NOFOLLOW substitute: it must
    // reject reparse-point (junction/symlink) components as it descends.
    assert!(
        fn_body(production, "fn win_walk_components(").contains("win_reject_reparse_point"),
        "the Windows scoped walk must reject reparse-point components"
    );
}

#[cfg(unix)]
#[test]
fn scoped_append_creates_missing_parent_dirs() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("logs/deep/qa.jsonl");
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });
    let target = scoped_mutation_target("append_file", &path, FsAccess::Write)
        .unwrap()
        .expect("restricted policy yields scoped target");
    append_scoped_target(&target, b"line1\n").unwrap();
    append_scoped_target(&target, b"line2\n").unwrap();
    pop_execution_policy();

    assert_eq!(std::fs::read(&path).unwrap(), b"line1\nline2\n".to_vec());
}

#[cfg(unix)]
#[test]
fn scoped_locked_append_creates_missing_parent_dirs() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("locks/deep/coord.ndjson");
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });
    let target = scoped_mutation_target("append_file_locked", &path, FsAccess::Write)
        .unwrap()
        .expect("restricted policy yields scoped target");
    let options = AppendLockOptions {
        timeout: std::time::Duration::from_millis(100),
        sync_data: true,
    };
    locked_append::append_locked_scoped_target(&target, b"line1\n", options).unwrap();
    locked_append::append_locked_scoped_target(&target, b"line2\n", options).unwrap();
    pop_execution_policy();

    assert_eq!(std::fs::read(&path).unwrap(), b"line1\nline2\n".to_vec());
}

// ----------------------------------------------------------------------
// (d) Two-thread race on the same auto-create (CVE-2024-45310 class). Both
// threads ensure the same deep parent chain concurrently; the loser of each
// `mkdirat` sees EEXIST and must tolerate it. All calls must succeed, the
// chain must exist exactly once inside the root, and nothing may escape.
// ----------------------------------------------------------------------
#[cfg(unix)]
#[test]
fn scoped_parent_autocreate_tolerates_concurrent_creators() {
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().to_path_buf();
    let target = ScopedMutationTarget {
        root: root.clone(),
        relative: PathBuf::from("race/deep/nested/tree/plan.json"),
    };

    let threads: Vec<_> = (0..4)
        .map(|_| {
            let target = target.clone();
            std::thread::spawn(move || {
                for _ in 0..64 {
                    // Each call re-walks from the root; the EEXIST branch is
                    // the one under contention.
                    ensure_parent_dirs_scoped(&target)
                        .expect("concurrent ensure must tolerate EEXIST");
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("worker thread panicked");
    }

    assert!(
        root.join("race/deep/nested/tree").is_dir(),
        "the raced parent chain must exist inside the root"
    );
    // A final ensure resolves cleanly and yields the leaf name.
    let (_parent, leaf) = ensure_parent_dirs_scoped(&target).unwrap();
    assert_eq!(leaf, "plan.json");
}

// A *non-directory* planted as an intermediate component must abort the walk
// (ENOTDIR from the O_DIRECTORY open), not be silently mkdir'd over — the
// walk descends by opening each level as a directory.
#[cfg(unix)]
#[test]
fn scoped_parent_autocreate_refuses_file_as_intermediate_component() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("a")).unwrap();
    std::fs::write(workspace.path().join("a/b"), b"i am a file").unwrap();
    let target = ScopedMutationTarget {
        root: workspace.path().to_path_buf(),
        relative: PathBuf::from("a/b/c/plan.json"),
    };

    let error = ensure_parent_dirs_scoped(&target).unwrap_err();

    assert_eq!(
        error.raw_os_error(),
        Some(libc::ENOTDIR),
        "a regular-file intermediate must fail with ENOTDIR, got {error:?}"
    );
    assert!(
        !workspace.path().join("a/b/c").exists(),
        "a non-directory intermediate must not be traversed or created through"
    );
}

#[test]
fn unscoped_write_creates_missing_parent_dirs() {
    // With no active sandbox scope (`scoped_mutation_target` returns None),
    // writes flow through `atomic_write_unscoped`. That path must honor the
    // same `mkdir -p` contract, otherwise a trusted-context write (CLI
    // scripts, `harn run`, conformance) into a fresh directory fails.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x/y/z/plan.json");
    atomic_write_unscoped(&path, b"{\"plan\":\"Redis-backed\"}").unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"{\"plan\":\"Redis-backed\"}".to_vec()
    );
}

#[test]
fn unscoped_append_creates_missing_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("logs/deep/qa.jsonl");
    append_unscoped(&path, b"line1\n").unwrap();
    append_unscoped(&path, b"line2\n").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"line1\nline2\n".to_vec());
}

#[cfg(unix)]
#[test]
fn scoped_write_parent_autocreate_refuses_symlinked_intermediate() {
    // Auto-creating parents must stay symlink-safe: the create walk opens
    // each level with O_NOFOLLOW, so a symlinked intermediate directory
    // cannot be used to escape the workspace root.
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("escape")).unwrap();
    let path = workspace.path().join("escape/sub/plan.json");
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });
    // The escape is refused at whichever layer sees it first: path
    // resolution (`scoped_mutation_target` canonicalizes and rejects a
    // target outside `workspace_roots`) or, for a symlink swapped in after
    // resolution, the auto-create walk itself (`O_NOFOLLOW` on each level).
    let escaped = match scoped_mutation_target("write_file", &path, FsAccess::Write) {
        Ok(Some(target)) => atomic_write_scoped_target(&target, b"escape").is_ok(),
        Ok(None) | Err(_) => false,
    };
    pop_execution_policy();

    assert!(
        !escaped,
        "must not write through a symlinked intermediate dir"
    );
    assert!(
        !outside.path().join("sub/plan.json").exists(),
        "scoped write escaped the workspace via a symlinked parent"
    );
}

#[cfg(unix)]
#[test]
fn scoped_append_rejects_final_symlink_created_after_policy_match() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let safe_parent = workspace.path().join("safe");
    std::fs::create_dir(&safe_parent).unwrap();
    let outside_file = outside.path().join("state.log");
    std::fs::write(&outside_file, b"outside").unwrap();
    let path = safe_parent.join("state.log");
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });
    let target = scoped_mutation_target("append_file", &path, FsAccess::Write)
        .unwrap()
        .expect("restricted policy yields scoped target");

    std::os::unix::fs::symlink(&outside_file, &path).unwrap();
    let error = append_scoped_target(&target, b"\nescape").unwrap_err();
    pop_execution_policy();

    assert_eq!(std::fs::read(&outside_file).unwrap(), b"outside");
    assert!(
        error.raw_os_error() == Some(libc::ELOOP)
            || error.kind() == io::ErrorKind::PermissionDenied
            || error.kind() == io::ErrorKind::Other,
        "expected symlink refusal, got {error:?}"
    );
}

#[cfg(unix)]
#[test]
fn scoped_create_dir_all_rejects_parent_swapped_to_symlink_after_policy_match() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let safe_parent = workspace.path().join("safe");
    std::fs::create_dir(&safe_parent).unwrap();
    let path = safe_parent.join("nested/deeper");
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });
    let target = scoped_mutation_target("mkdir", &path, FsAccess::Write)
        .unwrap()
        .expect("restricted policy yields scoped target");

    std::fs::remove_dir(&safe_parent).unwrap();
    std::os::unix::fs::symlink(outside.path(), &safe_parent).unwrap();
    let error = create_dir_all_scoped_target(&target).unwrap_err();
    pop_execution_policy();

    assert!(
        !outside.path().join("nested").exists(),
        "scoped mkdir must not follow swapped parent symlink; error={error}"
    );
}

#[test]
fn sandboxed_process_config_defaults_cwd_to_current_when_allowed() {
    let cwd = std::env::current_dir().unwrap();
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![cwd.to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    };

    let resolved = sandboxed_process_config(&ProcessCommandConfig::default(), &policy).unwrap();

    assert_eq!(resolved.cwd.unwrap(), normalize_for_policy(&cwd));
}

#[test]
fn sandboxed_process_config_defaults_cwd_to_workspace_when_current_is_outside() {
    let workspace = tempfile::tempdir().unwrap();
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    };

    let resolved = sandboxed_process_config(&ProcessCommandConfig::default(), &policy).unwrap();

    assert_eq!(
        resolved.cwd.unwrap(),
        normalize_for_policy(workspace.path())
    );
}

#[test]
fn sandboxed_process_config_rejects_explicit_cwd_outside_workspace() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    };
    let config = ProcessCommandConfig {
        cwd: Some(outside.path().to_path_buf()),
        ..ProcessCommandConfig::default()
    };

    assert!(sandboxed_process_config(&config, &policy).is_err());
}

#[test]
fn sandboxed_process_config_neutralizes_rustc_wrapper() {
    let cwd = std::env::current_dir().unwrap();
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![cwd.to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    };

    // A sandboxed spawn must bypass sccache so it can never spawn (and
    // thereby permanently confine) the shared daemon.
    let resolved = sandboxed_process_config(&ProcessCommandConfig::default(), &policy).unwrap();
    let env: std::collections::BTreeMap<_, _> = resolved.env.into_iter().collect();
    assert_eq!(env.get("RUSTC_WRAPPER").map(String::as_str), Some(""));
    assert_eq!(
        env.get("CARGO_BUILD_RUSTC_WRAPPER").map(String::as_str),
        Some("")
    );
}

#[test]
fn neutralize_rustc_wrapper_overrides_caller_supplied_wrapper() {
    // Even if a caller (or inherited env) asked for sccache, the sandboxed
    // config forces it off rather than appending a duplicate entry.
    let mut env = vec![
        ("RUSTC_WRAPPER".to_string(), "sccache".to_string()),
        ("PATH".to_string(), "/usr/bin".to_string()),
    ];
    neutralize_rustc_wrapper(&mut env);
    let collected: std::collections::BTreeMap<_, _> = env.iter().cloned().collect();
    assert_eq!(collected.get("RUSTC_WRAPPER").map(String::as_str), Some(""));
    assert_eq!(
        collected
            .get("CARGO_BUILD_RUSTC_WRAPPER")
            .map(String::as_str),
        Some("")
    );
    assert_eq!(collected.get("PATH").map(String::as_str), Some("/usr/bin"));
    // No duplicate RUSTC_WRAPPER entries.
    assert_eq!(env.iter().filter(|(k, _)| k == "RUSTC_WRAPPER").count(), 1);
}

#[test]
fn workspace_local_tmpdir_lands_inside_the_first_writable_root() {
    let workspace = tempfile::tempdir().unwrap();
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    };

    let tmpdir = workspace_local_tmpdir(&policy).expect("a writable root yields a temp dir");

    // The temp dir is created, lives under the writable workspace root, and
    // is named by the documented convention.
    assert!(tmpdir.is_dir(), "temp dir must be created: {tmpdir:?}");
    assert!(
        path_is_within(&tmpdir, &normalize_for_policy(workspace.path())),
        "temp dir {tmpdir:?} must be inside the writable workspace root"
    );
    assert!(tmpdir.ends_with(WORKSPACE_TMPDIR_NAME));
    // It self-ignores so its churn never shows in a git diff.
    let ignore = std::fs::read_to_string(tmpdir.join(".gitignore")).unwrap_or_default();
    assert!(
        ignore.lines().any(|line| line.trim() == "*"),
        "temp dir must carry a self-ignoring .gitignore, got {ignore:?}"
    );
    // It is within the sandbox's writable scope: a write under it passes the
    // same path-scope check the OS sandbox enforces.
    push_execution_policy(policy);
    assert!(
        check_fs_path_scope(&tmpdir.join("rustcXXXX/intermediate.o"), FsAccess::Write).is_ok(),
        "writes under the workspace-local temp dir must be in sandbox scope"
    );
    pop_execution_policy();
}

#[test]
fn inject_workspace_tmpdir_is_a_noop_under_unrestricted_profile() {
    // The unrestricted profile short-circuits the injection helper: an
    // unsandboxed child keeps whatever TMPDIR it would otherwise inherit.
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Unrestricted,
        workspace_roots: vec!["/definitely/not/writable/xyzzy".to_string()],
        ..CapabilityPolicy::default()
    };
    let mut env = Vec::new();
    inject_workspace_tmpdir(&mut env, &policy);
    assert!(
        env.is_empty(),
        "unrestricted profile must not inject a TMPDIR override, got {env:?}"
    );
}

#[test]
fn inject_workspace_tmpdir_sets_all_three_keys_inside_workspace() {
    let workspace = tempfile::tempdir().unwrap();
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    };
    let mut env = Vec::new();
    inject_workspace_tmpdir(&mut env, &policy);

    let collected: std::collections::BTreeMap<_, _> = env.into_iter().collect();
    let expected = workspace_local_tmpdir(&policy)
        .unwrap()
        .display()
        .to_string();
    for key in TMPDIR_ENV_KEYS {
        assert_eq!(
            collected.get(key).map(String::as_str),
            Some(expected.as_str()),
            "{key} must point at the workspace-local temp dir"
        );
    }
}

#[test]
fn deterministic_message_locale_env_forces_english_utf8_safe_messages() {
    let env: std::collections::BTreeMap<_, _> =
        deterministic_message_locale_env().into_iter().collect();
    // gettext tools (gcc/clang, git-l10n, coreutils, gradle) honor
    // LC_MESSAGES; `C` yields untranslated English.
    assert_eq!(env.get("LC_MESSAGES").map(String::as_str), Some("C"));
    // .NET ignores LC_* and localizes from its own variable.
    assert_eq!(
        env.get("DOTNET_CLI_UI_LANGUAGE").map(String::as_str),
        Some("en")
    );
    // Deliberately NOT setting LC_ALL/LC_CTYPE/LANG so UTF-8 handling of
    // non-ASCII source and identifiers is preserved (unlike `LC_ALL=C`).
    assert!(
        !env.contains_key("LC_ALL"),
        "must not force LC_ALL (would clobber UTF-8 ctype)"
    );
    assert!(!env.contains_key("LC_CTYPE"));
    assert!(!env.contains_key("LANG"));
    // The override-strip constant names the one variable that would defeat
    // LC_MESSAGES if inherited.
    assert_eq!(MESSAGE_LOCALE_OVERRIDE_ENV, "LC_ALL");
}

#[test]
fn inject_workspace_tmpdir_respects_a_caller_pinned_tmpdir() {
    let workspace = tempfile::tempdir().unwrap();
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    };
    // Caller already pinned TMPDIR; only the untouched siblings get filled.
    let mut env = vec![("TMPDIR".to_string(), "/caller/explicit/tmp".to_string())];
    inject_workspace_tmpdir(&mut env, &policy);

    let collected: std::collections::BTreeMap<_, _> = env.iter().cloned().collect();
    assert_eq!(
        collected.get("TMPDIR").map(String::as_str),
        Some("/caller/explicit/tmp"),
        "an explicit caller TMPDIR must be preserved untouched"
    );
    let expected = workspace_local_tmpdir(&policy)
        .unwrap()
        .display()
        .to_string();
    assert_eq!(
        collected.get("TMP").map(String::as_str),
        Some(expected.as_str())
    );
    assert_eq!(
        collected.get("TEMP").map(String::as_str),
        Some(expected.as_str())
    );
    // And no duplicate TMPDIR entry was appended.
    assert_eq!(env.iter().filter(|(k, _)| k == "TMPDIR").count(), 1);
}

#[test]
fn sandboxed_process_config_injects_workspace_tmpdir() {
    let workspace = tempfile::tempdir().unwrap();
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    };
    let config = ProcessCommandConfig {
        cwd: Some(workspace.path().to_path_buf()),
        ..ProcessCommandConfig::default()
    };
    let resolved = sandboxed_process_config(&config, &policy).unwrap();
    let env: std::collections::BTreeMap<_, _> = resolved.env.into_iter().collect();
    let expected = workspace_local_tmpdir(&policy)
        .unwrap()
        .display()
        .to_string();
    assert_eq!(
        env.get("TMPDIR").map(String::as_str),
        Some(expected.as_str()),
        "the command_output path must inject a workspace-local TMPDIR"
    );
}

#[test]
fn read_only_root_outside_workspace_allows_read_denies_write() {
    // Models an embedder (burin's in-process TUI) that grants a
    // read-only root R holding bundled pipelines/partials outside the
    // user's writable workspace. A read under R passes; a write under R
    // is denied; a read outside both R and the workspace is denied.
    let workspace = tempfile::tempdir().unwrap();
    let read_only = tempfile::tempdir().unwrap();
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        read_only_roots: vec![read_only.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });

    let asset = read_only
        .path()
        .join("partials/agent-web-tools.harn.prompt");
    // READ under the read-only root is allowed.
    assert!(
        check_fs_path_scope(&asset, FsAccess::Read).is_ok(),
        "read under a configured read-only root must be allowed"
    );

    // WRITE under the read-only root is denied, flagged read_only.
    let write_err = check_fs_path_scope(&asset, FsAccess::Write)
        .expect_err("write under a read-only root must be denied");
    assert!(write_err.read_only, "write rejection must set read_only");

    // DELETE under the read-only root is likewise denied.
    assert!(
        check_fs_path_scope(&asset, FsAccess::Delete).is_err(),
        "delete under a read-only root must be denied"
    );

    // A read inside the writable workspace still passes.
    assert!(check_fs_path_scope(&workspace.path().join("src/main.rs"), FsAccess::Read).is_ok());

    // A read outside BOTH the workspace and the read-only root is denied
    // and is NOT flagged read_only (it fell outside every root).
    let stranger = tempfile::tempdir().unwrap();
    let outside_err = check_fs_path_scope(&stranger.path().join("secret.txt"), FsAccess::Read)
        .expect_err("read outside all roots must be denied");
    assert!(
        !outside_err.read_only,
        "out-of-scope rejection must not be flagged read_only"
    );

    pop_execution_policy();
}

#[cfg(unix)]
#[test]
fn standard_io_device_files_allowed_under_restricted_profile() {
    // Writing to the standard process I/O streams is not a workspace
    // mutation, so a restricted profile with a workspace root that does
    // not contain /dev must still allow them — while a genuine
    // out-of-root write is still rejected.
    let workspace = tempfile::tempdir().unwrap();
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });

    for device in ["/dev/stdout", "/dev/stderr", "/dev/null"] {
        assert!(
            check_fs_path_scope(Path::new(device), FsAccess::Write).is_ok(),
            "write to standard device {device} must be allowed"
        );
        // Reads of the same devices are likewise allowed.
        assert!(
            check_fs_path_scope(Path::new(device), FsAccess::Read).is_ok(),
            "read of standard device {device} must be allowed"
        );
    }
    assert!(
        check_fs_path_scope(Path::new("/dev/stdin"), FsAccess::Read).is_ok(),
        "read of standard device /dev/stdin must be allowed"
    );
    assert!(
        check_fs_path_scope(Path::new("/dev/stdin"), FsAccess::Write).is_err(),
        "write to /dev/stdin is not a standard output stream"
    );
    assert!(
        check_fs_path_scope(Path::new("/dev/null"), FsAccess::Delete).is_err(),
        "standard devices must not bypass delete scoping"
    );
    // Numeric /dev/fd/<N> descriptors are allowed.
    assert!(check_fs_path_scope(Path::new("/dev/fd/1"), FsAccess::Write).is_ok());
    assert!(check_fs_path_scope(Path::new("/dev/fd/2"), FsAccess::Write).is_ok());

    // A non-device path outside the workspace is still rejected.
    let stranger = tempfile::tempdir().unwrap();
    assert!(
        check_fs_path_scope(&stranger.path().join("escape.txt"), FsAccess::Write).is_err(),
        "a real out-of-root write must still be rejected"
    );
    // Other /dev entries are NOT broadly allowed — the allowlist is narrow.
    assert!(
        check_fs_path_scope(Path::new("/dev/sda"), FsAccess::Write).is_err(),
        "/dev/sda must not be allowed by the standard-device allowlist"
    );
    assert!(
        check_fs_path_scope(Path::new("/dev/fd/notanumber"), FsAccess::Write).is_err(),
        "non-numeric /dev/fd/<x> must not be allowed"
    );

    pop_execution_policy();
}

#[test]
fn is_standard_io_device_matches_only_known_streams() {
    assert!(is_standard_io_device_for_access(
        Path::new("/dev/stdin"),
        FsAccess::Read
    ));
    assert!(!is_standard_io_device_for_access(
        Path::new("/dev/stdin"),
        FsAccess::Write
    ));
    assert!(is_standard_io_device_for_access(
        Path::new("/dev/stdout"),
        FsAccess::Write
    ));
    assert!(is_standard_io_device_for_access(
        Path::new("/dev/stderr"),
        FsAccess::Write
    ));
    assert!(is_standard_io_device_for_access(
        Path::new("/dev/null"),
        FsAccess::Write
    ));
    assert!(is_standard_io_device_for_access(
        Path::new("/dev/fd/0"),
        FsAccess::Read
    ));
    assert!(is_standard_io_device_for_access(
        Path::new("/dev/fd/12"),
        FsAccess::Write
    ));
    assert!(!is_standard_io_device_for_access(
        Path::new("/dev/null"),
        FsAccess::Delete
    ));
    assert!(!is_standard_io_device_for_access(
        Path::new("/dev/fd/"),
        FsAccess::Write
    ));
    assert!(!is_standard_io_device_for_access(
        Path::new("/dev/fd/1a"),
        FsAccess::Write
    ));
    assert!(!is_standard_io_device_for_access(
        Path::new("/dev/stdoutx"),
        FsAccess::Write
    ));
    assert!(!is_standard_io_device_for_access(
        Path::new("/dev/random"),
        FsAccess::Read
    ));
    assert!(!is_standard_io_device_for_access(
        Path::new("/tmp/dev/null"),
        FsAccess::Write
    ));
}

#[test]
fn path_within_root_accepts_root_and_children() {
    let root = Path::new("/tmp/harn-root");
    assert!(path_is_within(root, root));
    assert!(path_is_within(Path::new("/tmp/harn-root/file"), root));
    assert!(!path_is_within(
        Path::new("/tmp/harn-root-other/file"),
        root
    ));
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn developer_toolchain_roots_cover_common_home_managed_runtimes() {
    let temp_home = tempfile::tempdir().expect("temp home");
    let roots = developer_toolchain_read_roots_for_home(temp_home.path());
    let normalized_home = normalize_for_policy(temp_home.path());

    for suffix in [
        Path::new(".cargo"),
        Path::new(".rustup"),
        Path::new(".pyenv"),
        Path::new(".nvm"),
        Path::new(".volta"),
        Path::new(".local/share/uv"),
        Path::new("go"),
    ] {
        assert!(
            roots.iter().any(|path| path.ends_with(suffix)),
            "expected a developer-toolchain grant for {}",
            suffix.display()
        );
    }
    assert!(
        roots.iter().all(|path| path.starts_with(&normalized_home)),
        "developer-toolchain roots must stay under HOME"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn developer_toolchain_cache_roots_cover_jvm_and_ios_toolchains() {
    let temp_home = tempfile::tempdir().expect("temp home");
    let roots = developer_toolchain_cache_write_roots_for_home(temp_home.path());
    let normalized_home = normalize_for_policy(temp_home.path());

    for suffix in [
        Path::new(".gradle"),
        Path::new(".m2"),
        Path::new(".konan"),
        Path::new("Library/Caches/CocoaPods"),
        Path::new("Library/Developer/Xcode/DerivedData"),
    ] {
        assert!(
            roots.iter().any(|path| path.ends_with(suffix)),
            "expected a JVM/iOS toolchain cache grant for {}",
            suffix.display()
        );
    }
    assert!(
        roots.iter().all(|path| path.starts_with(&normalized_home)),
        "toolchain cache roots must stay under HOME"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn developer_toolchain_cache_roots_require_developer_toolchains_preset() {
    let mut policy = CapabilityPolicy {
        workspace_roots: vec!["/tmp/harn-workspace".to_string()],
        ..CapabilityPolicy::default()
    };
    // Default presets include DeveloperToolchains -> cache roots present
    // (only when an absolute HOME is resolvable on this host).
    if sandbox_user_home_dir().is_some() {
        assert!(
            !process_sandbox_developer_toolchain_cache_roots(&policy).is_empty(),
            "default presets should render JVM/iOS cache roots"
        );
    }
    // Explicitly dropping DeveloperToolchains removes them.
    policy.process_sandbox.presets = Some(vec![ProcessSandboxPreset::SystemRuntime]);
    assert!(
        process_sandbox_developer_toolchain_cache_roots(&policy).is_empty(),
        "cache roots must be gated on the DeveloperToolchains preset"
    );
}

#[test]
fn os_hardened_profile_overrides_fallback_env() {
    // `OsHardened` ignores `HARN_HANDLER_SANDBOX=off` — the whole
    // point of the profile is that the OS sandbox is required.
    // We cannot mutate the env here without races, so just check
    // the pure resolution function.
    assert_eq!(
        effective_fallback(SandboxProfile::OsHardened),
        SandboxFallback::Enforce
    );
}

#[test]
fn unrestricted_profile_skips_active_sandbox() {
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Unrestricted,
        workspace_roots: vec!["/tmp".to_string()],
        ..Default::default()
    };
    crate::orchestration::push_execution_policy(policy);
    let result = active_sandbox_policy();
    crate::orchestration::pop_execution_policy();
    assert!(
        result.is_none(),
        "Unrestricted profile must short-circuit sandbox dispatch"
    );
}

#[test]
fn worktree_profile_engages_active_sandbox() {
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec!["/tmp".to_string()],
        ..Default::default()
    };
    crate::orchestration::push_execution_policy(policy);
    let result = active_sandbox_policy();
    crate::orchestration::pop_execution_policy();
    assert!(
        result.is_some(),
        "Worktree profile must keep sandbox dispatch active"
    );
}
