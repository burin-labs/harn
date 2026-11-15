use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{apply_changes, FileChange};
use crate::fs::{
    commit_staged, discard_staged, hash_label, read, scope_conditional_replace_lock_root, set_mode,
    stage_write_or_none, staged_status, FsMode,
};

fn hash(bytes: &[u8]) -> String {
    hash_label(bytes).trim_start_matches("sha256:").to_string()
}

fn recovery_session(root: &std::path::Path) -> String {
    format!("interrupted-{}", hash(root.to_string_lossy().as_bytes()))
}

#[test]
fn file_group_crash_fixture() {
    let Some(root) = std::env::var_os("HARN_EXT_FILE_BATCH_CRASH_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let session = recovery_session(&root);
    let _locks = scope_conditional_replace_lock_root(root.join("locks"));
    let creation_first = std::env::var("HARN_EXT_FILE_BATCH_CREATE_FIRST").unwrap() == "1";
    let mode = if std::env::var("HARN_EXT_FILE_BATCH_STAGED").unwrap() == "1" {
        FsMode::Staged
    } else {
        FsMode::Immediate
    };
    let old = root.join(if creation_first { "Z.java" } else { "A.java" });
    let new = root.join(if creation_first { "A.java" } else { "Z.java" });
    set_mode(&session, mode, Some(&root)).unwrap();
    let changes = [
        FileChange {
            path: old.clone(),
            expected_hash: Some(hash(b"class Old {}")),
            contents: None,
            permissions_from: None,
        },
        FileChange {
            path: new,
            expected_hash: None,
            contents: Some(b"class New {}".to_vec()),
            permissions_from: Some(old),
        },
        FileChange {
            path: root.join("M.java"),
            expected_hash: Some(hash(b"Old reference")),
            contents: Some(b"New reference".to_vec()),
            permissions_from: None,
        },
    ];
    super::effects::EXIT_AFTER_FIRST_PUBLISH.with(|armed| armed.set(true));
    apply_changes("test", &root, &changes, Some(&session), |_| Ok(())).unwrap();
    if mode == FsMode::Staged {
        commit_staged(&session, &[]).unwrap();
    }
    panic!("the crash point was not reached");
}

fn interrupted_move(creation_first: bool, staged: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let old = root.join(if creation_first { "Z.java" } else { "A.java" });
    std::fs::write(&old, b"class Old {}").unwrap();
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(root.join("M.java"), b"Old reference").unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "fs::batch::tests::file_group_crash_fixture",
            "--nocapture",
        ])
        .env("HARN_EXT_FILE_BATCH_CRASH_ROOT", &root)
        .env(
            "HARN_EXT_FILE_BATCH_CREATE_FIRST",
            if creation_first { "1" } else { "0" },
        )
        .env("HARN_EXT_FILE_BATCH_STAGED", if staged { "1" } else { "0" })
        .output()
        .unwrap();
    assert_eq!(
        child.status.code(),
        Some(73),
        "{}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    if creation_first {
        assert_eq!(std::fs::read(root.join("A.java")).unwrap(), b"class New {}");
        assert_eq!(std::fs::read(root.join("Z.java")).unwrap(), b"class Old {}");
    } else {
        assert!(!root.join("A.java").exists());
        assert!(!root.join("Z.java").exists());
    }
    assert_eq!(
        std::fs::read(root.join("M.java")).unwrap(),
        b"Old reference"
    );
    dir
}

#[test]
fn restarted_file_moves_restore_complete_before_images_and_keep_staged_intent() {
    let _env = crate::process::test_support::declare_inherited();
    for creation_first in [false, true] {
        for staged in [false, true] {
            let dir = interrupted_move(creation_first, staged);
            let root = dir.path().canonicalize().unwrap();
            let session = recovery_session(&root);
            crate::fs::configure_session_root(&session, &root);
            let _locks = scope_conditional_replace_lock_root(root.join("locks"));
            let old = root.join(if creation_first { "Z.java" } else { "A.java" });
            let new = root.join(if creation_first { "A.java" } else { "Z.java" });
            assert!(read(&old, Some(&session)).unwrap().is_err());
            assert_eq!(staged_status(&session).unwrap().pending_writes.len(), 3);
            let error = commit_staged(&session, &[]).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("recovered an incomplete file transaction"),
                "{error}"
            );
            assert_eq!(std::fs::read(&old).unwrap(), b"class Old {}");
            assert_eq!(
                std::fs::metadata(&old).unwrap().permissions().mode() & 0o777,
                0o755
            );
            assert!(!new.exists());
            assert_eq!(
                std::fs::read(root.join("M.java")).unwrap(),
                b"Old reference"
            );
            let pending = staged_status(&session).unwrap().pending_writes.len();
            assert_eq!(pending, if staged { 3 } else { 0 });
            if staged {
                let result = commit_staged(&session, &[]).unwrap();
                assert_eq!(result.committed_paths.len(), 3);
                assert!(result.failed_paths_with_reasons.is_empty());
                assert!(!old.exists());
                assert_eq!(std::fs::read(&new).unwrap(), b"class New {}");
                assert_eq!(
                    std::fs::read(root.join("M.java")).unwrap(),
                    b"New reference"
                );
            }
        }
    }
}

