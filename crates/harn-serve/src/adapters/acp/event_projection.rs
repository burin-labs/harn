//! Helpers for projecting `AgentEvent`s onto the ACP wire.
//!
//! A handful of [`AgentEvent`] variants are pure diagnostics: they differ only
//! in their wire name and each carries the whole record its stdlib emitter
//! built. Projecting them to ACP is therefore the same two lines every time —
//! name plus payload — which is a match arm's worth of ceremony per variant and
//! an easy thing to forget when a new handler event lands.
//!
//! Collecting them here means a new `std/llm` handler event costs a row in this
//! table rather than an arm in the sink, and the sink's own `match` stays about
//! events that need real work. Small payload predicates the sink reads while
//! projecting live here too, for the same reason.

use harn_vm::agent_events::AgentEvent;

/// The wire name and payload for an event whose ACP projection is a
/// passthrough, or `None` when the event needs a real projection.
pub(super) fn passthrough_projection(
    event: &AgentEvent,
) -> Option<(&'static str, &serde_json::Value)> {
    let (name, payload) = match event {
        AgentEvent::CacheHit { payload, .. } => ("cache_hit", payload),
        AgentEvent::CacheMiss { payload, .. } => ("cache_miss", payload),
        AgentEvent::LlmCallLog { payload, .. } => ("llm_call_log", payload),
        AgentEvent::LlmRoutingDecision { payload, .. } => ("llm_routing_decision", payload),
        AgentEvent::LlmFallbackAttempt { payload, .. } => ("llm_fallback_attempt", payload),
        AgentEvent::LlmShadowDiff { payload, .. } => ("llm_shadow_diff", payload),
        AgentEvent::SemanticCacheHit { payload, .. } => ("semantic_cache_hit", payload),
        AgentEvent::SemanticCacheMiss { payload, .. } => ("semantic_cache_miss", payload),
        _ => return None,
    };
    Some((name, payload))
}

pub(super) fn has_progress_entries(entries: &serde_json::Value) -> bool {
    entries
        .as_array()
        .map(|entries| !entries.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handler_event_projects_under_its_wire_name_with_its_whole_record() {
        let event = AgentEvent::LlmCallLog {
            session_id: "s1".to_string(),
            model: "gpt-5.6-luna".to_string(),
            provider: "openai".to_string(),
            status: "ok".to_string(),
            latency_ms: 3067,
            iteration: 1,
            attempt: 1,
            payload: serde_json::json!({"latency_ms": 3067, "status": "ok"}),
        };
        let (name, payload) = passthrough_projection(&event).expect("a passthrough family member");
        assert_eq!(name, "llm_call_log");
        assert_eq!(payload["latency_ms"], 3067);
    }

    #[test]
    fn an_event_that_needs_real_projection_is_left_to_the_sink() {
        let event = AgentEvent::AgentMessageChunk {
            session_id: "s1".to_string(),
            content: "hello".to_string(),
        };
        assert!(
            passthrough_projection(&event).is_none(),
            "claiming an event here would silently strip its real ACP shape"
        );
    }
}
