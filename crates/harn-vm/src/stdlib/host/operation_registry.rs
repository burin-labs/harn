use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{LazyLock, RwLock};

use crate::value::DictMap;

type Operations = BTreeMap<String, BTreeMap<String, String>>;

thread_local! {
    // Mock declarations follow the thread that executes a test VM. Keeping
    // them local prevents concurrent embedded suites from clearing each other.
    static SCOPED_MOCKABLE: RefCell<Operations> = const { RefCell::new(BTreeMap::new()) };
}

// Embedder registrations remain process-wide across async worker migration.
static MOCKABLE: LazyLock<RwLock<Operations>> = LazyLock::new(|| RwLock::new(BTreeMap::new()));
static CALLABLE: LazyLock<RwLock<Operations>> = LazyLock::new(|| RwLock::new(BTreeMap::new()));

pub(super) fn register_mockable(
    capability: impl AsRef<str>,
    operation: impl AsRef<str>,
    description: impl AsRef<str>,
) {
    insert_shared(&MOCKABLE, capability, operation, description);
}

pub(super) fn register_scoped_mockable(
    capability: impl AsRef<str>,
    operation: impl AsRef<str>,
    description: impl AsRef<str>,
) {
    SCOPED_MOCKABLE.with(|registered| {
        insert(
            &mut registered.borrow_mut(),
            capability,
            operation,
            description,
        );
    });
}

pub(super) fn clear_scoped_mockable() {
    SCOPED_MOCKABLE.with(|registered| registered.borrow_mut().clear());
}

pub(super) fn register_callable(
    capability: impl AsRef<str>,
    operation: impl AsRef<str>,
    description: impl AsRef<str>,
) {
    insert_shared(&CALLABLE, capability, operation, description);
}

fn insert_shared(
    registry: &RwLock<Operations>,
    capability: impl AsRef<str>,
    operation: impl AsRef<str>,
    description: impl AsRef<str>,
) {
    insert(
        &mut registry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        capability,
        operation,
        description,
    );
}

fn insert(
    registry: &mut Operations,
    capability: impl AsRef<str>,
    operation: impl AsRef<str>,
    description: impl AsRef<str>,
) {
    registry
        .entry(capability.as_ref().to_string())
        .or_default()
        .insert(
            operation.as_ref().to_string(),
            description.as_ref().to_string(),
        );
}

pub(super) fn apply_mockable(root: &mut DictMap) {
    let registered = MOCKABLE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    apply(root, &registered);
    SCOPED_MOCKABLE.with(|scoped| apply(root, &scoped.borrow()));
}

pub(super) fn apply_callable(root: &mut DictMap) {
    let registered = CALLABLE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    apply(root, &registered);
}

fn apply(root: &mut DictMap, registered: &Operations) {
    for (capability, operations) in registered {
        for (operation, description) in operations {
            super::ensure_registered_operation(root, capability, operation, description);
        }
    }
}
