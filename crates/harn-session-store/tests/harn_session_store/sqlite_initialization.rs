use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use harn_session_store::{CreateSession, ListFilter, SessionStore, SqliteSessionStore};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::process_test_support::ProcessTestChild;

const PROCESS_TEST_DATABASE: &str = "HARN_SESSION_STORE_PROCESS_TEST_DATABASE";
const PROCESS_TEST_SESSION_ID: &str = "HARN_SESSION_STORE_PROCESS_TEST_SESSION_ID";

#[test]
fn sqlite_store_process_child() {
    let Some(database) = std::env::var_os(PROCESS_TEST_DATABASE) else {
        return;
    };
    let session_id = std::env::var(PROCESS_TEST_SESSION_ID).expect("child session id");

    println!("READY");
    std::io::stdout().flush().expect("flush ready signal");
    let mut release = [0_u8; 1];
    std::io::stdin()
        .read_exact(&mut release)
        .expect("read release signal");

    let store = SqliteSessionStore::open(&database).expect("open shared session store");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build child runtime");
    let (created, described) = runtime.block_on(async {
        let created = store
            .create(CreateSession {
                id: Some(session_id.clone()),
                ..CreateSession::default()
            })
            .await
            .expect("create child session");
        let described = store
            .describe(&session_id)
            .await
            .expect("read child session");
        (created, described)
    });
    assert_eq!(described, created);
    println!("DONE");
}

#[test]
fn sqlite_first_open_is_serialized_across_processes() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("first-open.sqlite");
    let mut children = vec![
        spawn_store_child(&path, "first-open-a"),
        spawn_store_child(&path, "first-open-b"),
    ];
    run_store_children(&mut children);

    let connection = rusqlite::Connection::open(path).expect("inspect shared session store");
    let journal_mode = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
        .expect("journal mode")
        .to_ascii_lowercase();
    let schema_version = connection
        .query_row(
            "SELECT version FROM _harn_sqlite_schema_versions WHERE name = 'session_store'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("session-store schema marker");
    let session_ids = connection
        .prepare("SELECT id FROM sessions ORDER BY id")
        .expect("prepare session query")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query sessions")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect sessions");
    assert_eq!(
        (journal_mode.as_str(), schema_version, session_ids),
        (
            "wal",
            SqliteSessionStore::SCHEMA_VERSION,
            vec!["first-open-a".to_string(), "first-open-b".to_string()]
        )
    );
}

#[test]
fn sqlite_read_only_open_reads_without_a_durable_delta() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("read-only.sqlite");
    let store = SqliteSessionStore::open(&path).expect("initialize store");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        store
            .create(CreateSession {
                id: Some("inspect-me".to_string()),
                ..CreateSession::default()
            })
            .await
            .expect("seed session");
    });
    let lock = initialization_lock_path(&path);
    std::fs::remove_file(&lock).expect("remove initializer lock before inventory");
    let before = durable_file_digests_ignoring_wal_index(dir.path());
    assert!(
        before
            .iter()
            .any(|(relative_path, _)| relative_path == Path::new("read-only.sqlite-wal")),
        "fixture must exercise committed WAL-backed state"
    );

    let reader = SqliteSessionStore::open_read_only(&path).expect("open read-only store");
    let sessions = runtime
        .block_on(reader.list(ListFilter::default()))
        .expect("list through read-only handle");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "inspect-me");
    drop(reader);

    assert_eq!(durable_file_digests_ignoring_wal_index(dir.path()), before);
    assert!(!lock.exists(), "inspection must not recreate the init lock");
    drop(store);
}

#[test]
fn sqlite_read_only_handle_denies_mutations_without_a_durable_delta() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("read-only.sqlite");
    drop(SqliteSessionStore::open(&path).expect("initialize store"));
    let lock = initialization_lock_path(&path);
    std::fs::remove_file(&lock).expect("remove initializer lock before inventory");
    let before = durable_file_digests(dir.path());
    let reader = SqliteSessionStore::open_read_only(&path).expect("open read-only store");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    let error = runtime
        .block_on(reader.create(CreateSession {
            id: Some("must-not-exist".to_string()),
            ..CreateSession::default()
        }))
        .expect_err("read-only handle must reject create");
    assert!(error.to_string().to_ascii_lowercase().contains("readonly"));
    drop(reader);

    assert_eq!(durable_file_digests(dir.path()), before);
    assert!(
        !lock.exists(),
        "denied mutation must not recreate the init lock"
    );
}

