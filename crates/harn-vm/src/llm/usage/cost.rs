//! Cost estimates use the same normalized usage settlement as provider receipts.

use harn_builtin_macros::harn_builtin;
use serde::{Deserialize, Serialize};

use super::{
    BillingUsage, LlmUsage, PricingFacts, ProviderReceiptSettlement, ProviderUsageReceipt,
};
use crate::llm::api::PromptCacheTtl;
pub use crate::llm::cost::CallCostBreakdown;
use crate::value::{VmError, VmValue};

/// Normalized usage for one call. Input includes cache reads and writes.
/// The request instant selects promotions, schedules, and serving-tier rates.
#[derive(Clone, Debug)]
pub struct CallCostInput {
    pub provider: String,
    pub model: String,
    pub started_at_ms: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub prompt_cache_ttl: Option<PromptCacheTtl>,
    pub served_fast: bool,
    pub billing: BillingUsage,
}

/// A catalog estimate, without recording a provider request or spending a budget.
/// A partial estimate keeps its priced portion and names the missing units.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CallCostSettlement {
    pub provider: String,
    pub model: String,
    pub cost_usd: Option<f64>,
    pub known_cost_usd: f64,
    pub pricing: Option<Box<PricingFacts>>,
    pub unpriced: Vec<CallCostUnpriced>,
    pub rates: Option<CallCostRates>,
    pub breakdown: Option<CallCostBreakdown>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CallCostRates {
    pub provider: String,
    pub model: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: Option<f64>,
    pub cache_write_per_mtok: Option<f64>,
    pub cache_write_1h_per_mtok: Option<f64>,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CallCostUnpriced {
    PricingUnknown,
    CacheReadRateUnknown,
    CacheTtlUnpriced,
    HostedTool { name: String },
    Modality { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid call cost input: {0}")]
pub struct CallCostInputError(String);

/// Settle supplied usage against the current catalog at the supplied instant.
/// This performs no provider request, ledger write, or budget mutation.
/// Monthly hosted-tool allowances remain unapplied without account-wide usage.
pub fn settle_call_cost(input: &CallCostInput) -> Result<CallCostSettlement, CallCostInputError> {
    validate(input)?;
    let receipt = ProviderUsageReceipt {
        input_tokens: Some(input.input_tokens),
        reported_input_tokens: Some(input.input_tokens),
        output_tokens: Some(input.output_tokens),
        cache_read_tokens: input.cache_read_tokens,
        cache_write_tokens: input.cache_write_tokens,
        started_at_ms: Some(input.started_at_ms),
        prompt_cache_ttl: input.prompt_cache_ttl,
        served_fast: input.served_fast,
        billing: Some(Box::new(input.billing.clone())),
        ..ProviderUsageReceipt::default()
    };
    let settled = LlmUsage::settle_provider_receipt(&input.provider, &input.model, &receipt);
    project_settlement(input, settled)
}

fn project_settlement(
    input: &CallCostInput,
    settled: ProviderReceiptSettlement,
) -> Result<CallCostSettlement, CallCostInputError> {
    let ProviderReceiptSettlement {
        usage,
        pricing: detail,
        billing,
        free_inference,
    } = settled;
    let token_rate = |rate| if free_inference { 0.0 } else { rate * 1000.0 };
    let cache_rate = |rate: Option<f64>| {
        if free_inference {
            Some(0.0)
        } else {
            rate.map(token_rate)
        }
    };
    let rates = detail.as_ref().map(|detail| CallCostRates {
        provider: input.provider.clone(),
        model: input.model.clone(),
        input_per_mtok: token_rate(detail.input_per_1k),
        output_per_mtok: token_rate(detail.output_per_1k),
        cache_read_per_mtok: cache_rate(detail.cache_read_per_1k),
        cache_write_per_mtok: cache_rate(detail.cache_write_per_1k),
        cache_write_1h_per_mtok: cache_rate(detail.cache_write_1h_per_1k),
        source: if free_inference {
            crate::llm::cost::PricingSource::ProviderEconomics.as_str()
        } else {
            detail.source.as_str()
        }
        .into(),
    });
    let breakdown = detail.as_ref().map(|detail| {
        let mut breakdown = if free_inference {
            CallCostBreakdown::default()
        } else {
            crate::llm::cost::project_call_cost_breakdown(
                detail,
                input.input_tokens,
                input.output_tokens,
                input.cache_read_tokens,
                input.cache_write_tokens,
                input.prompt_cache_ttl,
            )
        };
        breakdown.adjustments_usd = usage.known_cost_usd - breakdown.total();
        breakdown
    });
    let mut unpriced = Vec::new();
    if let Some(pricing) = &usage.pricing {
        if !free_inference
            && input.cache_read_tokens > 0
            && detail
                .as_ref()
                .is_some_and(|detail| detail.cache_read_per_1k.is_none())
        {
            unpriced.push(CallCostUnpriced::CacheReadRateUnknown);
        }
        if !free_inference && pricing.cache_ttl_unpriced && input.cache_write_tokens > 0 {
            unpriced.push(CallCostUnpriced::CacheTtlUnpriced);
        }
    }
    unpriced.extend(
        billing
            .hosted_tool_unpriced
            .iter()
            .map(|name| CallCostUnpriced::HostedTool { name: name.clone() }),
    );
    unpriced.extend(
        billing
            .modality_unpriced
            .iter()
            .map(|name| CallCostUnpriced::Modality { name: name.clone() }),
    );
    if usage.cost_usd.is_none() {
        unpriced.push(CallCostUnpriced::PricingUnknown);
    }
    if !usage.known_cost_usd.is_finite() || usage.cost_usd.is_some_and(|cost| !cost.is_finite()) {
        return Err(CallCostInputError(
            "settled cost exceeds the supported range".into(),
        ));
    }
    Ok(CallCostSettlement {
        provider: input.provider.clone(),
        model: input.model.clone(),
        cost_usd: (usage.unpriced_calls == 0 && unpriced.is_empty())
            .then_some(usage.cost_usd)
            .flatten(),
        known_cost_usd: usage.known_cost_usd,
        pricing: usage.pricing,
        unpriced,
        rates,
        breakdown,
    })
}

#[cfg(test)]
#[path = "cost_tests.rs"]
mod tests;

fn validate(input: &CallCostInput) -> Result<(), CallCostInputError> {
    let invalid = |message: &str| CallCostInputError(message.into());
    if input.provider.trim().is_empty() || input.model.trim().is_empty() {
        return Err(invalid("provider and model must be nonempty"));
    }
    if time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(input.started_at_ms) * 1_000_000)
        .is_err()
    {
        return Err(invalid("started_at_ms is outside the supported date range"));
    }
    for (name, count) in [
        ("input_tokens", input.input_tokens),
        ("output_tokens", input.output_tokens),
        ("cache_read_tokens", input.cache_read_tokens),
        ("cache_write_tokens", input.cache_write_tokens),
    ] {
        if count < 0 {
            return Err(CallCostInputError(format!("{name} must be nonnegative")));
        }
    }
    if input.cache_read_tokens > input.input_tokens
        || input.cache_write_tokens > input.input_tokens - input.cache_read_tokens
    {
        return Err(invalid(
            "cache reads and writes exceed the full input count",
        ));
    }
    if !input.billing.counts_fit(
        input.input_tokens,
        input.output_tokens,
        input.cache_read_tokens,
        input.cache_write_tokens,
    ) {
        return Err(invalid("audio counts exceed their enclosing token counts"));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CostOptions {
    provider: Option<String>,
    model: String,
    started_at_ms: Option<i64>,
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    cache_read_tokens: i64,
    #[serde(default)]
    cache_write_tokens: i64,
    prompt_cache_ttl: Option<String>,
    #[serde(default)]
    served_fast: bool,
    billing: Option<CostBilling>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CostBilling {
    #[serde(default)]
    hosted_tool_calls: std::collections::BTreeMap<String, u64>,
    audio_input_tokens: Option<u64>,
    audio_output_tokens: Option<u64>,
    cached_audio_input_tokens: Option<u64>,
}

impl From<CostBilling> for BillingUsage {
    fn from(value: CostBilling) -> Self {
        Self {
            invalid_modality_counts: false,
            hosted_tool_calls: value.hosted_tool_calls,
            audio_input_tokens: value.audio_input_tokens,
            audio_output_tokens: value.audio_output_tokens,
            cached_audio_input_tokens: value.cached_audio_input_tokens,
        }
    }
}

#[harn_builtin(
    exposure = "runtime_internal", effects = [],
    sig = "__llm_settle_call_cost(input: dict) -> dict", category = "llm.economics",
    doc = "Estimate supplied usage through canonical cost settlement at an explicit request instant. Does not dispatch or record a provider call."
)]
fn llm_settle_call_cost_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let error = |message: String| VmError::Runtime(format!("llm_settle_call_cost: {message}"));
    let value = args
        .first()
        .ok_or_else(|| error("input is required".into()))?;
    let options: CostOptions = serde_json::from_value(crate::llm::vm_value_to_json(value))
        .map_err(|cause| error(cause.to_string()))?;
    let (provider, model) = if let Some(provider) = options.provider {
        (provider, options.model)
    } else {
        let route = crate::llm_config::resolve_model_info(&options.model);
        (route.provider, route.id)
    };
    let prompt_cache_ttl = options
        .prompt_cache_ttl
        .map(|ttl| {
            PromptCacheTtl::parse(&ttl)
                .ok_or_else(|| error("prompt_cache_ttl must be 5m or 1h".into()))
        })
        .transpose()?;
    let result = settle_call_cost(&CallCostInput {
        provider,
        model,
        started_at_ms: options.started_at_ms.unwrap_or_else(|| {
            (crate::llm::cost::settlement_now().unix_timestamp_nanos() / 1_000_000) as i64
        }),
        input_tokens: options.input_tokens,
        output_tokens: options.output_tokens,
        cache_read_tokens: options.cache_read_tokens,
        cache_write_tokens: options.cache_write_tokens,
        prompt_cache_ttl,
        served_fast: options.served_fast,
        billing: options.billing.unwrap_or_default().into(),
    })
    .map_err(|cause| error(cause.to_string()))?;
    let json = serde_json::to_value(result).map_err(|cause| error(cause.to_string()))?;
    Ok(crate::schema::json_to_vm_value(&json))
}

pub(crate) fn register_settlement_builtin(vm: &mut crate::vm::Vm) {
    vm.register_builtin_def(&LLM_SETTLE_CALL_COST_BUILTIN_DEF);
}
