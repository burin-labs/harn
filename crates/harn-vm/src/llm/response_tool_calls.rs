//! Canonical parsed-tool-call projection for provider response events.
//!
//! Native calls used to be serialized twice under `tool_calls` and
//! `parsed_tool_calls`. This module owns the compact representation and the
//! compatibility read path so consumers never infer omission semantics.

use std::fmt;

use serde_json::Value;

pub const NATIVE_TOOL_CALLS_REF: &str = "tool_calls";

/// Build the observability-only merged view without mutating request history.
pub(crate) fn merged_from_result(result: &crate::llm::api::LlmResult) -> Vec<serde_json::Value> {
    if !result.tool_calls.is_empty() {
        return result.tool_calls.clone();
    }
    result
        .text_projection
        .as_deref()
        .map(|projection| projection.merged_tool_calls(result))
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolveError {
    /// A row claimed both inline and referenced representations.
    ConflictingRepresentations,
    /// The historical inline field was present with a non-array value.
    InlineProjectionIsNotArray,
    /// The reference field was present with a non-string value.
    ReferenceIsNotString,
    /// The row named a projection source this Harn version does not support.
    UnsupportedReference(String),
    /// The native target named by the reference was absent or not an array.
    ReferencedNativeProjectionIsNotArray,
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConflictingRepresentations => formatter.write_str(
                "provider response carries both `parsed_tool_calls` and `parsed_tool_calls_ref`",
            ),
            Self::InlineProjectionIsNotArray => {
                formatter.write_str("`parsed_tool_calls` must be an array")
            }
            Self::ReferenceIsNotString => {
                formatter.write_str("`parsed_tool_calls_ref` must be a string")
            }
            Self::UnsupportedReference(reference) => write!(
                formatter,
                "unsupported `parsed_tool_calls_ref` value `{reference}`"
            ),
            Self::ReferencedNativeProjectionIsNotArray => formatter
                .write_str("`parsed_tool_calls_ref: tool_calls` requires a `tool_calls` array"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Project one canonical parsed-call view onto a response event.
///
/// Equality is structural JSON equality after provider normalization. The
/// explicit alias distinguishes deduplication from old or partial rows where
/// the parsed projection is simply absent.
pub(crate) fn project_onto_response(
    event: &mut Value,
    native_tool_calls: &[Value],
    parsed_tool_calls: Vec<Value>,
) {
    let Some(object) = event.as_object_mut() else {
        return;
    };
    // An empty inline array is smaller than the reference field. Native tool
    // calls are object records, so every non-empty equal projection benefits
    // from the alias while no-tool terminal responses keep the compact form.
    if !parsed_tool_calls.is_empty() && parsed_tool_calls == native_tool_calls {
        object.insert(
            "parsed_tool_calls_ref".to_string(),
            Value::String(NATIVE_TOOL_CALLS_REF.to_string()),
        );
    } else {
        object.insert(
            "parsed_tool_calls".to_string(),
            Value::Array(parsed_tool_calls),
        );
    }
}

/// Resolve the canonical parsed-call view from current and historical rows.
///
/// `Ok(None)` means the row made no parsed-view claim. Callers may retain
/// their historical fallback to the native array. Malformed or conflicting
/// new representations fail closed instead of being mistaken for absence.
pub fn resolve(event: &Value) -> Result<Option<&[Value]>, ResolveError> {
    let inline = event.get("parsed_tool_calls");
    let reference = event.get("parsed_tool_calls_ref");
    if inline.is_some() && reference.is_some() {
        return Err(ResolveError::ConflictingRepresentations);
    }
    if let Some(inline) = inline {
        return inline
            .as_array()
            .map(|calls| Some(calls.as_slice()))
            .ok_or(ResolveError::InlineProjectionIsNotArray);
    }
    let Some(reference) = reference else {
        return Ok(None);
    };
    let reference = reference
        .as_str()
        .ok_or(ResolveError::ReferenceIsNotString)?;
    if reference != NATIVE_TOOL_CALLS_REF {
        return Err(ResolveError::UnsupportedReference(reference.to_string()));
    }
    event
        .get(NATIVE_TOOL_CALLS_REF)
        .and_then(Value::as_array)
        .map(|calls| Some(calls.as_slice()))
        .ok_or(ResolveError::ReferencedNativeProjectionIsNotArray)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_native_projection_uses_an_explicit_alias() {
        let calls = vec![serde_json::json!({"name": "look", "arguments": {"path": "x"}})];
        let mut event = serde_json::json!({"tool_calls": calls});

        project_onto_response(&mut event, &calls, calls.clone());

        assert_eq!(event["parsed_tool_calls_ref"], NATIVE_TOOL_CALLS_REF);
        assert!(event.get("parsed_tool_calls").is_none());
        assert_eq!(resolve(&event).unwrap(), Some(calls.as_slice()));
    }

    #[test]
    fn alias_serializes_large_arguments_once() {
        let calls = vec![serde_json::json!({
            "name": "edit",
            "arguments": {"path": "src/main.rs", "content": "x".repeat(16 * 1024)},
        })];
        let legacy = serde_json::json!({
            "tool_calls": calls,
            "parsed_tool_calls": calls,
        });
        let mut compact = serde_json::json!({"tool_calls": calls});

        project_onto_response(&mut compact, &calls, calls.clone());

        let legacy_bytes = serde_json::to_vec(&legacy).unwrap().len();
        let compact_bytes = serde_json::to_vec(&compact).unwrap().len();
        assert!(
            legacy_bytes - compact_bytes > 16 * 1024,
            "the alias must eliminate one materialized content body: {legacy_bytes} -> {compact_bytes}"
        );
        assert_eq!(resolve(&compact).unwrap(), Some(calls.as_slice()));
    }

    #[test]
    fn distinct_projection_stays_inline() {
        let native = vec![serde_json::json!({"name": "native"})];
        let parsed = vec![
            serde_json::json!({"name": "native"}),
            serde_json::json!({"name": "text"}),
        ];
        let mut event = serde_json::json!({"tool_calls": native});

        project_onto_response(&mut event, &native, parsed.clone());

        assert_eq!(event["parsed_tool_calls"], Value::Array(parsed.clone()));
        assert!(event.get("parsed_tool_calls_ref").is_none());
        assert_eq!(resolve(&event).unwrap(), Some(parsed.as_slice()));
    }

    #[test]
    fn empty_projection_stays_inline_because_it_is_smaller() {
        let mut event = serde_json::json!({"tool_calls": []});

        project_onto_response(&mut event, &[], Vec::new());

        assert_eq!(event["parsed_tool_calls"], serde_json::json!([]));
        assert!(event.get("parsed_tool_calls_ref").is_none());
        assert_eq!(resolve(&event).unwrap(), Some(&[] as &[Value]));
    }

    #[test]
    fn historical_inline_and_absent_rows_remain_distinct() {
        let inline = serde_json::json!({
            "tool_calls": [],
            "parsed_tool_calls": [{"name": "run"}],
        });
        let absent = serde_json::json!({"tool_calls": [{"name": "look"}]});

        assert_eq!(resolve(&inline).unwrap().unwrap()[0]["name"], "run");
        assert_eq!(resolve(&absent).unwrap(), None);
    }

    #[test]
    fn malformed_or_conflicting_aliases_fail_closed() {
        let conflicting = serde_json::json!({
            "tool_calls": [],
            "parsed_tool_calls": [],
            "parsed_tool_calls_ref": "tool_calls",
        });
        let unknown = serde_json::json!({
            "tool_calls": [],
            "parsed_tool_calls_ref": "raw_tool_calls",
        });

        assert_eq!(
            resolve(&conflicting),
            Err(ResolveError::ConflictingRepresentations)
        );
        assert_eq!(
            resolve(&unknown),
            Err(ResolveError::UnsupportedReference(
                "raw_tool_calls".to_string()
            ))
        );
    }
}
