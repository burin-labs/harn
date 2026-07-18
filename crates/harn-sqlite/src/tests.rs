use std::cell::Cell;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags};
use wait_timeout::ChildExt;

use super::{
    current_journal_mode, initialization_lock_path, initialize_file, initialize_transient,
    InitializationError, SchemaVersion,
};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const CHILD_TIMEOUT: Duration = Duration::from_secs(10);
const CHILD_DATABASE: &str = "HARN_SQLITE_TEST_DATABASE";
const CHILD_EXPECTED_JOURNAL: &str = "HARN_SQLITE_TEST_EXPECTED_JOURNAL";
const CHILD_VALUE: &str = "HARN_SQLITE_TEST_VALUE";
const TEST_SCHEMA: SchemaVersion = SchemaVersion::new("process_rows", 1);
const TEST_SCHEMA_SQL: &str =
    "CREATE TABLE IF NOT EXISTS process_rows (value INTEGER PRIMARY KEY);";

#[test]
fn sqlite_initializer_process_child() {
    let Some(database) = std::env::var_os(CHILD_DATABASE) else {
        return;
    };
    let value = std::env::var(CHILD_VALUE)
        .expect("child value")
        .parse::<i64>()
        .expect("integer child value");
    let expected_journal = std::env::var(CHILD_EXPECTED_JOURNAL).expect("expected journal mode");
    let connection = Connection::open(&database).expect("child database open");
    assert_eq!(
        current_journal_mode(&connection)
            .expect("fresh child journal mode")
            .to_ascii_lowercase(),
        expected_journal
    );

    println!("READY");
    std::io::stdout().flush().expect("flush ready signal");
    let mut release = [0_u8; 1];
    std::io::stdin()
        .read_exact(&mut release)
        .expect("read release signal");

    initialize_test_database(&connection).expect("initialize shared database");
    connection
        .execute(
            "INSERT INTO process_rows(value) VALUES (?1)",
            params![value],
        )
        .expect("insert child row");
    let observed = connection
        .query_row(
            "SELECT value FROM process_rows WHERE value = ?1",
            params![value],
            |row| row.get::<_, i64>(0),
        )
        .expect("read child row");
    assert_eq!(observed, value);
    println!("DONE");
}

#[test]
fn first_open_is_serialized_across_processes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let database = dir.path().join("runtime.sqlite");
    let mut children = vec![
        spawn_child(&database, 1, "delete"),
        spawn_child(&database, 2, "delete"),
    ];
    run_children(&mut children);

    let connection = Connection::open(&database).expect("open initialized database");
    assert_eq!(
        current_journal_mode(&connection)
            .expect("journal mode")
            .to_ascii_lowercase(),
        "wal"
    );
    let mut statement = connection
        .prepare("SELECT value FROM process_rows ORDER BY value")
        .expect("prepare row query");
    let rows = statement
        .query_map([], |row| row.get::<_, i64>(0))
        .expect("query rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect rows");
    assert_eq!(rows, vec![1, 2]);
}

#[test]
fn already_ready_wal_fast_path_does_not_open_initialization_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let database = dir.path().join("runtime.sqlite");
    let connection = Connection::open(&database).expect("open database");
    initialize_test_database(&connection).expect("initial setup");
    let lock_path = initialization_lock_path::<rusqlite::Error>(&connection).expect("lock path");
    drop(connection);
    std::fs::remove_file(&lock_path).expect("remove initial lock file");
    std::fs::create_dir(&lock_path).expect("install inaccessible lock sentinel");

    let mut children = vec![
        spawn_child(&database, 1, "wal"),
        spawn_child(&database, 2, "wal"),
    ];
    run_children(&mut children);

    let connection = Connection::open(&database).expect("reopen WAL database");
    assert_eq!(
        current_journal_mode(&connection)
            .expect("journal mode")
            .to_ascii_lowercase(),
        "wal"
    );
    let mut statement = connection
        .prepare("SELECT value FROM process_rows ORDER BY value")
        .expect("prepare row query");
    let rows = statement
        .query_map([], |row| row.get::<_, i64>(0))
        .expect("query rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect rows");
    assert_eq!(rows, vec![1, 2]);
}

