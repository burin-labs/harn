//! Canonical LLM usage accounting and its public projections.
//!
//! Provider adapters own wire parsing, but once a call has produced token and
//! cache counts every consumer must read this ledger. VM envelopes,
//! transcripts, traces, metrics, and provider probes must not independently
//! recompute cost or cache semantics.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::value::{VmDictExt, VmValue};

use super::api::{LlmResult, ProviderAttempts};

mod billing;
mod cache_fields;
pub use billing::BillingUsage;
mod prompt_tokens;
mod receipt;
mod reported_cache;
pub(crate) use cache_fields::{
    extract_cache_read_tokens, extract_cache_write_tokens, reported_cache_read_tokens,
    reported_cache_write_tokens,
};
pub(crate) use prompt_tokens::{InputTokenBasis, PromptTokenCounts, ReportedTokenUsage};
pub(crate) use receipt::ProviderUsageReceipt;
pub use reported_cache::ReportedCacheUsage;

/// The normalized accounting facts for one completed provider call.
///
/// This is the sole owner of derived cost/cache facts. It deliberately keeps
/// provider/model identity out of the public usage object: those remain route
/// metadata on the enclosing result and transcript event.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageAccountingStatus {
    Reported,
    /// Some attempt in this ledger was priced and some was not. The priced
    /// portion is a real measurement, so it is reported rather than blacked
    /// out; the unpriced attempts stay visible in `unpriced_calls`,
    /// `unpriced_tokens`, and `unpriced_reason`.
    Partial,
    #[default]
    Unknown,
}

impl UsageAccountingStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::Partial => "partial",
            Self::Unknown => "unknown",
        }
    }
}

/// What a ledger's unpriced attempts amount to, absent when every attempt was
/// priced.
///
/// Boxed, and behind an `Option`, because `LlmUsage` is embedded in stack
/// frames throughout the CLI and those frames are budgeted: carrying these
/// three fields inline grew 107 of them past their budget. The common ledger
/// prices every attempt and pays one null pointer for this.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnpricedFacts {
    /// Tokens the unpriced attempts did report, which is what bounds them.
    pub tokens: i64,
    /// Why they carry no price.
    pub reason: UnpricedReason,
    /// Worst case USD for the unpriced attempts alone, on top of the ledger's
    /// `known_cost_usd`. `None` means at least one of them has no bound at any
    /// token count, and a ceiling consumer must fail closed on that.
    pub projection_usd: Option<f64>,
}

/// Why an attempt in a ledger carries no price.
///
/// A ceiling consumer needs this to tell a bound it can compute from one it
/// cannot: an attempt that reported tokens on a priced route has a worst case,
/// while a route with no price table has none at any token count.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// One vocabulary, shared with `std/llm/economics`, which already emits
/// `pricing_unknown` and `cache_read_rate_unknown` on the same field name for
/// the price-table side of the same question. These add the usage side.
pub enum UnpricedReason {
    /// The route has no entry in the price table, so no token count bounds it.
    /// Named to match `economics.harn`, which emits this for the same cause.
    PricingUnknown,
    /// The route is priced but the attempt reported no usable token counts.
    UsageUnreported,
    /// The attempt produced no response at all, so neither a token count nor
    /// a price table bounds it.
    NoResponse,
    /// Mid-stream schema validation severed the connection, so the provider's
    /// end-of-stream usage frame never arrived. Partial output was generated
    /// and billed by the provider, but nothing local bounds how much, which is
    /// why this is distinct from `NoResponse`: the call did consume supply.
    StreamAborted,
    /// Unpriced attempts in this ledger disagree, or the ledger predates this
    /// field. Either way the projection refuses.
    Mixed,
}

