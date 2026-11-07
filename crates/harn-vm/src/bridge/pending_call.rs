//! One RPC registration lives only as long as its waiting call.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{oneshot, Mutex};

pub(super) struct PendingCallRegistration {
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    id: u64,
    cancel: Option<Arc<dyn Fn(u64) + Send + Sync>>,
}

impl PendingCallRegistration {
    pub(super) fn new(pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>, id: u64) -> Self {
        Self {
            pending,
            id,
            cancel: None,
        }
    }

    /// Notify the remote owner only if the call ends before its response arrives.
    pub(super) fn with_cancellation(mut self, cancel: Arc<dyn Fn(u64) + Send + Sync>) -> Self {
        self.cancel = Some(cancel);
        self
    }
}

impl Drop for PendingCallRegistration {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.try_lock() {
            let removed = pending.remove(&self.id).is_some();
            drop(pending);
            if removed {
                if let Some(cancel) = &self.cancel {
                    cancel(self.id);
                }
            }
            return;
        }
        // A response reader can briefly own the map. Retire only this ID when
        // that lock becomes available, without blocking the current executor.
        let pending = self.pending.clone();
        let id = self.id;
        let cancel = self.cancel.clone();
        let retire = move || async move {
            let removed = pending.lock().await.remove(&id).is_some();
            if removed {
                if let Some(cancel) = cancel {
                    cancel(id);
                }
            }
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            crate::task_spawn::spawn_on(&runtime, retire());
        } else {
            // A caller may drop a previously polled future outside its runtime.
            crate::task_spawn::thread(move || {
                futures::executor::block_on(retire());
            });
        }
    }
}
