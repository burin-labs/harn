use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::time::Duration as StdDuration;

use clap::builder::{PossibleValue, StringValueParser, TypedValueParser};

#[derive(Clone)]
pub(crate) struct CompletionValueParser {
    candidates: fn() -> Vec<String>,
}

impl CompletionValueParser {
    fn new(candidates: fn() -> Vec<String>) -> Self {
        Self { candidates }
    }
}

impl TypedValueParser for CompletionValueParser {
    type Value = String;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &OsStr,
    ) -> Result<Self::Value, clap::Error> {
        StringValueParser::new().parse_ref(cmd, arg, value)
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue> + '_>> {
        let values = (self.candidates)().into_iter().map(PossibleValue::new);
        Some(Box::new(values))
    }
}

pub(crate) fn llm_provider_completion_parser() -> CompletionValueParser {
    CompletionValueParser::new(llm_provider_candidates)
}

pub(crate) fn llm_model_completion_parser() -> CompletionValueParser {
    CompletionValueParser::new(llm_model_candidates)
}

pub(crate) fn trigger_provider_completion_parser() -> CompletionValueParser {
    CompletionValueParser::new(trigger_provider_candidates)
}

fn llm_provider_candidates() -> Vec<String> {
    harn_vm::llm_config::provider_names()
}

fn llm_model_candidates() -> Vec<String> {
    let mut candidates: BTreeSet<String> = harn_vm::llm_config::known_model_names()
        .into_iter()
        .collect();
    candidates.extend(
        harn_vm::llm_config::model_catalog_entries()
            .into_iter()
            .map(|(id, _model)| id),
    );
    candidates.into_iter().collect()
}

fn trigger_provider_candidates() -> Vec<String> {
    harn_vm::registered_provider_metadata()
        .into_iter()
        .map(|metadata| metadata.provider)
        .collect()
}

pub(crate) fn parse_duration_arg(raw: &str) -> Result<StdDuration, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("duration cannot be empty".to_string());
    }

    let (digits, unit) = raw
        .chars()
        .position(|ch| !ch.is_ascii_digit())
        .map(|index| raw.split_at(index))
        .ok_or_else(|| {
            "duration must include a unit suffix like ms, s, m, h, d, or w".to_string()
        })?;
    if digits.is_empty() || unit.is_empty() {
        return Err("duration must be formatted like 30s, 5m, 2h, or 7d".to_string());
    }

    let value = digits
        .parse::<u64>()
        .map_err(|error| format!("invalid duration '{raw}': {error}"))?;
    match unit {
        "ms" => Ok(StdDuration::from_millis(value)),
        "s" => Ok(StdDuration::from_secs(value)),
        "m" => Ok(StdDuration::from_secs(checked_duration_product(
            raw, value, 60,
        )?)),
        "h" => Ok(StdDuration::from_secs(checked_duration_product(
            raw,
            value,
            60 * 60,
        )?)),
        "d" => Ok(StdDuration::from_secs(checked_duration_product(
            raw,
            value,
            60 * 60 * 24,
        )?)),
        "w" => Ok(StdDuration::from_secs(checked_duration_product(
            raw,
            value,
            60 * 60 * 24 * 7,
        )?)),
        _ => Err(format!(
            "unsupported duration unit '{unit}'; expected ms, s, m, h, d, or w"
        )),
    }
}

fn checked_duration_product(raw: &str, value: u64, multiplier: u64) -> Result<u64, String> {
    value
        .checked_mul(multiplier)
        .ok_or_else(|| format!("duration '{raw}' is too large"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_arg_rejects_unit_overflow() {
        let err = parse_duration_arg("18446744073709551615w").unwrap_err();
        assert!(err.contains("too large"), "{err}");
    }

    #[test]
    fn parse_duration_arg_accepts_large_millisecond_value() {
        assert_eq!(
            parse_duration_arg("18446744073709551615ms").unwrap(),
            StdDuration::from_millis(u64::MAX)
        );
    }
}
