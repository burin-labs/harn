use std::collections::BTreeMap;

use serde_json::Value as JsonValue;
use time::OffsetDateTime;

pub(super) fn parse_json_i64ish(value: &JsonValue) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|raw| i64::try_from(raw).ok()))
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<i64>().ok()))
}

pub(super) fn parse_string_array(value: &JsonValue) -> Vec<String> {
    let Some(array) = value.as_array() else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|entry| {
            entry.as_str().map(ToString::to_string).or_else(|| {
                entry
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string)
            })
        })
        .collect()
}

pub(super) fn header_value<'a>(
    headers: &'a BTreeMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

pub(super) fn json_stringish(raw: &JsonValue, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| {
        let value = raw.get(*field)?;
        value
            .as_str()
            .map(ToString::to_string)
            .or_else(|| parse_json_i64ish(value).map(|number| number.to_string()))
            .or_else(|| value.as_u64().map(|number| number.to_string()))
    })
}

pub(super) fn parse_rfc3339(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()
}
