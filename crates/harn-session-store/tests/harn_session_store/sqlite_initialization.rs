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
            5,
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
    let before = durable_file_digests(dir.path());
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

    assert_eq!(durable_file_digests(dir.path()), before);
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
    fn collect(root: &Path, directory: &Path, inventory: &mut Vec<(PathBuf, String)>) {
        for entry in std::fs::read_dir(directory).expect("read inventory directory") {
            let entry = entry.expect("read inventory entry");
            let path = entry.path();
            if entry.file_type().expect("inventory file type").is_dir() {
                collect(root, &path, inventory);
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
    collect(root, root, &mut inventory);
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
