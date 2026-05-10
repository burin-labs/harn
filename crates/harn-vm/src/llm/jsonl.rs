//! JSONL fixture format for the CLI LLM-mock surface.
//!
//! Same format consumed by `harn run --llm-mock <path>` and
//! `harn test-bench --llm-fixture <path>`. Centralized here so both the
//! CLI and the testbench composition primitive parse identically.

use std::path::Path;

use crate::llm::mock::{LlmMock, MockError};
use crate::value::ErrorCategory;

/// Parse a JSONL fixture file into a vector of [`LlmMock`] entries.
/// Empty lines are skipped; every other line must be a JSON object.
pub fn load_llm_mocks_jsonl(path: &Path) -> Result<Vec<LlmMock>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut mocks = Vec::new();
    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
            format!(
                "invalid JSON in {} line {}: {error}",
                path.display(),
                line_no
            )
        })?;
        mocks.push(parse_llm_mock_value(&value).map_err(|error| {
            format!(
                "invalid LLM mock fixture in {} line {}: {error}",
                path.display(),
                line_no
            )
        })?);
    }
    Ok(mocks)
}

/// Parse a single JSON value into an [`LlmMock`]. Public so callers
/// that already have parsed JSON (e.g. inline test fixtures) can reuse
/// the same schema without re-encoding through a file.
pub fn parse_llm_mock_value(value: &serde_json::Value) -> Result<LlmMock, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "fixture line must be a JSON object".to_string())?;

    let match_pattern = optional_string_field(object, "match")?;
    let consume_on_match = object
        .get("consume_match")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let text = optional_string_field(object, "text")?.unwrap_or_default();
    let input_tokens = optional_i64_field(object, "input_tokens")?;
    let output_tokens = optional_i64_field(object, "output_tokens")?;
    let cache_read_tokens = optional_i64_field(object, "cache_read_tokens")?;
    let cache_write_tokens = optional_i64_field(object, "cache_write_tokens")?
        .or(optional_i64_field(object, "cache_creation_input_tokens")?);
    let thinking = optional_string_field(object, "thinking")?;
    let thinking_summary = optional_string_field(object, "thinking_summary")?;
    let stop_reason = optional_string_field(object, "stop_reason")?;
    let model = optional_string_field(object, "model")?.unwrap_or_else(|| "mock".to_string());
    let provider = optional_string_field(object, "provider")?;
    let blocks = optional_vec_field(object, "blocks")?;
    let logprobs = optional_vec_field(object, "logprobs")?.unwrap_or_default();
    let tool_calls = parse_llm_tool_calls(object.get("tool_calls"))?;
    let error = parse_llm_mock_error(object.get("error"))?;

    Ok(LlmMock {
        text,
        tool_calls,
        match_pattern,
        consume_on_match,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        thinking,
        thinking_summary,
        stop_reason,
        model,
        provider,
        blocks,
        logprobs,
        error,
    })
}

/// Serialize a recorded [`LlmMock`] back into a JSON object suitable for
/// JSONL emission.
pub fn serialize_llm_mock(mock: LlmMock) -> Result<String, String> {
    let mut object = serde_json::Map::new();
    if let Some(match_pattern) = mock.match_pattern {
        object.insert(
            "match".to_string(),
            serde_json::Value::String(match_pattern),
        );
    }
    if !mock.text.is_empty() {
        object.insert("text".to_string(), serde_json::Value::String(mock.text));
    }
    if !mock.tool_calls.is_empty() {
        let tool_calls = mock
            .tool_calls
            .into_iter()
            .map(|tool_call| {
                let object = tool_call
                    .as_object()
                    .ok_or_else(|| "recorded tool call must be an object".to_string())?;
                let name = object
                    .get("name")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| "recorded tool call is missing `name`".to_string())?;
                Ok(serde_json::json!({
                    "name": name,
                    "args": object
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({})),
                }))
            })
            .collect::<Result<Vec<_>, String>>()?;
        object.insert(
            "tool_calls".to_string(),
            serde_json::Value::Array(tool_calls),
        );
    }
    if let Some(input_tokens) = mock.input_tokens {
        object.insert(
            "input_tokens".to_string(),
            serde_json::Value::Number(input_tokens.into()),
        );
    }
    if let Some(output_tokens) = mock.output_tokens {
        object.insert(
            "output_tokens".to_string(),
            serde_json::Value::Number(output_tokens.into()),
        );
    }
    if let Some(cache_read_tokens) = mock.cache_read_tokens {
        object.insert(
            "cache_read_tokens".to_string(),
            serde_json::Value::Number(cache_read_tokens.into()),
        );
    }
    if let Some(cache_write_tokens) = mock.cache_write_tokens {
        object.insert(
            "cache_write_tokens".to_string(),
            serde_json::Value::Number(cache_write_tokens.into()),
        );
        object.insert(
            "cache_creation_input_tokens".to_string(),
            serde_json::Value::Number(cache_write_tokens.into()),
        );
    }
    if let Some(thinking) = mock.thinking {
        object.insert("thinking".to_string(), serde_json::Value::String(thinking));
    }
    if let Some(thinking_summary) = mock.thinking_summary {
        object.insert(
            "thinking_summary".to_string(),
            serde_json::Value::String(thinking_summary),
        );
    }
    if let Some(stop_reason) = mock.stop_reason {
        object.insert(
            "stop_reason".to_string(),
            serde_json::Value::String(stop_reason),
        );
    }
    object.insert("model".to_string(), serde_json::Value::String(mock.model));
    if let Some(provider) = mock.provider {
        object.insert("provider".to_string(), serde_json::Value::String(provider));
    }
    if let Some(blocks) = mock.blocks {
        object.insert("blocks".to_string(), serde_json::Value::Array(blocks));
    }
    if !mock.logprobs.is_empty() {
        object.insert(
            "logprobs".to_string(),
            serde_json::Value::Array(mock.logprobs),
        );
    }
    if let Some(error) = mock.error {
        object.insert(
            "error".to_string(),
            serde_json::json!({
                "category": error.category.as_str(),
                "message": error.message,
            }),
        );
    }
    serde_json::to_string(&serde_json::Value::Object(object))
        .map_err(|error| format!("failed to serialize recorded fixture: {error}"))
}

