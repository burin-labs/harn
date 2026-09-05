//! The one owner of process-environment mutation in the `llm` test suites.
//!
//! The process environment is global to the test binary, so a case that sets a
//! variable is writing shared state. Serializing that is the whole job, and it
//! only works if there is one lock. There were three, plus eight copies of the
//! scoped setter below, each guarded by whichever lock its own module happened
//! to know about — so two cases in different modules serialized against
//! different mutexes and clobbered each other anyway (harn#7960). The DeepSeek
//! admission case read `Missing API key` for a key it had just set.
//!
//! `nextest` hides this by running each case in its own process. The single
//! `cargo test --lib` process does not, which is why a developer's full-suite
//! run produced a different failure set every time and could not be used to
//! attribute their own breakage.
//!
//! So: one lock, and a setter that cannot be constructed without holding it.
//! [`ScopedEnvVar`] carries the guard for its own lifetime and restores the
//! previous value before releasing it, which makes "mutate the environment
//! without the lock" unrepresentable rather than merely discouraged.

use std::cell::Cell;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

pub(crate) fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

thread_local! {
    /// How many guards this thread already holds.
    ///
    /// A case that takes the guard itself and then builds a [`ScopedEnvVar`]
    /// would deadlock on a plain mutex, and that pattern is the normal one
    /// here. Re-entrancy is sound because the lock protects the process
    /// environment against *other* threads: a thread that already holds it
    /// cannot race with itself.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Exclusive access to the process environment for the caller's thread.
pub(crate) struct EnvGuard {
    /// `None` when an outer guard on this thread already holds the lock.
    _inner: Option<MutexGuard<'static, ()>>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Acquire the environment lock, recovering from poisoning.
///
/// The mutex serializes environment mutation and guards no invariant of its
/// own, so a panicking holder leaves nothing corrupted behind. Treating poison
/// as fatal would turn one failing case into a failure in every sibling that
/// touches an environment variable.
pub(crate) fn env_guard() -> EnvGuard {
    let already_held = DEPTH.with(|depth| {
        let held = depth.get();
        depth.set(held + 1);
        held
    });
    let inner =
        (already_held == 0).then(|| env_lock().lock().unwrap_or_else(PoisonError::into_inner));
    EnvGuard { _inner: inner }
}

/// One environment variable, set or removed for the life of the value, with
/// the previous value restored on drop.
///
/// The guard field is declared last so the restore in `Drop::drop` runs while
/// the lock is still held.
pub(crate) struct ScopedEnvVar {
    key: &'static str,
    previous: Option<String>,
    _guard: EnvGuard,
}

impl ScopedEnvVar {
    /// Set `key` to `value` until the returned value is dropped.
    pub(crate) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let guard = env_guard();
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self {
            key,
            previous,
            _guard: guard,
        }
    }

    /// Remove `key` until the returned value is dropped.
    pub(crate) fn remove(key: &'static str) -> Self {
        let guard = env_guard();
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::remove_var(key);
        }
        Self {
            key,
            previous,
            _guard: guard,
        }
    }

    /// Spelling used by the provider suites; same behavior as [`Self::remove`].
    pub(crate) fn unset(key: &'static str) -> Self {
        Self::remove(key)
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "HARN_TEST_SCOPED_ENV_VAR_REENTRANCY";

    /// Taking the guard and then a scoped variable is the ordinary shape here,
    /// and on a plain mutex it would deadlock instead of failing.
    #[test]
    fn a_scoped_variable_nests_inside_an_explicit_guard() {
        let _outer = env_guard();
        {
            let _scoped = ScopedEnvVar::set(KEY, "inner");
            assert_eq!(std::env::var(KEY).ok().as_deref(), Some("inner"));
        }
        assert!(
            std::env::var(KEY).is_err(),
            "the previous absence is restored"
        );
    }
}
