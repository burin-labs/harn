//! Shared initialization for Harn-owned SQLite databases.
//!
//! File-backed databases use a persistent sidecar advisory lock while changing
//! journal mode and committing a versioned schema marker. Once both WAL and the
//! exact marker are visible, later opens avoid the lock. Transient databases
//! share the schema transaction contract without a filesystem lock or WAL.

use rusqlite::{
    params, Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior,
};
use std::ffi::OsString;
use std::fmt;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

const SCHEMA_MARKER_TABLE: &str = "_harn_sqlite_schema_versions";
const CREATE_SCHEMA_MARKER_TABLE: &str =
    "CREATE TABLE IF NOT EXISTS main._harn_sqlite_schema_versions (
    name TEXT PRIMARY KEY,
    version INTEGER NOT NULL CHECK(version > 0)
);";
static TRANSIENT_INITIALIZATION_LOCK: Mutex<()> = Mutex::new(());

/// Stable identity for one schema stored in a Harn-owned SQLite database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemaVersion {
    name: &'static str,
    version: i64,
}

/// Lock-contention reason reported by SQLite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqliteContention {
    /// Another connection currently owns the database write lock.
    Busy,
    /// A shared-cache table lock prevents the operation from proceeding.
    Locked,
}

/// Classify a SQLite error without relying on rendered error text.
#[must_use]
pub fn sqlite_contention(error: &rusqlite::Error) -> Option<SqliteContention> {
    match error {
        rusqlite::Error::SqliteFailure(failure, _) => match failure.code {
            ErrorCode::DatabaseBusy => Some(SqliteContention::Busy),
            ErrorCode::DatabaseLocked => Some(SqliteContention::Locked),
            _ => None,
        },
        _ => None,
    }
}

impl SchemaVersion {
    /// Define a non-empty schema name and positive version.
    ///
    /// Invalid constants fail during compile-time evaluation.
    #[must_use]
    pub const fn new(name: &'static str, version: i64) -> Self {
        assert!(!name.is_empty(), "SQLite schema name must not be empty");
        assert!(version > 0, "SQLite schema version must be positive");
        Self { name, version }
    }
}

