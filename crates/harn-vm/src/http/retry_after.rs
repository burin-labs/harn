use std::time::{Duration, SystemTime};

use super::MAX_RETRY_DELAY_MS;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(super) fn requested_at(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(secs) = value.parse::<f64>() {
        if !secs.is_finite() || secs < 0.0 {
            return Some(Duration::ZERO);
        }
        return Some(Duration::from_millis((secs * 1_000.0) as u64));
    }
    httpdate::parse_http_date(value)
        .ok()
        .map(|target| target.duration_since(now).unwrap_or(Duration::ZERO))
}

pub(super) fn parse_retry_after_value_at(value: &str, now: SystemTime) -> Option<Duration> {
    requested_at(value, now).map(|delay| delay.min(Duration::from_millis(MAX_RETRY_DELAY_MS)))
}

pub(super) fn parse_retry_after_value(value: &str) -> Option<Duration> {
    parse_retry_after_value_at(value, SystemTime::now())
}

pub(crate) fn hint(value: &str) -> Option<(u64, bool)> {
    let millis = requested_at(value, SystemTime::now())?.as_millis();
    Some((
        millis.min(u128::from(MAX_RETRY_DELAY_MS)) as u64,
        millis > u128::from(MAX_RETRY_DELAY_MS),
    ))
}

pub(super) fn register(vm: &mut Vm) {
    vm.register_builtin_def(&HTTP_RETRY_AFTER_MS_AT_IMPL_DEF);
}

#[harn_builtin(
    exposure = "stdlib_internal",
    effects = [],
    sig = "__http_retry_after_ms_at(value: string, now_seconds: float) -> int?",
    category = "network"
)]
fn http_retry_after_ms_at_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let Some(VmValue::String(value)) = args.first() else {
        return Err(VmError::Runtime(
            "retry-after value must be a string".to_string(),
        ));
    };
    let seconds = match args.get(1) {
        Some(VmValue::Float(value)) => *value,
        Some(VmValue::Int(value)) => *value as f64,
        _ => {
            return Err(VmError::Runtime(
                "retry-after clock must be numeric".to_string(),
            ))
        }
    };
    let delta = Duration::try_from_secs_f64(seconds.abs())
        .map_err(|_| VmError::Runtime("retry-after clock is out of range".to_string()))?;
    let now = if seconds >= 0.0 {
        SystemTime::UNIX_EPOCH.checked_add(delta)
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(delta)
    }
    .ok_or_else(|| VmError::Runtime("retry-after clock is out of range".to_string()))?;
    Ok(requested_at(value, now)
        .map(|delay| VmValue::Int(delay.as_millis().min(i64::MAX as u128) as i64))
        .unwrap_or(VmValue::Nil))
}
