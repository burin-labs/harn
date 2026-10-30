use super::*;
use std::fs;

#[cfg(unix)]
#[test]
fn private_storage_stays_private_under_an_ordinary_umask() {
    use std::os::unix::fs::PermissionsExt;
    const CHILD: &str = "WORKSPACE_SEED_TEST_UMASK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let evidence = tempfile::tempdir().unwrap();
        let marker = evidence.path().join("completed");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "workspace_checkpoint::seed::tests::private_storage_stays_private_under_an_ordinary_umask", "--nocapture"])
            .env(CHILD, &marker)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            fs::read_to_string(marker).unwrap(),
            "capture and restore verified"
        );
        return;
    }
    // Only this isolated test process changes its process-wide creation mask.
    unsafe {
        libc::umask(0o022);
    }
    let parent = tempfile::tempdir().unwrap();
    fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let source = tempfile::tempdir().unwrap();
    fs::set_permissions(source.path(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(source.path().join("canary"), "harmless private seed canary").unwrap();
    fs::set_permissions(
        source.path().join("canary"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let mut replay =
        ReplayWorkspace::capture_in(source.path(), parent.path(), SeedLimits::default()).unwrap();
    assert_eq!(
        fs::metadata(replay.storage.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::read_to_string(replay.path().join("canary")).unwrap(),
        "harmless private seed canary"
    );
    replay.restore().unwrap();
    assert_eq!(
        fs::metadata(replay.storage.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    fs::write(
        std::env::var_os(CHILD).unwrap(),
        "capture and restore verified",
    )
    .unwrap();
}

#[test]
fn restores_the_complete_private_copy_at_the_same_path() {
    let source = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("ignored/empty")).unwrap();
    fs::write(source.path().join(".gitignore"), "ignored/\n").unwrap();
    fs::write(source.path().join("ignored/data.bin"), [0, 255, 1]).unwrap();
    fs::write(source.path().join("input.txt"), "initial").unwrap();
    let mut replay = ReplayWorkspace::capture(source.path(), SeedLimits::default()).unwrap();
    assert_eq!(replay.receipt().files, 3);
    assert_eq!(replay.receipt().entries, 5);
    assert_eq!(replay.receipt().bytes, 19);
    let original_receipt = replay.receipt().clone();
    let path = replay.path().to_path_buf();
    fs::write(source.path().join("input.txt"), "source changed later").unwrap();
    fs::write(path.join("input.txt"), "trial changed").unwrap();
    fs::remove_file(path.join("ignored/data.bin")).unwrap();
    fs::write(path.join("new.txt"), "trial only").unwrap();
    assert_eq!(replay.restore().unwrap(), &original_receipt);
    assert_eq!(replay.path(), path);
    assert_eq!(
        fs::read_to_string(path.join("input.txt")).unwrap(),
        "initial"
    );
    assert_eq!(
        fs::read(path.join("ignored/data.bin")).unwrap(),
        [0, 255, 1]
    );
    assert!(path.join("ignored/empty").is_dir());
    assert!(!path.join("new.txt").exists());
    assert_eq!(
        fs::read_to_string(source.path().join("input.txt")).unwrap(),
        "source changed later"
    );
    assert_eq!(replay.restore().unwrap(), &original_receipt);
    drop(replay);
    assert!(!path.exists());
    assert!(source.path().join("input.txt").exists());
}

#[test]
fn a_changed_seed_refuses_before_replacing_the_working_copy() {
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("file"), "seed").unwrap();
    let mut replay = ReplayWorkspace::capture(source.path(), SeedLimits::default()).unwrap();
    fs::write(replay.path().join("file"), "keep this trial").unwrap();
    fs::write(replay.storage.path().join("seed/file"), "tampered").unwrap();
    assert!(matches!(replay.restore(), Err(SeedError::SeedChanged)));
    assert_eq!(
        fs::read_to_string(replay.path().join("file")).unwrap(),
        "keep this trial"
    );
}

#[test]
fn limits_refuse_measured_nonempty_sources_and_accept_the_exact_budget() {
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("file"), "seed").unwrap();
    for limits in [
        SeedLimits {
            max_entries: 0,
            ..SeedLimits::default()
        },
        SeedLimits {
            max_file_bytes: 3,
            ..SeedLimits::default()
        },
        SeedLimits {
            max_bytes: 3,
            ..SeedLimits::default()
        },
        SeedLimits {
            max_depth: 0,
            ..SeedLimits::default()
        },
    ] {
        assert!(matches!(
            ReplayWorkspace::capture(source.path(), limits),
            Err(SeedError::Limit { .. })
        ));
    }
    let replay = ReplayWorkspace::capture(
        source.path(),
        SeedLimits {
            max_entries: 1,
            max_file_bytes: 4,
            max_bytes: 4,
            max_depth: 1,
        },
    )
    .unwrap();
    assert_eq!(replay.receipt().files, 1);
    assert_eq!(replay.receipt().bytes, 4);
}

#[test]
fn storage_inside_the_source_is_refused_before_capture() {
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("file"), "seed").unwrap();
    assert!(matches!(
        ReplayWorkspace::capture_in(source.path(), source.path(), SeedLimits::default()),
        Err(SeedError::Unsupported { .. })
    ));
    assert_eq!(fs::read_dir(source.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn symlinks_refuse_instead_of_following_an_external_source() {
    let source = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    fs::write(external.path().join("private"), "outside").unwrap();
    std::os::unix::fs::symlink(external.path(), source.path().join("outside")).unwrap();
    assert!(matches!(
        ReplayWorkspace::capture(source.path(), SeedLimits::default()),
        Err(SeedError::Unsupported { .. })
    ));
    assert_eq!(
        fs::read_to_string(external.path().join("private")).unwrap(),
        "outside"
    );
}

#[cfg(unix)]
#[test]
fn internal_links_survive_restore_and_relative_escapes_refuse() {
    let source = tempfile::tempdir().unwrap();
    fs::create_dir(source.path().join("docs")).unwrap();
    fs::write(source.path().join("AGENTS.md"), "instructions").unwrap();
    std::os::unix::fs::symlink("AGENTS.md", source.path().join("CLAUDE.md")).unwrap();
    std::os::unix::fs::symlink("../AGENTS.md", source.path().join("docs/guide")).unwrap();
    let mut replay = ReplayWorkspace::capture(source.path(), SeedLimits::default()).unwrap();
    assert_eq!(replay.receipt().files, 1);
    assert_eq!(replay.receipt().entries, 4);
    fs::remove_file(replay.path().join("CLAUDE.md")).unwrap();
    replay.restore().unwrap();
    assert_eq!(
        fs::read_link(replay.path().join("CLAUDE.md")).unwrap(),
        Path::new("AGENTS.md")
    );
    assert_eq!(
        fs::read_to_string(replay.path().join("docs/guide")).unwrap(),
        "instructions"
    );
    std::os::unix::fs::symlink("../../", source.path().join("docs/outside")).unwrap();
    assert!(matches!(
        ReplayWorkspace::capture(source.path(), SeedLimits::default()),
        Err(SeedError::Unsupported { .. })
    ));
}

#[cfg(unix)]
#[test]
fn executable_permissions_and_backslash_names_survive_restore() {
    use std::os::unix::fs::PermissionsExt;
    let source = tempfile::tempdir().unwrap();
    fs::create_dir(source.path().join("a")).unwrap();
    fs::write(source.path().join("a/b"), "nested").unwrap();
    fs::write(source.path().join("a\\b"), "literal backslash").unwrap();
    fs::set_permissions(source.path().join("a/b"), fs::Permissions::from_mode(0o755)).unwrap();
    let mut replay = ReplayWorkspace::capture(source.path(), SeedLimits::default()).unwrap();
    assert_eq!(replay.receipt().files, 2);
    fs::remove_file(replay.path().join("a\\b")).unwrap();
    replay.restore().unwrap();
    assert_eq!(
        fs::read_to_string(replay.path().join("a\\b")).unwrap(),
        "literal backslash"
    );
    assert_eq!(
        fs::metadata(replay.path().join("a/b"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[cfg(unix)]
#[test]
fn directory_permissions_survive_restore_and_private_cleanup() {
    use std::os::unix::fs::PermissionsExt;
    let source = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("readonly/child")).unwrap();
    fs::write(source.path().join("readonly/child/file"), "unchanged").unwrap();
    fs::set_permissions(
        source.path().join("readonly"),
        fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    fs::set_permissions(source.path(), fs::Permissions::from_mode(0o750)).unwrap();
    let mut replay = ReplayWorkspace::capture(source.path(), SeedLimits::default()).unwrap();
    let storage = replay.storage.path().to_path_buf();
    for restored in [false, true] {
        if restored {
            replay.restore().unwrap();
        }
        assert_eq!(
            fs::metadata(replay.path()).unwrap().permissions().mode() & 0o777,
            0o750
        );
        assert_eq!(
            fs::metadata(replay.path().join("readonly"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
        assert!(fs::write(replay.path().join("readonly/new"), "must refuse").is_err());
        assert_eq!(
            fs::read_to_string(replay.path().join("readonly/child/file")).unwrap(),
            "unchanged"
        );
    }
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("canary"), "outside the trial").unwrap();
    fs::set_permissions(outside.path(), fs::Permissions::from_mode(0o555)).unwrap();
    std::os::unix::fs::symlink(outside.path(), replay.path().join("outside")).unwrap();
    fs::set_permissions(
        replay.path().join("readonly/child"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    fs::set_permissions(replay.path(), fs::Permissions::from_mode(0o000)).unwrap();
    replay.restore().unwrap();
    assert_eq!(
        fs::metadata(outside.path()).unwrap().permissions().mode() & 0o777,
        0o555
    );
    assert_eq!(
        fs::read_to_string(outside.path().join("canary")).unwrap(),
        "outside the trial"
    );
    fs::set_permissions(outside.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        fs::read_to_string(replay.path().join("readonly/child/file")).unwrap(),
        "unchanged"
    );
    // Directory-only tampering must change seed identity before replacement.
    fs::set_permissions(
        replay.storage.path().join("seed/readonly"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(matches!(replay.restore(), Err(SeedError::SeedChanged)));
    drop(replay);
    assert!(!storage.exists());
    fs::set_permissions(
        source.path().join("readonly"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
}
