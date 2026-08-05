//! The canonical, versioned compaction receipt.
//!
//! A single transcript compaction is one lifecycle fact. Historically that fact
//! was reconstructed independently in three places — the persisted transcript
//! `compaction` event metadata, the live `AgentEvent::TranscriptCompacted`
//! payload, and `RunObservabilityRecord.compaction_events` (which reparsed the
//! transcript JSON key-by-key and dropped `reason`/`recap`). ACP consumers had
//! no stable identity to key on and synthesized host-local UUIDs.
//!
//! [`CompactionReceipt`] is the one owner of that fact. It is constructed once
//! at the lifecycle boundary (`run_compaction_lifecycle`) or, for host-script
//! driven compaction, normalized once at the `__host_agent_record_compaction`
//! builtin boundary. It is then:
//!
//!   * embedded verbatim under `metadata.receipt` on the transcript event, whose
//!     event `id` is set to [`CompactionReceipt::receipt_id`];
//!   * carried on `AgentEvent::TranscriptCompacted` and forwarded through ACP;
//!   * deserialized (typed, not scraped) into `CompactionEventRecord`.
//!
//! So one compaction operation yields exactly one `receipt_id` across all four
//! projections, and `reason`, `recap`, snapshot provenance, and policy survive
//! every projection without a host synthesizing identity.

use serde::{Deserialize, Serialize};

use super::RecapMetrics;

/// Current on-the-wire schema version for [`CompactionReceipt`]. Bump this when
/// the receipt's meaning changes in a way readers must branch on; additive
/// optional fields do not require a bump because `#[serde(default)]` fills them.
pub const COMPACTION_RECEIPT_SCHEMA_VERSION: u32 = 1;

fn default_schema_version() -> u32 {
    COMPACTION_RECEIPT_SCHEMA_VERSION
}

/// One serializable, versioned record of a single transcript compaction. See the
/// module docs for how it flows through every projection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionReceipt {
    /// Schema version. Lets readers migrate: a receipt read from an older
    /// persisted transcript that predates a meaning change carries its original
    /// version, and a transcript with no embedded receipt at all is migrated by
    /// the record builder (see `compaction_events_from_transcript`).
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Stable identity for this compaction operation, shared verbatim across the
    /// transcript event id, the live event, ACP, and the observability record.
    pub receipt_id: String,
    /// Owning agent session, when the compaction ran against one.
    pub session_id: Option<String>,
    /// Owning transcript, when known. Best-effort provenance; the record's
    /// `transcript_id` is sourced authoritatively from the transcript itself.
    pub transcript_id: Option<String>,
    /// Call-site surface that initiated compaction (`CompactMode::as_str`).
    pub mode: String,
    /// Why compaction fired (`CompactionTrigger::as_str`, or a host-supplied
    /// reason such as `context_overflow`).
    pub reason: String,
    /// User-facing policy label (may be broader than the engine strategy).
    pub strategy: String,
    /// Engine strategy actually used after honoring any PreCompact `Modify`.
    pub engine_strategy: String,
    pub archived_messages: usize,
    pub estimated_tokens_before: usize,
    pub estimated_tokens_after: usize,
    /// Id of the pre-compaction snapshot asset, when one was built.
    pub snapshot_asset_id: Option<String>,
    pub instruction_mode: Option<String>,
    pub instruction_source: Option<String>,
    pub compaction_policy: Option<serde_json::Value>,
    /// Observation-mask recap metrics; `None` for the LLM/truncate/custom
    /// strategies, which do not spend a recap budget.
    pub recap: Option<RecapMetrics>,
}

/// Generate a fresh, unique compaction receipt id.
pub fn new_compaction_receipt_id() -> String {
    format!("compaction-{}", uuid::Uuid::now_v7())
}

impl CompactionReceipt {
    /// Serialize the receipt for verbatim embedding under `metadata.receipt` on
    /// the transcript event.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Read a receipt embedded under `metadata.receipt` on a transcript
    /// `compaction` event. Returns `None` when the key is absent (a legacy
    /// transcript written before receipts existed) or malformed, so the caller
    /// can fall back to the legacy flat-metadata migration path.
    pub fn from_event_metadata(metadata: Option<&serde_json::Value>) -> Option<Self> {
        let receipt = metadata?.get("receipt")?;
        serde_json::from_value(receipt.clone()).ok()
    }

