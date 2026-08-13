//! Permission-event selection for the portable session-bundle projection.

use serde_json::Value as JsonValue;

use super::BundlePermission;

pub(super) fn collect_permission_events(
    permissions: &mut Vec<BundlePermission>,
    source: &str,
    transcript: &Option<JsonValue>,
) {
    let Some(transcript) = transcript else {
        return;
    };
    for event in transcript
        .get("events")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
    {
        let kind = event
            .get("type")
            .or_else(|| event.get("kind"))
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let normalized_kind = kind.to_ascii_lowercase();
        if normalized_kind.contains("permission")
            || normalized_kind.contains("approval")
            || normalized_kind.starts_with("hitl_")
        {
            permissions.push(permission_from_event(event, source, kind));
        }
    }
}

fn permission_from_event(event: &JsonValue, source: &str, kind: &str) -> BundlePermission {
    let activity = event
        .get("metadata")
        .and_then(|metadata| metadata.get("activity"));
    BundlePermission {
        kind: kind.to_string(),
        source: source.to_string(),
        request_id: activity
            .and_then(|activity| activity.get("request_id"))
            .or_else(|| event.get("request_id"))
            .or_else(|| event.get("id"))
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        agent: activity
            .and_then(|activity| activity.get("requester"))
            .and_then(|requester| requester.get("agent_id"))
            .or_else(|| event.get("agent"))
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        payload: activity.cloned().unwrap_or_else(|| event.clone()),
    }
}
