use harn_vm::agent_events::StagedWriteSummary;

pub(super) fn harn_meta(
    pending_count: usize,
    total_bytes: u64,
    pending_writes: &[StagedWriteSummary],
) -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::from_iter([
        (
            "kind".to_string(),
            serde_json::Value::String("staged_writes_pending".to_string()),
        ),
        (
            "pendingCount".to_string(),
            serde_json::Value::from(pending_count as u64),
        ),
        (
            "totalBytes".to_string(),
            serde_json::Value::from(total_bytes),
        ),
        (
            "pendingWrites".to_string(),
            serde_json::to_value(pending_writes).unwrap_or_default(),
        ),
    ])
}

#[cfg(test)]
pub(super) fn assert_capabilities(capabilities: &serde_json::Value) {
    use super::schema::{
        HARN_PROMPT_RESULT_EXTENSION_FIELDS, HARN_STAGED_WRITES_PENDING_FIELDS,
        HARN_STAGED_WRITE_FIELDS,
    };

    assert_eq!(
        capabilities["promptResultExtensionFields"],
        serde_json::json!(HARN_PROMPT_RESULT_EXTENSION_FIELDS)
    );
    assert_eq!(
        capabilities["stagedWritesPendingFields"],
        serde_json::json!(HARN_STAGED_WRITES_PENDING_FIELDS)
    );
    assert_eq!(
        capabilities["stagedWriteFields"],
        serde_json::json!(HARN_STAGED_WRITE_FIELDS)
    );
}
