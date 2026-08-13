use serde::{Deserialize, Serialize};

use super::super::LlmUsageRecord;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
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
            models: value.models.clone(),
        }
    }
}

impl RunViewUsage {
    pub(super) fn add_usage(&mut self, usage: &Self) {
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.total_duration_ms += usage.total_duration_ms;
        self.call_count += usage.call_count;
        self.unpriced_calls += usage.unpriced_calls;
        self.usage_unknown_calls += usage.usage_unknown_calls;
        self.known_cost_usd += usage.known_cost_usd;
        self.total_cost = self.known_cost_usd;
        self.cost_usd = (self.unpriced_calls == 0).then_some(self.known_cost_usd);
        for model in &usage.models {
            if !model.is_empty() && !self.models.contains(model) {
                self.models.push(model.clone());
            }
        }
    }
}
