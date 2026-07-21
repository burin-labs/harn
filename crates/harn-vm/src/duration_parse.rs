//! The single duration grammar for `<number><unit>` strings.
//!
//! Every subsystem that accepts a human-written duration ("5m", "200ms",
//! "1h") parses it here, under one set of rules:
//!
//! * A unit suffix is **required**. `"30"` is an error, not an implicit
//!   millisecond count — a unitless number reads as seconds to most people
//!   and as milliseconds to most APIs, so guessing silently misreads it.
//! * The vocabulary is `ms`, `s`, `m`, `h`, `d`, `w`, matched
//!   case-insensitively, tolerating whitespace before the suffix.
//! * Arithmetic is **checked**. An oversized value is reported, never clamped:
//!   a timeout that silently becomes `u64::MAX` presents to the user as a hang,
//!   which is far harder to diagnose than a parse error.
//!
//! These rules were previously three forked copies that disagreed. Most
//! sharply, `when_budget.timeout` was parsed by *both* the CLI manifest
//! validator and the runtime `trigger_register`, which disagreed on overflow —
//! the same string was rejected at validation but accepted and clamped at
//! registration. One grammar removes that class of divergence.
//!
//! [`parse_millis`] owns the arithmetic and returns a structured
//! [`DurationParseError`]; each caller maps that onto its own error type and
//! wording, which stays the caller's business.
//!
//! The float/long-form cache-TTL parser (`llm::cache`) and the
//! `OptionsParser` millis path (`stdlib::options`, which rejects unit strings
//! outright) are deliberate outliers and do not use this module.

/// Split `raw` into its leading ASCII-digit run and the trimmed, lowercased
/// unit suffix. Returns `None` when `raw` is blank or has no digit prefix.
/// An all-digits input yields an empty unit string; the caller maps that to
/// its chosen default unit.
pub(crate) fn split_amount_unit(raw: &str) -> Option<(&str, String)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let split = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(trimmed.len());
    if split == 0 {
        return None; // no numeric prefix
    }
    Some((
        &trimmed[..split],
        trimmed[split..].trim().to_ascii_lowercase(),
    ))
}

/// Canonical milliseconds-per-unit for the duration vocabulary shared across
/// the codebase. `""` (no suffix) is intentionally excluded — callers decide
/// the default unit themselves. Returns `None` for an unknown unit.
pub(crate) fn unit_to_millis(unit: &str) -> Option<u64> {
    Some(match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 604_800_000,
        _ => return None,
    })
}

/// Why a duration string could not be interpreted.
///
/// Structured rather than stringly-typed so each caller can attach its own
/// wording — the messages are user-facing and differ per surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurationParseError {
    /// Input was empty or entirely whitespace.
    Empty,
    /// Input had no leading digit run (e.g. `"abc"`, `"-5s"`).
    NoDigits,
    /// Input was a bare number, which this grammar does not accept.
    MissingUnit,
    /// The digit run did not fit in `u64`.
    AmountOverflow,
    /// The suffix is not one of `ms`, `s`, `m`, `h`, `d`, `w`.
    UnknownUnit(String),
    /// The millisecond product did not fit in `u64`.
    TooLarge,
}

/// Parse a `<number><unit>` duration string into milliseconds.
///
/// See the module docs for the grammar. Units are matched case-insensitively
/// and tolerate whitespace before the suffix.
pub fn parse_millis(raw: &str) -> Result<u64, DurationParseError> {
    let Some((digits, unit)) = split_amount_unit(raw) else {
        return Err(if raw.trim().is_empty() {
            DurationParseError::Empty
        } else {
            DurationParseError::NoDigits
        });
    };
    if unit.is_empty() {
        return Err(DurationParseError::MissingUnit);
    }
    let amount: u64 = digits
        .parse()
        .map_err(|_| DurationParseError::AmountOverflow)?;
    let multiplier = unit_to_millis(&unit).ok_or(DurationParseError::UnknownUnit(unit))?;
    amount
        .checked_mul(multiplier)
        .ok_or(DurationParseError::TooLarge)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unit_suffix_is_required() {
        // Previously the two config-file parsers read this as 30ms while the
        // CLI rejected it. It is now uniformly an error.
        assert_eq!(parse_millis("30"), Err(DurationParseError::MissingUnit));
    }

    #[test]
    fn every_unit_through_weeks_is_accepted() {
        assert_eq!(parse_millis("200ms"), Ok(200));
        assert_eq!(parse_millis("5s"), Ok(5_000));
        assert_eq!(parse_millis("5m"), Ok(300_000));
        assert_eq!(parse_millis("2h"), Ok(7_200_000));
        assert_eq!(parse_millis("7d"), Ok(7 * 86_400_000));
        assert_eq!(parse_millis("2w"), Ok(2 * 604_800_000));
    }

    #[test]
    fn overflow_is_reported_never_clamped() {
        // `trigger_register` used to saturate this to u64::MAX, presenting to
        // the user as a hang rather than an error.
        assert_eq!(
            parse_millis("99999999999999999h"),
            Err(DurationParseError::TooLarge)
        );
        assert_eq!(
            parse_millis("99999999999999999999999s"),
            Err(DurationParseError::AmountOverflow)
        );
    }

    #[test]
    fn blank_digitless_and_unknown_units_are_distinguishable() {
        assert_eq!(parse_millis("  "), Err(DurationParseError::Empty));
        assert_eq!(parse_millis("abc"), Err(DurationParseError::NoDigits));
        assert_eq!(parse_millis("-5s"), Err(DurationParseError::NoDigits));
        assert_eq!(
            parse_millis("5y"),
            Err(DurationParseError::UnknownUnit("y".to_string()))
        );
    }

    #[test]
    fn units_are_case_insensitive_and_tolerate_internal_space() {
        assert_eq!(parse_millis("5 M"), Ok(300_000));
        assert_eq!(parse_millis("1H"), Ok(3_600_000));
    }

    #[test]
    fn splits_number_and_unit() {
        assert_eq!(split_amount_unit("5m"), Some(("5", "m".to_string())));
        assert_eq!(
            split_amount_unit("  200 ms "),
            Some(("200", "ms".to_string()))
        );
        assert_eq!(split_amount_unit("30"), Some(("30", String::new())));
        assert_eq!(split_amount_unit("1H"), Some(("1", "h".to_string())));
    }

    #[test]
    fn rejects_blank_and_unitless_prefix() {
        assert_eq!(split_amount_unit(""), None);
        assert_eq!(split_amount_unit("   "), None);
        assert_eq!(split_amount_unit("abc"), None);
    }

    #[test]
    fn unit_table_is_canonical() {
        assert_eq!(unit_to_millis("ms"), Some(1));
        assert_eq!(unit_to_millis("s"), Some(1_000));
        assert_eq!(unit_to_millis("m"), Some(60_000));
        assert_eq!(unit_to_millis("h"), Some(3_600_000));
        assert_eq!(unit_to_millis("d"), Some(86_400_000));
        assert_eq!(unit_to_millis("w"), Some(604_800_000));
        assert_eq!(unit_to_millis(""), None);
        assert_eq!(unit_to_millis("y"), None);
    }
}
