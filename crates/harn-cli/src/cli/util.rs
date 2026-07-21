use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::time::Duration as StdDuration;

use clap::builder::{PossibleValue, StringValueParser, TypedValueParser};
use harn_vm::duration_parse::DurationParseError;

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
    harn_vm::duration_parse::parse_millis(raw)
        .map(StdDuration::from_millis)
        .map_err(|error| match error {
            DurationParseError::Empty => "duration cannot be empty".to_string(),
            DurationParseError::MissingUnit => {
                "duration must include a unit suffix like ms, s, m, h, d, or w".to_string()
            }
            DurationParseError::NoDigits => {
                "duration must be formatted like 30s, 5m, 2h, or 7d".to_string()
            }
            DurationParseError::AmountOverflow | DurationParseError::TooLarge => {
                format!("duration '{raw}' is too large")
            }
            DurationParseError::UnknownUnit(unit) => {
                format!("unsupported duration unit '{unit}'; expected ms, s, m, h, d, or w")
            }
        })
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

    #[test]
    fn parse_duration_arg_still_requires_a_unit_suffix() {
        // The CLI's distinguishing policy: unlike the config-file parsers, a
        // bare number is a typo here, not an implicit millisecond count.
        let err = parse_duration_arg("30").unwrap_err();
        assert!(err.contains("must include a unit suffix"), "{err}");
    }

    #[test]
    fn parse_duration_arg_spans_the_full_unit_vocabulary() {
        // Compare in seconds: `from_days`/`from_weeks` are unstable, and
        // `from_secs(7 * 86_400)` would trip a clippy unit lint.
        assert_eq!(parse_duration_arg("7d").unwrap().as_secs(), 7 * 86_400);
        assert_eq!(parse_duration_arg("2w").unwrap().as_secs(), 2 * 7 * 86_400);
    }

    #[test]
    fn parse_duration_arg_reports_unknown_units_and_digitless_input() {
        assert!(parse_duration_arg("5y")
            .unwrap_err()
            .contains("unsupported duration unit"));
        assert!(parse_duration_arg("abc")
            .unwrap_err()
            .contains("must be formatted like"));
        assert!(parse_duration_arg("  ")
            .unwrap_err()
            .contains("cannot be empty"));
    }
}