/// Failure to configure or initialize a Harn-owned SQLite database.
#[derive(Debug)]
#[non_exhaustive]
pub enum InitializationError<E> {
    /// The configured busy timeout cannot be represented by SQLite.
    BusyTimeoutTooLarge { milliseconds: u128 },
    /// The connection rejected its configured busy timeout.
    BusyTimeout(rusqlite::Error),
    /// The current journal mode could not be observed.
    JournalModeQuery(rusqlite::Error),
    /// The connection does not own a file-backed main database.
    DatabasePathUnavailable,
    /// A file-backed connection was passed to the transient initializer.
    FileBackedTransient { path: PathBuf },
    /// The opened database path could not be resolved to one lock identity.
    DatabasePath {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The persistent sidecar lock could not be opened.
    InitializationLockOpen {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Exclusive ownership of the sidecar lock could not be acquired.
    InitializationLockAcquire {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The sidecar lock was still held when the wait budget expired, or the
    /// operating system refused it. Carries the path and the elapsed wait.
    InitializationLock(harn_flock::LockError),
    /// SQLite rejected WAL journal mode for a non-contention reason.
    WalPragma(rusqlite::Error),
    /// SQLite accepted the journal-mode request but returned another mode.
    WalNotEnabled { mode: String },
    /// WAL promotion was busy and the database remained in another mode.
    WalBusyNotWal { mode: String },
    /// WAL promotion and the diagnostic journal-mode query both failed.
    WalBusyQuery {
        wal_error: Box<rusqlite::Error>,
        query_error: Box<rusqlite::Error>,
    },
    /// The connection rejected the runtime synchronous setting.
    Synchronous(rusqlite::Error),
    /// The schema marker could not be inspected.
    SchemaReadiness(rusqlite::Error),
    /// No initializer committed the exact schema version before the readiness
    /// lease became available.
    SchemaNotInitialized { name: &'static str, version: i64 },
    /// The database was initialized by a newer incompatible schema owner.
    NewerSchemaVersion {
        name: &'static str,
        stored: i64,
        supported: i64,
    },
    /// The atomic schema transaction could not begin.
    Transaction(rusqlite::Error),
    /// The owning schema callback failed.
    Initialize(E),
    /// The schema marker table or row could not be written.
    SchemaMarker(rusqlite::Error),
    /// The schema and marker transaction could not commit.
    Commit(rusqlite::Error),
}

impl InitializationError<rusqlite::Error> {
    /// Whether the SQLite portion of this failure reports lock contention.
    #[must_use]
    pub fn is_busy_or_locked(&self) -> bool {
        if let Self::Initialize(error) = self {
            is_sqlite_busy_or_locked(error)
        } else {
            initialization_stage_is_busy_or_locked(self)
        }
    }
}

impl<E: fmt::Display> fmt::Display for InitializationError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BusyTimeoutTooLarge { milliseconds } => write!(
                f,
                "busy_timeout {milliseconds}ms exceeds SQLite's maximum"
            ),
            Self::BusyTimeout(error) => write!(f, "busy_timeout failed: {error}"),
            Self::JournalModeQuery(error) => write!(f, "journal_mode query failed: {error}"),
            Self::DatabasePathUnavailable => {
                write!(f, "SQLite connection has no file-backed main database path")
            }
            Self::FileBackedTransient { path } => write!(
                f,
                "transient SQLite initialization requires a private non-file database, got {}",
                path.display()
            ),
            Self::DatabasePath { path, source } => write!(
                f,
                "could not resolve SQLite database path {}: {source}",
                path.display()
            ),
            Self::InitializationLockOpen { path, source } => write!(
                f,
                "could not open SQLite initialization lock {}: {source}",
                path.display()
            ),
            Self::InitializationLockAcquire { path, source } => write!(
                f,
                "could not acquire SQLite initialization lock {}: {source}",
                path.display()
            ),
            Self::InitializationLock(error) => {
                write!(f, "SQLite initialization lock unavailable: {error}")
            }
            Self::WalPragma(error) => write!(f, "WAL journal_mode pragma failed: {error}"),
            Self::WalNotEnabled { mode } => {
                write!(f, "WAL journal_mode request returned {mode}")
            }
            Self::WalBusyNotWal { mode } => {
                write!(f, "WAL journal_mode request left journal_mode at {mode}")
            }
            Self::WalBusyQuery {
                wal_error,
                query_error,
            } => write!(
                f,
                "WAL journal_mode pragma failed: {wal_error}; journal_mode query also failed: {query_error}"
            ),
            Self::Synchronous(error) => write!(f, "synchronous pragma failed: {error}"),
            Self::SchemaReadiness(error) => write!(f, "schema readiness query failed: {error}"),
            Self::SchemaNotInitialized { name, version } => {
                write!(f, "SQLite schema {name} version {version} is not initialized")
            }
            Self::NewerSchemaVersion {
                name,
                stored,
                supported,
            } => write!(
                f,
                "SQLite schema {name} version {stored} is newer than supported version {supported}"
            ),
            Self::Transaction(error) => write!(f, "schema transaction failed: {error}"),
            Self::Initialize(error) => write!(f, "schema initialization failed: {error}"),
            Self::SchemaMarker(error) => write!(f, "schema marker update failed: {error}"),
            Self::Commit(error) => write!(f, "schema transaction commit failed: {error}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for InitializationError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BusyTimeout(error)
            | Self::JournalModeQuery(error)
            | Self::WalPragma(error)
            | Self::Synchronous(error)
            | Self::SchemaReadiness(error)
            | Self::Transaction(error)
            | Self::SchemaMarker(error)
            | Self::Commit(error) => Some(error),
            Self::DatabasePath { source, .. }
            | Self::InitializationLockOpen { source, .. }
            | Self::InitializationLockAcquire { source, .. } => Some(source),
            Self::InitializationLock(error) => Some(error),
            Self::WalBusyQuery { wal_error, .. } => Some(wal_error),
            Self::Initialize(error) => Some(error),
            Self::BusyTimeoutTooLarge { .. }
            | Self::DatabasePathUnavailable
            | Self::FileBackedTransient { .. }
            | Self::SchemaNotInitialized { .. }
            | Self::WalNotEnabled { .. }
            | Self::WalBusyNotWal { .. }
            | Self::NewerSchemaVersion { .. } => None,
        }
    }
}

/// Initialize a file-backed Harn database exactly once per schema version.
///
/// The lock identity comes from the connection's VFS-resolved main-database
/// filename, so callers cannot accidentally serialize a different path.
/// Initialization serializes WAL promotion and the schema transaction across
/// processes. The callback and version marker commit atomically. Ready opens
/// observe WAL plus the exact marker and do not acquire the sidecar lock. The
/// callback is invoked at most once by this function and should keep all
/// persistent effects inside the supplied transaction so an error rolls them
/// back with the marker.
pub fn initialize_file<E, F>(
    connection: &Connection,
    busy_timeout: Duration,
    schema: SchemaVersion,
    initialize: F,
) -> Result<(), InitializationError<E>>
where
    F: FnOnce(&Transaction<'_>) -> Result<(), E>,
{
    configure_busy_timeout(connection, busy_timeout)?;
    if fast_path_is_ready(connection, schema)? {
        return configure_connection(connection);
    }

    let _initialization_lock = acquire_initialization_lock(connection, lock_timeout())?;
    ensure_wal_journal_mode(connection)?;
    configure_connection(connection)?;
    initialize_schema(connection, schema, initialize)
}

fn fast_path_is_ready<E>(
    connection: &Connection,
    schema: SchemaVersion,
) -> Result<bool, InitializationError<E>> {
    match is_wal_journal_mode(connection) {
        Ok(true) => {}
        Ok(false) => return Ok(false),
        Err(error) if initialization_stage_is_busy_or_locked(&error) => return Ok(false),
        Err(error) => return Err(error),
    }
    match schema_is_ready(connection, schema) {
        Ok(ready) => Ok(ready),
        Err(error) if initialization_stage_is_busy_or_locked(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

fn initialization_stage_is_busy_or_locked<E>(error: &InitializationError<E>) -> bool {
    match error {
        InitializationError::BusyTimeout(error)
        | InitializationError::JournalModeQuery(error)
        | InitializationError::WalPragma(error)
        | InitializationError::Synchronous(error)
        | InitializationError::SchemaReadiness(error)
        | InitializationError::Transaction(error)
        | InitializationError::SchemaMarker(error)
        | InitializationError::Commit(error) => is_sqlite_busy_or_locked(error),
        InitializationError::WalBusyNotWal { .. } => true,
        InitializationError::WalBusyQuery {
            wal_error,
            query_error,
        } => is_sqlite_busy_or_locked(wal_error) || is_sqlite_busy_or_locked(query_error),
        InitializationError::BusyTimeoutTooLarge { .. }
        | InitializationError::DatabasePath { .. }
        | InitializationError::DatabasePathUnavailable
        | InitializationError::FileBackedTransient { .. }
        | InitializationError::InitializationLockOpen { .. }
        | InitializationError::InitializationLockAcquire { .. }
        | InitializationError::InitializationLock(_)
        | InitializationError::SchemaNotInitialized { .. }
        | InitializationError::NewerSchemaVersion { .. }
        | InitializationError::WalNotEnabled { .. }
        | InitializationError::Initialize(_) => false,
    }
}

/// Require an exact schema version on a file-backed connection without
/// initializing it.
///
/// Ready databases avoid the sidecar lock. When initialization is in flight,
/// this waits for its persistent lease and validates the marker after the
/// writer releases it. If no initializer owns the lease, an absent marker is
/// reported here instead of allowing a read-only consumer to fail later on a
/// missing application table.
pub fn require_file_initialized<E>(
    connection: &Connection,
    busy_timeout: Duration,
    schema: SchemaVersion,
) -> Result<(), InitializationError<E>> {
    require_file_initialized_impl(connection, busy_timeout, schema, || {})
}

fn require_file_initialized_impl<E>(
    connection: &Connection,
    busy_timeout: Duration,
    schema: SchemaVersion,
    on_readiness_contention: impl FnOnce(),
) -> Result<(), InitializationError<E>> {
    configure_busy_timeout(connection, busy_timeout)?;
    if fast_path_is_ready(connection, schema)? {
        return Ok(());
    }

    let _readiness_lock =
        acquire_readiness_lock(connection, schema, lock_timeout(), on_readiness_contention)?;
    if is_wal_journal_mode(connection)? && schema_is_ready(connection, schema)? {
        return Ok(());
    }
    Err(InitializationError::SchemaNotInitialized {
        name: schema.name,
        version: schema.version,
    })
}

/// Initialize a transient database with the same atomic schema-marker contract.
///
/// No filesystem lock or WAL promotion is performed because the connection is
/// not shared across processes. A process-local lock serializes connection
/// configuration, readiness inspection, and first use of named shared-memory
/// databases because shared-cache schema reads conflict with schema creation.
/// The callback follows the same transaction-local effects and at-most-once
/// invocation contract as [`initialize_file`].
pub fn initialize_transient<E, F>(
    connection: &Connection,
    busy_timeout: Duration,
    schema: SchemaVersion,
    initialize: F,
) -> Result<(), InitializationError<E>>
where
    F: FnOnce(&Transaction<'_>) -> Result<(), E>,
{
    configure_busy_timeout(connection, busy_timeout)?;
    if let Some(path) = main_database_path(connection) {
        return Err(InitializationError::FileBackedTransient { path });
    }
    let _initialization_lock = TRANSIENT_INITIALIZATION_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    configure_connection(connection)?;
    if schema_is_ready(connection, schema)? {
        return Ok(());
    }
    initialize_schema(connection, schema, initialize)
}

fn initialize_schema<E, F>(
    connection: &Connection,
    schema: SchemaVersion,
    initialize: F,
) -> Result<(), InitializationError<E>>
where
    F: FnOnce(&Transaction<'_>) -> Result<(), E>,
{
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(InitializationError::Transaction)?;
    transaction
        .execute_batch(CREATE_SCHEMA_MARKER_TABLE)
        .map_err(InitializationError::SchemaMarker)?;
    if schema_marker_is_ready(&transaction, schema)? {
        return transaction.commit().map_err(InitializationError::Commit);
    }
    initialize(&transaction).map_err(InitializationError::Initialize)?;
    transaction
        .execute(
            "INSERT INTO main._harn_sqlite_schema_versions(name, version) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET version = excluded.version",
            params![schema.name, schema.version],
        )
        .map_err(InitializationError::SchemaMarker)?;
    transaction.commit().map_err(InitializationError::Commit)
}

fn configure_busy_timeout<E>(
    connection: &Connection,
    busy_timeout: Duration,
) -> Result<(), InitializationError<E>> {
    let milliseconds = busy_timeout.as_millis();
    if milliseconds > i32::MAX as u128 {
        return Err(InitializationError::BusyTimeoutTooLarge { milliseconds });
    }
    connection
        .busy_timeout(busy_timeout)
        .map_err(InitializationError::BusyTimeout)
}

fn configure_connection<E>(connection: &Connection) -> Result<(), InitializationError<E>> {
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(InitializationError::Synchronous)
}

/// Environment override for [`lock_timeout`], in whole seconds.
pub const LOCK_TIMEOUT_SECONDS_ENV: &str = "HARN_SQLITE_LOCK_TIMEOUT_SECONDS";

/// Default wait for the sidecar lock.
///
/// The critical section it guards is WAL promotion plus one schema
/// transaction — milliseconds of work, and only on the first open of a
/// database. Three orders of magnitude of headroom means expiry says the
/// holder is wedged, not that the disk was slow.
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for the sidecar lock.
///
/// Operators can raise this for a filesystem where the default is genuinely
/// too tight. An unparseable or zero value falls back to the default rather
/// than failing the open: this is a diagnostic bound, and a typo in it must not
/// take a database offline.
fn lock_timeout() -> Duration {
    std::env::var(LOCK_TIMEOUT_SECONDS_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map_or(DEFAULT_LOCK_TIMEOUT, Duration::from_secs)
}

fn acquire_initialization_lock<E>(
    connection: &Connection,
    timeout: Duration,
) -> Result<SqliteInitializationLock, InitializationError<E>> {
    let path = initialization_lock_path(connection)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|source| InitializationError::InitializationLockOpen {
            path: path.clone(),
            source,
        })?;
    harn_flock::lock_with_deadline(&file, &path, harn_flock::LockMode::Exclusive, timeout)
        .map_err(InitializationError::InitializationLock)?;
    Ok(SqliteInitializationLock { file })
}

fn acquire_readiness_lock<E>(
    connection: &Connection,
    schema: SchemaVersion,
    timeout: Duration,
    on_contention: impl FnOnce(),
) -> Result<SqliteInitializationLock, InitializationError<E>> {
    let path = initialization_lock_path(connection)?;
    let file = match OpenOptions::new().read(true).open(&path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(InitializationError::SchemaNotInitialized {
                name: schema.name,
                version: schema.version,
            });
        }
        Err(source) => {
            return Err(InitializationError::InitializationLockOpen { path, source });
        }
    };
    match file.try_lock_shared() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            on_contention();
            harn_flock::lock_with_deadline(&file, &path, harn_flock::LockMode::Shared, timeout)
                .map_err(InitializationError::InitializationLock)?;
        }
        Err(TryLockError::Error(source)) => {
            return Err(InitializationError::InitializationLockAcquire { path, source });
        }
    }
    Ok(SqliteInitializationLock { file })
}

fn initialization_lock_path<E>(connection: &Connection) -> Result<PathBuf, InitializationError<E>> {
    let database_path =
        main_database_path(connection).ok_or(InitializationError::DatabasePathUnavailable)?;
    let canonical = std::fs::canonicalize(&database_path).map_err(|source| {
        InitializationError::DatabasePath {
            path: database_path.clone(),
            source,
        }
    })?;
    let mut path = OsString::from(canonical.as_os_str());
    path.push(".harn-init.lock");
    Ok(PathBuf::from(path))
}

#[cfg(unix)]
fn main_database_path(connection: &Connection) -> Option<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    // sqlite3_db_filename owns this pointer for the connection lifetime. Copy
    // its bytes immediately so Unix paths retain their exact VFS identity.
    let filename = unsafe {
        let pointer =
            rusqlite::ffi::sqlite3_db_filename(connection.handle(), rusqlite::MAIN_DB.as_ptr());
        (!pointer.is_null()).then(|| CStr::from_ptr(pointer).to_bytes())
    }?;
    (!filename.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(filename)))
}

#[cfg(not(unix))]
fn main_database_path(connection: &Connection) -> Option<PathBuf> {
    connection
        .path()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

struct SqliteInitializationLock {
    file: File,
}

impl Drop for SqliteInitializationLock {
    fn drop(&mut self) {
        // Keep the lock file linked so queued openers cannot split across
        // different inodes. Process exit also releases the advisory lock.
        let _ = self.file.unlock();
    }
}

fn schema_is_ready<E>(
    connection: &Connection,
    schema: SchemaVersion,
) -> Result<bool, InitializationError<E>> {
    let marker_exists = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM main.sqlite_schema WHERE type = 'table' AND name = ?1
             )",
            params![SCHEMA_MARKER_TABLE],
            |row| row.get::<_, bool>(0),
        )
        .map_err(InitializationError::SchemaReadiness)?;
    if !marker_exists {
        return Ok(false);
    }
    schema_marker_is_ready(connection, schema)
}

fn schema_marker_is_ready<E>(
    connection: &Connection,
    schema: SchemaVersion,
) -> Result<bool, InitializationError<E>> {
    let stored = connection
        .query_row(
            "SELECT version FROM main._harn_sqlite_schema_versions WHERE name = ?1",
            params![schema.name],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(InitializationError::SchemaReadiness)?;
    match stored {
        Some(version) if version > schema.version => Err(InitializationError::NewerSchemaVersion {
            name: schema.name,
            stored: version,
            supported: schema.version,
        }),
        Some(version) => Ok(version == schema.version),
        None => Ok(false),
    }
}

fn is_wal_journal_mode<E>(connection: &Connection) -> Result<bool, InitializationError<E>> {
    current_journal_mode(connection)
        .map(|mode| mode.eq_ignore_ascii_case("wal"))
        .map_err(InitializationError::JournalModeQuery)
}

fn ensure_wal_journal_mode<E>(connection: &Connection) -> Result<(), InitializationError<E>> {
    if is_wal_journal_mode(connection)? {
        return Ok(());
    }
    match connection.query_row("PRAGMA journal_mode = WAL", [], |row| {
        row.get::<_, String>(0)
    }) {
        Ok(mode) if mode.eq_ignore_ascii_case("wal") => Ok(()),
        Ok(mode) => Err(InitializationError::WalNotEnabled { mode }),
        Err(error) if is_sqlite_busy_or_locked(&error) => match current_journal_mode(connection) {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => Ok(()),
            Ok(mode) => Err(InitializationError::WalBusyNotWal { mode }),
            Err(query_error) => Err(InitializationError::WalBusyQuery {
                wal_error: Box::new(error),
                query_error: Box::new(query_error),
            }),
        },
        Err(error) => Err(InitializationError::WalPragma(error)),
    }
}

fn current_journal_mode(connection: &Connection) -> Result<String, rusqlite::Error> {
    connection.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
}

fn is_sqlite_busy_or_locked(error: &rusqlite::Error) -> bool {
    sqlite_contention(error).is_some()
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
