use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use super::TriggerRegistry;

/// Trigger bindings and background lifecycle tasks owned by one VM tree.
#[derive(Default)]
pub(crate) struct TriggerRegistryRuntime {
    registry: parking_lot::Mutex<TriggerRegistry>,
    background_tasks: parking_lot::Mutex<BTreeMap<String, tokio::task::JoinHandle<()>>>,
    background_completions:
        parking_lot::Mutex<BTreeMap<String, (i64, tokio::sync::watch::Receiver<bool>)>>,
}

impl TriggerRegistryRuntime {
    pub(crate) fn replace_background_task(
        &self,
        id: String,
        task: tokio::task::JoinHandle<()>,
        deadline_unix_ms: i64,
        completion: tokio::sync::watch::Receiver<bool>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let previous = self.background_tasks.lock().insert(id.clone(), task);
        let previous_completion = self
            .background_completions
            .lock()
            .insert(id, (deadline_unix_ms, completion));
        assert!(
            previous_completion.is_none(),
            "trigger background completion replaced without cancellation"
        );
        previous
    }

    pub(crate) fn detach_background_task(&self, id: &str) -> Option<tokio::task::JoinHandle<()>> {
        self.background_tasks.lock().remove(id)
    }

    pub(crate) fn cancel_background_task(&self, id: &str) -> Option<tokio::task::JoinHandle<()>> {
        self.background_completions.lock().remove(id);
        self.background_tasks.lock().remove(id)
    }

    pub(crate) fn due_background_completion(
        &self,
        id: &str,
        now_unix_ms: i64,
    ) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.background_completions
            .lock()
            .get(id)
            .filter(|(deadline, _)| now_unix_ms >= *deadline)
            .map(|(_, completion)| completion.clone())
    }

    pub(crate) fn drain_background_tasks(&self) -> Vec<tokio::task::JoinHandle<()>> {
        self.background_completions.lock().clear();
        std::mem::take(&mut *self.background_tasks.lock())
            .into_values()
            .collect()
    }
}

thread_local! {
    static ACTIVE_TRIGGER_REGISTRY: RefCell<Arc<TriggerRegistryRuntime>> =
        RefCell::new(fresh_trigger_registry());
}

pub(crate) fn fresh_trigger_registry() -> Arc<TriggerRegistryRuntime> {
    Arc::new(TriggerRegistryRuntime::default())
}

pub(crate) fn active_trigger_registry() -> Arc<TriggerRegistryRuntime> {
    ACTIVE_TRIGGER_REGISTRY.with(|slot| Arc::clone(&slot.borrow()))
}

pub(crate) fn swap_active_trigger_registry(
    next: Arc<TriggerRegistryRuntime>,
) -> Arc<TriggerRegistryRuntime> {
    ACTIVE_TRIGGER_REGISTRY.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), next))
}

pub(super) fn with_trigger_registry<T>(read: impl FnOnce(&TriggerRegistry) -> T) -> T {
    let owner = active_trigger_registry();
    let registry = owner.registry.lock();
    read(&registry)
}

pub(super) fn with_trigger_registry_mut<T>(write: impl FnOnce(&mut TriggerRegistry) -> T) -> T {
    let owner = active_trigger_registry();
    let mut registry = owner.registry.lock();
    write(&mut registry)
}
