use std::fmt;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode};

pub(crate) const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(crate) enum RuntimeSqliteError {
    BusyTimeout(rusqlite::Error),
    JournalModeQuery(rusqlite::Error),
    WalPragma(rusqlite::Error),
    WalBusyNotWal {
        mode: String,
    },
    WalBusyQuery {
        wal_error: rusqlite::Error,
        query_error: rusqlite::Error,
    },
    Synchronous(rusqlite::Error),
}

impl RuntimeSqliteError {
    pub(crate) fn is_busy_or_locked(&self) -> bool {
        match self {
            Self::BusyTimeout(error)
            | Self::JournalModeQuery(error)
            | Self::WalPragma(error)
            | Self::Synchronous(error) => is_sqlite_busy_or_locked(error),
            Self::WalBusyNotWal { .. } => true,
            Self::WalBusyQuery {
                wal_error,
                query_error,
            } => is_sqlite_busy_or_locked(wal_error) || is_sqlite_busy_or_locked(query_error),
        }
    }
}

impl fmt::Display for RuntimeSqliteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BusyTimeout(error) => write!(f, "busy_timeout failed: {error}"),
            Self::JournalModeQuery(error) => write!(f, "journal_mode query failed: {error}"),
            Self::WalPragma(error) => write!(f, "WAL journal_mode pragma failed: {error}"),
            Self::WalBusyNotWal { mode } => write!(
                f,
                "WAL journal_mode pragma was busy and journal_mode stayed {mode}"
            ),
            Self::WalBusyQuery {
                wal_error,
                query_error,
            } => write!(
                f,
                "WAL journal_mode pragma failed: {wal_error}; journal_mode query also failed: {query_error}"
            ),
            Self::Synchronous(error) => write!(f, "synchronous pragma failed: {error}"),
        }
    }
}

impl std::error::Error for RuntimeSqliteError {}

pub(crate) fn configure_runtime_sqlite(
    connection: &Connection,
    busy_timeout: Duration,
) -> Result<(), RuntimeSqliteError> {
    connection
        .busy_timeout(busy_timeout)
        .map_err(RuntimeSqliteError::BusyTimeout)?;
    ensure_wal_journal_mode(connection)?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(RuntimeSqliteError::Synchronous)?;
    Ok(())
}

fn ensure_wal_journal_mode(connection: &Connection) -> Result<(), RuntimeSqliteError> {
    match current_journal_mode(connection) {
        Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
        Ok(_) => {}
        Err(error) => return Err(RuntimeSqliteError::JournalModeQuery(error)),
    }

    match connection.pragma_update(None, "journal_mode", "WAL") {
        Ok(()) => Ok(()),
        Err(error) if is_sqlite_busy_or_locked(&error) => match current_journal_mode(connection) {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => Ok(()),
            Ok(mode) => Err(RuntimeSqliteError::WalBusyNotWal { mode }),
            Err(query_error) => Err(RuntimeSqliteError::WalBusyQuery {
                wal_error: error,
                query_error,
            }),
        },
        Err(error) => Err(RuntimeSqliteError::WalPragma(error)),
    }
}

fn current_journal_mode(connection: &Connection) -> Result<String, rusqlite::Error> {
    connection.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
}

fn is_sqlite_busy_or_locked(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(failure.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}
