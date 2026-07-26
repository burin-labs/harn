//! Advisory file locks that cannot wait forever.
//!
//! [`std::fs::File::lock`] has no timed form. A caller blocked on a lock nobody
//! will release produces no error, no log line, and no stack — the process
//! simply stops, and whatever supervises it can report only that it stopped.
//! Recovering the reason means reproducing the wedge, and a rare interleaving
//! can cost days of runner time to hit twice.
//!
//! Every acquisition here carries a deadline and names the path it waited on,
//! so a wedge surfaces as an error that says which lock and for how long.
//!
//! The deadline is a *diagnostic* bound, not a coordination mechanism. Callers
//! pick one from how long the critical section legitimately runs — schema
//! initialization is milliseconds, a package install can be minutes — and set
//! it far enough above that ceiling that expiry means something is wrong rather
//! than merely slow.

use std::fmt;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Longest gap between attempts. Short enough that a freed lock is picked up
/// promptly, long enough that a minutes-long wait is not a spin.
const MAX_BACKOFF: Duration = Duration::from_millis(50);

/// Whether a waiter needs the file to itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockMode {
    /// One holder at a time.
    Exclusive,
    /// Any number of concurrent holders, excluded by an [`LockMode::Exclusive`]
    /// holder.
    Shared,
}

impl fmt::Display for LockMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exclusive => formatter.write_str("exclusive"),
            Self::Shared => formatter.write_str("shared"),
        }
    }
}

/// A lock that was not acquired.
#[derive(Debug)]
#[non_exhaustive]
pub enum LockError {
    /// The deadline passed while another holder kept the lock.
    ///
    /// The holder is not named: advisory locks record no owner, and asking the
    /// operating system who holds one is neither portable nor race-free. The
    /// path and the elapsed wait are what a reader needs to find it.
    Timeout {
        /// The lock file waited on.
        path: PathBuf,
        /// The access that was requested.
        mode: LockMode,
        /// How long the caller waited before giving up.
        waited: Duration,
    },
    /// The operating system refused the lock outright.
    Io {
        /// The lock file waited on.
        path: PathBuf,
        /// The access that was requested.
        mode: LockMode,
        /// The refusal.
        source: std::io::Error,
    },
}

impl fmt::Display for LockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout { path, mode, waited } => write!(
                formatter,
                "timed out after {:.1}s waiting for the {mode} lock on {}; \
                 another process or task is holding it",
                waited.as_secs_f64(),
                path.display()
            ),
            Self::Io { path, mode, source } => write!(
                formatter,
                "failed to take the {mode} lock on {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for LockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Timeout { .. } => None,
            Self::Io { source, .. } => Some(source),
        }
    }
}

/// Take an advisory lock on `file`, giving up after `timeout`.
///
/// `path` names the file only so failures can report it; the lock is taken on
/// the supplied handle. Blocks the calling thread, so an async caller belongs
/// on a blocking-capable executor — on a single-threaded runtime this stalls
/// every other task on that thread, which is precisely how a lock wait becomes
/// an unbreakable deadlock.
///
/// # Errors
///
/// [`LockError::Timeout`] if another holder still owns the lock when the
/// deadline passes, or [`LockError::Io`] if the operating system refuses.
pub fn lock_with_deadline(
    file: &File,
    path: &Path,
    mode: LockMode,
    timeout: Duration,
) -> Result<(), LockError> {
    let started = Instant::now();
    let mut backoff = Duration::from_millis(1);
    loop {
        let attempt = match mode {
            LockMode::Exclusive => file.try_lock(),
            LockMode::Shared => file.try_lock_shared(),
        };
        match attempt {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(source)) => {
                return Err(LockError::Io {
                    path: path.to_path_buf(),
                    mode,
                    source,
                })
            }
        }
        // Checked after the attempt, not before, so a zero timeout still tries
        // once. Callers that only want a single try use `try_lock` directly;
        // one that passes a deadline should never be refused without an
        // attempt against the current state of the lock.
        let waited = started.elapsed();
        if waited >= timeout {
            return Err(LockError::Timeout {
                path: path.to_path_buf(),
                mode,
                waited,
            });
        }
        // Never overshoot the deadline: a caller that budgeted 30s should hear
        // back at 30s, not at the end of whichever backoff straddles it.
        let remaining = timeout - waited;
        std::thread::sleep(backoff.min(remaining));
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two handles on one path, so the second waiter is genuinely blocked
    /// rather than re-entering its own lock. File locks are per-handle on
    /// Windows and per-open-file-description under `flock`, so this deadlocks
    /// a single process just as readily as two.
    fn locked_pair() -> (tempfile::TempDir, PathBuf, File, File) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("guard.lock");
        let first = File::create(&path).expect("create lock file");
        let second = File::open(&path).expect("reopen lock file");
        (dir, path, first, second)
    }

    #[test]
    fn an_uncontended_lock_is_taken_immediately() {
        let (_dir, path, first, _second) = locked_pair();
        lock_with_deadline(&first, &path, LockMode::Exclusive, Duration::from_secs(30))
            .expect("uncontended exclusive lock");
    }

    /// The whole point: a held lock must produce an error naming it, not a
    /// process that stops.
    #[test]
    fn a_held_lock_times_out_and_names_itself() {
        let (_dir, path, first, second) = locked_pair();
        first.lock().expect("hold the lock");

        let error = lock_with_deadline(
            &second,
            &path,
            LockMode::Exclusive,
            Duration::from_millis(50),
        )
        .expect_err("second acquisition must not succeed");

        match error {
            LockError::Timeout {
                path: reported,
                mode,
                waited,
            } => {
                assert_eq!(reported, path);
                assert_eq!(mode, LockMode::Exclusive);
                assert!(waited >= Duration::from_millis(50), "waited {waited:?}");
            }
            other => panic!("expected a timeout, got {other:?}"),
        }
        assert!(format!(
            "{}",
            LockError::Timeout {
                path: path.clone(),
                mode: LockMode::Exclusive,
                waited: Duration::from_millis(50),
            }
        )
        .contains(&path.display().to_string()));
    }

    /// Readers must not exclude each other, or every concurrent open of a
    /// ready database would serialize behind the first.
    #[test]
    fn shared_locks_admit_each_other() {
        let (_dir, path, first, second) = locked_pair();
        first.lock_shared().expect("first shared lock");
        lock_with_deadline(&second, &path, LockMode::Shared, Duration::from_millis(50))
            .expect("a second shared lock must be admitted");
    }

    /// An exclusive holder must still block a reader, or the deadline would be
    /// hiding a lock that never actually excluded anything.
    #[test]
    fn an_exclusive_holder_blocks_a_shared_waiter() {
        let (_dir, path, first, second) = locked_pair();
        first.lock().expect("hold exclusively");
        let error = lock_with_deadline(&second, &path, LockMode::Shared, Duration::from_millis(50))
            .expect_err("shared waiter must not pass an exclusive holder");
        assert!(matches!(
            error,
            LockError::Timeout {
                mode: LockMode::Shared,
                ..
            }
        ));
    }
}
