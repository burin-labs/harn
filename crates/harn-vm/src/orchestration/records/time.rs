//! Canonical parsing for timestamp formats found in persisted run records.

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub(super) fn parse_timestamp_ms(value: &str) -> Option<i128> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<i128>() {
        return Some(seconds.saturating_mul(1000));
    }
    if let Ok(uuid) = uuid::Uuid::parse_str(value) {
        let (seconds, nanos) = uuid.get_timestamp()?.to_unix();
        return Some(i128::from(seconds).saturating_mul(1000) + i128::from(nanos / 1_000_000));
    }
    let parsed = OffsetDateTime::parse(value, &Rfc3339).ok()?;
    Some(
        i128::from(parsed.unix_timestamp()).saturating_mul(1000) + i128::from(parsed.millisecond()),
    )
}

pub(super) fn timestamp_delta_ms(started_at: &str, finished_at: &str) -> Option<u64> {
    let start = parse_timestamp_ms(started_at)?;
    let end = parse_timestamp_ms(finished_at)?;
    u64::try_from(end.checked_sub(start)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_persisted_timestamp_format() {
        let worker_id = uuid::Uuid::now_v7().to_string();
        assert!(parse_timestamp_ms(&worker_id).is_some());
        assert_eq!(parse_timestamp_ms("1753000000"), Some(1_753_000_000_000));
        assert_eq!(
            parse_timestamp_ms("2026-08-02T10:00:01Z"),
            Some(1_785_664_801_000)
        );
    }

    #[test]
    fn rejects_negative_duration() {
        assert_eq!(timestamp_delta_ms("2", "1"), None);
    }
}
