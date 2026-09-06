use std::io::{Read, Write};
use std::path::Path;

use harn_session_store::{
    session_lease_directory, CreateSession, RetentionPolicy, SessionLeaseError,
    SessionMaintenanceLease, SessionStore, SessionWriteLease, SqliteSessionStore, StoreContention,
    StoreError,
};

use super::process_test_support::ProcessTestChild;

const PROCESS_TEST_ROOT: &str = "HARN_SESSION_LEASE_PROCESS_TEST_ROOT";
const PROCESS_TEST_ACTION: &str = "HARN_SESSION_LEASE_PROCESS_TEST_ACTION";
const PROCESS_TEST_DATABASE: &str = "HARN_SESSION_LEASE_PROCESS_TEST_DATABASE";
const PROCESS_TEST_CHANGED_CWD: &str = "HARN_SESSION_LEASE_PROCESS_TEST_CHANGED_CWD";

#[test]
fn lease_process_child() {
    let Some(root) = std::env::var_os(PROCESS_TEST_ROOT) else {
        return;
    };
    match std::env::var(PROCESS_TEST_ACTION)
        .expect("child action")
        .as_str()
    {
        "hold-writer" => {
            let _lease = SessionWriteLease::try_acquire(Path::new(&root), "live-session")
                .expect("child writer lease");
            println!("READY");
            std::io::stdout().flush().expect("flush ready signal");
            std::io::stdin()
                .read_exact(&mut [0_u8; 1])
                .expect("read release signal");
            println!("DONE");
        }
        "probe-writer" => {
            let error = SessionWriteLease::try_acquire(Path::new(&root), "new-session")
                .expect_err("parent maintenance lease must exclude child writer");
            assert!(matches!(error, SessionLeaseError::Contended { .. }));
            println!("CONTENDED");
        }
        action => panic!("unknown child action {action}"),
    }
}

#[tokio::test]
async fn cwd_store_process_child() {
    let Some(database) = std::env::var_os(PROCESS_TEST_DATABASE) else {
        return;
    };
    let changed_cwd = std::env::var_os(PROCESS_TEST_CHANGED_CWD).expect("changed cwd");
    let store = SqliteSessionStore::open(Path::new(&database)).expect("child ordinary store");
    std::env::set_current_dir(&changed_cwd).expect("change child cwd");
    println!("READY");
    std::io::stdout().flush().expect("flush ready signal");
    std::io::stdin()
        .read_exact(&mut [0_u8; 1])
        .expect("read mutation signal");

    let error = store
        .create(CreateSession::default())
        .await
        .expect_err("maintenance must exclude mutation after cwd changes");
    assert!(matches!(
        error,
        StoreError::Contention {
            kind: StoreContention::MaintenanceActive,
            ..
        }
    ));
    println!("CONTENDED");
}

#[test]
fn writer_and_maintenance_exclude_each_other_across_processes() {
    let root = tempfile::tempdir().expect("lease root");
    let mut writer = ProcessTestChild::spawn("lease_process_child", |command| {
        command
            .env(PROCESS_TEST_ROOT, root.path())
            .env(PROCESS_TEST_ACTION, "hold-writer");
    });
    writer.wait_for("READY");

    let error = SessionMaintenanceLease::try_acquire(root.path())
        .expect_err("child writer must exclude parent maintenance");
    assert!(matches!(error, SessionLeaseError::Contended { .. }));
    writer.send(b"release\n");
    writer.wait_for("DONE");
    writer.wait_success();

    let _maintenance =
        SessionMaintenanceLease::try_acquire(root.path()).expect("parent maintenance lease");
    let mut blocked_writer = ProcessTestChild::spawn("lease_process_child", |command| {
        command
            .env(PROCESS_TEST_ROOT, root.path())
            .env(PROCESS_TEST_ACTION, "probe-writer");
    });
    blocked_writer.wait_for("CONTENDED");
    blocked_writer.wait_success();
}

#[test]
fn killed_writer_releases_its_project_lease() {
    let root = tempfile::tempdir().expect("lease root");
    let mut writer = ProcessTestChild::spawn("lease_process_child", |command| {
        command
            .env(PROCESS_TEST_ROOT, root.path())
            .env(PROCESS_TEST_ACTION, "hold-writer");
    });
    writer.wait_for("READY");
    writer.kill_and_reap();

    SessionMaintenanceLease::try_acquire(root.path())
        .expect("the operating system releases a killed writer's lease");
}

#[test]
fn mutation_lock_domain_survives_cwd_change() {
    for relative_open in [true, false] {
        let root = tempfile::tempdir().expect("store root");
        let changed_cwd = root.path().join("changed-cwd");
        std::fs::create_dir(&changed_cwd).expect("changed cwd directory");
        let database = root.path().join("session-store.sqlite");
        let child_database = if relative_open {
            Path::new("session-store.sqlite")
        } else {
            database.as_path()
        };
        let mut child = ProcessTestChild::spawn("cwd_store_process_child", |command| {
            command
                .current_dir(root.path())
                .env(PROCESS_TEST_DATABASE, child_database)
                .env(PROCESS_TEST_CHANGED_CWD, &changed_cwd);
        });
        child.wait_for("READY");

        let _maintenance =
            SqliteSessionStore::open_for_maintenance(&database).expect("parent maintenance store");
        child.send(b"mutate\n");
        child.wait_for("CONTENDED");
        child.wait_success();
    }
}

#[tokio::test]
async fn maintenance_store_owns_mutation_admission_once() {
    let root = tempfile::tempdir().expect("store root");
    let database = root.path().join("session-store.sqlite");
    let ordinary = SqliteSessionStore::open(&database).expect("ordinary store");
    let error = ordinary
        .sweep_retention(&RetentionPolicy::default(), 0)
        .await
        .expect_err("file-backed retention requires maintenance ownership");
    assert!(matches!(
        error,
        StoreError::MaintenanceRequired {
            operation: "sweeping retention"
        }
    ));
    let maintenance =
        SqliteSessionStore::open_for_maintenance(&database).expect("maintenance store");

    let competing_maintenance = match SqliteSessionStore::open_for_maintenance(&database) {
        Ok(_) => panic!("a second maintenance owner must not enter"),
        Err(error) => error,
    };
    assert!(matches!(
        competing_maintenance,
        StoreError::Contention {
            kind: StoreContention::ProjectLeaseHeld,
            ..
        }
    ));

    let error = ordinary
        .create(CreateSession::default())
        .await
        .expect_err("ordinary mutation must not enter during maintenance");
    assert!(matches!(
        error,
        StoreError::Contention {
            kind: StoreContention::MaintenanceActive,
            ..
        }
    ));

    let session = maintenance
        .create(CreateSession::default())
        .await
        .expect("maintenance owner does not reacquire its own shared lease");
    maintenance
        .hard_delete(&session.id)
        .await
        .expect("maintenance mutation");
    let lease_directory = session_lease_directory(root.path());
    assert!(
        lease_directory.is_dir(),
        "maintenance preserves lease directory"
    );
    assert!(
        lease_directory.join("project.lock").is_file(),
        "maintenance preserves the project lock inode"
    );

    drop(maintenance);
    ordinary
        .create(CreateSession::default())
        .await
        .expect("ordinary mutation enters after maintenance drops");
}
