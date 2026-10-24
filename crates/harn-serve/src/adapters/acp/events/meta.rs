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

pub(in crate::adapters::acp) fn inference_speed_notification(
    event: &AgentEvent,
) -> serde_json::Value {
    let AgentEvent::InferenceSpeed {
        session_id,
        call_id,
        provider,
        model,
        speed,
    } = event
    else {
        unreachable!("inference_speed_notification requires InferenceSpeed")
    };
    let mut update = serde_json::json!({"sessionUpdate": "inference_speed"});
    let mut harn_meta = serde_json::Map::new();
    harn_meta.insert("callId".to_string(), serde_json::json!(call_id));
    harn_meta.insert("provider".to_string(), serde_json::json!(provider));
    harn_meta.insert("model".to_string(), serde_json::json!(model));
    harn_meta.insert("phase".to_string(), serde_json::json!(speed.phase));
    harn_meta.insert("isFinal".to_string(), serde_json::json!(speed.is_final));
    harn_meta.insert("elapsedMs".to_string(), serde_json::json!(speed.elapsed_ms));
    harn_meta.insert(
        "outputTokens".to_string(),
        serde_json::json!(speed.output_tokens),
    );
    harn_meta.insert(
        "tokenSource".to_string(),
        serde_json::json!(speed.token_source),
    );
    if let Some(ms) = speed.time_to_first_token_ms {
        harn_meta.insert("timeToFirstTokenMs".to_string(), serde_json::json!(ms));
    }
    if let Some(rate) = speed.output_tokens_per_second {
        harn_meta.insert("outputTokensPerSecond".to_string(), serde_json::json!(rate));
    }
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
