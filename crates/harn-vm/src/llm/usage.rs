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
    /// At least one provider request priced and at least one did not. The
    /// priced portion is a real measurement and stays readable; the unpriced
    /// portion is enumerated in `unpriced_attempts` with the reason it could
    /// not be priced. This is deliberately distinct from `Unknown`: an
    /// unmeasured sibling must not black out a measurement that was taken.
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

/// Why one provider request carries no price.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnpricedReason {
    /// The provider reported token counts but the route has no price table,
    /// so nothing can turn those tokens into money.
    NoPriceTable,
    /// The provider reported usage and every count in it was zero.
    ZeroUsageReported,
    /// The provider reported no usage at all.
    ProviderUnreported,
}

/// One provider request that produced no price, kept as a fact rather than
/// folded away.
///
/// Recording the reason and the tokens the provider did report is what keeps
/// an unmeasured attempt from being reported as a measured zero.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnpricedAttempt {
    pub reason: UnpricedReason,
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_total_tokens: Option<i64>,
    /// Worst case this attempt could have cost: the route's price table
    /// applied to the tokens it reported. `None` means the route has no price
    /// table, so no projection is possible. An unprojectable attempt
    /// contributes zero to a projected total and is counted separately, so a
    /// reader is never told an unmeasured attempt was free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projected_cost_usd: Option<f64>,
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
    /// Every unpriced provider request this ledger covers, with the reason and
    /// the tokens it reported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unpriced_attempts: Vec<UnpricedAttempt>,
    /// `known_cost_usd` plus a worst-case projection for each unpriced
    /// attempt. This is the number a spend governor must consume: it never
    /// under-reports what a call may have cost, and it never blacks out a
    /// measured sibling because an unmeasured one sat beside it.
    #[serde(default)]
    pub projected_cost_usd: f64,
    /// Unpriced attempts whose route has no price table, so they contribute
    /// zero to `projected_cost_usd`. Surfaced rather than absorbed, because a
    /// projection built on an unprojectable attempt is a floor, not a ceiling.
    #[serde(default)]
    pub unprojectable_attempts: i64,
}

/// Record one provider request that produced no price.
///
/// Returns an empty list when the request priced, so a caller cannot enumerate
/// an attempt that does not exist. `projected` is the route's price table
/// applied to the tokens the request reported, or `None` when the route has no
/// table and therefore no projection.
fn unpriced_attempt_for(
    cost_usd: Option<f64>,
    reason: UnpricedReason,
    input_tokens: i64,
    output_tokens: i64,
    reported_total_tokens: Option<i64>,
    projected: Option<f64>,
) -> Vec<UnpricedAttempt> {
    if cost_usd.is_some() {
        return Vec::new();
    }
    vec![UnpricedAttempt {
        reason,
        input_tokens,
        output_tokens,
        reported_total_tokens,
        projected_cost_usd: projected,
    }]
}

impl UnpricedAttempt {
    const fn reason_str(&self) -> &'static str {
        match self.reason {
            UnpricedReason::NoPriceTable => "no_price_table",
            UnpricedReason::ZeroUsageReported => "zero_usage_reported",
            UnpricedReason::ProviderUnreported => "provider_unreported",
        }
    }

    fn to_vm_value(&self) -> VmValue {
        let mut attempt = crate::value::DictMap::new();
        attempt.put_str("reason", self.reason_str());
        attempt.insert(
            crate::value::intern_key("input_tokens"),
            VmValue::Int(self.input_tokens),
        );
        attempt.insert(
            crate::value::intern_key("output_tokens"),
            VmValue::Int(self.output_tokens),
        );
        attempt.insert(
            crate::value::intern_key("reported_total_tokens"),
            self.reported_total_tokens
                .map_or(VmValue::Nil, VmValue::Int),
        );
        attempt.insert(
            crate::value::intern_key("projected_cost_usd"),
            self.projected_cost_usd.map_or(VmValue::Nil, VmValue::Float),
        );
        VmValue::dict_map(attempt)
    }
}

