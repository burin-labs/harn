//! Session-scoped facts supplied by the host for the current prompt.

use super::HostBridge;

impl HostBridge {
    /// Set the ACP session ID for session-scoped notifications.
    pub fn set_session_id(&self, id: &str) {
        *self.session_id.lock().unwrap_or_else(|e| e.into_inner()) = id.to_string();
    }

    pub fn get_session_id(&self) -> String {
        self.session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Bind the already normalized identity of the current ACP prompt.
    pub fn set_caller_message_id(&self, id: Option<String>) {
        *self
            .caller_message_id
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = id;
    }

    /// Resolve identity only for the ACP session this prompt actually targets.
    pub(crate) fn caller_message_id_for_session(&self, session_id: &str) -> Option<String> {
        if session_id != self.get_session_id() {
            return None;
        }
        self.caller_message_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn caller_message_identity_is_bound_to_the_targeted_session() {
        let bridge = super::super::tests::test_bridge();
        bridge.set_session_id("parent");
        bridge.set_caller_message_id(Some("caller-turn".into()));
        assert_eq!(
            bridge.caller_message_id_for_session("parent").as_deref(),
            Some("caller-turn")
        );
        assert_eq!(bridge.caller_message_id_for_session("child"), None);
        bridge.set_caller_message_id(None);
        assert_eq!(bridge.caller_message_id_for_session("parent"), None);
    }
}
