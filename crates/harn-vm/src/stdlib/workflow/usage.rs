//! LLM usage/tracing snapshots and delta accounting.

use crate::orchestration::LlmUsageRecord;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct UsageSnapshot {
    pub(super) input_tokens: i64,
    pub(super) output_tokens: i64,
    pub(super) total_duration_ms: i64,
    pub(super) call_count: i64,
    pub(super) total_cost: f64,
    pub(super) unpriced_calls: i64,
    pub(super) usage_unknown_calls: i64,
    pub(super) trace_len: usize,
}

pub(super) fn llm_usage_snapshot() -> UsageSnapshot {
    let trace = crate::llm::peek_trace_usage_summary();
    UsageSnapshot {
        input_tokens: trace.input_tokens,
        output_tokens: trace.output_tokens,
        total_duration_ms: trace.duration_ms,
        call_count: trace.call_count,
        total_cost: trace.cost.known_cost_usd,
        unpriced_calls: trace.cost.unpriced_calls,
        usage_unknown_calls: trace.cost.usage_unknown_calls,
        trace_len: usize::try_from(trace.call_count).unwrap_or(usize::MAX),
    }
}

pub(super) fn llm_usage_delta(before: &UsageSnapshot, after: &UsageSnapshot) -> LlmUsageRecord {
    let trace = crate::llm::peek_trace();
    let start = before.trace_len.min(trace.len());
    let models = trace[start..]
        .iter()
        .map(|entry| entry.model.clone())
        .filter(|model| !model.is_empty())
        .fold(Vec::<String>::new(), |mut acc, model| {
            if !acc.iter().any(|existing| existing == &model) {
                acc.push(model);
            }
            acc
        });

    let known_cost_usd = (after.total_cost - before.total_cost).max(0.0);
    let unpriced_calls = after.unpriced_calls.saturating_sub(before.unpriced_calls);
    LlmUsageRecord {
        input_tokens: after.input_tokens.saturating_sub(before.input_tokens),
        output_tokens: after.output_tokens.saturating_sub(before.output_tokens),
        total_duration_ms: after
            .total_duration_ms
            .saturating_sub(before.total_duration_ms),
        call_count: after.call_count.saturating_sub(before.call_count),
        unpriced_calls,
        usage_unknown_calls: after
            .usage_unknown_calls
            .saturating_sub(before.usage_unknown_calls),
        cost_usd: (unpriced_calls == 0).then_some(known_cost_usd),
        known_cost_usd,
        total_cost: known_cost_usd,
        models,
    }
}
