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

mod receipt;
pub(crate) use receipt::ProviderUsageReceipt;

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

/// Why an attempt in a ledger carries no price.
///
/// A ceiling consumer needs this to tell a bound it can compute from one it
/// cannot: an attempt that reported tokens on a priced route has a worst case,
/// while a route with no price table has none at any token count.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnpricedReason {
    /// The route has no entry in the price table, so no token count bounds it.
    NoPriceTable,
    /// The route is priced but the attempt reported no usable token counts.
    UsageUnreported,
    /// The attempt produced no response at all, so neither a token count nor
    /// a price table bounds it.
    NoResponse,
    /// Unpriced attempts in this ledger disagree, or the ledger predates this
    /// field. Either way the projection refuses.
    Mixed,
}

impl UnpricedReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NoPriceTable => "no_price_table",
            Self::UsageUnreported => "usage_unreported",
            Self::NoResponse => "no_response",
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
    #[serde(default)]
    pub provider_call_count: i64,
    #[serde(default)]
    pub unpriced_calls: i64,
    #[serde(default)]
    pub usage_unknown_calls: i64,
    /// Tokens the unpriced attempts in this ledger did report. This is what
    /// the worst-case projection below is priced from, so a reader can see
    /// how much of that bound rests on measured counts.
    #[serde(default)]
    pub unpriced_tokens: i64,
    /// Why the unpriced attempts carry no price. `None` when nothing here is
    /// unpriced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unpriced_reason: Option<UnpricedReason>,
    /// Worst case USD for this ledger: the priced portion plus a price-table
    /// bound on every unpriced attempt's reported tokens. `None` means at
    /// least one unpriced attempt is unprojectable, and a ceiling consumer
    /// must fail closed on it.
    ///
    /// A ledger deserialized from before this field existed reads `None`.
    /// `summarize_usage_cost_certainty` reconstructs those from `cost_usd`
    /// rather than letting the absent field read as a refusal.
    #[serde(default)]
    pub projected_cost_usd: Option<f64>,
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
        self.unpriced_calls < self.provider_call_count
    }
}

