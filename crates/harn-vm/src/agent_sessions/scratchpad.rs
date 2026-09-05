use super::*;

pub fn scratchpad(id: &str) -> Option<VmValue> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .and_then(|state| state.scratchpad.clone())
    })
}

pub fn scratchpad_version(id: &str) -> Option<u64> {
    SESSIONS.with(|s| s.borrow().get(id).map(|state| state.scratchpad_version))
}

pub fn set_scratchpad(
    id: &str,
    scratchpad: VmValue,
    source: impl Into<String>,
    reason: Option<String>,
    metadata: serde_json::Value,
) -> Result<u64, String> {
    validate_scratchpad_value(&scratchpad)?;
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let version = state.scratchpad_version.saturating_add(1);
        let event = scratchpad_transcript_event(
            "set",
            version,
            Some(&scratchpad),
            source.into(),
            reason,
            metadata,
        );
        append_event_to_state(state, event, "set_scratchpad")?;
        state.scratchpad = Some(scratchpad);
        state.scratchpad_version = version;
        state.touch();
        Ok(version)
    })
}

pub fn clear_scratchpad(
    id: &str,
    source: impl Into<String>,
    reason: Option<String>,
    metadata: serde_json::Value,
) -> Result<u64, String> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let version = state.scratchpad_version.saturating_add(1);
        let event =
            scratchpad_transcript_event("clear", version, None, source.into(), reason, metadata);
        append_event_to_state(state, event, "clear_scratchpad")?;
        state.scratchpad = None;
        state.scratchpad_version = version;
        state.touch();
        Ok(version)
    })
}

fn validate_scratchpad_value(value: &VmValue) -> Result<(), String> {
    if !matches!(value, VmValue::Dict(_)) {
        return Err("agent session scratchpad must be a dict".to_string());
    }
    let json = crate::llm::helpers::vm_value_to_json(value);
    let approx_bytes = serde_json::to_vec(&json)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX);
    if approx_bytes > MAX_SCRATCHPAD_BYTES {
        return Err(format!(
            "agent session scratchpad is {approx_bytes} bytes; max is {MAX_SCRATCHPAD_BYTES}"
        ));
    }
    Ok(())
}

fn scratchpad_transcript_event(
    action: &str,
    version: u64,
    scratchpad: Option<&VmValue>,
    source: String,
    reason: Option<String>,
    metadata: serde_json::Value,
) -> VmValue {
    let scratchpad_json = scratchpad.map(crate::llm::helpers::vm_value_to_json);
    let approx_bytes = scratchpad_json
        .as_ref()
        .and_then(|value| serde_json::to_vec(value).ok().map(|bytes| bytes.len()))
        .unwrap_or(0);
    let event_metadata = serde_json::json!({
        "action": action,
        "version": version,
        "source": normalize_scratchpad_source(source),
        "reason": reason.unwrap_or_default(),
        "approx_bytes": approx_bytes,
        "counts": scratchpad_json
            .as_ref()
            .map(scratchpad_counts_json)
            .unwrap_or_else(|| serde_json::json!({})),
        "metadata": metadata,
    });
    let content = format!("Agent scratchpad {action}");
    crate::llm::helpers::transcript_event(
        "agent_scratchpad",
        "system",
        "internal",
        &content,
        Some(event_metadata),
    )
}

fn normalize_scratchpad_source(source: String) -> String {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        "harn.agent_scratchpad".to_string()
    } else {
        trimmed.to_string()
    }
}

fn scratchpad_counts_json(value: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "goals": scratchpad_array_len(value, "goals"),
        "open_items": scratchpad_array_len(value, "open_items"),
        "facts": scratchpad_array_len(value, "facts"),
        "refs": scratchpad_array_len(value, "refs"),
    })
}

fn scratchpad_array_len(value: &serde_json::Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}
