//! Shared tokenizer and unit table for `<number><unit>` duration strings.
//!
//! Several subsystems accept human-written duration strings ("5m", "200ms",
//! "1h") with slightly different policies — the default unit when none is
//! written, which suffixes are allowed, whether zero is permitted, and the
//! return type. They share this tokenizer and the canonical
//! unit-to-milliseconds table so the *vocabulary* stays consistent and the
//! split-the-number-from-the-unit logic isn't re-implemented (and re-bugged)
//! in each one. Each caller keeps its own policy and error wording.
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

#[cfg(test)]
mod tests {
    use super::*;

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