impl UnpricedReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PricingUnknown => "pricing_unknown",
            Self::UsageUnreported => "usage_unreported",
            Self::NoResponse => "no_response",
            Self::StreamAborted => "stream_aborted",
            Self::Mixed => "mixed",
        }
    }

    /// Fold two reasons from sibling attempts into the one this ledger carries.
    fn merge(self, other: Self) -> Self {
        if self == other {
            self
        } else {
            Self::Mixed
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmUsage {
    /// Full prompt size, including cache reads and writes, on every provider.
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Provider-reported whole-call token count when available. This remains
    /// separate from the component counters because some providers return
    /// only a total; Harn must not fabricate a prompt/completion split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_total_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cache_supported: bool,
    /// Route-level `cache_usage_accounting` declaration carried from
    /// `ProviderTelemetry`. `None` covers undeclared routes and ledgers
    /// recorded before the field existed; both read as undeclared rather
    /// than borrowing `cache_supported`'s false precision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_accounting_declared: Option<bool>,
    pub cache_hit_ratio: Option<f64>,
    pub cache_savings_usd: f64,
    pub cache_hit: bool,
    pub served_fast: bool,
    #[serde(default)]
    pub accounting_status: UsageAccountingStatus,
    /// Known priced portion across every provider request represented here.
    /// This remains available when `cost_usd` is null because one sibling
    /// request was unpriced.
    #[serde(default)]
    pub known_cost_usd: f64,
    /// Provider requests represented by this ledger. Aggregated logical calls
    /// retain their physical transaction count instead of collapsing to one.
    ///
    /// `None` is a ledger recorded before this field existed, whose one-call
    /// certainty `summarize_usage_cost_certainty` reconstructs from the
    /// original stable fields. `Some(0)` is a measured zero: the producer
    /// observed no dispatch. Reading an integer zero as "legacy" made the
    /// second unrepresentable, so every pre-dispatch refusal folded into one
    /// unpriced call (burin-labs/harn#8529).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_call_count: Option<i64>,
    #[serde(default)]
    pub unpriced_calls: i64,
    #[serde(default)]
    pub usage_unknown_calls: i64,
    /// What this ledger's unpriced attempts amount to. `None` when every
    /// attempt was priced, which is both the common case and the one the
    /// stack-frame budget cares about.
    ///
    /// A ledger deserialized from before this field existed reads `None`,
    /// which would say "nothing unpriced" about a ledger that may have had
    /// unpriced attempts. `summarize_usage_cost_certainty` reconstructs those
    /// from `cost_usd` rather than letting the absent field read as clean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unpriced: Option<Box<UnpricedFacts>>,
    /// Which rate card settled this ledger's priced calls, and when.
    ///
    /// `None` is a ledger whose cost did not come from the catalog at all
    /// (a provider-authoritative cost, a self-hosted zero, or an unpriced
    /// attempt), or one recorded before this field existed. Boxed so the
    /// ledger's own frame stays inside the stack-frame budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<Box<PricingFacts>>,
    #[serde(default, flatten, deserialize_with = "billing::deserialize_optional")]
    pub billing: Option<Box<BillingUsage>>,
}

/// How settlement picked the rate card for one ledger.
///
/// This exists so per-call spend is auditable after the fact: a reader can see
/// which card, which whole-request band and which serving tier produced the
/// recorded `cost_usd`, without re-resolving a catalog that may have moved.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PricingFacts {
    /// `base`, `promotion:<id>`, or `schedule:<id>`.
    pub rate_card: String,
    /// The instant the card was resolved at: the call's start, not the moment
    /// it was priced.
    pub settled_at_ms: i64,
    /// Whole-request input band applied, named by its lower bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_band_minimum: Option<u64>,
    /// Serving tier whose rates applied, when the call was served on one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serving_tier: Option<String>,
    /// True when the request asked for a cache lifetime this route publishes
    /// no rate for, so the write settled at the short-lifetime rate. Naming it
    /// keeps an under-count visible instead of silent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cache_ttl_unpriced: bool,
    #[serde(default)]
    pub platform_fee_estimate_usd: f64,
    /// A catalog percentage is an estimate, not a provider-reported charge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_fee_basis: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosted_tool_unpriced: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modality_unpriced: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub monthly_allowance_unapplied: Vec<String>,
}

impl LlmUsage {
    /// Tokens the unpriced attempts reported, zero when nothing is unpriced.
    #[must_use]
    pub fn unpriced_tokens(&self) -> i64 {
        self.unpriced.as_ref().map_or(0, |facts| facts.tokens)
    }

    /// Why the unpriced attempts carry no price, `None` when nothing is.
    #[must_use]
    pub fn unpriced_reason(&self) -> Option<UnpricedReason> {
        self.unpriced.as_ref().map(|facts| facts.reason)
    }

    /// Worst case USD for the whole ledger: everything priced, plus a bound on
    /// everything that was not. `None` refuses, and a ceiling consumer that
    /// gets `None` must fail closed.
    #[must_use]
    pub fn projected_cost_usd(&self) -> Option<f64> {
        match &self.unpriced {
            None => Some(self.known_cost_usd),
            Some(facts) => facts
                .projection_usd
                .map(|projection| self.known_cost_usd + projection),
        }
    }
}

/// Aggregate cost certainty for a collection of canonical call ledgers.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageCostCertainty {
    pub known_cost_usd: f64,
    pub provider_call_count: i64,
    pub unpriced_calls: i64,
    pub usage_unknown_calls: i64,
    pub unpriced_tokens: i64,
    pub unpriced_reason: Option<UnpricedReason>,
    /// Worst case USD attributable to the unpriced attempts alone.
    pub unpriced_projection_usd: f64,
    /// At least one unpriced attempt has no computable bound.
    pub unprojectable: bool,
}

impl UsageCostCertainty {
    /// The number a ceiling consumer spends against: everything measured plus
    /// the worst case for everything that was not. `None` refuses, and a
    /// governor or budget that gets `None` must fail closed.
    #[must_use]
    pub fn projected_cost_usd(&self) -> Option<f64> {
        (!self.unprojectable).then_some(self.known_cost_usd + self.unpriced_projection_usd)
    }

    /// Whether any attempt folded here carried a price.
    #[must_use]
    pub const fn has_priced_attempt(&self) -> bool {
        self.unpriced_calls < self.provider_call_count || self.known_cost_usd > 0.0
    }

    /// Measured total only when every physical attempt has a price.
    #[must_use]
    pub fn cost_usd(&self) -> Option<f64> {
        (self.unpriced_calls == 0).then_some(self.known_cost_usd)
    }

    /// Fold one completed call without retaining its detailed trace.
    pub(crate) fn record(&mut self, usage: &LlmUsage) {
        let summary = self;
        // An absent call count identifies ledgers recorded before the
        // aggregation fields existed. Reconstruct their one-call
        // certainty from the original stable fields. A present zero is a
        // measurement and remains zero.
        let legacy = usage.provider_call_count.is_none();
        summary.known_cost_usd += if legacy {
            usage.cost_usd.unwrap_or(0.0)
        } else {
            usage.known_cost_usd
        };
        summary.provider_call_count += usage.provider_call_count.unwrap_or(1);
        summary.unpriced_calls += if legacy {
            i64::from(usage.cost_usd.is_none())
        } else {
            usage.unpriced_calls
        };
        summary.usage_unknown_calls += if legacy {
            i64::from(usage.accounting_status == UsageAccountingStatus::Unknown)
        } else {
            usage.usage_unknown_calls
        };
        summary.unpriced_tokens += if legacy {
            if usage.cost_usd.is_none() {
                usage.input_tokens.saturating_add(usage.output_tokens)
            } else {
                0
            }
        } else {
            usage.unpriced_tokens()
        };
        let reason = if legacy {
            usage.cost_usd.is_none().then_some(UnpricedReason::Mixed)
        } else {
            usage.unpriced_reason()
        };
        if let Some(reason) = reason {
            summary.unpriced_reason = Some(
                summary
                    .unpriced_reason
                    .map_or(reason, |existing| existing.merge(reason)),
            );
        }
        // A legacy ledger has no stored projection, so its own cost stands
        // in: priced members project to exactly what they cost, unpriced
        // ones refuse.
        let member_projection = if legacy {
            usage.cost_usd
        } else {
            usage.projected_cost_usd()
        };
        let member_known = if legacy {
            usage.cost_usd.unwrap_or(0.0)
        } else {
            usage.known_cost_usd
        };
        match member_projection {
            Some(projected) => {
                summary.unpriced_projection_usd += (projected - member_known).max(0.0);
            }
            None => summary.unprojectable = true,
        }
    }
}

/// Fold cost and accounting certainty once for every reporting projection.
pub fn summarize_usage_cost_certainty<'a>(
    usages: impl IntoIterator<Item = &'a LlmUsage>,
) -> UsageCostCertainty {
    let mut summary = UsageCostCertainty::default();
    for usage in usages {
        summary.record(usage);
    }
    summary
}

