use super::*;
use filetime::FileTime;
use tempfile::tempdir;

fn artifact_dir(parent: &Path, pid: u32, nanos: u128, counter: u64) -> PathBuf {
    parent.join(format!("harn-command-cmd_{pid}_{nanos}_{counter}"))
}

fn create_artifact_dir(parent: &Path, pid: u32, nanos: u128, counter: u64) -> PathBuf {
    let path = artifact_dir(parent, pid, nanos, counter);
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("combined.txt"), "output").unwrap();
    path
}

fn set_dir_mtime(path: &Path, time: SystemTime) {
    let file_time = FileTime::from_system_time(time);
    filetime::set_file_mtime(path, file_time).unwrap();
}

fn artifacts_in(dir: &Path) -> CommandArtifacts {
    CommandArtifacts {
        output_path: dir.join("combined.txt"),
        stdout_path: dir.join("stdout.txt"),
        stderr_path: dir.join("stderr.txt"),
        line_count: 0,
        byte_count: 0,
        output_sha256: String::new(),
    }
}

fn dead_pid() -> u32 {
    (900_000..=999_999)
        .find(|pid| {
            crate::process_liveness::process_liveness(*pid)
                == crate::process_liveness::ProcessLiveness::Dead
        })
        .expect("test host should have an unused high pid")
}

#[test]
fn command_artifact_sweep_deletes_stale_artifact_dirs() {
    let temp = tempdir().unwrap();
    let now = SystemTime::now();
    let stale = create_artifact_dir(temp.path(), dead_pid(), 100, 1);
    set_dir_mtime(&stale, now - Duration::from_secs(10));

    sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);

    assert!(!stale.exists());
}

#[test]
fn command_artifact_sweep_preserves_recent_artifact_dirs() {
    let temp = tempdir().unwrap();
    let now = SystemTime::now();
    let recent = create_artifact_dir(temp.path(), dead_pid(), 100, 1);
    set_dir_mtime(&recent, now - Duration::from_secs(3));

    sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);

    assert!(recent.exists());
}

#[test]
fn command_artifact_sweep_removes_completed_current_process_artifact_dirs() {
    let temp = tempdir().unwrap();
    let now = SystemTime::now();
    let completed = create_artifact_dir(temp.path(), std::process::id(), 100, 1);
    set_dir_mtime(&completed, now - Duration::from_secs(10));

    sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);

    assert!(!completed.exists());
}

#[test]
fn command_artifact_sweep_preserves_active_current_process_artifact_dirs() {
    let temp = tempdir().unwrap();
    let now = SystemTime::now();
    let active = create_artifact_dir(temp.path(), std::process::id(), 100, 1);
    let artifacts = CommandArtifacts {
        output_path: active.join("combined.txt"),
        stdout_path: active.join("stdout.txt"),
        stderr_path: active.join("stderr.txt"),
        line_count: 0,
        byte_count: 0,
        output_sha256: String::new(),
    };
    mark_artifacts_active(&artifacts).unwrap();
    set_dir_mtime(&active, now - Duration::from_secs(10));

    sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);

    assert!(active.exists());
    mark_artifacts_inactive(&artifacts);
}

#[test]
fn command_artifact_sweep_uses_cross_process_active_lease_not_pid_liveness() {
    let temp = tempdir().unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_hours(1);
    let active = create_artifact_dir(temp.path(), dead_pid(), 100, 1);
    let lease_path = active.join(ACTIVE_LEASE_FILE);
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lease_path)
        .unwrap();
    lease.lock().unwrap();
    set_dir_mtime(&active, now - Duration::from_secs(10));

    sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);
    assert!(active.exists());

    lease.unlock().unwrap();
    sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);
    assert!(!active.exists());
}

#[test]
fn contended_command_artifact_lease_names_itself() {
    let temp = tempdir().unwrap();
    let active = create_artifact_dir(temp.path(), dead_pid(), 100, 1);
    let artifacts = CommandArtifacts {
        output_path: active.join("combined.txt"),
        stdout_path: active.join("stdout.txt"),
        stderr_path: active.join("stderr.txt"),
        line_count: 0,
        byte_count: 0,
        output_sha256: String::new(),
    };
    let lease_path = active.join(ACTIVE_LEASE_FILE);
    let holder = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lease_path)
        .unwrap();
    holder.lock().unwrap();

    let error = mark_artifacts_active_with_timeout(&artifacts, Duration::ZERO).unwrap_err();

    assert!(error
        .to_string()
        .contains(&lease_path.display().to_string()));
    assert!(error.to_string().contains("timed out"));
}

