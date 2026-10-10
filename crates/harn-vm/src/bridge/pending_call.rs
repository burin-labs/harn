//! One RPC registration lives only as long as its waiting call.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{oneshot, Mutex};

pub(super) struct PendingCallRegistration {
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    id: u64,
}

impl PendingCallRegistration {
    pub(super) fn new(pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>, id: u64) -> Self {
        Self { pending, id }
    }
}

impl Drop for PendingCallRegistration {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.try_lock() {
            pending.remove(&self.id);
            return;
        }
        // A response reader can briefly own the map. Retire only this ID when
        // that lock becomes available, without blocking the current executor.
        let pending = self.pending.clone();
        let id = self.id;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                pending.lock().await.remove(&id);
            });
        } else {
            // A caller may drop a previously polled future outside its runtime.
            crate::runtime_stack::spawn(move || {
                pending.blocking_lock().remove(&id);
            });
        }
    }
}
