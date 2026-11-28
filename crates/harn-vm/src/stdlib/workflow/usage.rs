//! LLM usage/tracing snapshots and delta accounting.

use crate::llm::usage::ReportedCacheTotals;
use crate::orchestration::LlmUsageRecord;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct UsageSnapshot {
    pub(super) input_tokens: i64,
    pub(super) output_tokens: i64,
    pub(super) total_duration_ms: i64,
    pub(super) call_count: i64,
    #[serde(default)]
    pub(super) provider_call_count: Option<i64>,
    pub(super) total_cost: f64,
    pub(super) unpriced_calls: i64,
    pub(super) usage_unknown_calls: i64,
    pub(super) trace_len: usize,
    #[serde(default)]
    // Boxed: snapshots sit by value in workflow frames across awaits, so inline size is paid per frame.
    pub(super) cache: Box<ReportedCacheTotals>,
}

pub(super) fn llm_usage_snapshot() -> UsageSnapshot {
    let trace = crate::llm::peek_trace_usage_summary();
    UsageSnapshot {
        input_tokens: trace.input_tokens,
        output_tokens: trace.output_tokens,
        total_duration_ms: trace.duration_ms,
        call_count: trace.call_count,
        provider_call_count: if trace.call_count == 0 {
            Some(0)
        } else {
            trace.provider_call_count
        },
        total_cost: trace.cost.known_cost_usd,
        unpriced_calls: trace.cost.unpriced_calls,
        usage_unknown_calls: trace.cost.usage_unknown_calls,
        trace_len: usize::try_from(trace.call_count).unwrap_or(usize::MAX),
        cache: Box::new(trace.cache),
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
    let cache = after.cache.since(&before.cache);
    LlmUsageRecord {
        input_tokens: after.input_tokens.saturating_sub(before.input_tokens),
        output_tokens: after.output_tokens.saturating_sub(before.output_tokens),
        total_duration_ms: after
            .total_duration_ms
            .saturating_sub(before.total_duration_ms),
        call_count: after.call_count.saturating_sub(before.call_count),
        provider_call_count: after
            .provider_call_count
            .zip(before.provider_call_count)
            .map(|(after, before)| after.saturating_sub(before)),
        unpriced_calls,
        usage_unknown_calls: after
            .usage_unknown_calls
            .saturating_sub(before.usage_unknown_calls),
        cost_usd: (unpriced_calls == 0).then_some(known_cost_usd),
        known_cost_usd,
        total_cost: known_cost_usd,
        cache_read_tokens: cache.reported_read_tokens(),
        cache_write_tokens: cache.reported_write_tokens(),
        reported_cache: Some(Box::new(cache)),
        models,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_delta_preserves_physical_units_and_legacy_unknown() {
        let before = UsageSnapshot {
            call_count: 1,
            provider_call_count: Some(1),
            ..UsageSnapshot::default()
        };
        let after = UsageSnapshot {
            call_count: 2,
            provider_call_count: Some(4),
            cache: Box::new(ReportedCacheTotals {
                reported_calls: 1,
                read_reported_calls: 1,
                write_reported_calls: 0,
                read_tokens: 40,
                write_tokens: 0,
            }),
            ..UsageSnapshot::default()
        };
        let usage = llm_usage_delta(&before, &after);
        assert_eq!(usage.call_count, 1);
        assert_eq!(usage.provider_call_count, Some(3));
        assert_eq!(
            usage.reported_cache.as_ref().unwrap().read_reported_calls,
            1
        );
        let mut legacy = serde_json::to_value(&before).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("provider_call_count");
        let legacy: UsageSnapshot = serde_json::from_value(legacy).unwrap();
        assert_eq!(llm_usage_delta(&legacy, &after).provider_call_count, None);
    }
}