#[test]
fn namespace_admission_precedes_artifact_directory_publication() {
    let temp = tempdir().unwrap();
    let dir = artifact_dir(temp.path(), std::process::id(), 200, 1);
    let artifacts = CommandArtifacts {
        output_path: dir.join("combined.txt"),
        stdout_path: dir.join("stdout.txt"),
        stderr_path: dir.join("stderr.txt"),
        line_count: 0,
        byte_count: 0,
        output_sha256: String::new(),
    };
    let namespace_path = artifact_namespace_lease_path(temp.path());
    let holder = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&namespace_path)
        .unwrap();
    holder.lock().unwrap();

    let error =
        create_and_mark_artifacts_active_with_timeout(&artifacts, Duration::ZERO).unwrap_err();

    assert!(error
        .to_string()
        .contains(&namespace_path.display().to_string()));
    assert!(!dir.exists(), "directory became visible before its lease");
}

/// Unix only: the descriptor budget this pins is a Unix per-process limit,
/// and neither descriptor directory exists on Windows. The integration test
/// that drives the real tool is gated the same way.
#[cfg(unix)]
fn open_descriptor_count() -> usize {
    let dir = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    std::fs::read_dir(dir)
        .expect("the process must be able to enumerate its own descriptors")
        .count()
}

/// The falsifier for the descriptor exhaustion, at the seam that owns the
/// lease.
///
/// Each completed command used to leave its active-lease descriptor open
/// until the artifact was retired, so a session paid one descriptor per
/// command. The retention cap is deliberately set far above the number of
/// commands here: if retirement were the only thing releasing descriptors
/// this would grow by one per command, which is the pre-fix behavior and
/// what the negative control shows.
#[cfg(unix)]
#[test]
fn completed_commands_do_not_accumulate_lease_descriptors() {
    let temp = tempdir().unwrap();
    let unretired = 100_000;
    let cycle = |counter: u64| {
        let dir = artifact_dir(temp.path(), std::process::id(), 400, counter);
        let artifacts = artifacts_in(&dir);
        create_and_mark_artifacts_active_with_timeout(&artifacts, Duration::from_secs(5)).unwrap();
        register_completed_artifacts_with_guard_options(
            &format!("command-descriptors-{counter}"),
            Some(&format!("handle-descriptors-{counter}")),
            &artifacts,
            ActiveArtifactLeaseGuard::new(&artifacts),
            unretired,
            Duration::from_secs(5),
        )
        .unwrap();
        dir
    };

    // Warm up first: the namespace lease and any lazily-opened global
    // must already be counted, or their one-time cost reads as growth.
    for counter in 0..5 {
        cycle(counter);
    }
    let before = open_descriptor_count();
    for counter in 5..125 {
        cycle(counter);
    }
    let after = open_descriptor_count();

    // The gate runs each test in its own process, so this counts almost
    // nothing but the code under test. The slack covers descriptors the run
    // itself opens transiently. The defect this pins spends one per command,
    // so the bound sits far below the command count either way.
    const UNRELATED_SLACK: usize = 16;
    assert!(
        after <= before + UNRELATED_SLACK,
        "descriptor count grew from {before} to {after} across 120 completed commands"
    );
    assert!(
        ACTIVE_ARTIFACT_LEASES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "a completed command must hold no active lease"
    );
}

/// A dead session's lease is collected; a live one's is left alone.
///
/// Without this the namespace would gather one small file per harn
/// process that ever ran there, which is a slower version of the problem
/// the session lease exists to solve.
#[test]
fn stale_session_leases_are_collected_and_live_ones_are_kept() {
    let temp = tempdir().unwrap();
    let dead = session_lease_path(temp.path(), dead_pid());
    std::fs::write(&dead, b"").unwrap();

    // A live one, held exactly the way a running session holds it.
    let live_path = session_lease_path(temp.path(), dead_pid() - 1);
    let live = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&live_path)
        .unwrap();
    live.lock().unwrap();

    // This process's own lease is never a candidate, held or not.
    let own = session_lease_path(temp.path(), std::process::id());
    std::fs::write(&own, b"").unwrap();

    sweep_stale_session_leases(temp.path());

    assert!(!dead.exists(), "a lease no session holds must be collected");
    assert!(live_path.exists(), "a held lease must be left alone");
    assert!(own.exists(), "this process's own lease must be left alone");
    live.unlock().unwrap();
}

#[test]
fn registration_failure_releases_active_lease_without_namespace_reacquisition() {
    let temp = tempdir().unwrap();
    let dir = artifact_dir(temp.path(), std::process::id(), 300, 1);
    let artifacts = artifacts_in(&dir);
    create_and_mark_artifacts_active_with_timeout(&artifacts, Duration::ZERO).unwrap();

    let namespace_path = artifact_namespace_lease_path(temp.path());
    let holder = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&namespace_path)
        .unwrap();
    holder.lock().unwrap();

    let error = register_completed_artifacts_with_guard_options(
        "command-registration-failure",
        Some("handle-registration-failure"),
        &artifacts,
        ActiveArtifactLeaseGuard::new(&artifacts),
        1,
        Duration::ZERO,
    )
    .unwrap_err();

    assert!(error
        .to_string()
        .contains(&namespace_path.display().to_string()));
    assert!(
        !ACTIVE_ARTIFACT_LEASES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&dir),
        "failure guard must release without waiting for the held namespace lock"
    );
    let probe = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join(ACTIVE_LEASE_FILE))
        .unwrap();
    probe.try_lock().unwrap();
    probe.unlock().unwrap();
    drop(probe);
    holder.unlock().unwrap();
    drop(holder);
}

