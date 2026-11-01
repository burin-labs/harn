use super::{summarize_usage_cost_certainty, LlmUsage};
use serde::{Deserialize, Serialize};

/// Cumulative physical usage for a live provider-attempt notification.
///
/// `cost_usd` is an exact total only when every dispatched request is priced.
/// Otherwise `known_cost_usd` is the measured lower bound. The count includes
/// dispatched requests still in flight, whose usage remains unpriced.
/// Token totals are measured subtotals when `usage_unknown_calls` is nonzero.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderUsage {
    /// Identity of the Harn run whose cumulative ledger this records. Empty
    /// means the runtime could not measure the identity.
    pub run_id: String,
    /// Empty when the known session has no parent, or when lineage is unknown.
    pub parent_session_id: String,
    /// False when the session owner could not measure lineage.
    pub lineage_known: bool,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    #[serde(default)]
    pub reported_cache: Option<Box<super::ReportedCacheTotals>>,
    pub provider_call_count: i64,
    pub unpriced_calls: i64,
    pub usage_unknown_calls: i64,
    pub known_cost_usd: f64,
    pub cost_usd: Option<f64>,
}

impl ProviderUsage {
    pub(crate) fn from_ledger(
        usage: &LlmUsage,
        run_id: String,
        parent_session_id: String,
        lineage_known: bool,
    ) -> Self {
        let certainty = summarize_usage_cost_certainty([usage]);
        Self {
            run_id,
            parent_session_id,
            lineage_known,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            reported_cache: usage.reported_cache.clone(),
            provider_call_count: certainty.provider_call_count,
            unpriced_calls: certainty.unpriced_calls,
            usage_unknown_calls: certainty.usage_unknown_calls,
            known_cost_usd: certainty.known_cost_usd,
            cost_usd: certainty.cost_usd(),
        }
    }
}

/// Schema used to generate the live-usage record for protocol clients.
#[must_use]
pub fn provider_usage_schema() -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(ProviderUsage))
        .expect("provider usage schema serializes");
    // Serde always emits this nullable field. Keep it present when generated
    // clients encode an unpriced ledger.
    let required = schema["required"]
        .as_array_mut()
        .expect("provider usage has required fields");
    if !required.iter().any(|field| field == "cost_usd") {
        required.push("cost_usd".into());
    }
    schema
}
