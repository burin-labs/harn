//! Session-owned identities and capture failures for automatic boundaries.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct State {
    ids: HashSet<String>,
    failures: HashMap<String, String>,
    pending: HashSet<String>,
    closed: bool,
    unavailable: bool,
}

#[derive(Clone, Default)]
pub(super) struct CheckpointTracking(Arc<Mutex<State>>);

impl CheckpointTracking {
    pub(super) fn contains(&self, id: &str) -> Result<bool, &'static str> {
        let state = self
            .0
            .lock()
            .map_err(|_| "checkpoint tracking unavailable")?;
        if state.unavailable {
            return Err("checkpoint tracking unavailable");
        }
        Ok(state.ids.contains(id))
    }

    pub(super) fn reserve(&self, id: &str) -> Result<bool, &'static str> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| "checkpoint tracking unavailable")?;
        if state.unavailable || state.closed {
            return Err("checkpoint tracking unavailable");
        }
        Ok(state.ids.insert(id.to_string()))
    }

    pub(super) fn fail(&self, id: &str, detail: String) {
        if let Ok(mut state) = self.0.lock() {
            state.failures.insert(id.to_string(), detail);
        }
    }

    #[cfg(any(test, feature = "hostlib"))]
    pub(super) fn failure(&self, id: &str) -> Result<Option<String>, &'static str> {
        let state = self
            .0
            .lock()
            .map_err(|_| "checkpoint tracking unavailable")?;
        if state.unavailable {
            return Err("checkpoint tracking unavailable");
        }
        if state.pending.contains(id) {
            return Ok(Some("message delivery has not finished".to_string()));
        }
        Ok(state.failures.get(id).cloned())
    }

    pub(super) fn fork(&self, inherit: bool) -> Self {
        let mut copy = match self.0.lock() {
            Ok(state) => state.clone(),
            Err(_) => State {
                unavailable: true,
                ..State::default()
            },
        };
        if !inherit {
            copy.failures.clear();
            copy.pending.clear();
        } else {
            for id in copy.pending.drain() {
                copy.failures.insert(
                    id,
                    "message delivery was incomplete when the session forked".to_string(),
                );
            }
        }
        copy.closed = false;
        Self(Arc::new(Mutex::new(copy)))
    }

    pub(super) fn close(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.closed = true;
        }
    }

    #[cfg(feature = "hostlib")]
    pub(super) fn drop_checkpoint(
        &self,
        session_id: &str,
    ) -> Result<harn_hostlib::workspace_checkpoint::DropReceipt, &'static str> {
        let state = self
            .0
            .lock()
            .map_err(|_| "checkpoint tracking unavailable")?;
        if state.unavailable {
            return Err("checkpoint tracking unavailable");
        }
        Ok(harn_hostlib::workspace_checkpoint::drop_session(session_id))
    }

    #[cfg(feature = "hostlib")]
    pub(super) fn capture_boundary(
        &self,
        session_id: &str,
        message_id: &str,
        root: &std::path::Path,
        store_scope: &harn_vm::SessionStoreScope,
        already_reserved: bool,
    ) {
        let Ok(mut state) = self.0.lock() else { return };
        if state.closed || state.unavailable {
            return;
        }
        if already_reserved && !state.ids.contains(message_id) {
            state.failures.insert(
                message_id.to_string(),
                "checkpoint identity was not reserved".to_string(),
            );
            return;
        }
        if !already_reserved && !state.ids.insert(message_id.to_string()) {
            state.failures.insert(
                message_id.to_string(),
                "messageId was already used by this checkpoint lineage".to_string(),
            );
            return;
        }
        if !already_reserved {
            state.pending.insert(message_id.to_string());
        }
        let _scope = store_scope.enter();
        let capture =
            harn_hostlib::workspace_checkpoint::capture(session_id, message_id, Some(root));
        let failure = match capture {
            Ok(harn_hostlib::workspace_checkpoint::Outcome::Ready(_)) => None,
            Ok(harn_hostlib::workspace_checkpoint::Outcome::Unavailable(reason)) => {
                Some(format!("{}: {}", reason.reason, reason.detail))
            }
            Err(error) => Some(error.to_string()),
        };
        if let Some(failure) = failure {
            state.failures.insert(message_id.to_string(), failure);
        }
    }

    #[cfg(any(test, feature = "hostlib"))]
    pub(super) fn finish_injection(&self, message_id: &str, delivered: bool) {
        if let Ok(mut state) = self.0.lock() {
            state.pending.remove(message_id);
            if !delivered {
                state.failures.insert(
                    message_id.to_string(),
                    "message delivery failed".to_string(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_does_not_promote_an_incomplete_delivery_to_a_valid_boundary() {
        let tracking = CheckpointTracking::default();
        {
            let mut state = tracking.0.lock().unwrap();
            state.ids.insert("message".to_string());
            state.pending.insert("message".to_string());
        }
        assert!(tracking.failure("message").unwrap().is_some());
        let fork = tracking.fork(true);
        tracking.finish_injection("message", true);
        assert!(tracking.failure("message").unwrap().is_none());
        assert!(fork.failure("message").unwrap().is_some());
        assert!(fork.contains("message").unwrap());
    }
}
