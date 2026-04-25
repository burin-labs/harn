//! Per-thread "enabled features" registry for the deterministic tools.
//!
//! `harn-hostlib` exposes the deterministic tool builtins on every VM that
//! `install_default` runs against, but pipelines must explicitly opt in to
//! their use by calling the `hostlib_enable("tools:deterministic")` builtin
//! before any of the tool methods will execute. This keeps the surface
//! sandbox-friendly: a script that doesn't ask for the tools cannot poke
//! the host filesystem or shell out to `git` even though the contract is
//! registered.
//!
//! State is held in a thread-local so that:
//!
//! * Independent VM runs stay isolated when the embedder executes them on
//!   separate threads.
//! * Cargo test isolation works without extra ceremony.
//!
//! Embedders can also call [`enable_for_test`] / [`reset`] from Rust if
//! they need to bypass the builtin (for example, tests that don't drive
//! a live VM).

use std::cell::RefCell;
use std::collections::BTreeSet;

/// Feature key for the deterministic-tools surface.
///
/// Kept here as a constant so [`tools::register_builtins`](super::register_builtins)
/// and the integration tests share the exact same string.
pub const FEATURE_TOOLS_DETERMINISTIC: &str = "tools:deterministic";

thread_local! {
    static ENABLED: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

/// Mark `feature` as enabled on the current thread. Returns `true` if the
/// feature was newly enabled, `false` if it was already on.
pub fn enable(feature: &str) -> bool {
    ENABLED.with(|cell| cell.borrow_mut().insert(feature.to_string()))
}

/// Mark `feature` as disabled on the current thread. Returns `true` if the
/// feature was previously enabled. Mostly useful in tests that want to
/// assert the gate works.
pub fn disable(feature: &str) -> bool {
    ENABLED.with(|cell| cell.borrow_mut().remove(feature))
}

/// Bulk-clear every enabled feature on the current thread. Tests use this
/// to start from a known state.
pub fn reset() {
    ENABLED.with(|cell| cell.borrow_mut().clear());
}

/// Report whether `feature` is enabled on the current thread.
pub fn is_enabled(feature: &str) -> bool {
    ENABLED.with(|cell| cell.borrow().contains(feature))
}

/// Convenience wrapper for tests: enable the deterministic tools in the
/// current thread without needing to reach for the builtin.
pub fn enable_for_test() {
    enable(FEATURE_TOOLS_DETERMINISTIC);
}
