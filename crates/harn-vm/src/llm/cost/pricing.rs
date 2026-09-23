//! Rate-card resolution for a (provider, model) route at a request instant.
//!
//! `cost.rs` owns budget arithmetic and the builtins; this owns the question
//! those ask first: what did this route cost per token at the moment this call
//! started, and which card said so. Keeping the two apart means a change to
//! how a card is selected never reaches into how a budget is spent.

use time::OffsetDateTime;

use super::super::api::PromptCacheTtl;
use crate::llm_config::RateCard;

/// Resolved pricing for a (provider, model) pair, expressed per 1k tokens.
/// The `source` discriminates how the rate was found so callers (CLI cost
/// explanation, economics helpers, `cost_route` summaries) can report it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PricingDetail {
    pub input_per_1k: f64,
    pub output_per_1k: f64,
    pub cache_read_per_1k: Option<f64>,
    pub cache_write_per_1k: Option<f64>,
    /// Rate for a cache write the request asked to hold for an hour. `None`
    /// means the route publishes no such tier; a request that asked for one
    /// settles at `cache_write_per_1k` and the receipt says so.
    pub cache_write_1h_per_1k: Option<f64>,
    pub source: PricingSource,
    /// Which dated or recurring card the request instant selected.
    pub rate_card: RateCard,
    /// Whole-request input band that applied, named by its lower bound.
    pub input_band_minimum: Option<u64>,
    pub hosted_tool_fees: std::collections::BTreeMap<String, crate::llm_config::HostedToolFee>,
    pub modality_rates: Option<crate::llm_config::ModalityRates>,
    pub platform_fee_percent: f64,
}

impl PricingDetail {
    fn for_provider(mut self, provider: &str) -> Self {
        self.platform_fee_percent = crate::llm_config::provider_config(provider)
            .and_then(|provider| provider.platform_fee_percent)
            .unwrap_or(0.0);
        self
    }
    fn from_pricing(
        pricing: &crate::llm_config::ModelPricing,
        rate_card: RateCard,
        input_band_minimum: Option<u64>,
        source: PricingSource,
    ) -> Self {
        Self {
            input_per_1k: pricing.input_per_mtok / 1000.0,
            output_per_1k: pricing.output_per_mtok / 1000.0,
            cache_read_per_1k: pricing.cache_read_per_mtok.map(|rate| rate / 1000.0),
            cache_write_per_1k: pricing.cache_write_per_mtok.map(|rate| rate / 1000.0),
            cache_write_1h_per_1k: pricing.cache_write_1h_per_mtok.map(|rate| rate / 1000.0),
            source,
            rate_card,
            input_band_minimum,
            hosted_tool_fees: pricing.hosted_tool_fees.clone(),
            modality_rates: pricing.modality_rates.clone(),
            platform_fee_percent: 0.0,
        }
    }

    /// The cache-write rate for the TTL the request asked for, and whether the
    /// route could price that TTL. A route with no one-hour tier reports
    /// `false` so the receipt can say `cache_ttl_unpriced` instead of letting
    /// a longer, more expensive write settle silently at the short rate.
    /// Whether the route publishes a rate for the lifetime the request asked
    /// for. False means the write settled at the short-lifetime rate.
    pub(crate) fn cache_write_priced(&self, ttl: Option<PromptCacheTtl>) -> bool {
        self.cache_write_rate(ttl).1
    }

    pub(super) fn cache_write_rate(&self, ttl: Option<PromptCacheTtl>) -> (f64, bool) {
        let short = self.cache_write_per_1k.unwrap_or(self.input_per_1k);
        match ttl {
            Some(PromptCacheTtl::OneHour) => match self.cache_write_1h_per_1k {
                Some(rate) => (rate, true),
                None => (short, false),
            },
            _ => (short, true),
        }
    }
}

/// Settle at the pricing clock's current instant.
///
/// Only pre-call projections and presentation use this: a completed call
/// settles at the instant it started, which its telemetry carries. Reading the
/// active clock rather than the system clock keeps mocked-time tests
/// deterministic, which `ModelPricing::effective_today` is not.
pub(crate) fn settlement_now() -> OffsetDateTime {
    crate::llm_config::pricing_clock_now()
}

/// A wall-clock millisecond reading as an instant. Out-of-range readings fall
/// back to the epoch rather than panicking; a catalog card resolved there is
/// the base card, which is the same answer an empty schedule gives.
pub(crate) fn instant_from_wall_ms(value_ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(value_ms) * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PricingSource {
    /// Exact model entry in the catalog (configured `[llm.models.<id>]`).
    CatalogModel,
    /// The model's accelerated-serving tier (`serving_tiers[].pricing`), used
    /// when the provider confirmed it served the request fast.
    CatalogServingTier,
    /// Provider-level catalog economics (`[llm.providers.<name>]`).
    ProviderEconomics,
}

impl PricingSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PricingSource::CatalogModel => "catalog_model",
            PricingSource::CatalogServingTier => "catalog_serving_tier",
            PricingSource::ProviderEconomics => "provider_economics",
        }
    }
}

