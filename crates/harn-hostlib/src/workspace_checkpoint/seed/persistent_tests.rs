use super::*;
use std::fs;

#[cfg(unix)]
mod inherited_lease;

fn run_isolated(name: &str, case: fn()) {
    const WORKER: &str = "HARN_TEST_SEED_PERSISTENCE";
    let test = format!("workspace_checkpoint::seed::persistent_tests::{name}");
    let completed = format!("seed persistence completed: {test}");
    if std::env::var(WORKER).as_deref() == Ok(test.as_str()) {
        case();
        println!("{completed}");
        return;
    }
    // A neighboring test can fork with our lease open and retain it until exec.
    // Keep lease ownership in a process that runs only this case.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &test, "--nocapture"])
        .env(WORKER, &test)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{test}:\n{stdout}\n{stderr}");
    assert!(
        stdout.lines().any(|line| line == completed),
        "case did not complete: {stdout}"
    );
}

#[derive(Deserialize, Serialize)]
struct Invocation {
    saved: SavedReplayWorkspace,
    busy: bool,
    marker: PathBuf,
}

#[test]
fn saved_workspace_restarts_in_a_new_process_and_excludes_competing_owners() {
    run_isolated(
        "saved_workspace_restarts_in_a_new_process_and_excludes_competing_owners",
        saved_workspace_restarts_case,
    );
}

fn saved_workspace_restarts_case() {
    const CHILD: &str = "WORKSPACE_SEED_TEST_SAVED_CHILD";
    const TEST: &str = "workspace_checkpoint::seed::persistent_tests::saved_workspace_restarts_in_a_new_process_and_excludes_competing_owners";
    if let Some(path) = std::env::var_os(CHILD) {
        let invocation: Invocation = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let result = ReplayWorkspace::restore_saved(&invocation.saved);
        if invocation.busy {
            assert!(matches!(result, Err(SeedError::InUse { .. })));
            fs::write(invocation.marker, "busy without restoring").unwrap();
        } else {
            let workspace = result.unwrap();
            assert_eq!(workspace.receipt(), invocation.saved.receipt());
            assert_eq!(
                workspace.path(),
                invocation.saved.storage_path().join("workspace")
            );
            assert_eq!(
                fs::read_to_string(workspace.path().join("file")).unwrap(),
                "initial"
            );
            fs::write(workspace.path().join("file"), "child exit").unwrap();
            fs::write(invocation.marker, "restored in child").unwrap();
            // The next owner must acquire the lease without this handle's Drop.
            std::process::exit(0);
        }
        return;
    }
    let source = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    fs::write(source.path().join("file"), "initial").unwrap();
    let mut workspace =
        ReplayWorkspace::capture_in(source.path(), storage.path(), SeedLimits::default()).unwrap();
    let saved = workspace.persist();
    let path = workspace.path().to_path_buf();
    fs::write(path.join("file"), "trial changed").unwrap();
    let request = storage.path().join("invocation.json");
    let marker = storage.path().join("child-completed");
    let invoke = |busy| {
        fs::write(
            &request,
            serde_json::to_vec(&Invocation {
                saved: saved.clone(),
                busy,
                marker: marker.clone(),
            })
            .unwrap(),
        )
        .unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, &request)
            .status()
            .unwrap();
        assert!(status.success());
    };
    invoke(true);
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "busy without restoring"
    );
    assert_eq!(
        fs::read_to_string(path.join("file")).unwrap(),
        "trial changed"
    );
    drop(workspace);
    assert!(path.exists(), "persistent storage survives its first owner");
    fs::remove_file(&marker).unwrap();
    invoke(false);
    assert_eq!(fs::read_to_string(&marker).unwrap(), "restored in child");
    assert_eq!(fs::read_to_string(path.join("file")).unwrap(), "child exit");
    let reopened = ReplayWorkspace::restore_saved(&saved).unwrap();
    assert_eq!(reopened.path(), path);
    assert_eq!(fs::read_to_string(path.join("file")).unwrap(), "initial");
    assert!(matches!(
        ReplayWorkspace::restore_saved(&saved),
        Err(SeedError::InUse { .. })
    ));
    drop(reopened);
    assert!(path.exists(), "closing a reopened handle retains the seed");
}

#[test]
fn saved_seed_tampering_refuses_without_replacing_the_previous_trial() {
    run_isolated(
        "saved_seed_tampering_refuses_without_replacing_the_previous_trial",
        saved_seed_tampering_case,
    );
}