/// Classify one attempt's missing price and bound what it may have cost.
///
/// `table_cost` is what the route's price table makes of the counts the
/// attempt reported, independent of whether those counts were usable. A priced
/// attempt projects to its own cost. An unpriced attempt on a priced route is
/// bounded by that table figure, which is zero when it reported no tokens. An
/// unpriced attempt on a route with no price table has no bound at any token
/// count, so its projection refuses.
fn unpriced_projection(
    cost_usd: Option<f64>,
    table_cost: Option<f64>,
) -> (Option<UnpricedReason>, Option<f64>) {
    match (cost_usd, table_cost) {
        (Some(cost), _) => (None, Some(cost)),
        (None, Some(bound)) => (Some(UnpricedReason::UsageUnreported), Some(bound)),
        (None, None) => (Some(UnpricedReason::PricingUnknown), None),
    }
}

/// Build the boxed unpriced record, or `None` when nothing here is unpriced.
///
/// `projected` is the worst case for the WHOLE ledger as the caller computed
/// it; what the record stores is the part above `known_cost_usd`, so a fold can
/// add members without double counting the priced portion.
fn unpriced_facts(
    tokens: i64,
    reason: Option<UnpricedReason>,
    projected: Option<f64>,
) -> Option<Box<UnpricedFacts>> {
    reason.map(|reason| {
        Box::new(UnpricedFacts {
            tokens,
            reason,
            projection_usd: projected,
        })
    })
}

/// A per-request receipt cannot describe several requests' instants, tiers,
/// and fees. Aggregates retain it only when they contain one request.
fn aggregate_pricing_facts(usages: &[LlmUsage]) -> Option<Box<PricingFacts>> {
    match usages {
        [usage] => usage.pricing.clone(),
        _ => None,
    }
}

/// Record which card, band and tier produced a catalog-settled cost.
fn pricing_facts(
    detail: &super::cost::PricingDetail,
    settled_at_ms: i64,
    cache_ttl: Option<super::api::PromptCacheTtl>,
) -> PricingFacts {
    PricingFacts {
        rate_card: detail.rate_card.label(),
        settled_at_ms,
        input_band_minimum: detail.input_band_minimum,
        serving_tier: matches!(
            detail.source,
            super::cost::PricingSource::CatalogServingTier
        )
        .then(|| super::serving_tiers::FAST_TIER_ID.to_string()),
        cache_ttl_unpriced: !detail.cache_write_priced(cache_ttl),
        platform_fee_basis: (detail.platform_fee_percent > 0.0)
            .then(|| "catalog_estimate_funding_route_unknown".into()),
        ..PricingFacts::default()
    }
}

/// Tokens attributable to an attempt only when that attempt went unpriced.
const fn unpriced_token_count(cost_usd: Option<f64>, input: i64, output: i64) -> i64 {
    if cost_usd.is_some() {
        0
    } else {
        input.saturating_add(output)
    }
}

impl LlmUsage {
    /// Fold completed calls into one structured-operation ledger. Schema and
    /// repair retries must report every paid response, not only the final one.
    pub(crate) fn aggregate(usages: &[Self]) -> Self {
        let input_tokens = usages.iter().map(|usage| usage.input_tokens).sum();
        let output_tokens = usages.iter().map(|usage| usage.output_tokens).sum();
        let reported_total_tokens = (!usages.is_empty())
            .then(|| {
                usages.iter().try_fold(0_i64, |total, usage| {
                    usage
                        .reported_total_tokens
                        .map(|tokens| total.saturating_add(tokens))
                })
            })
            .flatten();
        let cache_read_tokens = usages.iter().map(|usage| usage.cache_read_tokens).sum();
        let cache_write_tokens = usages.iter().map(|usage| usage.cache_write_tokens).sum();
        let certainty = summarize_usage_cost_certainty(usages);
        let cache_supported = usages.iter().all(|usage| usage.cache_supported);
        // One undeclared member poisons the aggregate to undeclared: totals
        // that include uninformative zeros must not read as audited numbers.
        // Declared members agree on `true` or fall to `false` when mixed,
        // matching `cache_supported`'s all() conservatism above.
        let cache_accounting_declared = usages
            .iter()
            .map(|usage| usage.cache_accounting_declared)
            .try_fold(true, |all_true, declared| {
                declared.map(|declared| all_true && declared)
            });
        Self {
            input_tokens,
            output_tokens,
            reported_total_tokens,
            // The priced attempts measured something real. Reporting their
            // sum as null because a sibling was unpriced turns a measurement
            // into no measurement; the unpriced siblings stay visible in
            // `unpriced_calls`, `unpriced_tokens`, and `unpriced_reason`.
            cost_usd: (certainty.unpriced_calls == 0 || certainty.has_priced_attempt())
                .then_some(certainty.known_cost_usd),
            cache_read_tokens,
            cache_write_tokens,
            cache_supported,
            cache_accounting_declared,
            cache_hit_ratio: (cache_accounting_declared == Some(true) && cache_supported)
                .then(|| super::cost::cache_hit_ratio(input_tokens, cache_read_tokens)),
            cache_savings_usd: usages.iter().map(|usage| usage.cache_savings_usd).sum(),
            cache_hit: usages.iter().any(|usage| usage.cache_hit),
            served_fast: usages.iter().any(|usage| usage.served_fast),
            accounting_status: if certainty.usage_unknown_calls == 0
                && certainty.unpriced_calls == 0
            {
                UsageAccountingStatus::Reported
            } else if certainty.has_priced_attempt() {
                // Only a call that priced nothing at all blacks out.
                UsageAccountingStatus::Partial
            } else {
                UsageAccountingStatus::Unknown
            },
            known_cost_usd: certainty.known_cost_usd,
            provider_call_count: Some(certainty.provider_call_count),
            unpriced_calls: certainty.unpriced_calls,
            usage_unknown_calls: certainty.usage_unknown_calls,
            unpriced: unpriced_facts(
                certainty.unpriced_tokens,
                certainty.unpriced_reason,
                (!certainty.unprojectable).then_some(certainty.unpriced_projection_usd),
            ),
            // Request instants and fees belong to the per-attempt receipts.
            pricing: aggregate_pricing_facts(usages),
            billing: usages
                .iter()
                .filter_map(|usage| usage.billing.as_deref())
                .fold(None, |sum, value| {
                    let mut sum = sum.unwrap_or_else(|| Box::new(BillingUsage::default()));
                    sum.add(value);
                    Some(sum)
                }),
        }
    }