/// Aggregate cost certainty for a collection of canonical call ledgers.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageCostCertainty {
    pub known_cost_usd: f64,
    /// `known_cost_usd` plus the worst-case projection for every unpriced
    /// request folded in here.
    pub projected_cost_usd: f64,
    pub provider_call_count: i64,
    pub unpriced_calls: i64,
    pub usage_unknown_calls: i64,
    pub unprojectable_attempts: i64,
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
            // A ledger recorded before projections existed carries no
            // projection to add. Counting its unpriced request as
            // unprojectable keeps the projected total an honest floor instead
            // of implying the request was free.
            summary.projected_cost_usd += if legacy {
                usage.cost_usd.unwrap_or(0.0)
            } else {
                usage.projected_cost_usd
            };
            summary.unprojectable_attempts += if legacy {
                i64::from(usage.cost_usd.is_none())
            } else {
                usage.unprojectable_attempts
            };
            summary
        })
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
            // The priced portion survives an unpriced sibling. Voiding it
            // would convert a measurement that was taken into no measurement
            // at all, which is the one direction this ledger must never move.
            // `cost_usd` is null only when nothing priced.
            cost_usd: (certainty.unpriced_calls == 0
                || certainty.provider_call_count > certainty.unpriced_calls)
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
            // `Unknown` is reserved for a call where nothing priced. Anything
            // partly measured reads `partial`, so a reader can tell a blackout
            // from a gap.
            accounting_status: if certainty.usage_unknown_calls == 0
                && certainty.unpriced_calls == 0
            {
                UsageAccountingStatus::Reported
            } else if certainty.provider_call_count > certainty.unpriced_calls {
                UsageAccountingStatus::Partial
            } else {
                UsageAccountingStatus::Unknown
            },
            known_cost_usd: certainty.known_cost_usd,
            provider_call_count: certainty.provider_call_count,
            unpriced_calls: certainty.unpriced_calls,
            usage_unknown_calls: certainty.usage_unknown_calls,
            unpriced_attempts: usages
                .iter()
                .flat_map(|usage| usage.unpriced_attempts.iter().cloned())
                .collect(),
            projected_cost_usd: certainty.projected_cost_usd,
            unprojectable_attempts: certainty.unprojectable_attempts,
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

    /// An attempt that is known to have cost nothing, such as a request the
    /// provider rejected before serving it.
    ///
    /// This is only for attempts whose zero is a fact. An attempt whose usage
    /// simply was not reported must stay unknown: see `unknown_attempt`.
    pub(crate) fn known_zero_attempt() -> Self {
        Self {
            cost_usd: Some(0.0),
            accounting_status: UsageAccountingStatus::Reported,
            known_cost_usd: 0.0,
            provider_call_count: 1,
            unpriced_calls: 0,
            usage_unknown_calls: 0,
            unpriced_attempts: Vec::new(),
            projected_cost_usd: 0.0,
            unprojectable_attempts: 0,
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
            // No response reached us, so there are no tokens to project from
            // and no route to price. Each attempt is recorded as an
            // unprojectable hole rather than as a zero.
            unpriced_attempts: (0..count)
                .map(|_| UnpricedAttempt {
                    reason: UnpricedReason::ProviderUnreported,
                    input_tokens: 0,
                    output_tokens: 0,
                    reported_total_tokens: None,
                    projected_cost_usd: None,
                })
                .collect(),
            projected_cost_usd: 0.0,
            unprojectable_attempts: count,
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
        // The route's price table, consulted once. It is read whether or not
        // the provider reported usable counts, so a zero-token attempt on a
        // priced route can still project a real zero instead of leaving an
        // unprojectable hole.
        let priced_from_tokens = super::cost::pricing_detail_for_tier(
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
            .or_else(|| {
                component_usage_known
                    .then_some(priced_from_tokens)
                    .flatten()
            });
        let projected_if_unpriced = if free_route {
            Some(0.0)
        } else {
            priced_from_tokens
        };
        // Every count the provider did report came back zero. That is a
        // measurement, and it is not the same fact as a provider that reported
        // nothing, so the two carry different reasons.
        let reported_every_count_zero = usage_known
            && result.input_tokens == 0
            && result.output_tokens == 0
            && result.cache_read_tokens == 0
            && result.cache_write_tokens == 0
            && result.telemetry.server_total_tokens.unwrap_or(0) == 0;
        let unpriced_attempts = unpriced_attempt_for(
            cost_usd,
            if !usage_known {
                UnpricedReason::ProviderUnreported
            } else if reported_every_count_zero {
                UnpricedReason::ZeroUsageReported
            } else {
                UnpricedReason::NoPriceTable
            },
            result.input_tokens,
            result.output_tokens,
            result.telemetry.server_total_tokens,
            projected_if_unpriced,
        );
        let unprojectable_attempts = i64::from(
            unpriced_attempts
                .first()
                .is_some_and(|attempt| attempt.projected_cost_usd.is_none()),
        );
        let projected_cost_usd = cost_usd.unwrap_or_else(|| {
            unpriced_attempts
                .first()
                .and_then(|attempt| attempt.projected_cost_usd)
                .unwrap_or(0.0)
        });
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
            // One physical request cannot be partly accounted: it either
            // priced or it did not. `usage_unknown_calls` below still records
            // the separate question of whether the provider reported counts.
            accounting_status: if cost_usd.is_some() {
                UsageAccountingStatus::Reported
            } else {
                UsageAccountingStatus::Unknown
            },
            known_cost_usd: cost_usd.unwrap_or(0.0),
            provider_call_count: 1,
            unpriced_calls: i64::from(cost_usd.is_none()),
            usage_unknown_calls: i64::from(!usage_known && authoritative_cost.is_none()),
            unpriced_attempts,
            projected_cost_usd,
            unprojectable_attempts,
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
            // A probe's counts are known, so an absent price means the route
            // has no table. Nothing can be projected from tokens nobody
            // priced.
            unpriced_attempts: unpriced_attempt_for(
                cost_usd,
                UnpricedReason::NoPriceTable,
                input_tokens,
                output_tokens,
                None,
                None,
            ),
            projected_cost_usd: cost_usd.unwrap_or(0.0),
            unprojectable_attempts: i64::from(cost_usd.is_none()),
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
        let cost_usd = receipt
            .provider_cost_usd
            .or_else(|| free_route.then_some(0.0))
            .or_else(|| {
                complete_counts
                    .then(|| {
                        super::cost::pricing_detail_for_tier(
                            provider,
                            model,
                            receipt.served_fast,
                            input_tokens,
                        )
                    })
                    .flatten()
                    .map(|detail| {
                        super::cost::project_call_cost(
                            &detail,
                            input_tokens,
                            output_tokens,
                            receipt.cache_read_tokens,
                            receipt.cache_write_tokens,
                        )
                    })
            });
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
            // A partial receipt did not report what pricing needs. Recording
            // the reason keeps that distinct from a route with no price table.
            unpriced_attempts: unpriced_attempt_for(
                cost_usd,
                if usage_unknown == 0 {
                    UnpricedReason::NoPriceTable
                } else {
                    UnpricedReason::ProviderUnreported
                },
                input_tokens,
                output_tokens,
                receipt.reported_total_tokens,
                None,
            ),
            projected_cost_usd: cost_usd.unwrap_or(0.0),
            unprojectable_attempts: i64::from(cost_usd.is_none()),
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
        // The number a spend governor consumes, and the shape that tells a
        // reader how much of it is projection rather than measurement.
        usage.insert(
            crate::value::intern_key("projected_cost_usd"),
            VmValue::Float(self.projected_cost_usd),
        );
        usage.insert(
            crate::value::intern_key("unprojectable_attempts"),
            VmValue::Int(self.unprojectable_attempts),
        );
        usage.insert(
            crate::value::intern_key("unpriced_attempts"),
            VmValue::List(std::sync::Arc::new(
                self.unpriced_attempts
                    .iter()
                    .map(UnpricedAttempt::to_vm_value)
                    .collect::<Vec<_>>(),
            )),
        );
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
        fields.insert(
            "projected_cost_usd".to_string(),
            self.projected_cost_usd.into(),
        );
        fields.insert(
            "unprojectable_attempts".to_string(),
            self.unprojectable_attempts.into(),
        );
        // Enumerated rather than counted: a receipt that says only "one
        // attempt was unpriced" cannot tell a reader whether that attempt was
        // measured at zero or never measured at all.
        fields.insert(
            "unpriced_attempts".to_string(),
            serde_json::to_value(&self.unpriced_attempts).unwrap_or(Value::Null),
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
mod tests {
    use serde_json::json;

    use super::{
        extract_probe_usage, summarize_usage_cost_certainty, LlmUsage, ProviderUsageReceipt,
        ToolProbeUsage, UsageAccountingStatus,
    };
    use crate::llm::api::{LlmResult, ProviderAttempts, ProviderTelemetry};
    use crate::value::VmValue;

    fn accounted_result() -> LlmResult {
        LlmResult {
            text: "ok".to_string(),
            tool_calls: Vec::new(),
            text_projection: None,
            raw_tool_calls: Vec::new(),
            input_tokens: 1_000,
            output_tokens: 100,
            cache_read_tokens: 800,
            cache_write_tokens: 25,
            cache_supported: true,
            model: "claude-sonnet-4-20250514".to_string(),
            provider: "anthropic".to_string(),
            thinking: None,
            thinking_summary: None,
            stop_reason: Some("end_turn".to_string()),
            served_fast: false,
            blocks: Vec::new(),
            logprobs: Vec::new(),
            telemetry: ProviderTelemetry {
                cache_accounting_declared: Some(true),
                ..ProviderTelemetry::default()
            },
            attempts: ProviderAttempts {
                total: 3,
                rate_limited: 1,
                empty_completion: 1,
                other: 0,
                completed_retry_usage: vec![super::LlmUsage::from_probe_counts(
                    "anthropic",
                    "claude-sonnet-4-20250514",
                    250,
                    10,
                )],
            },
        }
    }

    /// A locally served call that reports no token usage at all, which is what
    /// a streaming llama.cpp server sends. Its cost is still known, because the
    /// route bills nothing; only its token counts are missing.
    fn self_hosted_result_without_usage() -> LlmResult {
        LlmResult {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_supported: false,
            model: "some-locally-served-model".to_string(),
            provider: "llamacpp".to_string(),
            telemetry: ProviderTelemetry {
                cache_accounting_declared: Some(false),
                ..ProviderTelemetry::default()
            },
            attempts: ProviderAttempts::default(),
            ..accounted_result()
        }
    }

    #[test]
    fn self_hosted_call_without_reported_usage_is_priced_but_still_usage_unknown() {
        let usage = self_hosted_result_without_usage().usage();

        // The half that was wrong: an unpriced call spends a whole USD ceiling,
        // so this is what ended budgeted local runs after one model call.
        assert_eq!(usage.cost_usd, Some(0.0));
        assert_eq!(usage.unpriced_calls, 0);

        // The half that must stay honest: nothing here tells us how many
        // tokens the call used, and the ledger should not pretend otherwise.
        assert_eq!(usage.usage_unknown_calls, 1);
        assert_eq!(usage.accounting_status, UsageAccountingStatus::Unknown);
    }

    #[test]
    fn live_tool_probe_preserves_missing_usage_as_unknown() {
        let result = LlmResult {
            provider: "together".to_string(),
            model: "Qwen/Qwen3.6-Plus".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            telemetry: ProviderTelemetry::default(),
            attempts: ProviderAttempts::default(),
            ..accounted_result()
        };

        let usage = ToolProbeUsage::from_llm_result(&result);
        let report = serde_json::to_value(&usage).expect("serialize probe usage");

        assert_eq!(usage.input_tokens, Some(0));
        assert_eq!(usage.output_tokens, Some(0));
        assert_eq!(usage.cost_usd, None);
        assert_eq!(usage.accounting_status, UsageAccountingStatus::Unknown);
        assert_eq!(report["accounting_status"], "unknown");
        assert!(report.get("cost_usd").is_none());

        let reported_zero = ToolProbeUsage::from_llm_result(&LlmResult {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            telemetry: ProviderTelemetry {
                server_prompt_tokens: Some(0),
                server_output_tokens: Some(0),
                ..ProviderTelemetry::default()
            },
            ..accounted_result()
        });
        assert_eq!(
            reported_zero.accounting_status,
            UsageAccountingStatus::Reported
        );
        assert_eq!(reported_zero.cost_usd, Some(0.0));
    }

    #[test]
    fn paid_call_without_reported_usage_stays_unpriced() {
        let result = LlmResult {
            provider: "anthropic".to_string(),
            ..self_hosted_result_without_usage()
        };
        let usage = result.usage();

        assert_eq!(
            usage.cost_usd, None,
            "a paid route with no usage counts and no provider cost cannot be priced"
        );
        assert_eq!(usage.unpriced_calls, 1);
    }

    #[test]
    fn partial_provider_error_receipt_stays_explicitly_unknown() {
        let receipt = ProviderUsageReceipt::new(Some(9), None, Some(0.25), false).with_cache(
            3,
            2,
            Some(true),
            true,
        );
        let VmValue::Dict(fields) = receipt.to_vm_value() else {
            panic!("receipt must lower to a dictionary");
        };
        assert_eq!(
            fields.get("input_tokens").and_then(VmValue::as_int),
            Some(9)
        );
        assert!(matches!(fields.get("output_tokens"), Some(VmValue::Nil)));
        assert_eq!(
            fields.get("cache_read_tokens").and_then(VmValue::as_int),
            Some(3)
        );

        let usage = LlmUsage::from_provider_error_receipt(
            "anthropic",
            "claude-sonnet-4-20250514",
            &receipt,
        );

        assert_eq!(usage.input_tokens, 9);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cost_usd, Some(0.25));
        assert_eq!(usage.known_cost_usd, 0.25);
        assert_eq!(usage.cache_read_tokens, 3);
        assert_eq!(usage.cache_write_tokens, 2);
        assert!(usage.cache_hit);
        assert_eq!(usage.unpriced_calls, 0);
        assert_eq!(usage.usage_unknown_calls, 1);
        assert_eq!(usage.accounting_status, UsageAccountingStatus::Unknown);
    }

    #[test]
    fn cost_certainty_fold_preserves_known_floor_and_unknown_counts() {
        let priced = accounted_result().usage();
        let mut unpriced = priced.clone();
        unpriced.cost_usd = None;
        unpriced.accounting_status = UsageAccountingStatus::Unknown;
        unpriced.known_cost_usd = 0.0;
        unpriced.unpriced_calls = 1;
        unpriced.usage_unknown_calls = 1;

        let summary = summarize_usage_cost_certainty([&priced, &unpriced]);

        assert_eq!(
            summary.known_cost_usd,
            priced.cost_usd.expect("priced call")
        );
        assert_eq!(summary.unpriced_calls, 1);
        assert_eq!(summary.usage_unknown_calls, 1);
    }

    #[test]
    fn terminal_unknown_ledger_counts_every_physical_attempt() {
        let usage = LlmUsage::unknown_attempts(3);

        assert_eq!(usage.provider_call_count, 3);
        assert_eq!(usage.unpriced_calls, 3);
        assert_eq!(usage.usage_unknown_calls, 3);
        assert_eq!(usage.cost_usd, None);
        assert_eq!(usage.accounting_status, UsageAccountingStatus::Unknown);
    }

    #[test]
    fn terminal_ledger_preserves_completed_receipts_before_unknown_attempts() {
        let mut completed = LlmUsage::known_zero_attempt();
        completed.cost_usd = Some(0.25);
        completed.known_cost_usd = 0.25;

        let usage = LlmUsage::aggregate_with_unknown_attempts(&[completed], 2);

        assert_eq!(usage.known_cost_usd, 0.25);
        assert_eq!(usage.cost_usd, None);
        assert_eq!(usage.provider_call_count, 3);
        assert_eq!(usage.unpriced_calls, 2);
        assert_eq!(usage.usage_unknown_calls, 2);
        assert_eq!(usage.accounting_status, UsageAccountingStatus::Unknown);
    }

    #[test]
    fn legacy_ledger_reconstructs_one_call_without_losing_known_cost() {
        let mut usage = LlmUsage::known_zero_attempt();
        usage.cost_usd = Some(0.25);
        usage.known_cost_usd = 0.0;
        usage.provider_call_count = 0;

        let summary = summarize_usage_cost_certainty([&usage]);

        assert_eq!(summary.known_cost_usd, 0.25);
        assert_eq!(summary.provider_call_count, 1);
        assert_eq!(summary.unpriced_calls, 0);
        assert_eq!(summary.usage_unknown_calls, 0);
    }

    #[test]
    fn one_ledger_projects_matching_vm_event_and_trace_accounting() {
        let mut result = accounted_result();
        result.telemetry.server_total_tokens = Some(1_100);
        result.attempts = ProviderAttempts::default();
        let usage = result.usage();
        let tool_usage = ToolProbeUsage::from_llm_result(&result);
        let vm_usage =
            crate::llm::vm_value_to_json(&VmValue::Dict(usage.to_vm_dict(&result.attempts).into()));
        let mut event = json!({});
        usage.project_onto_event(&mut event);
        let trace = usage
            .metadata_pairs(&result.provider, &result.model)
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();

        for field in [
            "input_tokens",
            "output_tokens",
            "reported_total_tokens",
            "cost_usd",
            "cache_read_tokens",
            "cache_write_tokens",
            "cache_hit_ratio",
            "cache_savings_usd",
            "served_fast",
        ] {
            assert_eq!(
                vm_usage.get(field),
                event.get(field),
                "{field} drifted between canonical projections"
            );
        }
        assert_eq!(
            trace[crate::tracing::meta::INPUT_TOKENS],
            event["input_tokens"]
        );
        assert_eq!(
            trace[crate::tracing::meta::OUTPUT_TOKENS],
            event["output_tokens"]
        );
        assert_eq!(
            trace[crate::tracing::meta::REPORTED_TOTAL_TOKENS],
            event["reported_total_tokens"]
        );
        assert_eq!(
            tool_usage.reported_total_tokens, usage.reported_total_tokens,
            "tool probes must retain the same measured whole-call total"
        );
        assert_eq!(trace[crate::tracing::meta::COST_USD], event["cost_usd"]);
        assert_eq!(vm_usage["provider_attempts"]["retries"], json!(0));
    }

    #[test]
    fn missing_stream_usage_stays_unknown_instead_of_becoming_free() {
        let mut result = accounted_result();
        result.provider = "fireworks".to_string();
        result.model = "accounts/fireworks/models/minimax-m3".to_string();
        result.input_tokens = 0;
        result.output_tokens = 0;
        result.telemetry = ProviderTelemetry::from_openai_response(
            &serde_json::json!({"usage": {}}),
            Some("chatcmpl-without-usage"),
        );

        let usage = result.usage();
        let vm_usage =
            crate::llm::vm_value_to_json(&VmValue::Dict(usage.to_vm_dict(&result.attempts).into()));

        assert_eq!(usage.cost_usd, None);
        assert_eq!(vm_usage["accounting_status"], "unknown");
        assert_eq!(vm_usage["cost_usd"], serde_json::Value::Null);
    }

    #[test]
    fn pre_accounting_status_record_replays_as_unknown() {
        let mut recorded = serde_json::to_value(accounted_result().usage()).expect("serialize");
        recorded
            .as_object_mut()
            .expect("usage object")
            .remove("accounting_status");

        let replayed: super::LlmUsage = serde_json::from_value(recorded).expect("old recording");

        assert_eq!(
            replayed.accounting_status,
            super::UsageAccountingStatus::Unknown
        );
    }

    #[test]
    fn public_usage_projections_do_not_recompute_accounting() {
        let projection_sources = [
            (
                "transcript",
                include_str!("agent_observe/transcript_observability.rs"),
            ),
            (
                "structured envelope",
                include_str!("structured_envelope.rs"),
            ),
            ("trace", include_str!("trace.rs")),
            ("agent result", include_str!("agent_config.rs")),
        ];
        for (name, source) in projection_sources {
            for forbidden in [
                "priced_cost_usd(",
                "cache_hit_ratio(",
                "cache_savings_usd_for_provider(",
                "struct LlmCallUsage",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "{name} rebuilt canonical usage via {forbidden}"
                );
            }
        }
    }

    #[test]
    fn extracts_openai_responses_usage() {
        let response = json!({
            "usage": {
                "input_tokens": 11,
                "output_tokens": 7
            }
        });

        let usage = extract_probe_usage("unknown", "unknown", &response).expect("usage");

        assert_eq!(usage.input_tokens, Some(11));
        assert_eq!(usage.output_tokens, Some(7));
        assert_eq!(usage.cost_usd, None);
    }

    #[test]
    fn extracts_gemini_usage_metadata_with_thoughts() {
        let response = json!({
            "usageMetadata": {
                "promptTokenCount": 3,
                "candidatesTokenCount": 4,
                "thoughtsTokenCount": 9
            }
        });

        let usage = extract_probe_usage("gemini", "gemini-2.5-pro", &response).expect("usage");

        assert_eq!(usage.input_tokens, Some(3));
        assert_eq!(usage.output_tokens, Some(13));
    }

    #[test]
    fn extracts_vertex_usage_metadata_from_message_wrapper() {
        let response = json!({
            "message": {
                "usageMetadata": {
                    "promptTokenCount": 5,
                    "candidatesTokenCount": 8
                }
            }
        });

        let usage = extract_probe_usage("vertex", "gemini-2.5-flash", &response).expect("usage");

        assert_eq!(usage.input_tokens, Some(5));
        assert_eq!(usage.output_tokens, Some(8));
    }

    #[test]
    fn extracts_bedrock_usage_tokens() {
        let response = json!({
            "usage": {
                "inputTokens": 17,
                "outputTokens": 23
            }
        });

        let usage = extract_probe_usage("bedrock", "claude-sonnet-5", &response).expect("usage");

        assert_eq!(usage.input_tokens, Some(17));
        assert_eq!(usage.output_tokens, Some(23));
    }

    #[test]
    fn uses_final_stream_usage_without_double_counting_prior_frames() {
        let response = json!({
            "frames": [
                {
                    "usage": {
                        "prompt_tokens": 1,
                        "completion_tokens": 1
                    }
                },
                {
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 2
                    }
                }
            ]
        });

        let usage = extract_probe_usage("unknown", "unknown", &response).expect("usage");

        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(2));
    }

    #[test]
    fn root_usage_dominates_copied_stream_frames() {
        let response = json!({
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 2
            },
            "frames": [
                {
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 2
                    }
                }
            ]
        });

        let usage = extract_probe_usage("unknown", "unknown", &response).expect("usage");

        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(2));
    }

    fn usage_with_declaration(declared: Option<bool>) -> LlmUsage {
        let mut result = accounted_result();
        result.telemetry.cache_accounting_declared = declared;
        result.attempts = ProviderAttempts::default();
        LlmUsage::from_result(&result)
    }

    #[test]
    fn cache_visibility_projects_three_states() {
        let declared_true = usage_with_declaration(Some(true));
        let mut fields = serde_json::Map::new();
        declared_true.project_onto_fields(&mut fields);
        assert_eq!(fields["cache_visibility"], serde_json::Value::Null);

        let declared_false = usage_with_declaration(Some(false));
        assert_eq!(declared_false.cache_hit_ratio, None);
        let mut fields = serde_json::Map::new();
        declared_false.project_onto_fields(&mut fields);
        assert_eq!(fields["cache_visibility"], json!("unsupported"));

        // The load-bearing state: an undeclared route's zeros carry no
        // information, and must not read as either audited or unsupported.
        let undeclared = usage_with_declaration(None);
        assert_eq!(undeclared.cache_hit_ratio, None);
        let mut fields = serde_json::Map::new();
        undeclared.project_onto_fields(&mut fields);
        assert_eq!(fields["cache_hit_ratio"], serde_json::Value::Null);
        assert_eq!(fields["cache_visibility"], json!("undeclared"));
    }

    #[test]
    fn one_undeclared_call_poisons_the_aggregate_to_undeclared() {
        let declared = usage_with_declaration(Some(true));
        let undeclared = usage_with_declaration(None);

        let all_declared = LlmUsage::aggregate(&[declared.clone(), declared.clone()]);
        assert_eq!(all_declared.cache_accounting_declared, Some(true));

        let poisoned = LlmUsage::aggregate(&[declared, undeclared]);
        assert_eq!(poisoned.cache_accounting_declared, None);
        assert_eq!(poisoned.cache_hit_ratio, None);
        let mut fields = serde_json::Map::new();
        poisoned.project_onto_fields(&mut fields);
        assert_eq!(fields["cache_visibility"], json!("undeclared"));
    }

    #[test]
    fn unknown_attempts_stay_neutral_for_the_accounting_declaration() {
        let declared = usage_with_declaration(Some(true));
        let usage = LlmUsage::aggregate_with_unknown_attempts(&[declared], 1);
        assert_eq!(usage.cache_accounting_declared, Some(true));
    }
}
