//! The `HARN_HANDLER_SANDBOX` read seam.
//!
//! Production reads the selector straight from the process environment; under
//! `cfg(test)` that environment is structurally invisible and the value comes
//! from a thread-local override instead, so ambient shell or CI configuration
//! can never flip a test's sandbox outcome. See [`handler_sandbox_env`].

use super::SandboxFallback;
#[cfg(not(test))]
use super::HANDLER_SANDBOX_ENV;
use crate::orchestration::SandboxProfile;

/// Resolve the fallback policy for the requested profile. `OsHardened`
/// always enforces — that is the entire point of the profile, so the
/// `HARN_HANDLER_SANDBOX` env var cannot weaken it. `Worktree` honors
/// the env var (default `warn`).
pub(crate) fn effective_fallback(profile: SandboxProfile) -> SandboxFallback {
    if matches!(profile, SandboxProfile::OsHardened) {
        return SandboxFallback::Enforce;
    }
    match handler_sandbox_env()
        .unwrap_or_else(|| "warn".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "0" | "false" | "off" | "none" => SandboxFallback::Off,
        "1" | "true" | "enforce" | "required" => SandboxFallback::Enforce,
        _ => SandboxFallback::Warn,
    }
}

/// Reads the `HARN_HANDLER_SANDBOX` fallback selector for
/// [`effective_fallback`].
///
/// Under `cfg(test)` the process environment is structurally invisible: the
/// value comes from a thread-local override instead, so an ambient
/// `HARN_HANDLER_SANDBOX` exported in a developer's shell or a CI wrapper can
/// never flip a test's sandbox outcome. The exec-path tests in
/// `vm::tests_runtime` that need a specific selector inject it through
/// [`HandlerSandboxTestGuard::set`]; every other test observes the built-in
/// `warn` default deterministically. The override is thread-keyed, matching
/// the same-thread `new_current_thread` runtime those tests drive, so no
/// cross-test lock is needed. In production this is a plain env read.
fn handler_sandbox_env() -> Option<String> {
    #[cfg(test)]
    {
        test_env::get()
    }
    #[cfg(not(test))]
    {
        std::env::var(HANDLER_SANDBOX_ENV).ok()
    }
}

/// Gives a test a hermetic `HARN_HANDLER_SANDBOX` universe: creation and drop
/// both clear this thread's override, so neither ambient configuration nor a
/// sibling test's leftover selector can leak in or out. Inject a selector for
/// the duration of the test with [`HandlerSandboxTestGuard::set`].
#[cfg(test)]
#[must_use]
pub(crate) fn handler_sandbox_test_guard() -> HandlerSandboxTestGuard {
    test_env::clear();
    HandlerSandboxTestGuard {}
}

/// Guard returned by [`handler_sandbox_test_guard`]. Injects a
/// `HARN_HANDLER_SANDBOX` selector for this thread via
/// [`HandlerSandboxTestGuard::set`] and clears it on drop.
#[cfg(test)]
pub(crate) struct HandlerSandboxTestGuard {}

#[cfg(test)]
impl HandlerSandboxTestGuard {
    /// Sets the `HARN_HANDLER_SANDBOX` selector for this thread only, visible
    /// to [`handler_sandbox_env`] readers on the same thread.
    pub(crate) fn set(&self, value: &str) {
        test_env::set(value);
    }
}

#[cfg(test)]
impl Drop for HandlerSandboxTestGuard {
    fn drop(&mut self) {
        test_env::clear();
    }
}

/// Per-thread override for `HARN_HANDLER_SANDBOX`, replacing the process
/// environment as the read source under `cfg(test)` (see
/// [`handler_sandbox_env`]).
#[cfg(test)]
mod test_env {
    use std::cell::RefCell;

    thread_local! {
        static OVERRIDE: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    pub(super) fn get() -> Option<String> {
        OVERRIDE.with(|slot| slot.borrow().clone())
    }

    pub(super) fn set(value: &str) {
        OVERRIDE.with(|slot| *slot.borrow_mut() = Some(value.to_owned()));
    }

    pub(super) fn clear() {
        OVERRIDE.with(|slot| *slot.borrow_mut() = None);
    }
}