fn parse_llm_tool_calls(
    value: Option<&serde_json::Value>,
) -> Result<Vec<serde_json::Value>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| "tool_calls must be an array".to_string())?;
    items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            normalize_llm_tool_call(item).map_err(|error| format!("tool_calls[{idx}] {error}"))
        })
        .collect()
}

fn normalize_llm_tool_call(value: &serde_json::Value) -> Result<serde_json::Value, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "must be a JSON object".to_string())?;
    let name = object
        .get("name")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "is missing string field `name`".to_string())?;
    let arguments = object
        .get("arguments")
        .cloned()
        .or_else(|| object.get("args").cloned())
        .unwrap_or_else(|| serde_json::json!({}));
    Ok(serde_json::json!({
        "name": name,
        "arguments": arguments,
    }))
}

fn parse_llm_mock_error(value: Option<&serde_json::Value>) -> Result<Option<MockError>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let object = value.as_object().ok_or_else(|| {
        "error must be an object {category, message, retry_after_ms?}".to_string()
    })?;
    let category_str = object
        .get("category")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "error.category is required".to_string())?;
    let category = ErrorCategory::parse(category_str);
    if category.as_str() != category_str {
        return Err(format!("unknown error category `{category_str}`"));
    }
    let message = object
        .get("message")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let retry_after_ms = match object.get("retry_after_ms") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Number(n)) => match n.as_u64() {
            Some(v) => Some(v),
            None => return Err("error.retry_after_ms must be a non-negative integer".to_string()),
        },
        Some(_) => return Err("error.retry_after_ms must be a non-negative integer".to_string()),
    };
    Ok(Some(MockError {
        category,
        message,
        retry_after_ms,
    }))
}

fn optional_string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn optional_i64_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<i64>, String> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be an integer")),
    }
}

fn optional_vec_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<Vec<serde_json::Value>>, String> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Array(items)) => Ok(Some(items.clone())),
        Some(_) => Err(format!("`{key}` must be an array")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_text_and_tool_calls() {
        let mock = parse_llm_mock_value(&serde_json::json!({
            "text": "hello",
            "model": "mock",
            "tool_calls": [
                { "name": "search", "args": { "q": "harn" } }
            ]
        }))
        .expect("parse");
        let line = serialize_llm_mock(mock).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&line).expect("reparse");
        let reparsed = parse_llm_mock_value(&value).expect("reparse mock");
        assert_eq!(reparsed.text, "hello");
        assert_eq!(reparsed.tool_calls.len(), 1);
        assert_eq!(reparsed.tool_calls[0]["name"].as_str(), Some("search"));
    }

    #[test]
    fn parse_rejects_unknown_error_category() {
        let result = parse_llm_mock_value(&serde_json::json!({
            "error": { "category": "wibble", "message": "x" }
        }));
        match result {
            Err(err) => assert!(err.contains("unknown error category"), "{err}"),
            Ok(_) => panic!("expected parse failure for unknown error category"),
        }
    }
}
