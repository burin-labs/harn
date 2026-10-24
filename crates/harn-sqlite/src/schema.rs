//! Schema-owning initialization for a database whose tables are one SQL batch.

use std::time::Duration;

use crate::{InitializationError, SchemaVersion};
use rusqlite::Connection;

pub const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub type RuntimeSqliteError = InitializationError<rusqlite::Error>;

pub struct RuntimeSqliteSchema {
    name: &'static str,
    version: i64,
    sql: &'static str,
}

impl RuntimeSqliteSchema {
    pub const fn new(name: &'static str, version: i64, sql: &'static str) -> Self {
        Self { name, version, sql }
    }

    const fn marker(&self) -> SchemaVersion {
        SchemaVersion::new(self.name, self.version)
    }
}

pub fn initialize_runtime_sqlite(
    connection: &Connection,
    busy_timeout: Duration,
    schema: &RuntimeSqliteSchema,
) -> Result<(), RuntimeSqliteError> {
    initialize_runtime_sqlite_migrating(connection, busy_timeout, schema, |_| Ok(()))
}

pub fn initialize_runtime_sqlite_migrating(
    connection: &Connection,
    busy_timeout: Duration,
    schema: &RuntimeSqliteSchema,
    migrate: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<()>,
) -> Result<(), RuntimeSqliteError> {
    crate::initialize_file(connection, busy_timeout, schema.marker(), |transaction| {
        transaction.execute_batch(schema.sql)?;
        migrate(transaction)
    })
}

pub fn require_runtime_sqlite_initialized(
    connection: &Connection,
    busy_timeout: Duration,
    schema: &RuntimeSqliteSchema,
) -> Result<(), RuntimeSqliteError> {
    crate::require_file_initialized::<rusqlite::Error>(connection, busy_timeout, schema.marker())
}

pub fn initialize_transient_runtime_sqlite(
    connection: &Connection,
    busy_timeout: Duration,
    schema: &RuntimeSqliteSchema,
) -> Result<(), RuntimeSqliteError> {
    crate::initialize_transient(
        connection,
        busy_timeout,
        SchemaVersion::new(schema.name, schema.version),
        |transaction| transaction.execute_batch(schema.sql),
    )
}
