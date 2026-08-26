//! LLM transaction-ledger projections for the shared runtime metrics surface.

use super::{labels, MetricsRegistry};
use crate::llm::usage::{summarize_usage_cost_certainty, LlmUsage};

impl MetricsRegistry {
    pub(crate) fn record_llm_call(
        &self,
        provider: &str,
        model: &str,
        outcome: &str,
        usage: &LlmUsage,
    ) {
        self.increment_counter(
            "harn_llm_calls_total",
            labels([
                ("provider", provider),
                ("model", model),
                ("outcome", outcome),
            ]),
            1,
        );
        let accounting_labels = labels([("provider", provider), ("model", model)]);
        let certainty = summarize_usage_cost_certainty([usage]);
        if certainty.known_cost_usd > 0.0 {
            self.increment_counter(
                "harn_llm_cost_usd_total",
                accounting_labels.clone(),
                certainty.known_cost_usd,
            );
        } else {
            self.ensure_counter("harn_llm_cost_usd_total", accounting_labels.clone());
        }
        self.increment_counter(
            "harn_llm_provider_requests_total",
            accounting_labels.clone(),
            certainty.provider_call_count as f64,
        );
        self.increment_counter(
            "harn_llm_unpriced_requests_total",
            accounting_labels.clone(),
            certainty.unpriced_calls as f64,
        );
        self.ensure_counter(
            "harn_llm_unpriced_requests_total",
            accounting_labels.clone(),
        );
        self.increment_counter(
            "harn_llm_usage_unknown_requests_total",
            accounting_labels.clone(),
            certainty.usage_unknown_calls as f64,
        );
        self.ensure_counter("harn_llm_usage_unknown_requests_total", accounting_labels);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_keep_sub_microdollar_cost_precision() {
        assert_eq!(super::super::prometheus_float(0.0000117), "0.0000117");
    }

    #[test]
    fn call_metrics_preserve_physical_request_spend_certainty() {
        let metrics = MetricsRegistry::default();
        let usage = LlmUsage {
            cost_usd: None,
            known_cost_usd: 0.01,
            provider_call_count: 2,
            unpriced_calls: 1,
            usage_unknown_calls: 1,
            ..LlmUsage::known_zero_attempt()
        };

        metrics.record_llm_call("mock", "mock", "succeeded", &usage);

        let rendered = metrics.render_prometheus();
        for needle in [
            "harn_llm_calls_total{model=\"mock\",outcome=\"succeeded\",provider=\"mock\"} 1",
            "harn_llm_cost_usd_total{model=\"mock\",provider=\"mock\"} 0.01",
            "harn_llm_provider_requests_total{model=\"mock\",provider=\"mock\"} 2",
            "harn_llm_unpriced_requests_total{model=\"mock\",provider=\"mock\"} 1",
            "harn_llm_usage_unknown_requests_total{model=\"mock\",provider=\"mock\"} 1",
        ] {
            assert!(rendered.contains(needle), "missing {needle}\n{rendered}");
        }
    }
}
