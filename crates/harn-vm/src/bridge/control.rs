use std::sync::{atomic::AtomicBool, Arc};

use tokio::sync::Notify;

use super::HostBridgeInjectionState;
use crate::tool_call_cancellations::{fresh_registry, CancellationRegistry};

/// Shared control state for one host-bridge execution domain.
///
/// Keeping cancellation, notification, queued injection, and targeted
/// tool-call routing in one value prevents protocol adapters from assembling
/// only part of the state a VM and its out-of-band control task must share.
#[derive(Clone)]
pub struct HostBridgeControlState {
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) cancel_notify: Arc<Notify>,
    pub(super) queued_transcript_injections: HostBridgeInjectionState,
    pub(super) tool_call_cancellations: Arc<CancellationRegistry>,
}

impl HostBridgeControlState {
    pub fn new(
        cancelled: Arc<AtomicBool>,
        cancel_notify: Arc<Notify>,
        queued_transcript_injections: HostBridgeInjectionState,
        tool_call_cancellations: Arc<CancellationRegistry>,
    ) -> Self {
        Self {
            cancelled,
            cancel_notify,
            queued_transcript_injections,
            tool_call_cancellations,
        }
    }

    pub(super) fn isolated(cancelled: Arc<AtomicBool>) -> Self {
        Self::new(
            cancelled,
            Arc::new(Notify::new()),
            HostBridgeInjectionState::default(),
            fresh_registry(),
        )
    }
}

/// Apply a host notification to the bridge's explicit cancellation address space.
pub(super) fn handle_cancel_tool_call_notification(
    registry: &CancellationRegistry,
    params: &serde_json::Value,
) {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let call_id = params
        .get("toolCallId")
        .or_else(|| params.get("tool_call_id"))
        .or_else(|| params.get("callId"))
        .or_else(|| params.get("call_id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if call_id.is_empty() {
        return;
    }
    let reason = params
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("host cancelled in-flight tool call")
        .to_string();
    let inject_reminder = params
        .get("injectReminder")
        .or_else(|| params.get("inject_reminder"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    registry.cancel(session_id, call_id, reason, inject_reminder);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targeted_cancel_notification_uses_the_supplied_registry() {
        let registry = Arc::new(CancellationRegistry::default());
        let (handle, _guard) = registry.register("session", "call", "shell");

        handle_cancel_tool_call_notification(
            &registry,
            &serde_json::json!({
                "sessionId": "session",
                "toolCallId": "call",
                "reason": "host stop",
                "injectReminder": false,
            }),
        );

        assert!(handle.is_cancelled());
        assert_eq!(handle.reason().as_deref(), Some("host stop"));
    }
}
