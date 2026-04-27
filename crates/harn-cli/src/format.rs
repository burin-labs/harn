//! Shared, lightweight formatting helpers for CLI commands.
//!
//! Multiple subcommands had grown private copies of the same RFC3339
//! timestamp / human-friendly duration formatters; consolidating them
//! here keeps formatting consistent across the user-facing surface.

use std::time::Duration as StdDuration;

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Render a UTC instant as RFC3339, falling back to `Display` if formatting
/// fails (which `time` only does on internally inconsistent components).
pub(crate) fn format_timestamp_rfc3339(value: OffsetDateTime) -> String {
    value.format(&Rfc3339).unwrap_or_else(|_| value.to_string())
}

/// Render a unix-epoch millisecond timestamp as RFC3339.
pub(crate) fn format_unix_ms_rfc3339(ms: i64) -> String {
    let seconds = ms.div_euclid(1000);
    let value = OffsetDateTime::from_unix_timestamp(seconds).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    format_timestamp_rfc3339(value)
}

/// Render a `std::time::Duration` as a coarse "5s / 2m / 3h / 1d" suffix.
///
/// Rounds toward zero on the chosen unit, mirroring the original behavior
/// of the orchestrator command output.
pub(crate) fn format_duration_coarse(value: StdDuration) -> String {
    if value.as_secs() == 0 {
        return format!("{}ms", value.as_millis());
    }
    let seconds = value.as_secs();
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 60 * 60 {
        return format!("{}m", seconds / 60);
    }
    if seconds < 60 * 60 * 24 {
        return format!("{}h", seconds / (60 * 60));
    }
    format!("{}d", seconds / (60 * 60 * 24))
}

/// Render a millisecond duration with a single decimal point of precision
/// for the ">= 1s" cases (used by portal output).
pub(crate) fn format_duration_ms(duration_ms: u64) -> String {
    if duration_ms >= 60_000 {
        format!("{:.1}m", duration_ms as f64 / 60_000.0)
    } else if duration_ms >= 1_000 {
        format!("{:.1}s", duration_ms as f64 / 1_000.0)
    } else {
        format!("{duration_ms}ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_uses_rfc3339() {
        let value = OffsetDateTime::UNIX_EPOCH;
        assert_eq!(format_timestamp_rfc3339(value), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn unix_ms_rounds_to_seconds() {
        assert_eq!(format_unix_ms_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_unix_ms_rfc3339(1500), "1970-01-01T00:00:01Z");
        // Negative ms before the epoch should not panic.
        assert_eq!(format_unix_ms_rfc3339(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn coarse_duration_picks_a_unit() {
        assert_eq!(format_duration_coarse(StdDuration::from_millis(0)), "0ms");
        assert_eq!(format_duration_coarse(StdDuration::from_secs(5)), "5s");
        assert_eq!(format_duration_coarse(StdDuration::from_secs(120)), "2m");
        assert_eq!(format_duration_coarse(StdDuration::from_secs(7200)), "2h");
        assert_eq!(
            format_duration_coarse(StdDuration::from_secs(86_400 * 3)),
            "3d"
        );
    }

    #[test]
    fn ms_duration_uses_one_decimal_above_a_second() {
        assert_eq!(format_duration_ms(500), "500ms");
        assert_eq!(format_duration_ms(1_500), "1.5s");
        assert_eq!(format_duration_ms(90_000), "1.5m");
    }
}
