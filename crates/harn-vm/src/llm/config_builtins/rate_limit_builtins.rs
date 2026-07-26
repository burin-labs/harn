//! Per-provider request and token rate limits: set, query, or clear.

use crate::llm_config;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

use super::provider_projection::rate_limits_to_vm_value;

enum RateLimitField<T> {
    Missing,
    Clear,
    Set(T),
}

fn rate_limit_u64_option(
    opts: &crate::value::DictMap,
    key: &str,
) -> Result<RateLimitField<u64>, VmError> {
    let Some(value) = opts.get(key) else {
        return Ok(RateLimitField::Missing);
    };
    let Some(parsed) = value.as_int() else {
        return Err(VmError::Runtime(format!(
            "llm_rate_limit: options.{key} must be an integer"
        )));
    };
    if parsed <= 0 {
        return Ok(RateLimitField::Clear);
    }
    Ok(RateLimitField::Set(parsed as u64))
}

fn rate_limit_u32_option(
    opts: &crate::value::DictMap,
    key: &str,
) -> Result<RateLimitField<u32>, VmError> {
    match rate_limit_u64_option(opts, key)? {
        RateLimitField::Missing => Ok(RateLimitField::Missing),
        RateLimitField::Clear => Ok(RateLimitField::Clear),
        RateLimitField::Set(parsed) => {
            let parsed = u32::try_from(parsed).map_err(|_| {
                VmError::Runtime(format!(
                    "llm_rate_limit: options.{key} must fit in an unsigned 32-bit integer"
                ))
            })?;
            Ok(RateLimitField::Set(parsed))
        }
    }
}

/// Set, query, or clear per-provider request/token rate limits.
#[harn_builtin(
    sig = "llm_rate_limit(provider: string, options?: dict|nil) -> bool|int|nil|dict",
    category = "llm.rate_limit"
)]
pub(super) fn llm_rate_limit_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let provider = args.first().map(|a| a.display()).unwrap_or_default();
    if provider.is_empty() {
        return Err(VmError::Runtime(
            "llm_rate_limit: provider name is required".to_string(),
        ));
    }
    if let Some(opts) = args.get(1).and_then(|a| a.as_dict()) {
        if opts
            .get("details")
            .is_some_and(|value| matches!(value, VmValue::Bool(true)))
        {
            return Ok(crate::llm::rate_limit::get_rate_limits(&provider)
                .map(|limits| rate_limits_to_vm_value(&limits))
                .unwrap_or(VmValue::Nil));
        }
        let mut limits = llm_config::RateLimitsDef::default();
        let mut saw_limit = false;
        let mut clear = false;
        match rate_limit_u32_option(opts, "rpm")? {
            RateLimitField::Set(parsed) => {
                limits.rpm = Some(parsed);
                saw_limit = true;
            }
            RateLimitField::Clear => clear = true,
            RateLimitField::Missing => {}
        }
        match rate_limit_u64_option(opts, "tpm")? {
            RateLimitField::Set(parsed) => {
                limits.tpm = Some(parsed);
                saw_limit = true;
            }
            RateLimitField::Clear => clear = true,
            RateLimitField::Missing => {}
        }
        match rate_limit_u64_option(opts, "input_tpm")? {
            RateLimitField::Set(parsed) => {
                limits.input_tpm = Some(parsed);
                saw_limit = true;
            }
            RateLimitField::Clear => clear = true,
            RateLimitField::Missing => {}
        }
        match rate_limit_u64_option(opts, "output_tpm")? {
            RateLimitField::Set(parsed) => {
                limits.output_tpm = Some(parsed);
                saw_limit = true;
            }
            RateLimitField::Clear => clear = true,
            RateLimitField::Missing => {}
        }
        match rate_limit_u32_option(opts, "concurrency")? {
            RateLimitField::Set(parsed) => {
                limits.concurrency = Some(parsed);
                saw_limit = true;
            }
            RateLimitField::Clear => clear = true,
            RateLimitField::Missing => {}
        }
        if clear && !saw_limit {
            crate::llm::rate_limit::clear_rate_limit(&provider);
            return Ok(VmValue::Bool(true));
        }
        if saw_limit {
            crate::llm::rate_limit::set_rate_limits(&provider, limits);
            return Ok(VmValue::Bool(true));
        }
        return Err(VmError::Runtime(
            "llm_rate_limit: options must include rpm, tpm, input_tpm, output_tpm, concurrency, or details"
                .to_string(),
        ));
    }
    match crate::llm::rate_limit::get_rate_limit(&provider) {
        Some(rpm) => Ok(VmValue::Int(rpm as i64)),
        None => Ok(VmValue::Nil),
    }
}