#[test]
fn wal_without_schema_marker_reenters_initialization() {
    let dir = tempfile::tempdir().expect("tempdir");
    let database = dir.path().join("runtime.sqlite");
    let connection = Connection::open(&database).expect("open database");
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .expect("simulate crash after WAL promotion");
    let lock_path = initialization_lock_path::<rusqlite::Error>(&connection).expect("lock path");
    std::fs::create_dir(&lock_path).expect("install inaccessible lock sentinel");

    let error = initialize_test_database(&connection)
        .expect_err("WAL without an exact schema marker must reacquire the lock");
    match error {
        InitializationError::InitializationLockOpen { path, .. } => assert_eq!(path, lock_path),
        other => panic!("expected initialization lock open error, got {other:?}"),
    }
}

#[test]
fn initialization_lock_open_failure_is_attributed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let database = dir.path().join("runtime.sqlite");
    let connection = Connection::open(&database).expect("open database");
    let lock_path = initialization_lock_path::<rusqlite::Error>(&connection).expect("lock path");
    std::fs::create_dir(&lock_path).expect("install inaccessible lock sentinel");

    let error = initialize_test_database(&connection)
        .expect_err("fresh initialization must fail on an inaccessible lock");
    match error {
        InitializationError::InitializationLockOpen { path, .. } => assert_eq!(path, lock_path),
        other => panic!("expected initialization lock open error, got {other:?}"),
    }
}

#[test]
fn newer_schema_marker_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let database = dir.path().join("runtime.sqlite");
    let connection = Connection::open(&database).expect("open database");
    initialize_file(
        &connection,
        BUSY_TIMEOUT,
        SchemaVersion::new("process_rows", 2),
        |transaction| transaction.execute_batch(TEST_SCHEMA_SQL),
    )
    .expect("initialize newer schema");

    let error = initialize_test_database(&connection)
        .expect_err("an older runtime must reject a newer schema marker");
    match error {
        InitializationError::NewerSchemaVersion {
            name,
            stored,
            supported,
        } => assert_eq!((name, stored, supported), ("process_rows", 2, 1)),
        other => panic!("expected newer schema version error, got {other:?}"),
    }
}

#[test]
fn temporary_schema_cannot_shadow_the_durable_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let database = dir.path().join("runtime.sqlite");
    let connection = Connection::open(&database).expect("open database");
    connection
        .execute_batch(
            "CREATE TEMP TABLE _harn_sqlite_schema_versions (
                name TEXT PRIMARY KEY,
                version INTEGER NOT NULL
             );
             INSERT INTO temp._harn_sqlite_schema_versions(name, version)
             VALUES ('process_rows', 99);",
        )
        .expect("install misleading temporary marker");

    initialize_test_database(&connection).expect("initialize durable schema");
    let versions = connection
        .query_row(
            "SELECT
                (SELECT version FROM main._harn_sqlite_schema_versions WHERE name = 'process_rows'),
                (SELECT version FROM temp._harn_sqlite_schema_versions WHERE name = 'process_rows')",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .expect("inspect durable and temporary markers");
    assert_eq!(versions, (1, 99));
}

#[test]
fn sqlite_callback_contention_remains_retryable() {
    let busy =
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY), None);
    let constraint = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
        None,
    );

    assert_eq!(
        (
            InitializationError::Initialize(busy).is_busy_or_locked(),
            InitializationError::Initialize(constraint).is_busy_or_locked(),
        ),
        (true, false)
    );
}

#[test]
fn failed_schema_initialization_rolls_back_schema_and_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let database = dir.path().join("runtime.sqlite");
    let connection = Connection::open(&database).expect("open database");

    let error = initialize_file(&connection, BUSY_TIMEOUT, TEST_SCHEMA, |transaction| {
        transaction
            .execute_batch(TEST_SCHEMA_SQL)
            .expect("create schema");
        Err::<(), _>("stop before commit")
    })
    .expect_err("failed initialization must roll back");
    match error {
        InitializationError::Initialize(reason) => assert_eq!(reason, "stop before commit"),
        other => panic!("expected initializer error, got {other:?}"),
    }

    let persisted_tables = connection
        .query_row(
            "SELECT
                EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'process_rows'),
                EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = '_harn_sqlite_schema_versions')",
            [],
            |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?)),
        )
        .expect("inspect rolled-back schema");
    assert_eq!(persisted_tables, (false, false));

    initialize_test_database(&connection).expect("retry initialization");
}

