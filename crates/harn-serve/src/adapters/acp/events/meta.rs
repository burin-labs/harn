//! Construction of ACP's namespaced Harn metadata envelope.

use harn_vm::agent_events::AgentEvent;

pub(in crate::adapters::acp) fn reminder_notification(event: &AgentEvent) -> serde_json::Value {
    let AgentEvent::ReminderEmitted {
        session_id,
        reminder_id,
        tags,
        body,
        role_hint,
        authority,
        rendered_role,
        source,
        ttl_turns,
    } = event
    else {
        unreachable!("reminder_notification requires ReminderEmitted")
    };
    let mut update = serde_json::json!({"sessionUpdate": "reminder_emitted"});
    let mut harn_meta = serde_json::Map::new();
    harn_meta.insert(
        "reminder".to_string(),
        serde_json::json!({
            "reminderId": reminder_id,
            "tags": tags,
            "body": body,
            "roleHint": role_hint,
            "authority": authority,
            "renderedRole": rendered_role,
            "source": source,
            "ttlTurns": ttl_turns,
        }),
    );
    merge_harn_meta(&mut update, harn_meta);
    serde_json::json!({"sessionId": session_id, "update": update})
}

/// Merge `harn_meta` keys into `value._meta.harn`, creating intermediate
/// objects as needed. Existing `_meta.harn` keys are preserved (unless
/// overwritten by `harn_meta`). No-op when `harn_meta` is empty or
/// `value` is not a JSON object.
pub(in crate::adapters::acp) fn merge_harn_meta(
    value: &mut serde_json::Value,
    harn_meta: serde_json::Map<String, serde_json::Value>,
) {
    if harn_meta.is_empty() {
        return;
    }
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let meta = obj
        .entry("_meta".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(meta_obj) = meta.as_object_mut() else {
        return;
    };
    let harn = meta_obj
        .entry("harn".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(harn_obj) = harn.as_object_mut() else {
        return;
    };
    for (key, value) in harn_meta {
        harn_obj.insert(key, value);
    }
}
