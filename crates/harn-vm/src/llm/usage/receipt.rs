use serde_json::Value;

use crate::value::VmValue;

/// Provider-reported accounting facts that arrive with an unusable response.
///
/// A parser may know that a response billed tokens before it knows whether the
/// response contains an answer or dispatchable tool call. Keeping that fact in
/// this small receipt lets the parser throw a typed completion error without
/// discarding spend that the observed-call ledger must retain. `None` means the
/// provider omitted a field; `Some(0)` is an observed zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ProviderUsageReceipt {
    pub(super) input_tokens: Option<i64>,
    pub(super) output_tokens: Option<i64>,
    pub(super) reported_total_tokens: Option<i64>,
    pub(super) cache_read_tokens: i64,
    pub(super) cache_write_tokens: i64,
    pub(super) cache_accounting_declared: Option<bool>,
    pub(super) cache_supported: bool,
    pub(super) provider_cost_usd: Option<f64>,
    pub(super) served_fast: bool,
}

impl ProviderUsageReceipt {
    pub(crate) fn new(
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        provider_cost_usd: Option<f64>,
        served_fast: bool,
    ) -> Self {
        Self {
            input_tokens: input_tokens.filter(|tokens| *tokens >= 0),
            output_tokens: output_tokens.filter(|tokens| *tokens >= 0),
            reported_total_tokens: None,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_accounting_declared: None,
            cache_supported: true,
            provider_cost_usd: provider_cost_usd.filter(|cost| cost.is_finite() && *cost >= 0.0),
            served_fast,
        }
    }

    /// Parse the token counters shared by OpenAI-compatible usage objects.
    /// A present malformed or negative counter invalidates the whole token
    /// receipt: accepting a positive sibling would hide contradictory wire
    /// evidence. An empty usage object remains a valid but unmeasured receipt.
    pub(crate) fn from_openai_usage_tokens(usage: &Value) -> Option<Self> {
        let input_tokens =
            optional_non_negative_json_int(usage, &["prompt_tokens", "input_tokens"]).ok()?;
        let output_tokens =
            optional_non_negative_json_int(usage, &["completion_tokens", "output_tokens"]).ok()?;
        let reported_total_tokens =
            optional_non_negative_json_int(usage, &["total_tokens"]).ok()?;
        Some(
            Self::new(input_tokens, output_tokens, None, false)
                .with_reported_total(reported_total_tokens),
        )
    }

    pub(crate) fn with_reported_total(mut self, total_tokens: Option<i64>) -> Self {
        self.reported_total_tokens = total_tokens.filter(|tokens| *tokens >= 0);
        self
    }

    pub(crate) fn has_any_reported_token_count(&self) -> bool {
        self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.reported_total_tokens.is_some()
    }

    pub(crate) fn input_tokens(&self) -> Option<i64> {
        self.input_tokens
    }

    pub(crate) fn reported_total_tokens(&self) -> Option<i64> {
        self.reported_total_tokens
    }

    /// Retain cache facts alongside the token receipt. The parser already
    /// derives these from the same usage object on successful calls, so an
    /// unusable response must not change its accounting convention.
    pub(crate) fn with_cache(
        mut self,
        cache_read_tokens: i64,
        cache_write_tokens: i64,
        cache_accounting_declared: Option<bool>,
        cache_supported: bool,
    ) -> Self {
        self.cache_read_tokens = cache_read_tokens.max(0);
        self.cache_write_tokens = cache_write_tokens.max(0);
        self.cache_accounting_declared = cache_accounting_declared;
        self.cache_supported = cache_supported;
        self
    }