#[test]
fn sqlite_read_only_open_rejects_missing_and_malformed_inputs_without_writes() {
    let dir = TempDir::new().expect("tempdir");
    let missing = dir.path().join("absent").join("session-store.sqlite");
    assert!(SqliteSessionStore::open_read_only(&missing).is_err());
    assert!(!missing.exists());
    assert!(!missing.parent().expect("missing parent").exists());

    let malformed = dir.path().join("malformed.sqlite");
    std::fs::write(&malformed, b"not a sqlite database").expect("write malformed fixture");
    let before = durable_file_digests(dir.path());
    assert!(SqliteSessionStore::open_read_only(&malformed).is_err());
    assert_eq!(durable_file_digests(dir.path()), before);
    assert!(!initialization_lock_path(&malformed).exists());
}

fn initialization_lock_path(database: &Path) -> PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push(".harn-init.lock");
    PathBuf::from(path)
}

fn durable_file_digests(root: &Path) -> Vec<(PathBuf, String)> {
    file_digests(root, false)
}

/// Same inventory without the `-shm` wal-index, which SQLite rewrites with
/// reader bookkeeping whenever a live connection reads. Only valid when this
/// process already holds the store open.
fn durable_file_digests_ignoring_wal_index(root: &Path) -> Vec<(PathBuf, String)> {
    file_digests(root, true)
}

fn file_digests(root: &Path, ignore_wal_index: bool) -> Vec<(PathBuf, String)> {
    fn collect(
        root: &Path,
        directory: &Path,
        ignore_wal_index: bool,
        inventory: &mut Vec<(PathBuf, String)>,
    ) {
        for entry in std::fs::read_dir(directory).expect("read inventory directory") {
            let entry = entry.expect("read inventory entry");
            let path = entry.path();
            if entry.file_type().expect("inventory file type").is_dir() {
                collect(root, &path, ignore_wal_index, inventory);
                continue;
            }
            if ignore_wal_index && path.to_string_lossy().ends_with("-shm") {
                continue;
            }
            let bytes = std::fs::read(&path).expect("read inventory bytes");
            inventory.push((
                path.strip_prefix(root)
                    .expect("relative inventory path")
                    .to_path_buf(),
                hex::encode(Sha256::digest(bytes)),
            ));
        }
    }

    let mut inventory = Vec::new();
    collect(root, root, ignore_wal_index, &mut inventory);
    inventory.sort_by(|left, right| left.0.cmp(&right.0));
    inventory
}

fn spawn_store_child(database: &Path, session_id: &str) -> ProcessTestChild {
    ProcessTestChild::spawn("sqlite_store_process_child", |command| {
        command
            .env(PROCESS_TEST_DATABASE, database)
            .env(PROCESS_TEST_SESSION_ID, session_id);
    })
}

fn run_store_children(children: &mut [ProcessTestChild]) {
    for child in children.iter_mut() {
        child.wait_for("READY");
    }
    for child in children.iter_mut() {
        child.send(b"initialize\n");
    }
    for child in children.iter_mut() {
        child.wait_for("DONE");
        child.wait_success();
    }
}

const SNAPSHOT_PEER_ENV: &str = "HARN_SESSION_STORE_SNAPSHOT_PEER_DATABASE";
const SNAPSHOT_SCENARIO_ENV: &str = "HARN_SESSION_STORE_SNAPSHOT_SCENARIO_DIR";

