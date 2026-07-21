//! Shared thread-local seam for reading process-environment variables in a way
//! that unit tests can override hermetically.
//!
//! Production code that must stay testable reads env through [`env_var_seamed`]
//! instead of [`std::env::var`]. Under `cfg(not(test))` that is exactly
//! `std::env::var(key).ok()`. Under `cfg(test)` the process environment is
//! structurally invisible: reads come from a per-thread override map instead,
//! so ambient shell configuration (a developer's exported `HARN_LLM_MODEL`, a
//! CI wrapper's provider default) can never seed state a test did not ask for,
//! and no cross-test serialization lock is needed for the seamed variables.
//!
//! Tests inject values through [`TestEnvGuard`], obtained from
//! [`test_env_guard`]. The guard clears this thread's overrides on both
//! creation and drop, giving each test hermetic edges: neither ambient
//! configuration nor a sibling test's leftovers leak in, and nothing leaks out.
//!
//! # Visibility across threads
//!
//! The override map is thread-local, so a value set on the test thread is
//! invisible to work the test spawns on other threads (e.g. a multi-threaded
//! Tokio runtime). Code exercised on a worker thread that must observe a
//! test-provided value has to keep reading real process env under a
//! process-global lock instead. The base-URL resolver
//! [`crate::llm_config::resolve_base_url`] is deliberately left on
//! [`std::env::var`] for this reason.
//!
//! # Convergence
//!
//! This module is the shared owner for the keyed-override seam pattern. Two
//! domain-local seams predate it and should converge here once their PRs land:
//!   - `egress::egress_env_var` / `EgressTestEnvGuard` (the `HARN_EGRESS_*`
//!     family, PR #5368)
//!   - `stdlib::sandbox::handler_env` (the single `HARN_HANDLER_SANDBOX`
//!     variable, PR #5379)
//!
//! Both additionally reset domain policy state on guard lifecycle; a converged
//! design would layer that domain reset over this generic core rather than
//! duplicate the thread-local map. Doing that consolidation is intentionally
//! out of scope until those PRs merge.

/// Reads one process-environment variable through the test seam.
///
/// - `cfg(not(test))`: `std::env::var(key).ok()`.
/// - `cfg(test)`: the calling thread's override map only; the process
///   environment is not consulted.
pub(crate) fn env_var_seamed(key: &str) -> Option<String> {
    #[cfg(test)]
    {
        overrides::get(key)
    }
    #[cfg(not(test))]
    {
        std::env::var(key).ok()
    }
}

#[cfg(test)]
mod overrides {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    thread_local! {
        static OVERRIDES: RefCell<BTreeMap<String, String>> =
            const { RefCell::new(BTreeMap::new()) };
    }

    pub(super) fn get(key: &str) -> Option<String> {
        OVERRIDES.with(|overrides| overrides.borrow().get(key).cloned())
    }

    pub(super) fn set(key: &str, value: &str) {
        OVERRIDES.with(|overrides| {
            overrides
                .borrow_mut()
                .insert(key.to_owned(), value.to_owned());
        });
    }

    pub(super) fn clear() {
        OVERRIDES.with(|overrides| overrides.borrow_mut().clear());
    }
}

/// Gives a test a clean, hermetic env universe for the seamed variables: on
/// both creation and drop it clears this thread's overrides, so neither ambient
/// configuration nor a sibling test's leftovers can leak in, and nothing leaks
/// out. All seamed state is thread-keyed, so no cross-test serialization is
/// needed and the guard is safe to hold across `await` points on a
/// single-threaded runtime.
#[cfg(test)]
#[must_use]
pub(crate) fn test_env_guard() -> TestEnvGuard {
    overrides::clear();
    TestEnvGuard {}
}

/// Guard returned by [`test_env_guard`]. Injects env values for this thread via
/// [`TestEnvGuard::set`] and clears all overrides on drop.
#[cfg(test)]
pub(crate) struct TestEnvGuard {}

#[cfg(test)]
impl TestEnvGuard {
    /// Sets an environment variable for this thread only, visible to
    /// [`env_var_seamed`] readers on the same thread.
    pub(crate) fn set(&self, key: &str, value: &str) {
        overrides::set(key, value);
    }
}

#[cfg(test)]
impl Drop for TestEnvGuard {
    fn drop(&mut self) {
        overrides::clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seam_reads_overrides_and_clears_on_drop() {
        {
            let env = test_env_guard();
            env.set("HARN_LLM_PROVIDER", "mock");
            assert_eq!(env_var_seamed("HARN_LLM_PROVIDER").as_deref(), Some("mock"));
            // A key never set reads as absent regardless of ambient env.
            assert_eq!(env_var_seamed("HARN_LLM_MODEL"), None);
        }
        // Dropping the guard clears the thread's overrides.
        assert_eq!(env_var_seamed("HARN_LLM_PROVIDER"), None);
    }

    #[test]
    fn fresh_guard_clears_prior_overrides() {
        let first = test_env_guard();
        first.set("HARN_DEFAULT_PROVIDER", "openai");
        drop(first);

        let second = test_env_guard();
        assert_eq!(env_var_seamed("HARN_DEFAULT_PROVIDER"), None);
        drop(second);
    }
}