    /// Preserve completed provider receipts when the enclosing logical call
    /// terminates on one or more attempts that produced no usable response.
    pub(crate) fn aggregate_with_unknown_attempts(
        completed: &[Self],
        unknown_attempts: usize,
    ) -> Self {
        assert!(
            unknown_attempts > 0,
            "terminal usage requires an unknown attempt"
        );
        let mut usages = Vec::with_capacity(completed.len().saturating_add(1));
        usages.extend_from_slice(completed);
        usages.push(Self::unknown_attempts(unknown_attempts));
        Self::aggregate(&usages)
    }

    pub(crate) fn known_zero_attempt() -> Self {
        Self {
            cost_usd: Some(0.0),
            accounting_status: UsageAccountingStatus::Reported,
            known_cost_usd: 0.0,
            provider_call_count: Some(1),
            unpriced_calls: 0,
            usage_unknown_calls: 0,
            unpriced: None,
            ..Self::unknown_attempt()
        }
    }

    /// A terminal that made no provider request at all.
    ///
    /// Distinct from `known_zero_attempt`, which is one real request that cost
    /// nothing (a cache or replay hit). Here nothing was dispatched, so the
    /// cost is exactly zero, there is no unpriced attempt to bound, and the
    /// physical request count is a measured zero rather than an assumed one.
    /// Pre-dispatch budget refusals and admission denials terminate here.
    pub(crate) fn no_provider_request() -> Self {
        Self {
            cost_usd: Some(0.0),
            accounting_status: UsageAccountingStatus::Reported,
            known_cost_usd: 0.0,
            provider_call_count: Some(0),
            unpriced_calls: 0,
            usage_unknown_calls: 0,
            unpriced: None,
            ..Self::unknown_attempt()
        }
    }

    pub(crate) fn unknown_attempt() -> Self {
        Self::unknown_attempts(1)
    }

    pub(crate) fn unknown_attempts(count: usize) -> Self {
        let count = i64::try_from(count.max(1)).unwrap_or(i64::MAX);
        Self {
            input_tokens: 0,
            output_tokens: 0,
            reported_total_tokens: None,
            cost_usd: None,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            // No response means no evidence against sibling responses' cache
            // support. These neutral values keep aggregation from turning one
            // transport-unknown attempt into "cache unsupported" (or
            // undeclared) for the completed logical call.
            cache_supported: true,
            cache_accounting_declared: Some(true),
            cache_hit_ratio: Some(0.0),
            cache_savings_usd: 0.0,
            cache_hit: false,
            served_fast: false,
            accounting_status: UsageAccountingStatus::Unknown,
            known_cost_usd: 0.0,
            provider_call_count: Some(count),
            unpriced_calls: count,
            usage_unknown_calls: count,
            // No response arrived, so neither a token count nor a price table
            // bounds what it may have cost. That refuses the projection, which
            // is what keeps a ceiling consumer failing closed.
            unpriced: Some(Box::new(UnpricedFacts {
                tokens: 0,
                reason: UnpricedReason::NoResponse,
                projection_usd: None,
            })),
            // Nothing was priced, so no card settled anything.
            pricing: None,
            billing: None,
        }
    }

    /// One provider request whose stream was severed mid-flight by schema
    /// validation.
    ///
    /// The provider generated and billed partial output, then never sent the
    /// end-of-stream usage frame, so this attempt has real spend and no
    /// measurement of it. Recording it as an unknown attempt keeps the request
    /// in `provider_call_count`, keeps the ledger's `cost_usd` refusing, and
    /// keeps `projected_cost_usd` unbounded so a ceiling consumer fails closed.
    /// Dropping the attempt instead would let a severed call read as a clean
    /// zero, which is the accounting hole this exists to close.
    pub(crate) fn stream_aborted_attempt() -> Self {
        Self {
            unpriced: Some(Box::new(UnpricedFacts {
                tokens: 0,
                reason: UnpricedReason::StreamAborted,
                projection_usd: None,
            })),
            ..Self::unknown_attempt()
        }
    }

