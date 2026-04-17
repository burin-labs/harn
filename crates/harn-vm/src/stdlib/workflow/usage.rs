//! LLM usage/tracing snapshots and delta accounting.

use crate::orchestration::LlmUsageRecord;

#[derive(Clone, Debug)]
pub(super) struct UsageSnapshot {
    pub(super) input_tokens: i64,
    pub(super) output_tokens: i64,
    pub(super) total_duration_ms: i64,
    pub(super) call_count: i64,
    pub(super) total_cost: f64,
    pub(super) trace_len: usize,
}

pub(super) fn llm_usage_snapshot() -> UsageSnapshot {
    let (input_tokens, output_tokens, total_duration_ms, call_count) =
        crate::llm::peek_trace_summary();
    let total_cost = crate::llm::cost::peek_total_cost();
    let trace_len = crate::llm::peek_trace().len();
    UsageSnapshot {
        input_tokens,
        output_tokens,
        total_duration_ms,
        call_count,
        total_cost,
        trace_len,
    }
}

pub(super) fn merge_usage(total: &mut LlmUsageRecord, usage: &LlmUsageRecord) {
    total.input_tokens += usage.input_tokens;
    total.output_tokens += usage.output_tokens;
    total.total_duration_ms += usage.total_duration_ms;
    total.call_count += usage.call_count;
    total.total_cost += usage.total_cost;
    for model in &usage.models {
        if !total.models.iter().any(|existing| existing == model) {
            total.models.push(model.clone());
        }
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

    LlmUsageRecord {
        input_tokens: after.input_tokens.saturating_sub(before.input_tokens),
        output_tokens: after.output_tokens.saturating_sub(before.output_tokens),
        total_duration_ms: after
            .total_duration_ms
            .saturating_sub(before.total_duration_ms),
        call_count: after.call_count.saturating_sub(before.call_count),
        total_cost: (after.total_cost - before.total_cost).max(0.0),
        models,
    }
}
