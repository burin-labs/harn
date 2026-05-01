use std::time::Duration as StdDuration;

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
        "m" => Ok(StdDuration::from_secs(value.saturating_mul(60))),
        "h" => Ok(StdDuration::from_secs(value.saturating_mul(60 * 60))),
        "d" => Ok(StdDuration::from_secs(value.saturating_mul(60 * 60 * 24))),
        "w" => Ok(StdDuration::from_secs(
            value.saturating_mul(60 * 60 * 24 * 7),
        )),
        _ => Err(format!(
            "unsupported duration unit '{unit}'; expected ms, s, m, h, d, or w"
        )),
    }
}
