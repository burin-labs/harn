use super::*;

#[test]
fn fork_refuses_when_a_call_starts_and_finishes_during_copy() {
    let (dir, id) = deleted_in_subdir("fork-race-call");
    let child = format!("{id}-child");
    let outcome = fork::fork_session_checked(&id, &child, dir.path(), || {
        call_begin(&id, "concurrent").unwrap();
        stdfs::write(dir.path().join("sub/f.txt"), "later edit\n").unwrap();
        call_end(&id, "concurrent", None).unwrap();
    })
    .unwrap();
    let value = serde_json::to_value(outcome).unwrap();
    assert_eq!(value["reason"], "source_changed", "{value}");
    assert!(find_session(&child).is_none());
    assert!(!session_dir(dir.path(), &child).exists());
    drop_session(&id);
}

#[test]
fn fork_refuses_when_a_restore_changes_the_same_boundary_during_copy() {
    let (dir, id) = deleted_in_subdir("fork-race-restore");
    let child = format!("{id}-child");
    let outcome = fork::fork_session_checked(&id, &child, dir.path(), || {
        assert!(matches!(
            restore(&id, "turn-1", None).unwrap(),
            Outcome::Ready(_)
        ));
        assert_eq!(
            stdfs::read_to_string(dir.path().join("sub/f.txt")).unwrap(),
            "f original\n"
        );
    })
    .unwrap();
    let value = serde_json::to_value(outcome).unwrap();
    assert_eq!(value["reason"], "source_changed", "{value}");
    assert!(find_session(&child).is_none());
    assert!(!session_dir(dir.path(), &child).exists());
    drop_session(&id);
}
use std::os::unix::fs::symlink;

#[test]
fn a_new_runtime_path_is_excluded_by_the_scan_without_a_second_list() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    stdfs::create_dir_all(root.join(".harn/future-runtime")).unwrap();
    stdfs::create_dir_all(root.join(".harn/skills")).unwrap();
    stdfs::write(root.join(".harn/future-runtime/state"), "runtime").unwrap();
    stdfs::write(root.join(".harn/skills/user.md"), "user").unwrap();
    let mut declared = crate::workspace_runtime_paths::owned_paths(root);
    declared.push(root.join(".harn/future-runtime"));
    let scan = scan_tree_with_exclusions(root, false, declared);
    assert_eq!(scan.errors, 0);
    assert!(!scan.index.contains_key(".harn/future-runtime/state"));
    assert!(scan.index.contains_key(".harn/skills/user.md"));
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// A worktree with `sub/f.txt` deleted by a tool call after boundary
/// `turn-1`, so restoring recreates it. Returns (root dir, session id).
fn deleted_in_subdir(tag: &str) -> (tempfile::TempDir, String) {
    deleted_leaf_in_subdir(tag, "f.txt")
}

fn deleted_leaf_in_subdir(tag: &str, leaf: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@example.com"]);
    git(root, &["config", "user.name", "t"]);
    stdfs::create_dir(root.join("sub")).unwrap();
    stdfs::write(root.join("sub").join(leaf), "f original\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);
    let id = format!("anchor-{tag}-{}", std::process::id());
    assert!(matches!(
        capture(&id, "turn-1", Some(root)).unwrap(),
        Outcome::Ready(_)
    ));
    call_begin(&id, "tool-1").unwrap();
    stdfs::remove_file(root.join("sub").join(leaf)).unwrap();
    call_end(&id, "tool-1", None).unwrap();
    (dir, id)
}

fn swap_sub_for_symlink(root: &Path, outside: &Path) {
    stdfs::rename(root.join("sub"), root.join("sub.moved")).unwrap();
    symlink(outside, root.join("sub")).unwrap();
}

fn with_hook(stage: Stage, hook: impl Fn() + 'static) {
    RESTORE_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |at| {
            if at == stage {
                hook();
            }
        }));
    });
}

fn clear_hook() {
    RESTORE_HOOK.with(|slot| *slot.borrow_mut() = None);
}

#[test]
fn checkpoint_operations_refuse_before_call_begin_returns() {
    let (dir, id) = deleted_in_subdir("beginning");
    let hook_id = id.clone();
    let hook_root = dir.path().to_path_buf();
    CALL_BEGIN_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            for outcome in [
                serde_json::to_value(plan(&hook_id, "turn-1").unwrap()).unwrap(),
                serde_json::to_value(restore(&hook_id, "turn-1", None).unwrap()).unwrap(),
                serde_json::to_value(capture(&hook_id, "turn-2", Some(&hook_root)).unwrap())
                    .unwrap(),
                serde_json::to_value(
                    fork_session(&hook_id, "beginning-child", &hook_root).unwrap(),
                )
                .unwrap(),
            ] {
                assert_eq!(outcome["available"], false, "{outcome}");
                assert_eq!(outcome["reason"], "calls_in_flight");
                assert!(outcome["detail"].as_str().unwrap().contains("tool-2"));
            }
            assert!(!hook_root.join("sub/f.txt").exists());
            assert!(call_begin(&hook_id, "tool-2").is_err());
            assert!(call_end(&hook_id, "tool-2", None).is_err());
            let outcome = serde_json::to_value(plan(&hook_id, "turn-1").unwrap()).unwrap();
            assert_eq!(outcome["reason"], "calls_in_flight", "{outcome}");
        }));
    });
    call_begin(&id, "tool-2").unwrap();
    CALL_BEGIN_HOOK.with(|slot| *slot.borrow_mut() = None);
    call_end(&id, "tool-2", None).unwrap();
    assert!(matches!(plan(&id, "turn-1").unwrap(), Outcome::Ready(_)));
    call_begin(&id, "tool-2").unwrap();
    call_end(&id, "tool-2", None).unwrap();
    drop_session(&id);
}