#[test]
fn completed_fifo_evicts_oldest_aliases_and_releases_its_lease() {
    let temp = tempdir().unwrap();
    let first_dir = artifact_dir(temp.path(), std::process::id(), 400, 1);
    let second_dir = artifact_dir(temp.path(), std::process::id(), 400, 2);
    let first = artifacts_in(&first_dir);
    let second = artifacts_in(&second_dir);
    create_and_mark_artifacts_active_with_timeout(&first, Duration::ZERO).unwrap();
    create_and_mark_artifacts_active_with_timeout(&second, Duration::ZERO).unwrap();

    let mut store = ArtifactRegistry::default();
    store.by_id.insert("command-first".into(), first.clone());
    store.by_id.insert("handle-first".into(), first);
    store.by_id.insert("command-second".into(), second.clone());
    store.by_id.insert("handle-second".into(), second.clone());
    store.completed.push_back(CompletedArtifact {
        path: first_dir.clone(),
        completed_at: SystemTime::UNIX_EPOCH,
    });
    store.completed.push_back(CompletedArtifact {
        path: second_dir.clone(),
        completed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
    });

    retire_completed_artifacts_under_namespace(&mut store, 1, None);

    assert!(!store.by_id.contains_key("command-first"));
    assert!(!store.by_id.contains_key("handle-first"));
    assert!(store.by_id.contains_key("command-second"));
    assert!(store.by_id.contains_key("handle-second"));
    assert_eq!(store.completed.len(), 1);
    let active = ACTIVE_ARTIFACT_LEASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(!active.contains_key(&first_dir));
    assert!(active.contains_key(&second_dir));
    drop(active);
    mark_artifacts_inactive(&second);
}

#[test]
fn fallback_registration_uses_the_same_bounded_alias_fifo() {
    let temp = tempdir().unwrap();
    let first_dir = artifact_dir(temp.path(), std::process::id(), 500, 1);
    let second_dir = artifact_dir(temp.path(), std::process::id(), 500, 2);
    let first = artifacts_in(&first_dir);
    let second = artifacts_in(&second_dir);
    let mut store = ArtifactRegistry::default();

    register_completed_artifacts_in_store(
        &mut store,
        "fallback-command-first",
        Some("fallback-handle-first"),
        &first,
        1,
        ArtifactLeaseCleanup::LeaseOnly,
    );
    register_completed_artifacts_in_store(
        &mut store,
        "fallback-command-second",
        Some("fallback-handle-second"),
        &second,
        1,
        ArtifactLeaseCleanup::LeaseOnly,
    );

    assert!(!store.by_id.contains_key("fallback-command-first"));
    assert!(!store.by_id.contains_key("fallback-handle-first"));
    assert!(store.by_id.contains_key("fallback-command-second"));
    assert!(store.by_id.contains_key("fallback-handle-second"));
    assert_eq!(store.completed.len(), 1);
    assert_eq!(store.completed.front().unwrap().path, second_dir);
}

#[test]
fn command_artifact_sweep_preserves_malformed_names() {
    let temp = tempdir().unwrap();
    let now = SystemTime::now();
    let malformed = temp.path().join("harn-command-cmd_123_not-nanos_1");
    std::fs::create_dir(&malformed).unwrap();
    set_dir_mtime(&malformed, now - Duration::from_secs(10));

    sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);

    assert!(malformed.exists());
}

#[cfg(unix)]
#[test]
fn command_artifact_sweep_does_not_follow_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().unwrap();
    let now = SystemTime::now();
    let target = temp.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep.txt"), "keep").unwrap();
    let link = artifact_dir(temp.path(), dead_pid(), 100, 1);
    symlink(&target, &link).unwrap();

    sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);

    assert!(link.exists());
    assert_eq!(
        std::fs::read_to_string(target.join("keep.txt")).unwrap(),
        "keep"
    );
}

#[test]
fn command_artifact_pressure_sweep_removes_oldest_dead_dirs_over_limit() {
    let temp = tempdir().unwrap();
    let now = SystemTime::now();
    let pid = dead_pid();
    let first = create_artifact_dir(temp.path(), pid, 100, 1);
    let second = create_artifact_dir(temp.path(), pid, 200, 1);
    let third = create_artifact_dir(temp.path(), pid, 300, 1);
    set_dir_mtime(&first, now - Duration::from_mins(30));
    set_dir_mtime(&second, now - Duration::from_mins(20));
    set_dir_mtime(&third, now - Duration::from_mins(10));

    sweep_command_artifact_dirs(temp.path(), Duration::from_hours(1), 2, now);

    assert!(!first.exists());
    assert!(second.exists());
    assert!(third.exists());
}