    /// Normalize a host-script compaction payload — the dict `.harn` code hands
    /// to `agent_record_compaction` / `__host_agent_record_compaction` — into a
    /// receipt at the builtin boundary, minting a fresh `receipt_id`. This is the
    /// one place the host-driven shape is validated, so the `.harn` auto-compact
    /// path yields the same unified receipt as the Rust lifecycle paths.
    pub fn from_host_payload(session_id: &str, payload: &serde_json::Value) -> Self {
        let str_field = |key: &str| {
            payload
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let usize_field = |key: &str| {
            payload
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize
        };
        let strategy = str_field("strategy")
            .or_else(|| str_field("engine_strategy"))
            .unwrap_or_default();
        let engine_strategy = str_field("engine_strategy").unwrap_or_else(|| strategy.clone());
        Self {
            schema_version: COMPACTION_RECEIPT_SCHEMA_VERSION,
            receipt_id: new_compaction_receipt_id(),
            session_id: Some(session_id.to_string()),
            transcript_id: None,
            mode: str_field("mode").unwrap_or_else(|| "auto".to_string()),
            reason: str_field("reason").unwrap_or_else(|| "threshold".to_string()),
            strategy,
            engine_strategy,
            archived_messages: usize_field("archived_messages"),
            estimated_tokens_before: usize_field("estimated_tokens_before"),
            estimated_tokens_after: usize_field("estimated_tokens_after"),
            snapshot_asset_id: str_field("snapshot_asset_id"),
            instruction_mode: str_field("instruction_mode"),
            instruction_source: str_field("instruction_source"),
            compaction_policy: payload.get("compaction_policy").cloned(),
            recap: payload
                .get("recap")
                .and_then(|value| serde_json::from_value::<RecapMetrics>(value.clone()).ok()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_round_trips_through_json_with_recap_and_policy() {
        let receipt = CompactionReceipt {
            schema_version: COMPACTION_RECEIPT_SCHEMA_VERSION,
            receipt_id: "compaction-abc".to_string(),
            session_id: Some("session-1".to_string()),
            transcript_id: Some("session-1".to_string()),
            mode: "auto".to_string(),
            reason: "threshold".to_string(),
            strategy: "hybrid".to_string(),
            engine_strategy: "observation_mask".to_string(),
            archived_messages: 7,
            estimated_tokens_before: 4000,
            estimated_tokens_after: 1200,
            snapshot_asset_id: Some("snapshot-9".to_string()),
            instruction_mode: Some("extend".to_string()),
            instruction_source: Some("author".to_string()),
            compaction_policy: Some(serde_json::json!({"scope": "summary"})),
            recap: Some(RecapMetrics {
                recap_bytes: 512,
                budget_bytes: 16_000,
                kept_results_count: 3,
                dropped_count: 1,
                carried_prior_recap: true,
            }),
        };
        let json = receipt.to_json();
        let decoded = CompactionReceipt::from_event_metadata(Some(&serde_json::json!({
            "receipt": json,
        })))
        .expect("embedded receipt decodes");
        assert_eq!(decoded, receipt);
    }

    #[test]
    fn absent_receipt_key_yields_none_for_legacy_migration() {
        let legacy = serde_json::json!({
            "mode": "manual",
            "strategy": "truncate",
            "archived_messages": 3,
        });
        assert!(CompactionReceipt::from_event_metadata(Some(&legacy)).is_none());
        assert!(CompactionReceipt::from_event_metadata(None).is_none());
    }

    #[test]
    fn missing_schema_version_defaults_to_current() {
        let receipt: CompactionReceipt = serde_json::from_value(serde_json::json!({
            "receipt_id": "compaction-xyz",
            "mode": "manual",
        }))
        .expect("partial receipt loads");
        assert_eq!(receipt.schema_version, COMPACTION_RECEIPT_SCHEMA_VERSION);
        assert_eq!(receipt.receipt_id, "compaction-xyz");
        assert!(receipt.recap.is_none());
    }
}
