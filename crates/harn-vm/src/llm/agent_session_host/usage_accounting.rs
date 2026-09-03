use crate::value::VmValue;

use super::AgentHostSession;

pub(super) struct CallAccounting {
    pub(super) cost_usd: Option<f64>,
    pub(super) usage_unknown: bool,
    /// Worst case USD for this call, priced portion included. `None` means the
    /// call has no computable bound, which is what a ceiling must fail closed
    /// on.
    pub(super) projected_cost_usd: Option<f64>,
}

pub(super) fn resolve_call_accounting(
    usage: &VmValue,
    provider: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
) -> CallAccounting {
    let explicit_cost = super::dict_get(usage, "cost_usd").and_then(|value| match value {
        VmValue::Float(value) => Some(*value),
        VmValue::Int(value) => Some(*value as f64),
        _ => None,
    });
    let accounting_status = super::dict_get(usage, "accounting_status");
    let cost_usd = explicit_cost.or_else(|| {
        accounting_status.is_none().then(|| {
            crate::llm::cost::pricing_aware_call_cost_with_cache(
                provider,
                model,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
            )
        })?
    });
    let usage_unknown = matches!(
        accounting_status,
        Some(VmValue::String(status)) if status.as_str() == "unknown"
    );
    // A ledger written before the projection existed carries no field. Its own
    // cost stands in: a priced call projects to what it cost, and an unpriced
    // one still refuses. Absence must not read as a free zero here.
    let projected_cost_usd = match super::dict_get(usage, "projected_cost_usd") {
        Some(VmValue::Float(value)) => Some(*value),
        Some(VmValue::Int(value)) => Some(*value as f64),
        Some(VmValue::Nil) => None,
        _ => cost_usd,
    };
    CallAccounting {
        cost_usd,
        usage_unknown,
        projected_cost_usd,
    }
}

#[derive(Clone, Copy)]
pub(super) struct SessionUsageTotals {
    pub(super) tokens_used: i64,
    pub(super) known_cost_usd: f64,
    pub(super) input_tokens: i64,
    pub(super) output_tokens: i64,
    pub(super) cache_read_tokens: i64,
    pub(super) cache_write_tokens: i64,
    pub(super) unpriced_calls: i64,
    pub(super) usage_unknown_calls: i64,
    pub(super) priced_calls: i64,
    pub(super) projected_cost_used: f64,
    pub(super) unprojectable_calls: i64,
}

impl From<&AgentHostSession> for SessionUsageTotals {
    fn from(session: &AgentHostSession) -> Self {
        Self {
            tokens_used: session.tokens_used,
            known_cost_usd: session.cost_used,
            input_tokens: session.input_tokens,
            output_tokens: session.output_tokens,
            cache_read_tokens: session.cache_read_tokens,
            cache_write_tokens: session.cache_write_tokens,
            unpriced_calls: session.unpriced_calls,
            usage_unknown_calls: session.usage_unknown_calls,
            priced_calls: session.priced_calls,
            projected_cost_used: session.projected_cost_used,
            unprojectable_calls: session.unprojectable_calls,
        }
    }
}

impl SessionUsageTotals {
    /// What a USD ceiling spends against: everything priced plus the worst
    /// case for everything that was not. `None` when any call in the session
    /// has no bound at all, and a governor or budget must then fail closed.
    pub(super) fn projected_cost_usd(self) -> Option<f64> {
        (self.unprojectable_calls == 0).then_some(self.projected_cost_used)
    }

    pub(super) fn to_vm(self, include_token_split: bool) -> VmValue {
        let mut out = crate::value::DictMap::new();
        out.insert(
            crate::value::intern_key("tokens_used"),
            VmValue::Int(self.tokens_used),
        );
        // The priced calls measured something real. Nulling their sum because
        // a sibling was unpriced turns a measurement into no measurement; only
        // a session that priced nothing at all blacks out.
        out.insert(
            crate::value::intern_key("cost_usd"),
            if self.priced_calls > 0 || self.unpriced_calls == 0 {
                VmValue::Float(self.known_cost_usd)
            } else {
                VmValue::Nil
            },
        );
        out.insert(
            crate::value::intern_key("projected_cost_usd"),
            self.projected_cost_usd()
                .map_or(VmValue::Nil, VmValue::Float),
        );
        out.insert(
            crate::value::intern_key("unprojectable_calls"),
            VmValue::Int(self.unprojectable_calls),
        );
        out.insert(
            crate::value::intern_key("known_cost_usd"),
            VmValue::Float(self.known_cost_usd),
        );
        if include_token_split {
            out.insert(
                crate::value::intern_key("input_tokens"),
                VmValue::Int(self.input_tokens),
            );
            out.insert(
                crate::value::intern_key("output_tokens"),
                VmValue::Int(self.output_tokens),
            );
        }
        out.insert(
            crate::value::intern_key("cache_read_tokens"),
            VmValue::Int(self.cache_read_tokens),
        );
        out.insert(
            crate::value::intern_key("cache_write_tokens"),
            VmValue::Int(self.cache_write_tokens),
        );
        out.insert(
            crate::value::intern_key("unpriced_calls"),
            VmValue::Int(self.unpriced_calls),
        );
        out.insert(
            crate::value::intern_key("usage_unknown_calls"),
            VmValue::Int(self.usage_unknown_calls),
        );
        VmValue::dict(out)
    }
}
