//! Process-owned exclusion between live session writers and store maintenance.
//!
//! A SQLite transaction makes one append atomic, but it does not say that the
//! process intends to append again. Retention, compaction, and project-wide
//! cleanup need that longer-lived fact so they cannot remove a transcript or
//! its sidecars between two writes from the same run.
//!
//! Writers hold a shared project lease for their whole lifetime and an
//! exclusive lease for their session. Maintenance takes the project lease
//! exclusively before it inventories or mutates anything. The operating
//! system releases both after a normal drop or process death, so liveness does
//! not depend on clocks, heartbeats, or stale-process detection.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

use harn_flock::{LockError, LockMode};

const LEASE_DIRECTORY: &str = "agent-run-writers";
const PROJECT_LEASE_FILE: &str = "project.lock";

/// Failure to open or claim a session-store lease.
#[derive(Debug)]
#[non_exhaustive]
pub enum SessionLeaseError {
    /// Another participating process currently owns an incompatible lease.
    Contended {
        /// Stable lock path whose ownership prevented the claim.
        path: PathBuf,
    },
    /// The lease directory or file could not be created or opened.
    Io {
        /// Path whose file operation failed.
        path: PathBuf,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
    /// The operating system refused a lock operation for a reason other than contention.
    Lock(LockError),
}

impl fmt::Display for SessionLeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contended { path } => write!(
                formatter,
                "session lease {} is held by another process or task",
                path.display()
            ),
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "could not open session lease {}: {source}",
                    path.display()
                )
            }
            Self::Lock(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SessionLeaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Contended { .. } => None,
            Self::Io { source, .. } => Some(source),
            Self::Lock(error) => Some(error),
        }
    }
}

/// A live process's claim that it may keep writing one session.
///
/// The project lease is acquired first. Keeping one global lock order prevents
/// a writer and maintenance operation from each holding the lock the other
/// needs. The fields exist only to tie both file-lock lifetimes to this value.
#[derive(Debug)]
pub struct SessionWriteLease {
    _project: SessionMutationLease,
    _session: File,
}

impl SessionWriteLease {
    /// Claim a writer slot without waiting.
    ///
    /// `store_dir` is the directory containing the canonical store database.
    /// A conflict means maintenance is running or another writer owns this
    /// exact session; the caller must not open or mutate the session afterward.
    pub fn try_acquire(store_dir: &Path, session_id: &str) -> Result<Self, SessionLeaseError> {
        let project = SessionMutationLease::try_acquire(store_dir)?;

        let session_path = session_write_lease_path(store_dir, session_id);
        let session = open_lease_file(&session_path)?;
        try_lock(&session, &session_path, LockMode::Exclusive)?;
        Ok(Self {
            _project: project,
            _session: session,
        })
    }
}

/// Short-lived admission for a file-backed session-store mutation.
///
/// Kept crate-private because [`crate::SqliteSessionStore`] is the boundary
/// that must apply it to every mutation. Session writers use the same type as
/// the project half of their longer-lived lease.
#[derive(Debug)]
pub(crate) struct SessionMutationLease {
    _project: File,
}

impl SessionMutationLease {
    pub(crate) fn try_acquire(store_dir: &Path) -> Result<Self, SessionLeaseError> {
        let project_path = project_lease_path(store_dir);
        let project = open_lease_file(&project_path)?;
        try_lock(&project, &project_path, LockMode::Shared)?;
        Ok(Self { _project: project })
    }
}

/// Exclusive proof that no participating process can write the project store.
#[derive(Debug)]
pub struct SessionMaintenanceLease {
    _project: File,
}

impl SessionMaintenanceLease {
    /// Claim the project for maintenance without waiting.
    ///
    /// The caller holds the returned value across the complete inventory and
    /// mutation, including any sidecars whose lifecycle follows the sessions.
    pub fn try_acquire(store_dir: &Path) -> Result<Self, SessionLeaseError> {
        let project_path = project_lease_path(store_dir);
        let project = open_lease_file(&project_path)?;
        try_lock(&project, &project_path, LockMode::Exclusive)?;
        Ok(Self { _project: project })
    }
}

/// Stable lease location for one durable session.
///
/// Lease files remain after unlock. Removing one creates an inode race with a
/// successor that has already opened the old path.
pub fn session_write_lease_path(store_dir: &Path, session_id: &str) -> PathBuf {
    let identity = blake3::hash(session_id.as_bytes()).to_hex();
    session_lease_directory(store_dir).join(format!("{identity}.lock"))
}

/// Directory permanently reserved for session-store coordination files.
///
/// Project-wide retention and clearing may remove session data and sidecars,
/// but must preserve this directory and its contents. Unlinking a lock file
/// can let two processes lock different inodes under the same path.
pub fn session_lease_directory(store_dir: &Path) -> PathBuf {
    store_dir.join(LEASE_DIRECTORY)
}

fn project_lease_path(store_dir: &Path) -> PathBuf {
    session_lease_directory(store_dir).join(PROJECT_LEASE_FILE)
}

fn open_lease_file(path: &Path) -> Result<File, SessionLeaseError> {
    let directory = path.parent().expect("lease path always has a parent");
    std::fs::create_dir_all(directory).map_err(|source| SessionLeaseError::Io {
        path: directory.to_path_buf(),
        source,
    })?;
    OpenOptions::new()
        .create(true)
        .read(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|source| SessionLeaseError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn try_lock(file: &File, path: &Path, mode: LockMode) -> Result<(), SessionLeaseError> {
    match harn_flock::lock_with_deadline(file, path, mode, Duration::ZERO) {
        Ok(()) => Ok(()),
        Err(LockError::Timeout { path, .. }) => Err(SessionLeaseError::Contended { path }),
        Err(LockError::Io { path, source, .. }) => Err(SessionLeaseError::Io { path, source }),
        Err(error) => Err(SessionLeaseError::Lock(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writers_share_the_project_but_exclude_the_same_session() {
        let root = tempfile::tempdir().expect("lease root");
        let first = SessionWriteLease::try_acquire(root.path(), "session-a").expect("first writer");
        let second = SessionWriteLease::try_acquire(root.path(), "session-b")
            .expect("different-session writer");
        let duplicate = SessionWriteLease::try_acquire(root.path(), "session-a")
            .expect_err("same-session writer must be excluded");
        assert!(matches!(duplicate, SessionLeaseError::Contended { .. }));
        drop((first, second));
        SessionWriteLease::try_acquire(root.path(), "session-a")
            .expect("released writer lease can be reclaimed");
    }

    #[test]
    fn maintenance_and_writers_exclude_each_other_without_waiting() {
        let root = tempfile::tempdir().expect("lease root");
        let writer =
            SessionWriteLease::try_acquire(root.path(), "session-a").expect("writer lease");
        let blocked_maintenance = SessionMaintenanceLease::try_acquire(root.path())
            .expect_err("live writer must exclude maintenance");
        assert!(matches!(
            blocked_maintenance,
            SessionLeaseError::Contended { .. }
        ));
        drop(writer);

        let maintenance = SessionMaintenanceLease::try_acquire(root.path())
            .expect("maintenance after writer exits");
        let blocked_writer = SessionWriteLease::try_acquire(root.path(), "session-b")
            .expect_err("maintenance must exclude a new writer");
        assert!(matches!(
            blocked_writer,
            SessionLeaseError::Contended { .. }
        ));
        drop(maintenance);

        SessionWriteLease::try_acquire(root.path(), "session-b")
            .expect("writer after maintenance exits");
    }
}