/// Fold cost and accounting certainty once for every reporting projection.
pub fn summarize_usage_cost_certainty<'a>(
    usages: impl IntoIterator<Item = &'a LlmUsage>,
) -> UsageCostCertainty {
    usages
        .into_iter()
        .fold(UsageCostCertainty::default(), |mut summary, usage| {
            // A zero call count identifies ledgers recorded before the
            // aggregation fields existed. Reconstruct their one-call
            // certainty from the original stable fields.
            let legacy = usage.provider_call_count == 0;
            summary.known_cost_usd += if legacy {
                usage.cost_usd.unwrap_or(0.0)
            } else {
                usage.known_cost_usd
            };
            summary.provider_call_count += if legacy { 1 } else { usage.provider_call_count };
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
                usage.unpriced_tokens
            };
            let reason = if legacy {
                usage.cost_usd.is_none().then_some(UnpricedReason::Mixed)
            } else {
                usage.unpriced_reason
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
                usage.projected_cost_usd
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
            summary
        })
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
        (None, None) => (Some(UnpricedReason::NoPriceTable), None),
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
            cache_hit_ratio: (cache_accounting_declared == Some(true) && cache_supported).then(
                || {
                    super::cost::cache_hit_ratio(
                        input_tokens,
                        cache_read_tokens,
                        cache_write_tokens,
                    )
                },
            ),
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
            provider_call_count: certainty.provider_call_count,
            unpriced_calls: certainty.unpriced_calls,
            usage_unknown_calls: certainty.usage_unknown_calls,
            unpriced_tokens: certainty.unpriced_tokens,
            unpriced_reason: certainty.unpriced_reason,
            projected_cost_usd: certainty.projected_cost_usd(),
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
            provider_call_count: 1,
            unpriced_calls: 0,
            usage_unknown_calls: 0,
            unpriced_tokens: 0,
            unpriced_reason: None,
            projected_cost_usd: Some(0.0),
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
            provider_call_count: count,
            unpriced_calls: count,
            usage_unknown_calls: count,
            unpriced_tokens: 0,
            // No response arrived, so neither a token count nor a price table
            // bounds what it may have cost. That refuses the projection, which
            // is what keeps a ceiling consumer failing closed.
            unpriced_reason: Some(UnpricedReason::NoResponse),
            projected_cost_usd: None,
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
        // The price table is looked up whether or not the counts are usable,
        // because it is what separates an unpriced attempt that still has a
        // worst case from one that has none at any token count.
        let table_cost = super::cost::pricing_detail_for_tier(
            &result.provider,
            &result.model,
            result.served_fast,
            result.input_tokens,
        )
        .map(|detail| {
            super::cost::project_call_cost(
                &detail,
                result.input_tokens,
                result.output_tokens,
                result.cache_read_tokens,
                result.cache_write_tokens,
            )
        });
        let cost_usd = authoritative_cost
            .or_else(|| free_route.then_some(0.0))
            .or_else(|| component_usage_known.then_some(table_cost).flatten());
        let (unpriced_reason, projected_cost_usd) = unpriced_projection(cost_usd, table_cost);
        let cache_hit_ratio = (result.telemetry.cache_accounting_declared == Some(true)
            && result.cache_supported)
            .then(|| {
                super::cost::cache_hit_ratio(
                    result.input_tokens,
                    result.cache_read_tokens,
                    result.cache_write_tokens,
                )
            });
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
            ),
            cache_hit: result.cache_read_tokens > 0,
            served_fast: result.served_fast,
            accounting_status: if usage_known || authoritative_cost.is_some() {
                UsageAccountingStatus::Reported
            } else {
                UsageAccountingStatus::Unknown
            },
            known_cost_usd: cost_usd.unwrap_or(0.0),
            provider_call_count: 1,
            unpriced_calls: i64::from(cost_usd.is_none()),
            usage_unknown_calls: i64::from(!usage_known && authoritative_cost.is_none()),
            unpriced_tokens: unpriced_token_count(
                cost_usd,
                result.input_tokens,
                result.output_tokens,
            ),
            unpriced_reason,
            projected_cost_usd,
        }
    }

    fn from_probe_counts(
        provider: &str,
        model: &str,
        input_tokens: i64,
        output_tokens: i64,
    ) -> Self {
        let cost_usd =
            super::cost::pricing_aware_call_cost(provider, model, input_tokens, output_tokens);
        Self {
            input_tokens,
            output_tokens,
            reported_total_tokens: None,
            cost_usd,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_supported: false,
            cache_accounting_declared: None,
            cache_hit_ratio: None,
            cache_savings_usd: 0.0,
            cache_hit: false,
            served_fast: false,
            accounting_status: UsageAccountingStatus::Reported,
            known_cost_usd: cost_usd.unwrap_or(0.0),
            provider_call_count: 1,
            unpriced_calls: i64::from(cost_usd.is_none()),
            usage_unknown_calls: 0,
            unpriced_tokens: unpriced_token_count(cost_usd, input_tokens, output_tokens),
            // A probe reports its own counts, so an unpriced probe is unpriced
            // because the route has no price table.
            unpriced_reason: cost_usd.is_none().then_some(UnpricedReason::NoPriceTable),
            projected_cost_usd: cost_usd,
        }
    }

    /// Normalize the accounting carried by a typed parser error. Complete
    /// token counts earn the same catalog pricing as a completed response;
    /// partial receipts retain their measured fields but remain explicitly
    /// unknown rather than turning absence into a free zero.
    pub(crate) fn from_provider_error_receipt(
        provider: &str,
        model: &str,
        receipt: &ProviderUsageReceipt,
    ) -> Self {
        let input_tokens = receipt.input_tokens.unwrap_or(0);
        let output_tokens = receipt.output_tokens.unwrap_or(0);
        let complete_counts = receipt.has_complete_token_counts();
        let free_route = crate::llm_config::provider_is_self_hosted(provider);
        let table_cost = super::cost::pricing_detail_for_tier(
            provider,
            model,
            receipt.served_fast,
            input_tokens,
        )
        .map(|detail| {
            super::cost::project_call_cost(
                &detail,
                input_tokens,
                output_tokens,
                receipt.cache_read_tokens,
                receipt.cache_write_tokens,
            )
        });
        let cost_usd = receipt
            .provider_cost_usd
            .or_else(|| free_route.then_some(0.0))
            .or_else(|| complete_counts.then_some(table_cost).flatten());
        let (unpriced_reason, projected_cost_usd) = unpriced_projection(cost_usd, table_cost);
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
                .then(|| {
                    super::cost::cache_hit_ratio(
                        input_tokens,
                        receipt.cache_read_tokens,
                        receipt.cache_write_tokens,
                    )
                }),
            cache_savings_usd: super::cost::cache_savings_usd_for_provider(
                provider,
                model,
                input_tokens,
                receipt.cache_read_tokens,
                receipt.cache_write_tokens,
            ),
            cache_hit: receipt.cache_read_tokens > 0,
            served_fast: receipt.served_fast,
            accounting_status: if usage_unknown == 0 {
                UsageAccountingStatus::Reported
            } else {
                UsageAccountingStatus::Unknown
            },
            known_cost_usd: cost_usd.unwrap_or(0.0),
            provider_call_count: 1,
            unpriced_calls: i64::from(cost_usd.is_none()),
            usage_unknown_calls: usage_unknown,
            unpriced_tokens: unpriced_token_count(cost_usd, input_tokens, output_tokens),
            unpriced_reason,
            projected_cost_usd,
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
        usage.insert(
            crate::value::intern_key("provider_call_count"),
            VmValue::Int(self.provider_call_count),
        );
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
            VmValue::Int(self.unpriced_tokens),
        );
        usage.insert(
            crate::value::intern_key("unpriced_reason"),
            self.unpriced_reason
                .map_or(VmValue::Nil, |reason| VmValue::string(reason.as_str())),
        );
        usage.insert(
            crate::value::intern_key("projected_cost_usd"),
            self.projected_cost_usd.map_or(VmValue::Nil, VmValue::Float),
        );
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
        fields.insert(
            "provider_call_count".to_string(),
            self.provider_call_count.into(),
        );
        fields.insert("unpriced_calls".to_string(), self.unpriced_calls.into());
        fields.insert(
            "usage_unknown_calls".to_string(),
            self.usage_unknown_calls.into(),
        );
        fields.insert("unpriced_tokens".to_string(), self.unpriced_tokens.into());
        fields.insert(
            "unpriced_reason".to_string(),
            self.unpriced_reason
                .map_or(Value::Null, |reason| reason.as_str().into()),
        );
        fields.insert(
            "projected_cost_usd".to_string(),
            self.projected_cost_usd
                .map_or(Value::Null, serde_json::Value::from),
        );
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

    pub(crate) fn empty_vm_dict() -> crate::value::DictMap {
        Self::unknown_attempt().to_vm_dict(&ProviderAttempts::default())
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

    fn from_totals(provider: &str, model: &str, totals: UsageTotals) -> Self {
        if let Some((input_tokens, output_tokens)) = totals.input_tokens.zip(totals.output_tokens) {
            return Self::from_usage(LlmUsage::from_probe_counts(
                provider,
                model,
                input_tokens,
                output_tokens,
            ));
        }
        Self {
            input_tokens: totals.input_tokens,
            output_tokens: totals.output_tokens,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct UsageTotals {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
}

impl UsageTotals {
    fn has_any(self) -> bool {
        self.input_tokens.is_some() || self.output_tokens.is_some()
    }

    fn add_input(&mut self, value: i64) {
        self.input_tokens = Some(self.input_tokens.unwrap_or(0).saturating_add(value.max(0)));
    }

    fn add_output(&mut self, value: i64) {
        self.output_tokens = Some(self.output_tokens.unwrap_or(0).saturating_add(value.max(0)));
    }
}

pub(crate) fn extract_probe_usage(
    provider: &str,
    model: &str,
    response: &Value,
) -> Option<ToolProbeUsage> {
    let totals = usage_totals_from_response(response)?;
    Some(ToolProbeUsage::from_totals(provider, model, totals))
}

fn usage_totals_from_response(response: &Value) -> Option<UsageTotals> {
    let root_totals = usage_totals_from_envelope(response);
    if root_totals.has_any() {
        return Some(root_totals);
    }
    let frame_totals = last_stream_frame_usage(response);
    frame_totals.has_any().then_some(frame_totals)
}

fn last_stream_frame_usage(response: &Value) -> UsageTotals {
    let mut final_totals = UsageTotals::default();
    let Some(frames) = response.get("frames").and_then(Value::as_array) else {
        return final_totals;
    };
    for frame in frames {
        let frame_totals = usage_totals_from_envelope(frame);
        if frame_totals.has_any() {
            final_totals = frame_totals;
        }
    }
    final_totals
}

fn usage_totals_from_envelope(envelope: &Value) -> UsageTotals {
    let mut totals = UsageTotals::default();
    accumulate_usage_object(envelope.get("usage"), &mut totals);
    accumulate_usage_object(envelope.pointer("/message/usage"), &mut totals);
    accumulate_usage_object(envelope.get("usageMetadata"), &mut totals);
    accumulate_usage_object(envelope.pointer("/message/usageMetadata"), &mut totals);
    totals
}

fn accumulate_usage_object(usage: Option<&Value>, totals: &mut UsageTotals) {
    let Some(usage) = usage else {
        return;
    };
    if let Some(value) = first_i64_field(
        usage,
        &[
            "input_tokens",
            "prompt_tokens",
            "promptTokenCount",
            "prompt_token_count",
            "inputTokens",
        ],
    ) {
        totals.add_input(value);
    }

    let output_tokens = first_i64_field(
        usage,
        &[
            "output_tokens",
            "completion_tokens",
            "candidatesTokenCount",
            "completion_token_count",
            "outputTokenCount",
            "outputTokens",
        ],
    );
    let thoughts_tokens = first_i64_field(usage, &["thoughtsTokenCount", "thought_tokens"]);
    match (output_tokens, thoughts_tokens) {
        (Some(output), Some(thoughts)) => totals.add_output(output.saturating_add(thoughts)),
        (Some(output), None) => totals.add_output(output),
        (None, Some(thoughts)) => totals.add_output(thoughts),
        (None, None) => {}
    }
}

fn first_i64_field(value: &Value, names: &[&str]) -> Option<i64> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_i64))
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