    pub(crate) fn from_result(result: &LlmResult) -> Self {
        let component_usage_known = result.input_tokens > 0
            || result.output_tokens > 0
            || result.telemetry.server_prompt_tokens.is_some()
            || result.telemetry.server_output_tokens.is_some();
        let usage_known = component_usage_known || result.telemetry.server_total_tokens.is_some();
        let authoritative_cost = result
            .telemetry
            .mock_replay_cost_usd()
            .or(result.telemetry.provider_cost_usd)
            .or_else(|| super::managed_supply::authoritative_cost_usd(result));
        // A self-hosted route bills nothing whether or not it reported token
        // counts, so its cost is known before its usage is. Leaving it to the
        // gate below would price it `None` on any server that omits usage
        // (streaming llama.cpp among them), and an unpriced call spends a USD
        // ceiling whole. `usage_unknown_calls` still records that the token
        // counts were missing: that stays unknown, only the cost does not.
        let free_route = crate::llm_config::provider_is_self_hosted(&result.provider);
        // Settle at the instant the request left the client. A promotion that
        // expired, or a time-of-day window the call started inside, priced the
        // call as it began, not as it was accounted for.
        let settled_at_ms = result
            .telemetry
            .started_at_ms
            .unwrap_or_else(crate::stdlib::clock::now_wall_ms_unrecorded);
        let at = super::cost::instant_from_wall_ms(settled_at_ms);
        let cache_ttl = result
            .telemetry
            .prompt_cache_ttl
            .as_deref()
            .and_then(super::api::PromptCacheTtl::parse);
        // The price table is looked up whether or not the counts are usable,
        // because it is what separates an unpriced attempt that still has a
        // worst case from one that has none at any token count.
        let detail = super::cost::pricing_detail_for_tier(
            &result.provider,
            &result.model,
            result.served_fast,
            result.input_tokens,
            at,
        );
        let token_cost = detail.as_ref().map(|detail| {
            super::cost::project_call_cost(
                detail,
                result.input_tokens,
                result.output_tokens,
                result.cache_read_tokens,
                result.cache_write_tokens,
                cache_ttl,
            )
        });
        let billing = result.telemetry.billing.clone();
        let settlement = detail.as_ref().zip(token_cost).map(|(detail, cost)| {
            billing
                .as_deref()
                .unwrap_or(&BillingUsage::default())
                .settle(detail, cost)
        });
        let table_cost = settlement.as_ref().map(|settlement| settlement.total_usd);
        let units_unpriced = authoritative_cost.is_none()
            && !free_route
            && (settlement
                .as_ref()
                .is_some_and(billing::BillingSettlement::has_unpriced_units)
                || (!component_usage_known && billing.is_some())
                || (result.cache_write_tokens > 0
                    && detail
                        .as_ref()
                        .is_some_and(|detail| !detail.cache_write_priced(cache_ttl))));
        let cost_usd = authoritative_cost
            .or_else(|| free_route.then_some(0.0))
            .or_else(|| {
                (component_usage_known || billing.is_some())
                    .then_some(table_cost)
                    .flatten()
            });
        let (unpriced_reason, projected_cost_usd) = if units_unpriced {
            (Some(UnpricedReason::Mixed), None)
        } else {
            unpriced_projection(cost_usd, table_cost)
        };
        let cache_hit_ratio = (result.telemetry.cache_accounting_declared == Some(true)
            && result.cache_supported)
            .then(|| super::cost::cache_hit_ratio(result.input_tokens, result.cache_read_tokens));
        Self {
            input_tokens: result.input_tokens,
            output_tokens: result.output_tokens,
            reported_total_tokens: result.telemetry.server_total_tokens,
            cost_usd,
            cache_read_tokens: result.cache_read_tokens,
            cache_write_tokens: result.cache_write_tokens,
            cache_supported: result.cache_supported,
            cache_accounting_declared: result.telemetry.cache_accounting_declared,
            cache_hit_ratio,
            cache_savings_usd: super::cost::cache_savings_usd_for_provider(
                &result.provider,
                &result.model,
                result.input_tokens,
                result.cache_read_tokens,
                result.cache_write_tokens,
                at,
                cache_ttl,
            ),
            cache_hit: result.cache_read_tokens > 0,
            served_fast: result.served_fast,
            accounting_status: if units_unpriced {
                UsageAccountingStatus::Partial
            } else if usage_known || authoritative_cost.is_some() {
                UsageAccountingStatus::Reported
            } else {
                UsageAccountingStatus::Unknown
            },
            known_cost_usd: cost_usd.unwrap_or(0.0),
            provider_call_count: Some(1),
            unpriced_calls: i64::from(cost_usd.is_none() || units_unpriced),
            usage_unknown_calls: i64::from(!usage_known && authoritative_cost.is_none()),
            unpriced: unpriced_facts(
                unpriced_token_count(cost_usd, result.input_tokens, result.output_tokens),
                unpriced_reason,
                projected_cost_usd,
            ),
            // Only a catalog-settled cost has a card to name. A provider's own
            // cost figure and a self-hosted zero did not come from one, and
            // claiming a card for them would put the catalog's name on a
            // number it did not produce.
            pricing: (cost_usd.is_some() && authoritative_cost.is_none() && !free_route)
                .then(|| {
                    detail.as_ref().map(|detail| {
                        let mut facts = pricing_facts(detail, settled_at_ms, cache_ttl);
                        if let Some(settlement) = &settlement {
                            facts.platform_fee_estimate_usd = settlement.platform_fee_estimate_usd;
                            facts.hosted_tool_unpriced = settlement.hosted_tool_unpriced.clone();
                            facts.modality_unpriced = settlement.modality_unpriced.clone();
                            facts.monthly_allowance_unapplied =
                                settlement.monthly_allowance_unapplied.clone();
                        }
                        Box::new(facts)
                    })
                })
                .flatten(),
            billing,
        }
    }

    /// Normalize accounting from a provider receipt or saved probe. Complete
    /// token counts earn the same catalog pricing as a completed response;
    /// partial receipts retain their measured fields but remain explicitly
    /// unknown rather than turning absence into a free zero.
    pub(crate) fn from_provider_receipt(
        provider: &str,
        model: &str,
        receipt: &ProviderUsageReceipt,
    ) -> Self {
        let input_tokens = receipt.input_tokens.unwrap_or(0);
        let output_tokens = receipt.output_tokens.unwrap_or(0);
        let complete_counts = receipt.has_complete_token_counts();
        let free_route = crate::llm_config::provider_is_self_hosted(provider);
        let settled_at_ms = receipt
            .started_at_ms
            .unwrap_or_else(crate::stdlib::clock::now_wall_ms_unrecorded);
        let cache_ttl = receipt.prompt_cache_ttl;
        let at = super::cost::instant_from_wall_ms(settled_at_ms);
        let detail = super::cost::pricing_detail_for_tier(
            provider,
            model,
            receipt.served_fast,
            input_tokens,
            at,
        );
        let token_cost = detail.as_ref().map(|detail| {
            super::cost::project_call_cost(
                detail,
                input_tokens,
                output_tokens,
                receipt.cache_read_tokens,
                receipt.cache_write_tokens,
                cache_ttl,
            )
        });
        let billing = receipt.billing.clone();
        let settlement = detail.as_ref().zip(token_cost).map(|(detail, cost)| {
            billing
                .as_deref()
                .unwrap_or(&BillingUsage::default())
                .settle(detail, cost)
        });
        let table_cost = settlement.as_ref().map(|settlement| settlement.total_usd);
        let units_unpriced = receipt.provider_cost_usd.is_none()
            && !free_route
            && (settlement
                .as_ref()
                .is_some_and(billing::BillingSettlement::has_unpriced_units)
                || (!complete_counts && billing.is_some())
                || (receipt.cache_write_tokens > 0
                    && detail
                        .as_ref()
                        .is_some_and(|detail| !detail.cache_write_priced(cache_ttl))));
        let cost_usd = receipt
            .provider_cost_usd
            .or_else(|| free_route.then_some(0.0))
            .or_else(|| {
                (complete_counts || billing.is_some())
                    .then_some(table_cost)
                    .flatten()
            });
        let (unpriced_reason, projected_cost_usd) = if units_unpriced {
            (Some(UnpricedReason::Mixed), None)
        } else {
            unpriced_projection(cost_usd, table_cost)
        };
        let usage_unknown = i64::from(!complete_counts);
        Self {
            input_tokens,
            output_tokens,
            reported_total_tokens: receipt.reported_total_tokens,
            cost_usd,
            cache_read_tokens: receipt.cache_read_tokens,
            cache_write_tokens: receipt.cache_write_tokens,
            cache_supported: receipt.cache_supported,
            cache_accounting_declared: receipt.cache_accounting_declared,
            cache_hit_ratio: (receipt.cache_accounting_declared == Some(true)
                && receipt.cache_supported)
                .then(|| super::cost::cache_hit_ratio(input_tokens, receipt.cache_read_tokens)),
            cache_savings_usd: super::cost::cache_savings_usd_for_provider(
                provider,
                model,
                input_tokens,
                receipt.cache_read_tokens,
                receipt.cache_write_tokens,
                at,
                cache_ttl,
            ),
            cache_hit: receipt.cache_read_tokens > 0,
            served_fast: receipt.served_fast,
            accounting_status: if units_unpriced {
                UsageAccountingStatus::Partial
            } else if usage_unknown == 0 {
                UsageAccountingStatus::Reported
            } else {
                UsageAccountingStatus::Unknown
            },
            known_cost_usd: cost_usd.unwrap_or(0.0),
            provider_call_count: Some(1),
            unpriced_calls: i64::from(cost_usd.is_none() || units_unpriced),
            usage_unknown_calls: usage_unknown,
            unpriced: unpriced_facts(
                unpriced_token_count(cost_usd, input_tokens, output_tokens),
                unpriced_reason,
                projected_cost_usd,
            ),
            pricing: (cost_usd.is_some() && receipt.provider_cost_usd.is_none() && !free_route)
                .then(|| {
                    detail.as_ref().map(|detail| {
                        let mut facts = pricing_facts(detail, settled_at_ms, cache_ttl);
                        if let Some(settlement) = &settlement {
                            facts.platform_fee_estimate_usd = settlement.platform_fee_estimate_usd;
                            facts.hosted_tool_unpriced = settlement.hosted_tool_unpriced.clone();
                            facts.modality_unpriced = settlement.modality_unpriced.clone();
                            facts.monthly_allowance_unapplied =
                                settlement.monthly_allowance_unapplied.clone();
                        }
                        Box::new(facts)
                    })
                })
                .flatten(),
            billing,
        }
    }