#[test]
fn file_initializer_rejects_transient_connection_identity() {
    let connection = Connection::open_in_memory().expect("open in-memory database");

    let error = initialize_file(&connection, BUSY_TIMEOUT, TEST_SCHEMA, |_transaction| {
        Ok::<(), rusqlite::Error>(())
    })
    .expect_err("file initialization requires a file-backed connection");

    assert!(matches!(
        error,
        InitializationError::DatabasePathUnavailable
    ));
}

#[test]
fn transient_initializer_rejects_file_backed_connection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let database = dir.path().join("runtime.sqlite");
    let connection = Connection::open(&database).expect("open database");

    let error = initialize_transient(&connection, BUSY_TIMEOUT, TEST_SCHEMA, |_transaction| {
        Ok::<(), rusqlite::Error>(())
    })
    .expect_err("transient initialization must reject a file-backed database");

    match error {
        InitializationError::FileBackedTransient { path } => assert_eq!(path, database),
        other => panic!("expected file-backed transient error, got {other:?}"),
    }
}

#[test]
fn oversized_busy_timeout_is_a_typed_error() {
    let connection = Connection::open_in_memory().expect("open in-memory database");
    let timeout = Duration::from_millis(i32::MAX as u64 + 1);

    let error = initialize_transient(&connection, timeout, TEST_SCHEMA, |_transaction| {
        Ok::<(), rusqlite::Error>(())
    })
    .expect_err("oversized busy timeout must not panic");

    match error {
        InitializationError::BusyTimeoutTooLarge { milliseconds } => {
            assert_eq!(milliseconds, i32::MAX as u128 + 1);
        }
        other => panic!("expected oversized busy timeout error, got {other:?}"),
    }
}

#[test]
fn wal_request_requires_the_returned_mode_to_be_wal() {
    let connection = Connection::open_in_memory().expect("open in-memory database");

    let error = super::ensure_wal_journal_mode::<rusqlite::Error>(&connection)
        .expect_err("in-memory SQLite cannot enter WAL mode");

    match error {
        InitializationError::WalNotEnabled { mode } => assert_eq!(mode, "memory"),
        other => panic!("expected unchanged journal mode, got {other:?}"),
    }
}

#[test]
fn transient_initializer_runs_schema_callback_once() {
    let connection = Connection::open_in_memory().expect("open in-memory database");
    let callback_runs = Cell::new(0);

    initialize_transient(&connection, BUSY_TIMEOUT, TEST_SCHEMA, |transaction| {
        callback_runs.set(callback_runs.get() + 1);
        transaction.execute_batch(TEST_SCHEMA_SQL)
    })
    .expect("first transient initialization");
    initialize_transient(&connection, BUSY_TIMEOUT, TEST_SCHEMA, |_transaction| {
        callback_runs.set(callback_runs.get() + 1);
        Err(rusqlite::Error::InvalidQuery)
    })
    .expect("ready transient initialization skips callback");

    let state = connection
        .query_row(
            "SELECT
                (SELECT version FROM _harn_sqlite_schema_versions WHERE name = 'process_rows'),
                EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'process_rows')",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
        )
        .expect("inspect transient schema");
    assert_eq!(callback_runs.get(), 1);
    assert_eq!(state, (1, true));
}