#[test]
fn restarted_file_move_preserves_foreign_change_and_journal_until_repaired() {
    let _env = crate::process::test_support::declare_inherited();
    let dir = interrupted_move(true, false);
    let root = dir.path().canonicalize().unwrap();
    let session = recovery_session(&root);
    crate::fs::configure_session_root(&session, &root);
    let _locks = scope_conditional_replace_lock_root(root.join("locks"));
    let changed = root.join("A.java");
    std::fs::write(&changed, b"foreign edit").unwrap();
    let error = commit_staged(&session, &[]).unwrap_err();
    assert!(error.to_string().contains("unattributed change"), "{error}");
    assert_eq!(std::fs::read(&changed).unwrap(), b"foreign edit");
    assert_eq!(std::fs::read(root.join("Z.java")).unwrap(), b"class Old {}");
    assert_eq!(
        std::fs::read(root.join("M.java")).unwrap(),
        b"Old reference"
    );
    assert!(read(&changed, Some(&session)).unwrap().is_err());
    assert!(discard_staged(&session, &[]).is_err());
    assert!(crate::fs::remove_session_state(&session, Some(&root)).is_err());
    std::fs::write(&changed, b"class New {}").unwrap();
    let error = commit_staged(&session, &[]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("recovered an incomplete file transaction"),
        "{error}"
    );
    assert!(!changed.exists());
    assert_eq!(std::fs::read(root.join("Z.java")).unwrap(), b"class Old {}");
    assert!(staged_status(&session).unwrap().pending_writes.is_empty());
}

#[test]
fn staged_regular_files_prepare_every_target_before_publishing_any() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let _locks = scope_conditional_replace_lock_root(root.join("locks"));
    let session = uuid::Uuid::new_v4().to_string();
    let first = root.join("first/a.txt");
    let second = root.join("second/b.txt");
    for path in [&first, &second] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"before").unwrap();
    }
    set_mode(&session, FsMode::Staged, Some(&root)).unwrap();
    for path in [&first, &second] {
        stage_write_or_none("test", path, b"after", false, true, Some(&session))
            .unwrap()
            .unwrap();
    }
    let parent = second.parent().unwrap();
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = commit_staged(&session, &[]);
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = result.unwrap();
    assert!(result.committed_paths.is_empty());
    assert_eq!(result.failed_paths_with_reasons.len(), 2);
    for path in [&first, &second] {
        assert_eq!(std::fs::read(path).unwrap(), b"before");
    }
    assert_eq!(staged_status(&session).unwrap().pending_writes.len(), 2);
    let result = commit_staged(&session, &[]).unwrap();
    assert_eq!(result.committed_paths.len(), 2);
    assert!(result.failed_paths_with_reasons.is_empty());
    for path in [&first, &second] {
        assert_eq!(std::fs::read(path).unwrap(), b"after");
    }
}

#[test]
fn filtered_file_group_commit_and_discard_expand_to_the_complete_move() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let _locks = scope_conditional_replace_lock_root(root.join("locks"));
    let session = uuid::Uuid::new_v4().to_string();
    let old = root.join("Old.java");
    let new = root.join("New.java");
    std::fs::write(&old, b"class Old {}").unwrap();
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
    set_mode(&session, FsMode::Staged, Some(&root)).unwrap();
    let changes = [
        FileChange {
            path: old.clone(),
            expected_hash: Some(hash(b"class Old {}")),
            contents: None,
            permissions_from: None,
        },
        FileChange {
            path: new.clone(),
            expected_hash: None,
            contents: Some(b"class New {}".to_vec()),
            permissions_from: Some(old.clone()),
        },
    ];
    apply_changes("test", &root, &changes, Some(&session), |_| Ok(())).unwrap();
    assert_eq!(std::fs::read(&old).unwrap(), b"class Old {}");
    assert!(!new.exists());
    assert_eq!(
        read(&new, Some(&session)).unwrap().unwrap(),
        b"class New {}"
    );
    assert_eq!(
        discard_staged(&session, &[new.to_string_lossy().into_owned()])
            .unwrap()
            .discarded_paths
            .len(),
        2
    );
    assert_eq!(std::fs::read(&old).unwrap(), b"class Old {}");
    assert!(!new.exists());
    apply_changes("test", &root, &changes, Some(&session), |_| Ok(())).unwrap();
    let result = commit_staged(&session, &[new.to_string_lossy().into_owned()]).unwrap();
    assert_eq!(result.committed_paths.len(), 2);
    assert!(result.failed_paths_with_reasons.is_empty());
    assert!(!old.exists());
    assert_eq!(std::fs::read(&new).unwrap(), b"class New {}");
    assert_eq!(
        std::fs::metadata(&new).unwrap().permissions().mode() & 0o7777,
        0o755
    );
}

#[test]
fn stale_file_group_input_refuses_before_history_or_filesystem_effects() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let _locks = scope_conditional_replace_lock_root(root.join("locks"));
    let session = uuid::Uuid::new_v4().to_string();
    let path = root.join("source.java");
    std::fs::write(&path, b"current").unwrap();
    set_mode(&session, FsMode::Immediate, Some(&root)).unwrap();
    let reached = AtomicBool::new(false);
    let result = apply_changes(
        "test",
        &root,
        &[FileChange {
            path: path.clone(),
            expected_hash: Some(hash(b"stale")),
            contents: Some(b"replacement".to_vec()),
            permissions_from: None,
        }],
        Some(&session),
        |_| {
            reached.store(true, Ordering::Relaxed);
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(!reached.load(Ordering::Relaxed));
    assert_eq!(std::fs::read(path).unwrap(), b"current");
    assert!(staged_status(&session).unwrap().pending_writes.is_empty());
}
