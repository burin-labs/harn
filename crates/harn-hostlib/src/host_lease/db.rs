use std::time::Duration;
use std::time::Instant;

use rusqlite::Connection;

use super::{sqlite_is_busy, HostLeaseError, HostLeaseStore};

// Poll interval while retrying the one-time WAL conversion of a fresh database
// (see enable_wal_journal). Short because the conversion the winner is running
// is a handful of page writes, not a mutation window.
const WAL_CONVERSION_RETRY_INTERVAL: Duration = Duration::from_millis(5);

impl HostLeaseStore {
    pub(super) fn connection(&self, busy_timeout: Duration) -> Result<Connection, HostLeaseError> {
        let conn = Connection::open(&self.db_path)?;
        // Install lock patience (busy timeout, or the test busy handler) before
        // the first statement runs. It covers ordinary query-layer contention,
        // but SQLite deliberately bypasses it for the exclusive-lock upgrade the
        // WAL conversion needs, so enable_wal_journal does its own retry.
        self.install_lock_patience(&conn, busy_timeout)?;
        self.enable_wal_journal(&conn, busy_timeout)?;
        Ok(conn)
    }

    /// Put the connection in WAL journal mode, retrying the one-time conversion
    /// of a fresh rollback-journal file until it wins or the busy budget elapses.
    ///
    /// WAL lets readers and writers proceed concurrently instead of taking a
    /// whole-database exclusive lock; under the default rollback journal a writer
    /// blocks every other connection, which is the "database is locked" flake the
    /// lease registry saw under heavy parallel load. WAL keeps the single-writer
    /// serialization the registry relies on (Immediate transactions still
    /// serialize writers) but no longer blocks readers.
    ///
    /// The catch is the conversion itself. `PRAGMA journal_mode=WAL` rewrites a
    /// brand-new database under an exclusive lock, and to reach EXCLUSIVE a
    /// connection must upgrade from the SHARED lock it already holds. When
    /// several connections open the same fresh registry at once, SQLite refuses
    /// to run the busy handler for that upgrade — it returns SQLITE_BUSY
    /// immediately to avoid a deadlock between two SHARED holders each waiting to
    /// upgrade — so the busy timeout installed above does not cover it, and on
    /// Windows (mandatory file locking) the losers surface "database is locked".
    /// We therefore retry the conversion ourselves within the same budget. This
    /// provably converges: once any connection wins, WAL is a persistent property
    /// of the file, so every later invocation is a lock-free no-op.
    ///
    /// `synchronous = NORMAL` is the conventional durability setting under WAL.
    /// Run via `execute_batch` because `PRAGMA journal_mode` returns the
    /// resulting mode as a row.
    fn enable_wal_journal(
        &self,
        conn: &Connection,
        busy_timeout: Duration,
    ) -> Result<(), HostLeaseError> {
        let deadline = Instant::now() + busy_timeout;
        loop {
            match conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;") {
                Ok(()) => return Ok(()),
                Err(error) if sqlite_is_busy(&error) && Instant::now() < deadline => {
                    std::thread::sleep(WAL_CONVERSION_RETRY_INTERVAL);
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn install_lock_patience(
        &self,
        conn: &Connection,
        busy_timeout: Duration,
    ) -> Result<(), HostLeaseError> {
        #[cfg(test)]
        if !busy_timeout.is_zero() {
            if let Some(handler) = self.busy_handler {
                conn.busy_handler(Some(handler))?;
                return Ok(());
            }
        }
        conn.busy_timeout(busy_timeout)?;
        Ok(())
    }
}