    /// Aggregate physical attempts once at a terminal observed-call boundary.
    /// Every attempt is either a reported ledger or one explicit unknown
    /// ledger, so a successful pricing total can never be mistaken for a
    /// shorter call sequence.
    pub(crate) fn aggregate_attempt_ledger(reported: &[Self], total_attempts: usize) -> Self {
        assert!(
            reported.len() <= total_attempts,
            "reported attempt ledgers cannot exceed physical attempts"
        );
        let unknown_attempts = total_attempts.saturating_sub(reported.len());
        if unknown_attempts == 0 {
            return Self::aggregate(reported);
        }
        Self::aggregate_with_unknown_attempts(reported, unknown_attempts)
    }

    /// Project the stable Harn `usage` envelope. Retry accounting is supplied
    /// by the observed-call boundary and stays nested under this one owner.
    pub(crate) fn to_vm_dict(&self, attempts: &ProviderAttempts) -> crate::value::DictMap {
        let mut usage = crate::value::DictMap::new();
        if let Some(billing) = &self.billing {
            if let Value::Object(fields) =
                serde_json::to_value(billing).expect("billing serializes")
            {
                for (key, value) in fields {
                    usage.insert(key.into(), crate::schema::json_to_vm_value(&value));
                }
            }
        }
        usage.insert(
            crate::value::intern_key("input_tokens"),
            VmValue::Int(self.input_tokens),
        );
        usage.insert(
            crate::value::intern_key("output_tokens"),
            VmValue::Int(self.output_tokens),
        );
        usage.insert(
            crate::value::intern_key("reported_total_tokens"),
            self.reported_total_tokens
                .map_or(VmValue::Nil, VmValue::Int),
        );
        usage.insert(
            crate::value::intern_key("cost_usd"),
            self.cost_usd.map_or(VmValue::Nil, VmValue::Float),
        );
        usage.insert(
            crate::value::intern_key("known_cost_usd"),
            VmValue::Float(self.known_cost_usd),
        );
        // An unmeasured count is absent, never null: a consumer that keys
        // "stamped" on the field's presence must not read a legacy ledger as
        // one that measured and found nothing.
        if let Some(provider_call_count) = self.provider_call_count {
            usage.insert(
                crate::value::intern_key("provider_call_count"),
                VmValue::Int(provider_call_count),
            );
        }
        usage.insert(
            crate::value::intern_key("unpriced_calls"),
            VmValue::Int(self.unpriced_calls),
        );
        usage.insert(
            crate::value::intern_key("usage_unknown_calls"),
            VmValue::Int(self.usage_unknown_calls),
        );
        usage.insert(
            crate::value::intern_key("unpriced_tokens"),
            VmValue::Int(self.unpriced_tokens()),
        );
        usage.insert(
            crate::value::intern_key("unpriced_reason"),
            self.unpriced_reason()
                .map_or(VmValue::Nil, |reason| VmValue::string(reason.as_str())),
        );
        usage.insert(
            crate::value::intern_key("projected_cost_usd"),
            self.projected_cost_usd()
                .map_or(VmValue::Nil, VmValue::Float),
        );
        // Present only when the catalog settled this ledger, so a consumer can
        // tell "no card applied" from "a base card applied".
        if let Some(pricing) = self.pricing.as_deref() {
            let mut card = crate::value::DictMap::new();
            card.insert(
                "platform_fee_estimate_usd".into(),
                VmValue::Float(pricing.platform_fee_estimate_usd),
            );
            card.insert(
                "platform_fee_basis".into(),
                pricing
                    .platform_fee_basis
                    .as_deref()
                    .map_or(VmValue::Nil, VmValue::string),
            );
            card.insert(
                "hosted_tool_unpriced".into(),
                crate::schema::json_to_vm_value(&serde_json::json!(pricing.hosted_tool_unpriced)),
            );
            card.insert(
                "modality_unpriced".into(),
                crate::schema::json_to_vm_value(&serde_json::json!(pricing.modality_unpriced)),
            );
            card.insert(
                "monthly_allowance_unapplied".into(),
                crate::schema::json_to_vm_value(&serde_json::json!(
                    pricing.monthly_allowance_unapplied
                )),
            );
            card.insert(
                crate::value::intern_key("rate_card"),
                VmValue::string(pricing.rate_card.as_str()),
            );
            card.insert(
                crate::value::intern_key("settled_at_ms"),
                VmValue::Int(pricing.settled_at_ms),
            );
            card.insert(
                crate::value::intern_key("input_band_minimum"),
                pricing
                    .input_band_minimum
                    .map_or(VmValue::Nil, |band| VmValue::Int(band as i64)),
            );
            card.insert(
                crate::value::intern_key("serving_tier"),
                pricing
                    .serving_tier
                    .as_deref()
                    .map_or(VmValue::Nil, VmValue::string),
            );
            card.insert(
                crate::value::intern_key("cache_ttl_unpriced"),
                VmValue::Bool(pricing.cache_ttl_unpriced),
            );
            usage.insert(
                crate::value::intern_key("pricing"),
                VmValue::Dict(std::sync::Arc::new(card)),
            );
        }
        usage.insert(
            crate::value::intern_key("cache_read_tokens"),
            VmValue::Int(self.cache_read_tokens),
        );
        usage.insert(
            crate::value::intern_key("cache_write_tokens"),
            VmValue::Int(self.cache_write_tokens),
        );
        usage.insert(
            crate::value::intern_key("cache_supported"),
            VmValue::Bool(self.cache_supported),
        );
        usage.insert(
            crate::value::intern_key("cache_hit_ratio"),
            self.cache_hit_ratio.map_or(VmValue::Nil, VmValue::Float),
        );
        match self.cache_visibility() {
            None => {
                usage.insert(crate::value::intern_key("cache_visibility"), VmValue::Nil);
            }
            Some(state) => usage.put_str("cache_visibility", state),
        }
        usage.insert(
            crate::value::intern_key("cache_savings_usd"),
            VmValue::Float(self.cache_savings_usd),
        );
        usage.insert(
            crate::value::intern_key("provider_attempts"),
            VmValue::dict(provider_attempts_vm_dict(attempts)),
        );
        usage.insert(
            crate::value::intern_key("served_fast"),
            VmValue::Bool(self.served_fast),
        );
        usage.put_str("accounting_status", self.accounting_status.as_str());
        usage
    }