/// Resolve catalog pricing for the route identity reported by a transport,
/// as the rate card stood at `at`.
fn model_pricing_for_observed_route(
    provider: &str,
    model: &str,
    at: OffsetDateTime,
) -> Option<crate::llm_config::ResolvedPricing> {
    crate::llm_config::model_pricing_per_mtok_for_route(provider, model, at).or_else(|| {
        // Mock responses carry the modeled provider's model identity while the
        // transport remains `mock`. Preserve catalog-backed budget accounting
        // without weakening provider scoping for any real route.
        (provider == "mock")
            .then(|| crate::llm_config::model_pricing_per_mtok(model, at))
            .flatten()
    })
}

/// Resolve full pricing detail for a (provider, model) pair. Prefers the
/// provider-scoped catalog entry, then falls back to provider economics.
/// Returns `None` for unknown pricing — callers must decide whether to
/// surface that explicitly or coerce to 0.0.
pub(crate) fn pricing_detail_for(
    provider: &str,
    model: &str,
    at: OffsetDateTime,
) -> Option<PricingDetail> {
    if let Some(resolved) = model_pricing_for_observed_route(provider, model, at) {
        return Some(
            PricingDetail::from_pricing(
                &resolved.pricing,
                resolved.rate_card,
                None,
                PricingSource::CatalogModel,
            )
            .for_provider(provider),
        );
    }
    let (input, output, _) = crate::llm_config::provider_economics(provider);
    match (input, output) {
        (Some(input_per_1k), Some(output_per_1k)) => Some(
            PricingDetail {
                input_per_1k,
                output_per_1k,
                cache_read_per_1k: None,
                cache_write_per_1k: None,
                cache_write_1h_per_1k: None,
                source: PricingSource::ProviderEconomics,
                rate_card: RateCard::Base,
                input_band_minimum: None,
                hosted_tool_fees: Default::default(),
                modality_rates: None,
                platform_fee_percent: 0.0,
            }
            .for_provider(provider),
        ),
        _ => None,
    }
}

pub(super) fn pricing_detail_for_usage(
    provider: &str,
    model: &str,
    input_tokens: i64,
    at: OffsetDateTime,
) -> Option<PricingDetail> {
    if let Some(resolved) = model_pricing_for_observed_route(provider, model, at) {
        let (band, pricing) = match resolved.pricing.band_for_input_tokens(input_tokens) {
            Some((minimum, banded)) => (Some(minimum), banded),
            None => (None, resolved.pricing),
        };
        return Some(
            PricingDetail::from_pricing(
                &pricing,
                resolved.rate_card,
                band,
                PricingSource::CatalogModel,
            )
            .for_provider(provider),
        );
    }
    pricing_detail_for(provider, model, at)
}

pub(crate) fn pricing_per_1k_for(provider: &str, model: &str) -> Option<(f64, f64)> {
    pricing_detail_for(provider, model, settlement_now()).map(|p| (p.input_per_1k, p.output_per_1k))
}

/// Resolve pricing for a (provider, model) pair, billing at the premium
/// accelerated-serving tier when `served_fast` is set and the catalog declares
/// explicit tier rates or an economic multiplier. Falls back to standard
/// pricing when the request was served at the standard tier, such as after a
/// capacity downgrade.
pub(crate) fn pricing_detail_for_tier(
    provider: &str,
    model: &str,
    served_fast: bool,
    input_tokens: i64,
    at: OffsetDateTime,
) -> Option<PricingDetail> {
    if served_fast {
        if let Some(mut resolved) = crate::llm_config::model_serving_tier_pricing_per_mtok_for_route(
            provider,
            model,
            crate::llm::serving_tiers::FAST_TIER_ID,
            at,
        ) {
            if let Some(model_pricing) =
                crate::llm_config::model_pricing_per_mtok_for_route(provider, model, at)
            {
                resolved.pricing.input_token_bands = model_pricing.pricing.input_token_bands;
            }
            let (band, pricing) = match resolved.pricing.band_for_input_tokens(input_tokens) {
                Some((minimum, banded)) => (Some(minimum), banded),
                None => (None, resolved.pricing),
            };
            return Some(
                PricingDetail::from_pricing(
                    &pricing,
                    resolved.rate_card,
                    band,
                    PricingSource::CatalogServingTier,
                )
                .for_provider(provider),
            );
        }
    }
    pricing_detail_for_usage(provider, model, input_tokens, at)
}
