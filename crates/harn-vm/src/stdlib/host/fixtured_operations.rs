//! Host operations answered by the active `harness.testing` fixture scope.
//!
//! `host_has` reports on the capability manifest, so a fixtured operation has to
//! appear there or a script that gates its host call on `host_has` never reaches
//! the fixture. The retired `with_host_mocks` wrapper got this for free through
//! the host-mock registry; capability fixtures need the same merge.

use std::cell::RefCell;

thread_local! {
    /// The stack mirrors `CapabilityFixtureState`'s scopes: only the innermost
    /// scope answers calls, so only its operations are visible.
    static FIXTURED_HOST_OPERATIONS: RefCell<Vec<Vec<(String, String)>>> =
        const { RefCell::new(Vec::new()) };
}

pub(crate) fn record_fixtured_host_operation(capability: &str, operation: &str) {
    FIXTURED_HOST_OPERATIONS.with(|scopes| {
        let mut scopes = scopes.borrow_mut();
        if scopes.is_empty() {
            scopes.push(Vec::new());
        }
        let entry = (capability.to_string(), operation.to_string());
        let current = scopes.last_mut().expect("fixture scope stack is non-empty");
        if !current.contains(&entry) {
            current.push(entry);
        }
    });
}

pub(crate) fn push_fixtured_host_operation_scope() {
    FIXTURED_HOST_OPERATIONS.with(|scopes| scopes.borrow_mut().push(Vec::new()));
}

pub(crate) fn pop_fixtured_host_operation_scope() {
    FIXTURED_HOST_OPERATIONS.with(|scopes| {
        scopes.borrow_mut().pop();
    });
}

pub(crate) fn clear_fixtured_host_operations() {
    FIXTURED_HOST_OPERATIONS.with(|scopes| {
        let mut scopes = scopes.borrow_mut();
        if let Some(current) = scopes.last_mut() {
            current.clear();
        }
    });
}

pub(crate) fn fixtured_host_operations() -> Vec<(String, String)> {
    FIXTURED_HOST_OPERATIONS
        .with(|scopes| scopes.borrow().last().cloned())
        .unwrap_or_default()
}

/// Merge the visible fixture scope's operations into a capability manifest.
pub(crate) fn apply_to_manifest(
    root: &mut crate::value::DictMap,
    ensure: fn(&mut crate::value::DictMap, &str, &str),
) {
    for (capability, operation) in fixtured_host_operations() {
        ensure(root, &capability, &operation);
    }
}
