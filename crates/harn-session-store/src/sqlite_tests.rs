use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::{params, Connection};

use super::{write_transaction, StoreContention, StoreError};

static IMMEDIATE_BUSY_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static DEFERRED_BUSY_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

fn record_immediate_busy(_attempt: i32) -> bool {
    IMMEDIATE_BUSY_CALLBACKS.fetch_add(1, Ordering::SeqCst);
    false
}

fn record_deferred_busy(_attempt: i32) -> bool {
    DEFERRED_BUSY_CALLBACKS.fetch_add(1, Ordering::SeqCst);
    false
}

#[test]
fn deferred_read_to_write_upgrade_bypasses_busy_handler() {
    let dir = tempfile::tempdir().expect("tempdir");
    let database = dir.path().join("deferred-upgrade.sqlite");
    let mut stale_reader = Connection::open(&database).expect("reader connection");
    let mut writer = Connection::open(&database).expect("writer connection");
    stale_reader
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE rows (value INTEGER PRIMARY KEY);
             INSERT INTO rows (value) VALUES (1);",
        )
        .expect("schema and seed");
    DEFERRED_BUSY_CALLBACKS.store(0, Ordering::SeqCst);
    stale_reader
        .busy_handler(Some(record_deferred_busy))
        .expect("install busy observer");

    let stale_tx = stale_reader.transaction().expect("deferred transaction");
    assert_eq!(
        stale_tx
            .query_row("SELECT COUNT(*) FROM rows", [], |row| row.get::<_, i64>(0))
            .expect("establish read snapshot"),
        1
    );
    let writer_tx = write_transaction(&mut writer).expect("writer transaction");
    writer_tx
        .execute("INSERT INTO rows (value) VALUES (?1)", params![2])
        .expect("concurrent write");
    writer_tx.commit().expect("concurrent commit");

    let error = stale_tx
        .execute("INSERT INTO rows (value) VALUES (?1)", params![3])
        .expect_err("stale read transaction cannot upgrade");
    assert_eq!(
        harn_sqlite::sqlite_contention(&error),
        Some(harn_sqlite::SqliteContention::Busy)
    );
    assert_eq!(DEFERRED_BUSY_CALLBACKS.load(Ordering::SeqCst), 0);
}

#[test]
fn write_transactions_acquire_ownership_before_reading() {
    let dir = tempfile::tempdir().expect("tempdir");
    let database = dir.path().join("write-ownership.sqlite");
    let mut owner = Connection::open(&database).expect("owner connection");
    let mut contender = Connection::open(&database).expect("contender connection");
    owner
        .execute_batch("CREATE TABLE rows (value INTEGER PRIMARY KEY);")
        .expect("schema");
    IMMEDIATE_BUSY_CALLBACKS.store(0, Ordering::SeqCst);
    contender
        .busy_handler(Some(record_immediate_busy))
        .expect("install busy observer");

    let owner_tx = write_transaction(&mut owner).expect("owner write transaction");
    owner_tx
        .execute("INSERT INTO rows (value) VALUES (?1)", params![1])
        .expect("owner write");

    let error = match write_transaction(&mut contender) {
        Ok(_) => panic!("a second writer must fail at transaction entry"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        StoreError::Contention {
            kind: StoreContention::DatabaseBusy,
            ..
        }
    ));
    assert_eq!(IMMEDIATE_BUSY_CALLBACKS.load(Ordering::SeqCst), 1);

    owner_tx.commit().expect("owner commit");
    let contender_tx = write_transaction(&mut contender).expect("writer after release");
    contender_tx
        .execute("INSERT INTO rows (value) VALUES (?1)", params![2])
        .expect("contender write");
    contender_tx.commit().expect("contender commit");

    assert_eq!(
        contender
            .query_row("SELECT COUNT(*) FROM rows", [], |row| row.get::<_, i64>(0))
            .expect("row count"),
        2
    );
}
