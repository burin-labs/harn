use harn_vm::orchestration::{CompactionReceipt, COMPACTION_RECEIPT_SCHEMA_VERSION};

/// A compaction receipt for ACP session-update fixtures. Only the fields the
/// fixtures vary are parameters; the remaining values describe one stable
/// sample event.
pub(super) fn fixture_compaction_receipt(
    receipt_id: &str,
    instruction_mode: Option<&str>,
    instruction_source: Option<&str>,
    compaction_policy: Option<serde_json::Value>,
) -> CompactionReceipt {
    CompactionReceipt {
        schema_version: COMPACTION_RECEIPT_SCHEMA_VERSION,
        receipt_id: receipt_id.to_string(),
        session_id: Some("session-1".to_string()),
        transcript_id: Some("session-1".to_string()),
        mode: "auto".to_string(),
        reason: "threshold".to_string(),
        strategy: "summary".to_string(),
        engine_strategy: "observation_mask".to_string(),
        requested_strategy: Some("summary".to_string()),
        resolved_threshold_tokens: Some(1),
        threshold_source: Some("runtime_config".to_string()),
        hard_limit_tokens: None,
        archived_messages: 3,
        estimated_tokens_before: 100,
        estimated_tokens_after: 40,
        snapshot_asset_id: Some("asset-1".to_string()),
        instruction_mode: instruction_mode.map(str::to_string),
        instruction_source: instruction_source.map(str::to_string),
        compaction_policy,
        // ACP does not project the source measurement, so the fixture keeps
        // that field absent rather than inventing a measured zero.
        source_measurement: None,
        recap: None,
    }
}