fn saved_seed_tampering_case() {
    let source = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    fs::write(source.path().join("file"), "initial").unwrap();
    let mut workspace =
        ReplayWorkspace::capture_in(source.path(), storage.path(), SeedLimits::default()).unwrap();
    let saved = workspace.persist();
    let path = workspace.path().to_path_buf();
    fs::write(path.join("file"), "keep trial").unwrap();
    drop(workspace);
    fs::write(saved.storage_path().join("seed/file"), "tampered").unwrap();
    assert!(matches!(
        ReplayWorkspace::restore_saved(&saved),
        Err(SeedError::SeedChanged)
    ));
    assert_eq!(fs::read_to_string(path.join("file")).unwrap(), "keep trial");
    let mut descriptor = serde_json::to_value(&saved).unwrap();
    descriptor["receipt"]["schema"] = serde_json::json!("unknown");
    assert!(serde_json::from_value::<SavedReplayWorkspace>(descriptor).is_err());
}

#[test]
fn saved_restore_recovers_both_interrupted_publish_states() {
    run_isolated(
        "saved_restore_recovers_both_interrupted_publish_states",
        saved_restore_recovers_case,
    );
}

fn saved_restore_recovers_case() {
    let source = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    fs::write(source.path().join("file"), "initial").unwrap();
    let mut workspace =
        ReplayWorkspace::capture_in(source.path(), storage.path(), SeedLimits::default()).unwrap();
    let saved = workspace.persist();
    let path = workspace.path().to_path_buf();
    drop(workspace);
    for published in [false, true] {
        fs::write(path.join("file"), "previous trial").unwrap();
        fs::rename(&path, saved.storage_path().join("previous")).unwrap();
        let abandoned = saved.storage_path().join("restore-interrupted");
        fs::create_dir(&abandoned).unwrap();
        fs::write(abandoned.join("partial"), "unfinished copy").unwrap();
        if published {
            fs::create_dir(&path).unwrap();
            fs::write(path.join("file"), "published before cleanup").unwrap();
        }
        let restored = ReplayWorkspace::restore_saved(&saved).unwrap();
        assert_eq!(restored.path(), path);
        assert_eq!(fs::read_to_string(path.join("file")).unwrap(), "initial");
        assert!(!saved.storage_path().join("previous").exists());
        assert!(!abandoned.exists());
        drop(restored);
    }
}

#[cfg(unix)]
#[test]
fn saved_restore_keeps_external_links_and_private_permissions_outside_its_authority() {
    run_isolated(
        "saved_restore_keeps_external_links_and_private_permissions_outside_its_authority",
        saved_restore_keeps_external_links_case,
    );
}

#[cfg(unix)]
fn saved_restore_keeps_external_links_case() {
    use std::os::unix::{fs::symlink, fs::PermissionsExt};
    let source = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(source.path().join("file"), "initial").unwrap();
    fs::write(outside.path().join("canary"), "outside").unwrap();
    let mut workspace =
        ReplayWorkspace::capture_in(source.path(), storage.path(), SeedLimits::default()).unwrap();
    let saved = workspace.persist();
    drop(workspace);
    symlink(
        outside.path(),
        saved.storage_path().join("restore-abandoned-link"),
    )
    .unwrap();
    symlink(outside.path(), saved.storage_path().join("previous")).unwrap();
    let restored = ReplayWorkspace::restore_saved(&saved).unwrap();
    assert_eq!(
        fs::read_to_string(outside.path().join("canary")).unwrap(),
        "outside"
    );
    drop(restored);
    fs::set_permissions(saved.storage_path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        ReplayWorkspace::restore_saved(&saved),
        Err(SeedError::Unsupported { .. })
    ));
    assert_eq!(
        fs::metadata(saved.storage_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    fs::set_permissions(saved.storage_path(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::rename(
        saved.storage_path().join("seed"),
        saved.storage_path().join("old-seed"),
    )
    .unwrap();
    symlink(outside.path(), saved.storage_path().join("seed")).unwrap();
    assert!(matches!(
        ReplayWorkspace::restore_saved(&saved),
        Err(SeedError::Unsupported { .. })
    ));
    assert_eq!(
        fs::read_to_string(outside.path().join("canary")).unwrap(),
        "outside"
    );
}