/// Runs only inside the child spawned by
/// `sqlite_read_only_snapshot_keeps_the_wal_index_intact`; a lost WAL index
/// lock surfaces as SIGBUS on the mapped `-shm`, which would kill the runner.
#[test]
fn sqlite_read_only_snapshot_scenario_child() {
    let Some(dir) = std::env::var_os(SNAPSHOT_SCENARIO_ENV) else {
        return;
    };
    let path = PathBuf::from(dir).join("snapshot-scenario.sqlite");
    let shm = harn_session_store::wal_watch::wal_sidecar_path(&path).with_extension("sqlite-shm");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let create = |store: &SqliteSessionStore, id: &str| {
        runtime.block_on(async {
            store
                .create(CreateSession {
                    id: Some(id.to_string()),
                    ..CreateSession::default()
                })
                .await
                .expect("create session");
        });
    };

    let writer = SqliteSessionStore::open(&path).expect("open writer");
    create(&writer, "seed");
    let watcher = harn_session_store::wal_watch::open_watch_reader(&path).expect("watch reader");
    harn_session_store::wal_watch::data_version(&watcher).expect("map the wal index");
    let shm_len = std::fs::metadata(&shm).expect("shm exists").len();
    assert!(shm_len > 0);

    let reader = SqliteSessionStore::open_read_only(&path).expect("read-only snapshot");
    drop(reader);

    // SQLite shares lock state between connections inside one process, so only
    // a separate process can observe a released WAL index lock.
    let peer = std::process::Command::new(std::env::current_exe().expect("test exe"))
        .args([
            "--exact",
            "sqlite_initialization::sqlite_read_only_snapshot_peer_child",
        ])
        .env(SNAPSHOT_PEER_ENV, &path)
        .output()
        .expect("spawn peer");
    assert!(peer.status.success(), "peer failed: {:?}", peer.status);
    create(&writer, "from-first-writer");

    assert_eq!(std::fs::metadata(&shm).expect("shm exists").len(), shm_len);
    harn_session_store::wal_watch::data_version(&watcher).expect("watcher still reads");
    assert_eq!(
        harn_session_store::wal_watch::list_title_snapshots(&watcher)
            .expect("watcher lists")
            .len(),
        3
    );
}

/// Second process that opens the store and writes after the snapshot.
#[test]
fn sqlite_read_only_snapshot_peer_child() {
    let Some(path) = std::env::var_os(SNAPSHOT_PEER_ENV) else {
        return;
    };
    let store = SqliteSessionStore::open(&path).expect("peer opens store");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(store.create(CreateSession {
            id: Some("after-snapshot".to_string()),
            ..CreateSession::default()
        }))
        .expect("peer create");
}

#[test]
fn sqlite_read_only_snapshot_keeps_the_wal_index_intact() {
    if std::env::var_os(SNAPSHOT_SCENARIO_ENV).is_some() {
        return;
    }
    let dir = TempDir::new().expect("tempdir");
    let output = std::process::Command::new(std::env::current_exe().expect("test exe"))
        .args([
            "--exact",
            "sqlite_initialization::sqlite_read_only_snapshot_scenario_child",
            "--nocapture",
        ])
        .env(SNAPSHOT_SCENARIO_ENV, dir.path())
        .output()
        .expect("spawn scenario child");
    assert!(
        output.status.success(),
        "scenario child failed ({:?}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn sqlite_read_only_open_works_in_a_non_writable_store_directory() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("sealed.sqlite");
    drop(SqliteSessionStore::open(&path).expect("initialize store"));
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555))
        .expect("seal directory");
    let opened = SqliteSessionStore::open_read_only(&path);
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
        .expect("unseal directory");
    assert!(
        opened.is_ok(),
        "sealed directory must still be readable: {:?}",
        opened.err()
    );
}

#[test]
fn sqlite_read_only_handle_keeps_one_frozen_view() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("frozen.sqlite");
    let writer = SqliteSessionStore::open(&path).expect("initialize store");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let create = |id: &str| {
        runtime.block_on(writer.create(CreateSession {
            id: Some(id.to_string()),
            ..CreateSession::default()
        }))
    };
    create("before").expect("seed");

    let reader = SqliteSessionStore::open_read_only(&path).expect("open read-only store");
    create("after").expect("commit after the reader opened");

    let listed = runtime
        .block_on(reader.list(ListFilter::default()))
        .expect("list");
    assert_eq!(listed.len(), 1, "reader must not see the later commit");
    drop(reader);
    let fresh = SqliteSessionStore::open_read_only(&path).expect("reopen");
    let listed = runtime
        .block_on(fresh.list(ListFilter::default()))
        .expect("list");
    assert_eq!(listed.len(), 2);
}

#[test]
fn sqlite_read_only_snapshot_of_an_unopened_store_leaves_every_file_untouched() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("cold.sqlite");
    let store = SqliteSessionStore::open(&path).expect("initialize store");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        store
            .create(CreateSession {
                id: Some("cold".to_string()),
                ..CreateSession::default()
            })
            .await
            .expect("seed session");
    });
    drop(store);
    std::fs::remove_file(initialization_lock_path(&path)).expect("remove initializer lock");
    let before = durable_file_digests(dir.path());

    let reader = SqliteSessionStore::open_read_only(&path).expect("open read-only store");
    let sessions = runtime
        .block_on(reader.list(ListFilter::default()))
        .expect("list");
    assert_eq!(sessions.len(), 1);
    drop(reader);

    assert_eq!(durable_file_digests(dir.path()), before);
}
