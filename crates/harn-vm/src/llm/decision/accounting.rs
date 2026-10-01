//! Project native completion into the shared usage owner without repricing it.

use crate::llm::trace::{trace_llm_call, LlmTraceEntry};
use crate::llm::usage::{LlmUsage, ProviderUsageReceipt, UnpricedReason};

use super::receipt::{AccountingStatus, EvaluationReceipt, EvaluationSource};

pub(super) fn record_native(receipt: &mut EvaluationReceipt) {
    if receipt.source != EvaluationSource::Live || receipt.physical_attempts == 0 {
        return;
    }
    let input = receipt.input_tokens.map(token_count);
    let output = receipt.output_tokens.map(token_count);
    let mut usage = if receipt.accounting_status == AccountingStatus::Settled {
        LlmUsage::from_provider_receipt(
            &receipt.requested_provider,
            &receipt.requested_model,
            &ProviderUsageReceipt::new(input, output, receipt.cost_usd, false),
        )
    } else {
        // A retained reservation is not a provider bill. Keep observed token
        // fragments, but do not price an incomplete or contradictory ledger.
        let mut usage = LlmUsage::unknown_attempts(receipt.physical_attempts as usize);
        usage.input_tokens = input.unwrap_or(0);
        usage.output_tokens = output.unwrap_or(0);
        usage.cache_accounting_declared = None;
        usage.cache_hit_ratio = None;
        if let Some(facts) = usage.unpriced.as_mut() {
            facts.tokens = usage.input_tokens.saturating_add(usage.output_tokens);
            facts.reason = UnpricedReason::UsageUnreported;
        }
        usage
    };
    // This counts observed outer requests. Gateway-reported downstream attempts
    // remain separate native transport evidence, including an absent report.
    usage.provider_call_count = Some(i64::from(receipt.physical_attempts));
    trace_llm_call(LlmTraceEntry {
        model: receipt.requested_model.clone(),
        provider: receipt.requested_provider.clone(),
        usage: usage.clone(),
        duration_ms: receipt.elapsed_ms,
    });
    if let Some(metrics) = crate::active_metrics_registry() {
        metrics.record_llm_call(
            &receipt.requested_provider,
            &receipt.requested_model,
            &receipt.outcome_kind,
            &usage,
        );
    }
    receipt.usage = Some(Box::new(usage));
}

fn token_count(tokens: u64) -> i64 {
    i64::try_from(tokens).unwrap_or(i64::MAX)
}
