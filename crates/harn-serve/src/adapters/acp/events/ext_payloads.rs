//! Payload bodies for `_harn/agentEvent` extension notifications.
//!
//! ACP defines no `session/update` discriminator for Harn-native milestones, so
//! they ride the `_harn/agentEvent` extension channel. Each builder here
//! produces that notification's `params` body minus the `sessionId` / `kind`
//! keys, which `emit_agent_event_ext` stamps on. Keeping them out of the
//! dispatch match leaves each arm a single readable line and gives the wire
//! shapes one owner.

use harn_vm::agent_events::AgentEvent;

/// The loud-boundary funnel (harn#5142). A client that renders this can tell
/// "the model produced nothing" from "the harness dropped what the model
/// produced" — the distinction the whole class of bug turned on. `owner`
/// carries the attribution, `boundary` says where, `excerpt` carries the bytes
/// that died.
pub(super) fn boundary_failure(event: &AgentEvent) -> serde_json::Value {
    let AgentEvent::BoundaryFailure {
        boundary,
        kind,
        owner,
        detail,
        excerpt,
        dropped_count,
        dropped_bytes,
        unreported,
        ..
    } = event
    else {
        return serde_json::json!({});
    };
    let mut payload = serde_json::json!({
        "boundary": boundary.as_str(),
        "kind": kind.as_str(),
        "owner": owner,
        "detail": detail,
        "droppedCount": dropped_count,
        "droppedBytes": dropped_bytes,
        "unreported": unreported,
    });
    if let Some(excerpt) = excerpt {
        payload["excerpt"] = serde_json::Value::String(excerpt.clone());
    }
    payload
}

/// A concrete provider/model pair lacking a catalog recommendation, and the
/// fallback the runtime chose instead.
pub(super) fn capability_gap(event: &AgentEvent) -> serde_json::Value {
    let AgentEvent::CapabilityGap {
        level,
        capability,
        provider,
        model,
        fallback_tool_format,
        requested_tool_format,
        message,
        ..
    } = event
    else {
        return serde_json::json!({});
    };
    let mut payload = serde_json::json!({
        "level": level,
        "capability": capability,
        "provider": provider,
        "model": model,
        "fallbackToolFormat": fallback_tool_format,
        "message": message,
    });
    if let Some(requested) = requested_tool_format {
        payload["requestedToolFormat"] = serde_json::Value::String(requested.clone());
    }
    payload
}
