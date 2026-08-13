use crate::value::VmValue;

use super::AgentHostSession;

pub(super) struct CallAccounting {
    pub(super) cost_usd: Option<f64>,
    pub(super) usage_unknown: bool,
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
    CallAccounting {
        cost_usd,
        usage_unknown,
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
        }
    }
}

impl SessionUsageTotals {
    pub(super) fn to_vm(self, include_token_split: bool) -> VmValue {
        let mut out = crate::value::DictMap::new();
        out.insert(
            crate::value::intern_key("tokens_used"),
            VmValue::Int(self.tokens_used),
        );
        out.insert(
            crate::value::intern_key("cost_usd"),
            if self.unpriced_calls == 0 {
                VmValue::Float(self.known_cost_usd)
            } else {
                VmValue::Nil
            },
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