#[test]
fn shared_memory_initialization_serializes_the_schema_callback() {
    const URI: &str = "file:harn-sqlite-shared-test?mode=memory&cache=shared";
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_URI;
    let anchor = Connection::open_with_flags(URI, flags).expect("open shared-memory anchor");
    let barrier = Arc::new(Barrier::new(3));
    let threads = (0..2)
        .map(|_| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let connection =
                    Connection::open_with_flags(URI, flags).expect("open shared-memory connection");
                barrier.wait();
                initialize_transient(&connection, BUSY_TIMEOUT, TEST_SCHEMA, |transaction| {
                    transaction.execute_batch(
                        "CREATE TABLE shared_rows (value INTEGER NOT NULL);
                         INSERT INTO shared_rows(value) VALUES (1);",
                    )
                })
                .expect("initialize shared-memory schema");
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for thread in threads {
        thread.join().expect("join shared-memory initializer");
    }

    let state = anchor
        .query_row(
            "SELECT
                (SELECT version FROM main._harn_sqlite_schema_versions WHERE name = 'process_rows'),
                (SELECT COUNT(*) FROM shared_rows)",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .expect("inspect serialized shared-memory initialization");
    assert_eq!(state, (1, 1));
}

#[cfg(unix)]
#[test]
fn file_initializer_preserves_non_utf8_lock_identity() {
    use std::os::unix::ffi::OsStringExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let database = dir.path().join(std::ffi::OsString::from_vec(
        b"runtime-\xFF.sqlite".to_vec(),
    ));
    let connection = Connection::open(&database).expect("open non-UTF-8 database");

    initialize_test_database(&connection).expect("initialize non-UTF-8 database");

    let lock_path = super::initialization_lock_path::<rusqlite::Error>(&connection)
        .expect("derive non-UTF-8 lock path");
    let mut expected = database.into_os_string();
    expected.push(".harn-init.lock");
    assert_eq!(lock_path.into_os_string(), expected);
}

fn initialize_test_database(
    connection: &Connection,
) -> Result<(), InitializationError<rusqlite::Error>> {
    initialize_file(connection, BUSY_TIMEOUT, TEST_SCHEMA, |transaction| {
        transaction.execute_batch(TEST_SCHEMA_SQL)
    })
}

struct TestChild {
    child: Child,
    stdout: Option<BufReader<std::process::ChildStdout>>,
}

fn spawn_child(database: &Path, value: i64, expected_journal: &str) -> TestChild {
    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("sqlite_initializer_process_child")
        .arg("--nocapture")
        .env(CHILD_DATABASE, database)
        .env(CHILD_EXPECTED_JOURNAL, expected_journal)
        .env(CHILD_VALUE, value.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn child test process");
    let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    TestChild {
        child,
        stdout: Some(stdout),
    }
}

fn run_children(children: &mut [TestChild]) {
    for child in children.iter_mut() {
        read_until(child, "READY");
    }
    for child in children.iter_mut() {
        child
            .child
            .stdin
            .as_mut()
            .expect("child stdin")
            .write_all(b"initialize\n")
            .expect("release child initializer");
    }
    for child in children.iter_mut() {
        read_until(child, "DONE");
        let Some(status) = child
            .child
            .wait_timeout(CHILD_TIMEOUT)
            .expect("wait for child initializer")
        else {
            kill_and_reap(&mut child.child);
            panic!("child initializer did not exit within {CHILD_TIMEOUT:?}");
        };
        assert_eq!(status.code(), Some(0), "child initializer status");
    }
}

fn read_until(child: &mut TestChild, marker: &str) {
    let mut reader = child.stdout.take().expect("child stdout reader");
    let expected_marker = marker.to_string();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let reader_thread = std::thread::spawn(move || {
        let result = loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break Err(format!("child exited before emitting {expected_marker}")),
                Ok(_) if line.split_whitespace().last() == Some(expected_marker.as_str()) => {
                    break Ok(());
                }
                Ok(_) => {}
                Err(error) => break Err(format!("could not read child output: {error}")),
            }
        };
        let _ = sender.send((reader, result));
    });
    match receiver.recv_timeout(CHILD_TIMEOUT) {
        Ok((reader, result)) => {
            child.stdout = Some(reader);
            reader_thread.join().expect("join child output reader");
            result.unwrap_or_else(|error| panic!("{error}"));
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            kill_and_reap(&mut child.child);
            reader_thread.join().expect("join timed-out output reader");
            panic!("child did not emit {marker} within {CHILD_TIMEOUT:?}");
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            reader_thread.join().expect("join failed output reader");
            panic!("child output reader disconnected before emitting {marker}");
        }
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}