    /// Three-state cache visibility. `None` (projected as null) means the
    /// cache numbers are visible and audited. `"unsupported"` means the route
    /// declares it reports nothing, so the zeros are intentional.
    /// `"undeclared"` means nobody declared either way: the numbers are
    /// preserved as parsed, and a zero carries no information — it must not
    /// read as a well-formed 0% hit rate.
    fn cache_visibility(&self) -> Option<&'static str> {
        match (self.cache_accounting_declared, self.cache_supported) {
            (None, _) => Some("undeclared"),
            (Some(false), _) | (Some(true), false) => Some("unsupported"),
            (Some(true), true) => None,
        }
    }

    /// Mechanically add the canonical accounting fields to the flat provider
    /// response event retained for CLI/backward compatibility.
    pub(crate) fn project_onto_event(&self, event: &mut serde_json::Value) {
        let fields = event
            .as_object_mut()
            .expect("usage projection target must be a JSON object");
        self.project_onto_fields(fields);
    }

    /// Add canonical accounting directly to an observability field map.
    /// Receipt producers use this instead of round-tripping through a JSON
    /// object or maintaining a second cost projection.
    pub(crate) fn project_onto_fields(&self, fields: &mut serde_json::Map<String, Value>) {
        if let Some(billing) = &self.billing {
            if let Value::Object(billing) =
                serde_json::to_value(billing).expect("billing serializes")
            {
                fields.extend(billing);
            }
        }
        fields.insert("input_tokens".to_string(), self.input_tokens.into());
        fields.insert("output_tokens".to_string(), self.output_tokens.into());
        fields.insert(
            "reported_total_tokens".to_string(),
            self.reported_total_tokens
                .map_or(Value::Null, serde_json::Value::from),
        );
        fields.insert(
            "cost_usd".to_string(),
            self.cost_usd.map_or(Value::Null, serde_json::Value::from),
        );
        fields.insert("known_cost_usd".to_string(), self.known_cost_usd.into());
        if let Some(provider_call_count) = self.provider_call_count {
            fields.insert(
                "provider_call_count".to_string(),
                provider_call_count.into(),
            );
        }
        fields.insert("unpriced_calls".to_string(), self.unpriced_calls.into());
        fields.insert(
            "usage_unknown_calls".to_string(),
            self.usage_unknown_calls.into(),
        );
        fields.insert("unpriced_tokens".to_string(), self.unpriced_tokens().into());
        fields.insert(
            "unpriced_reason".to_string(),
            self.unpriced_reason()
                .map_or(Value::Null, |reason| reason.as_str().into()),
        );
        fields.insert(
            "projected_cost_usd".to_string(),
            self.projected_cost_usd()
                .map_or(Value::Null, serde_json::Value::from),
        );
        if let Some(pricing) = self.pricing.as_deref() {
            fields.insert(
                "pricing".to_string(),
                serde_json::to_value(pricing).unwrap_or(Value::Null),
            );
        }
        fields.insert(
            "cache_read_tokens".to_string(),
            self.cache_read_tokens.into(),
        );
        fields.insert(
            "cache_write_tokens".to_string(),
            self.cache_write_tokens.into(),
        );
        fields.insert("cache_supported".to_string(), self.cache_supported.into());
        fields.insert(
            "cache_hit_ratio".to_string(),
            self.cache_hit_ratio
                .map_or(Value::Null, serde_json::Value::from),
        );
        fields.insert(
            "cache_visibility".to_string(),
            self.cache_visibility()
                .map_or(Value::Null, |state| Value::String(state.to_string())),
        );
        fields.insert(
            "cache_savings_usd".to_string(),
            self.cache_savings_usd.into(),
        );
        fields.insert("cache_hit".to_string(), self.cache_hit.into());
        fields.insert("served_fast".to_string(), self.served_fast.into());
        fields.insert(
            "accounting_status".to_string(),
            self.accounting_status.as_str().into(),
        );
    }

    /// The usage a terminal carries when no per-attempt ledger was recorded.
    ///
    /// `dispatches` is the call-scoped measurement from
    /// `crate::llm::provider_dispatch`: `Some(0)` is a terminal that never
    /// reached a provider, `Some(n)` is n requests whose usage never arrived,
    /// and `None` means nothing measured, which stays conservative by
    /// assuming one unknown attempt. Collapsing `Some(0)` into `None` is what
    /// charged a reserve for refusals that cost nothing
    /// (burin-labs/harn#8529).
    pub(crate) fn measured_vm_dict(dispatches: Option<i64>) -> crate::value::DictMap {
        let usage = match dispatches {
            Some(0) => Self::no_provider_request(),
            Some(count) if count > 0 => {
                Self::unknown_attempts(usize::try_from(count).unwrap_or(usize::MAX))
            }
            _ => Self::unknown_attempt(),
        };
        let attempts = ProviderAttempts {
            total: u32::try_from(dispatches.unwrap_or(0).max(0)).unwrap_or(u32::MAX),
            ..ProviderAttempts::default()
        };
        usage.to_vm_dict(&attempts)
    }

    /// Lower the ledger to canonical tracing metadata while keeping route
    /// identity on the enclosing call.
    pub(crate) fn metadata_pairs(
        &self,
        provider: &str,
        model: &str,
    ) -> Vec<(&'static str, serde_json::Value)> {
        use crate::tracing::meta;

        let mut pairs = vec![
            (meta::MODEL, serde_json::json!(model)),
            (meta::PROVIDER, serde_json::json!(provider)),
            (meta::INPUT_TOKENS, serde_json::json!(self.input_tokens)),
            (meta::OUTPUT_TOKENS, serde_json::json!(self.output_tokens)),
            (
                meta::CACHE_READ_TOKENS,
                serde_json::json!(self.cache_read_tokens),
            ),
            (
                meta::CACHE_WRITE_TOKENS,
                serde_json::json!(self.cache_write_tokens),
            ),
        ];
        if let Some(cost) = self.cost_usd {
            pairs.push((meta::COST_USD, serde_json::json!(cost)));
        }
        if let Some(total_tokens) = self.reported_total_tokens {
            pairs.push((meta::REPORTED_TOTAL_TOKENS, serde_json::json!(total_tokens)));
        }
        // A recorded `cost_usd` is only auditable alongside the card that
        // produced it: the same route settles at two prices on either side of
        // an off-peak boundary, and the number alone cannot say which.
        if let Some(pricing) = self.pricing.as_deref() {
            pairs.push((meta::RATE_CARD, serde_json::json!(pricing.rate_card)));
            pairs.push((
                "platform_fee_estimate_usd",
                serde_json::json!(pricing.platform_fee_estimate_usd),
            ));
            if let Some(basis) = &pricing.platform_fee_basis {
                pairs.push(("platform_fee_basis", serde_json::json!(basis)));
            }
            pairs.push((
                "hosted_tool_unpriced",
                serde_json::json!(pricing.hosted_tool_unpriced),
            ));
            pairs.push((
                "modality_unpriced",
                serde_json::json!(pricing.modality_unpriced),
            ));
            pairs.push((
                "monthly_allowance_unapplied",
                serde_json::json!(pricing.monthly_allowance_unapplied),
            ));
            if let Some(band) = pricing.input_band_minimum {
                pairs.push((meta::PRICING_BAND, serde_json::json!(band)));
            }
            if let Some(tier) = pricing.serving_tier.as_deref() {
                pairs.push((meta::PRICING_TIER, serde_json::json!(tier)));
            }
            if pricing.cache_ttl_unpriced {
                pairs.push((meta::CACHE_TTL_UNPRICED, serde_json::json!(true)));
            }
        }
        pairs
    }
}

