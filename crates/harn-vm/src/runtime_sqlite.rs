use std::time::Duration;

use harn_sqlite::{InitializationError, SchemaVersion};
use rusqlite::Connection;

pub(crate) const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) type RuntimeSqliteError = InitializationError<rusqlite::Error>;

pub(crate) struct RuntimeSqliteSchema {
    name: &'static str,
    version: i64,
    sql: &'static str,
}

impl RuntimeSqliteSchema {
    pub(crate) const fn new(name: &'static str, version: i64, sql: &'static str) -> Self {
        Self { name, version, sql }
    }

    const fn marker(&self) -> SchemaVersion {
        SchemaVersion::new(self.name, self.version)
    }
}

pub(crate) fn initialize_runtime_sqlite(
    connection: &Connection,
    busy_timeout: Duration,
    schema: &RuntimeSqliteSchema,
) -> Result<(), RuntimeSqliteError> {
    harn_sqlite::initialize_file(connection, busy_timeout, schema.marker(), |transaction| {
        transaction.execute_batch(schema.sql)
    })
}

pub(crate) fn require_runtime_sqlite_initialized(
    connection: &Connection,
    busy_timeout: Duration,
    schema: &RuntimeSqliteSchema,
) -> Result<(), RuntimeSqliteError> {
    harn_sqlite::require_file_initialized::<rusqlite::Error>(
        connection,
        busy_timeout,
        schema.marker(),
    )
}

pub(crate) fn initialize_transient_runtime_sqlite(
    connection: &Connection,
    busy_timeout: Duration,
    schema: &RuntimeSqliteSchema,
) -> Result<(), RuntimeSqliteError> {
    harn_sqlite::initialize_transient(
        connection,
        busy_timeout,
        SchemaVersion::new(schema.name, schema.version),
        |transaction| transaction.execute_batch(schema.sql),
    )
}