    /// Preserve this receipt on a typed parser error. It is deliberately a
    /// nested field so ordinary error classification does not need to learn
    /// provider-accounting vocabulary.
    pub(crate) fn to_vm_value(&self) -> VmValue {
        VmValue::dict(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("input_tokens"),
                self.input_tokens.map_or(VmValue::Nil, VmValue::Int),
            ),
            (
                crate::value::intern_key("output_tokens"),
                self.output_tokens.map_or(VmValue::Nil, VmValue::Int),
            ),
            (
                crate::value::intern_key("reported_total_tokens"),
                self.reported_total_tokens
                    .map_or(VmValue::Nil, VmValue::Int),
            ),
            (
                crate::value::intern_key("cache_read_tokens"),
                VmValue::Int(self.cache_read_tokens),
            ),
            (
                crate::value::intern_key("cache_write_tokens"),
                VmValue::Int(self.cache_write_tokens),
            ),
            (
                crate::value::intern_key("cache_accounting_declared"),
                self.cache_accounting_declared
                    .map_or(VmValue::Nil, VmValue::Bool),
            ),
            (
                crate::value::intern_key("cache_supported"),
                VmValue::Bool(self.cache_supported),
            ),
            (
                crate::value::intern_key("provider_cost_usd"),
                self.provider_cost_usd.map_or(VmValue::Nil, VmValue::Float),
            ),
            (
                crate::value::intern_key("served_fast"),
                VmValue::Bool(self.served_fast),
            ),
        ]))
    }

    /// Decode only the parser-owned, nested receipt. Malformed or absent
    /// receipts remain unknown; callers must never promote an error string to
    /// a measured ledger.
    pub(crate) fn from_error(error: &crate::value::VmError) -> Option<Self> {
        let crate::value::VmError::Thrown(VmValue::Dict(error_fields)) = error else {
            return None;
        };
        let VmValue::Dict(fields) = error_fields.get("provider_usage")? else {
            return None;
        };
        let input_tokens = optional_non_negative_int(fields, "input_tokens").ok()?;
        let output_tokens = optional_non_negative_int(fields, "output_tokens").ok()?;
        let reported_total_tokens =
            optional_non_negative_int_if_present(fields, "reported_total_tokens").ok()?;
        let cache_read_tokens = non_negative_int(fields, "cache_read_tokens")?;
        let cache_write_tokens = non_negative_int(fields, "cache_write_tokens")?;
        let cache_accounting_declared = optional_bool(fields, "cache_accounting_declared").ok()?;
        let VmValue::Bool(cache_supported) = fields.get("cache_supported")? else {
            return None;
        };
        let provider_cost_usd = optional_non_negative_float(fields, "provider_cost_usd").ok()?;
        let VmValue::Bool(served_fast) = fields.get("served_fast")? else {
            return None;
        };
        Some(Self {
            input_tokens,
            output_tokens,
            reported_total_tokens,
            cache_read_tokens,
            cache_write_tokens,
            cache_accounting_declared,
            cache_supported: *cache_supported,
            provider_cost_usd,
            served_fast: *served_fast,
        })
    }

    pub(super) fn has_complete_token_counts(&self) -> bool {
        self.input_tokens.is_some() && self.output_tokens.is_some()
    }

    pub(crate) fn output_tokens(&self) -> Option<i64> {
        self.output_tokens
    }
}

fn optional_non_negative_json_int(
    value: &Value,
    keys: &[&str],
) -> Result<Option<i64>, InvalidOptionalReceiptField> {
    let object = value.as_object().ok_or(InvalidOptionalReceiptField)?;
    let mut observed = None;
    for key in keys {
        let Some(raw) = object.get(*key) else {
            continue;
        };
        let tokens = raw
            .as_i64()
            .filter(|tokens| *tokens >= 0)
            .ok_or(InvalidOptionalReceiptField)?;
        if observed.is_some_and(|existing| existing != tokens) {
            return Err(InvalidOptionalReceiptField);
        }
        observed = Some(tokens);
    }
    Ok(observed)
}

fn non_negative_int(fields: &crate::value::DictMap, key: &str) -> Option<i64> {
    match fields.get(key)? {
        VmValue::Int(value) if *value >= 0 => Some(*value),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct InvalidOptionalReceiptField;

fn optional_bool(
    fields: &crate::value::DictMap,
    key: &str,
) -> Result<Option<bool>, InvalidOptionalReceiptField> {
    match fields.get(key) {
        None => Err(InvalidOptionalReceiptField),
        Some(value) => match value {
            VmValue::Nil => Ok(None),
            VmValue::Bool(value) => Ok(Some(*value)),
            _ => Err(InvalidOptionalReceiptField),
        },
    }
}

fn optional_non_negative_int(
    fields: &crate::value::DictMap,
    key: &str,
) -> Result<Option<i64>, InvalidOptionalReceiptField> {
    match fields.get(key) {
        None => Err(InvalidOptionalReceiptField),
        Some(value) => match value {
            VmValue::Nil => Ok(None),
            VmValue::Int(value) if *value >= 0 => Ok(Some(*value)),
            _ => Err(InvalidOptionalReceiptField),
        },
    }
}

fn optional_non_negative_int_if_present(
    fields: &crate::value::DictMap,
    key: &str,
) -> Result<Option<i64>, InvalidOptionalReceiptField> {
    match fields.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(value)) if *value >= 0 => Ok(Some(*value)),
        Some(_) => Err(InvalidOptionalReceiptField),
    }
}

fn optional_non_negative_float(
    fields: &crate::value::DictMap,
    key: &str,
) -> Result<Option<f64>, InvalidOptionalReceiptField> {
    match fields.get(key) {
        None => Err(InvalidOptionalReceiptField),
        Some(value) => match value {
            VmValue::Nil => Ok(None),
            VmValue::Float(value) if value.is_finite() && *value >= 0.0 => Ok(Some(*value)),
            _ => Err(InvalidOptionalReceiptField),
        },
    }
}