fn provider_attempts_vm_dict(attempts: &ProviderAttempts) -> crate::value::DictMap {
    let mut fields = crate::value::DictMap::new();
    fields.insert(
        crate::value::intern_key("total"),
        VmValue::Int(i64::from(attempts.total)),
    );
    fields.insert(
        crate::value::intern_key("retries"),
        VmValue::Int(i64::from(attempts.retries())),
    );
    fields.insert(
        crate::value::intern_key("rate_limited"),
        VmValue::Int(i64::from(attempts.rate_limited)),
    );
    fields.insert(
        crate::value::intern_key("empty_completion"),
        VmValue::Int(i64::from(attempts.empty_completion)),
    );
    fields.insert(
        crate::value::intern_key("other"),
        VmValue::Int(i64::from(attempts.other)),
    );
    fields
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolProbeUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported_total_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Whether this probe received provider accounting rather than inferred
    /// zeroes. Older saved reports did not carry this field and are therefore
    /// conservatively unknown.
    #[serde(default)]
    pub accounting_status: UsageAccountingStatus,
}

impl ToolProbeUsage {
    pub(crate) fn from_llm_result(result: &LlmResult) -> Self {
        Self::from_usage(LlmUsage::from_result(result))
    }

    fn from_reported(provider: &str, model: &str, reported: ReportedTokenUsage) -> Self {
        let input_tokens = reported
            .prompt_counts()
            .ok()
            .flatten()
            .map(|counts| counts.total);
        let output_tokens = reported.output_tokens.filter(|tokens| *tokens >= 0);
        if input_tokens.is_some() && output_tokens.is_some() {
            let receipt = ProviderUsageReceipt::new(input_tokens, output_tokens, None, false)
                .with_cache(
                    reported.cache_read_tokens.unwrap_or(0),
                    reported.cache_write_tokens.unwrap_or(0),
                    None,
                    reported.cache_read_tokens.is_some() || reported.cache_write_tokens.is_some(),
                );
            return Self::from_usage(LlmUsage::from_provider_receipt(provider, model, &receipt));
        }
        Self {
            input_tokens,
            output_tokens,
            reported_total_tokens: None,
            cost_usd: None,
            accounting_status: UsageAccountingStatus::Unknown,
        }
    }

    fn from_usage(usage: LlmUsage) -> Self {
        Self {
            input_tokens: Some(usage.input_tokens),
            output_tokens: Some(usage.output_tokens),
            reported_total_tokens: usage.reported_total_tokens,
            cost_usd: usage.cost_usd,
            accounting_status: usage.accounting_status,
        }
    }
}

pub(crate) fn extract_probe_usage(
    provider: &str,
    model: &str,
    response: &Value,
) -> Option<ToolProbeUsage> {
    let reported = reported_usage_from_response(response)?;
    Some(ToolProbeUsage::from_reported(provider, model, reported))
}

fn reported_usage_from_response(response: &Value) -> Option<ReportedTokenUsage> {
    let mut reported = ReportedTokenUsage::default();
    if let Some(frames) = response.get("frames").and_then(Value::as_array) {
        for frame in frames {
            reported.merge_reported(reported_usage_from_envelope(frame));
        }
    }
    // A saved response can contain both the terminal usage and copied frames.
    // Its reported root components win without counting any component twice.
    reported.merge_reported(reported_usage_from_envelope(response));
    reported.has_any().then_some(reported)
}

fn reported_usage_from_envelope(envelope: &Value) -> ReportedTokenUsage {
    let mut reported = ReportedTokenUsage::default();
    for usage in [
        envelope.get("usage"),
        envelope.pointer("/message/usage"),
        envelope.get("usageMetadata"),
        envelope.pointer("/message/usageMetadata"),
    ]
    .into_iter()
    .flatten()
    {
        reported.merge_reported(ReportedTokenUsage::from_value(usage));
    }
    reported
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
