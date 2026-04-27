use std::collections::{BTreeMap, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

use super::dto::{PortalSpan, PortalStage};

pub(super) fn system_time_ms(time: SystemTime) -> Option<u128> {
    time.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis())
}

pub(super) fn portal_timestamp_id(prefix: &str) -> String {
    let millis = system_time_ms(SystemTime::now()).unwrap_or_default();
    format!("{prefix}-{millis}")
}

pub(super) fn date_ms(value: &str) -> Option<u64> {
    let parsed =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()?;
    Some(parsed.unix_timestamp_nanos() as u64 / 1_000_000)
}

pub(super) fn compact_metadata(metadata: &BTreeMap<String, serde_json::Value>) -> String {
    if metadata.is_empty() {
        return "No extra metadata".to_string();
    }
    let sample = metadata
        .iter()
        .take(3)
        .map(|(key, value)| format!("{key}={}", compact_json(value)))
        .collect::<Vec<_>>()
        .join(" • ");
    if metadata.len() > 3 {
        format!("{sample} • +{} more", metadata.len() - 3)
    } else {
        sample
    }
}

pub(super) fn compact_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => value.to_string(),
        serde_json::Value::Array(values) => format!("{} items", values.len()),
        serde_json::Value::Object(values) => format!("{} fields", values.len()),
    }
}

pub(super) fn pretty_json(value: &serde_json::Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_string();
    }
    serde_json::to_string_pretty(value).unwrap_or_else(|_| compact_json(value))
}

pub(super) fn metadata_pretty_json(
    metadata: &BTreeMap<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    metadata.get(key).map(pretty_json)
}

pub(super) fn metadata_string(
    metadata: &BTreeMap<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    metadata
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

pub(super) fn string_array_value(
    metadata: &BTreeMap<String, serde_json::Value>,
    key: &str,
) -> Vec<String> {
    metadata
        .get(key)
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn span_kind_totals(spans: &[PortalSpan]) -> Vec<(String, u64)> {
    let mut totals = HashMap::<String, u64>::new();
    for span in spans {
        *totals.entry(span.kind.clone()).or_default() += span.duration_ms;
    }
    let mut values = totals.into_iter().collect::<Vec<_>>();
    values.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    values
}

pub(super) fn humanize_kind(kind: &str) -> String {
    kind.replace('_', " ")
}

pub(super) fn owning_stage<'a>(
    span: &PortalSpan,
    stages: &'a [PortalStage],
) -> Option<&'a PortalStage> {
    let offsets = stages
        .iter()
        .filter_map(|stage| stage.duration_ms.map(|duration| (stage, duration)));
    let mut cursor = 0u64;
    for (stage, duration) in offsets {
        let start = cursor;
        let end = cursor + duration;
        if span.start_ms >= start && span.end_ms <= end {
            return Some(stage);
        }
        cursor = end;
    }
    None
}

pub(super) fn preview_text(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n");
    let line = normalized
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.len() > 180 {
        format!("{}...", &line[..180])
    } else {
        line.to_string()
    }
}

pub(super) use crate::format::format_duration_ms as format_duration;

pub(super) fn is_completed_status(status: &str) -> bool {
    matches!(status, "complete" | "completed" | "success" | "verified")
}

pub(super) fn is_failed_status(status: &str) -> bool {
    matches!(status, "failed" | "error" | "cancelled")
}
