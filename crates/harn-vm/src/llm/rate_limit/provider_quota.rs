use std::time::Duration;

use super::RateLimitRequest;

/// Seed/update proactive TPM pacing from a structured provider response.
/// Provider/model keys both receive it because either can own the quota.
pub(crate) fn observe_for_llm_call(
    opts: &crate::llm::api::LlmCallOptions,
    error: &crate::value::VmError,
) {
    let request = RateLimitRequest::for_llm_call(opts);
    let Some(receipt) = receipt(error, request) else {
        return;
    };
    crate::events::log_debug_meta(
        "llm.rate_limit",
        "provider token quota observed",
        std::collections::BTreeMap::from([
            (
                "schema".to_string(),
                serde_json::json!("harn.llm.provider_token_quota.v1"),
            ),
            ("provider".to_string(), serde_json::json!(opts.provider)),
            ("model".to_string(), serde_json::json!(opts.model)),
            ("limit".to_string(), serde_json::json!(receipt.limit)),
            ("used".to_string(), serde_json::json!(receipt.used)),
            (
                "requested".to_string(),
                serde_json::json!(receipt.requested),
            ),
            (
                "window_ms".to_string(),
                serde_json::json!(receipt.window_ms),
            ),
        ]),
    );
    super::ensure_initialized_from_config();
    let keys = super::limiter_keys(&opts.provider, &opts.model);
    let now_ms = crate::clock_mock::instant_now().as_millis();
    let mut registry = super::registry()
        .lock()
        .expect("rate limiter mutex poisoned");
    for key in keys {
        super::limiter_for_key(&mut registry.limiters, &key).observe_token_quota(now_ms, receipt);
    }
}

/// Complete model-facing quota observation: provider state plus the request
/// Harn projected locally. `requested` is retained for receipts/tests even
/// though rejected work is not charged to the live bucket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ProviderTokenQuotaReceipt {
    pub(super) limit: u64,
    pub(super) used: u64,
    pub(super) requested: u64,
    pub(super) window_ms: u64,
}

pub(super) fn receipt(
    error: &crate::value::VmError,
    request: RateLimitRequest,
) -> Option<ProviderTokenQuotaReceipt> {
    let crate::value::VmError::Thrown(crate::value::VmValue::Dict(error)) = error else {
        return None;
    };
    let crate::value::VmValue::Dict(quota) = error.get("provider_quota")? else {
        return None;
    };
    let uint = |key: &str| match quota.get(key) {
        Some(crate::value::VmValue::Int(value)) if *value >= 0 => Some(*value as u64),
        _ => None,
    };
    if quota.get("resource")?.display() != "tokens" {
        return None;
    }
    Some(ProviderTokenQuotaReceipt {
        limit: uint("limit")?,
        used: uint("used")?,
        requested: request.total_tokens(),
        window_ms: uint("window_ms")?,
    })
}

/// Live provider quota state. Unlike the catalog's conservative sliding
/// window, provider TPM enforcement reports a continuously refilling bucket:
/// `retry_after ~= (used + requested - limit) / (limit / minute)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ObservedTokenQuota {
    limit: u64,
    used: u64,
    observed_at_ms: u128,
    window_ms: u64,
}

impl ObservedTokenQuota {
    pub(super) fn from_receipt(now_ms: u128, receipt: ProviderTokenQuotaReceipt) -> Self {
        Self {
            limit: receipt.limit.max(1),
            used: receipt.used.min(receipt.limit),
            observed_at_ms: now_ms,
            window_ms: receipt.window_ms.max(1),
        }
    }

    pub(super) fn usage_at(self, now_ms: u128) -> u64 {
        let elapsed_ms = now_ms.saturating_sub(self.observed_at_ms);
        let leaked = u128::from(self.limit)
            .saturating_mul(elapsed_ms)
            .checked_div(u128::from(self.window_ms))
            .unwrap_or(u128::MAX)
            .min(u128::from(u64::MAX)) as u64;
        self.used.saturating_sub(leaked)
    }

    fn charge(self, units: u64) -> u64 {
        units.min(self.limit)
    }

    pub(super) fn check(self, now_ms: u128, units: u64) -> Option<Duration> {
        let deficit = self
            .usage_at(now_ms)
            .saturating_add(self.charge(units))
            .saturating_sub(self.limit);
        if deficit == 0 {
            return None;
        }
        let numerator = u128::from(deficit).saturating_mul(u128::from(self.window_ms));
        let wait_ms = numerator.saturating_add(u128::from(self.limit).saturating_sub(1))
            / u128::from(self.limit);
        Some(Duration::from_millis(
            wait_ms.max(1).min(u128::from(u64::MAX)) as u64,
        ))
    }

    pub(super) fn record(&mut self, now_ms: u128, units: u64) {
        self.used = self
            .usage_at(now_ms)
            .saturating_add(self.charge(units))
            .min(self.limit);
        self.observed_at_ms = now_ms;
    }
}