#[test]
fn restore_and_fork_refuse_while_the_call_end_scan_is_running() {
    let (dir, id) = deleted_in_subdir("ending");
    call_begin(&id, "tool-2").unwrap();
    stdfs::write(dir.path().join("sub/f.txt"), "new contents\n").unwrap();
    let hook_id = id.clone();
    let hook_root = dir.path().to_path_buf();
    CALL_END_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            for outcome in [
                serde_json::to_value(plan(&hook_id, "turn-1").unwrap()).unwrap(),
                serde_json::to_value(restore(&hook_id, "turn-1", None).unwrap()).unwrap(),
                serde_json::to_value(fork_session(&hook_id, "ending-child", &hook_root).unwrap())
                    .unwrap(),
            ] {
                assert_eq!(outcome["available"], false, "{outcome}");
                assert_eq!(outcome["reason"], "calls_in_flight");
                assert!(outcome["detail"].as_str().unwrap().contains("tool-2"));
            }
            assert_eq!(
                stdfs::read_to_string(hook_root.join("sub/f.txt")).unwrap(),
                "new contents\n"
            );
        }));
    });
    call_end(&id, "tool-2", None).unwrap();
    CALL_END_HOOK.with(|slot| *slot.borrow_mut() = None);
    assert!(matches!(plan(&id, "turn-1").unwrap(), Outcome::Ready(_)));
    drop_session(&id);
}

#[test]
fn strict_restore_preserves_a_file_created_after_the_final_guard() {
    let (dir, id) = deleted_in_subdir("final-guard");
    lock(&find_session(&id).unwrap()).writer_policy = WriterPolicy::RequireKnown;
    let root = dir.path().to_path_buf();
    with_hook(Stage::AfterFinalGuard, move || {
        stdfs::write(root.join("sub/f.txt"), "person's concurrent edit\n").unwrap();
    });
    let confirmation = ConfirmedPath {
        session_id: id.clone(),
        boundary_id: "turn-1".to_string(),
        path: "sub/f.txt".to_string(),
        observed: ObservedFileState::Absent,
    };
    let result = restore_with_confirmations(&id, "turn-1", None, &[confirmation]).unwrap();
    clear_hook();
    let Outcome::Ready(receipt) = result else {
        panic!("restore unavailable");
    };
    assert!(receipt.restored.is_empty(), "{receipt:?}");
    assert_eq!(
        stdfs::read_to_string(dir.path().join("sub/f.txt")).unwrap(),
        "person's concurrent edit\n"
    );
    drop_session(&id);
}

#[test]
fn ancestor_swapped_after_planning_fails_the_path() {
    let (dir, id) = deleted_in_subdir("before");
    let outside = tempfile::TempDir::new().unwrap();
    let (root, out) = (dir.path().to_path_buf(), outside.path().to_path_buf());
    with_hook(Stage::BeforeWalk, move || swap_sub_for_symlink(&root, &out));
    let Outcome::Ready(receipt) = restore(&id, "turn-1", None).unwrap() else {
        panic!("restore unavailable");
    };
    clear_hook();
    drop_session(&id);
    assert!(receipt.restored.is_empty(), "{receipt:?}");
    let failed: Vec<_> = receipt.failed.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(failed, ["sub/f.txt"], "{receipt:?}");
    assert_eq!(stdfs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn ancestor_swapped_after_the_walk_cannot_escape_the_root() {
    let (dir, id) = deleted_in_subdir("after");
    let outside = tempfile::TempDir::new().unwrap();
    let (root, out) = (dir.path().to_path_buf(), outside.path().to_path_buf());
    with_hook(Stage::AfterWalk, move || swap_sub_for_symlink(&root, &out));
    let result = restore(&id, "turn-1", None);
    clear_hook();
    drop_session(&id);
    result.unwrap();
    assert_eq!(
        stdfs::read_dir(outside.path()).unwrap().count(),
        0,
        "a swapped ancestor redirected the write outside the root"
    );
}

#[test]
fn restore_recreates_a_file_with_a_255_byte_name() {
    let leaf = "a".repeat(255);
    let (dir, id) = deleted_leaf_in_subdir("longname", &leaf);
    let Outcome::Ready(receipt) = restore(&id, "turn-1", None).unwrap() else {
        panic!("restore unavailable");
    };
    drop_session(&id);
    assert!(receipt.failed.is_empty(), "{receipt:?}");
    let restored = stdfs::read_to_string(dir.path().join("sub").join(&leaf)).unwrap();
    assert_eq!(restored, "f original\n");
}
