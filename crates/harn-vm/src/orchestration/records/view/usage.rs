use serde::{Deserialize, Serialize};

use super::super::LlmUsageRecord;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(default)]
pub struct RunViewUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_duration_ms: i64,
    pub call_count: i64,
    pub unpriced_calls: i64,
    pub usage_unknown_calls: i64,
    pub cost_usd: Option<f64>,
    pub known_cost_usd: f64,
    pub total_cost: f64,
    /// Cache tokens summed over the calls that reported cache accounting;
    /// null when no call did, which is distinct from a measured zero.
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub models: Vec<String>,
}

impl From<&LlmUsageRecord> for RunViewUsage {
    fn from(value: &LlmUsageRecord) -> Self {
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            total_duration_ms: value.total_duration_ms,
            call_count: value.call_count,
            unpriced_calls: value.unpriced_calls,
            usage_unknown_calls: value.usage_unknown_calls,
            cost_usd: value.cost_usd,
            known_cost_usd: value.known_cost_usd,
            total_cost: value.total_cost,
            cache_read_tokens: value.cache_read_tokens,
            cache_write_tokens: value.cache_write_tokens,
            models: value.models.clone(),
        }
    }
}

impl RunViewUsage {
    pub(super) fn add_usage(&mut self, usage: &Self) {
        // A missing total is not a measured zero, even when the input has no
        // explicit unpriced count. Preserve each owner's certainty while folding.
        let total_is_exact = (self.call_count == 0 || self.cost_usd.is_some())
            && (usage.call_count == 0 || usage.cost_usd.is_some());
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.total_duration_ms += usage.total_duration_ms;
        self.call_count += usage.call_count;
        self.unpriced_calls += usage.unpriced_calls;
        self.usage_unknown_calls += usage.usage_unknown_calls;
        self.known_cost_usd += usage.known_cost_usd;
        self.total_cost = self.known_cost_usd;
        self.cost_usd = total_is_exact.then_some(self.known_cost_usd);
        self.cache_read_tokens = add_reported(self.cache_read_tokens, usage.cache_read_tokens);
        self.cache_write_tokens = add_reported(self.cache_write_tokens, usage.cache_write_tokens);
        for model in &usage.models {
            if !model.is_empty() && !self.models.contains(model) {
                self.models.push(model.clone());
            }
        }
    }
}

/// A reported count plus an unreported one is still a report; two unreported
/// counts stay unreported rather than summing to a measured zero.
fn add_reported(total: Option<i64>, more: Option<i64>) -> Option<i64> {
    match (total, more) {
        (None, None) => None,
        (total, more) => Some(total.unwrap_or(0).saturating_add(more.unwrap_or(0))),
    }
}
