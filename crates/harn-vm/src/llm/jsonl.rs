//! JSONL fixture format for the CLI LLM-mock surface.
//!
//! Same format consumed by `harn run --llm-mock <path>` and
//! `harn test-bench --llm-fixture <path>`. Centralized here so both the
//! CLI and the testbench composition primitive parse identically.

use std::path::Path;

use serde::Deserialize;

use crate::llm::api::RawProviderToolCall;
use crate::llm::mock::{self, LlmMock, LlmMockFixture, MockError, DEFAULT_MOCK_SCOPE};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureHeaderV1 {
    schema_version: u32,
    strict_scopes: bool,
}

/// Parse a JSONL fixture file through the text-level contract owner.
pub fn load_llm_mocks_jsonl(path: &Path) -> Result<LlmMockFixture, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    parse_llm_mocks_jsonl(&content)
        .map_err(|error| format!("invalid LLM mock fixture in {}: {error}", path.display()))
}

/// Parse a JSONL fixture document into a versioned [`LlmMockFixture`].
///
/// This is the sole document decoder used by file-backed CLI fixtures and the
/// in-process builtin. The first non-empty line may be a header carrying
/// `schemaVersion` and `strictScopes`. Headerless input is v0: one default
/// scope with the frozen FIFO/glob behavior. The whole document is decoded
/// before callers install it, so a malformed tail can never partially replace
/// an active fixture store.
pub fn parse_llm_mocks_jsonl(text: &str) -> Result<LlmMockFixture, String> {
    let mut fixture = LlmMockFixture::default();
    let mut header_slot_seen = false;
    let mut entry_index = 0usize;
    let mut v1_entry_ids = std::collections::BTreeSet::new();
    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("invalid JSON line {line_no}: {error}"))?;
        let header = parse_fixture_header(&value)
            .map_err(|error| format!("invalid fixture header line {line_no}: {error}"))?;
        // Only the first non-empty line is eligible to be a version header.
        if !header_slot_seen {
            header_slot_seen = true;
            if let Some((schema_version, strict_scopes)) = header {
                fixture.schema_version = schema_version;
                fixture.strict_scopes = strict_scopes;
                continue;
            }
        } else if header.is_some() {
            return Err(format!(
                "fixture header must be the first non-empty line (line {line_no})"
            ));
        }
        let mock = parse_llm_mock_value_versioned(&value, fixture.schema_version, entry_index)
            .map_err(|error| format!("invalid fixture entry line {line_no}: {error}"))?;
        if fixture.schema_version > 0 && !v1_entry_ids.insert(mock.entry_id.clone()) {
            return Err(format!(
                "duplicate fixture entry id {:?} at line {line_no}",
                mock.entry_id
            ));
        }
        entry_index += 1;
        fixture.mocks.push(mock);
    }
    Ok(fixture)
}

/// Detect and validate a contract header. Returns `Ok(None)` when the line is
/// an ordinary v0 entry (no `schemaVersion` key) and `Ok(Some((version,
/// strict)))` for a valid v1 header. V1 headers are structurally closed and
/// require `strictScopes`, so spelling errors never manufacture defaults.
fn parse_fixture_header(value: &serde_json::Value) -> Result<Option<(u32, bool)>, String> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if !object.contains_key("schemaVersion") {
        return Ok(None);
    }
    let header: FixtureHeaderV1 = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid v1 header: {error}"))?;
    let schema_version = header.schema_version;
    if schema_version == 0 || schema_version > mock::MAX_MOCK_SCHEMA_VERSION {
        return Err(format!(
            "unsupported schemaVersion {schema_version}; this build supports 1..={}",
            mock::MAX_MOCK_SCHEMA_VERSION
        ));
    }
    Ok(Some((schema_version, header.strict_scopes)))
}

/// Parse a single JSON value into an [`LlmMock`] using the v0 contract. Public
/// so callers that already have parsed JSON (e.g. inline test fixtures) can
/// reuse the same schema without re-encoding through a file.
pub fn parse_llm_mock_value(value: &serde_json::Value) -> Result<LlmMock, String> {
    parse_llm_mock_value_versioned(value, 0, 0)
}

/// Parse a single JSON value into an [`LlmMock`] under a specific contract
/// version. `entry_index` seeds only legacy v0 identity.
///
/// Under v0 the entry is pinned to the [`DEFAULT_MOCK_SCOPE`] and its
/// consumption is derived exactly as before (reusable glob unless
/// `consume_match`; FIFO entries always consumed), so v0 files replay
/// byte-identically. V1 is structurally closed and requires authored `scope`,
/// `consume`, and `id` fields.
pub fn parse_llm_mock_value_versioned(
    value: &serde_json::Value,
    schema_version: u32,
    entry_index: usize,
) -> Result<LlmMock, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "fixture line must be a JSON object".to_string())?;
    if schema_version > 0 {
        validate_v1_entry(object)?;
    }

    let match_pattern = optional_string_field(object, "match")?;
    let consume_match = object
        .get("consume_match")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let (scope, entry_id, sticky) = resolve_scope_consume(
        object,
        schema_version,
        entry_index,
        match_pattern.is_some(),
        consume_match,
    )?;
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
    let raw_tool_calls = parse_raw_provider_tool_calls(object.get("raw_tool_calls"))?;
    let error = parse_llm_mock_error(object.get("error"))?;
    let stream_chunks = match object.get("stream_chunks") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(|s| s.to_string())
                    .ok_or_else(|| "stream_chunks entries must be strings".to_string())
            })
            .collect::<Result<Vec<_>, String>>()?,
        Some(_) => return Err("stream_chunks must be an array of strings".to_string()),
    };

    Ok(LlmMock {
        text,
        tool_calls,
        raw_tool_calls,
        match_pattern,
        scope,
        entry_id,
        sticky,
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
        stream_chunks,
    })
}

/// Serialize one legacy v0 [`LlmMock`] entry.
///
/// New fixture documents must use [`serialize_llm_mock_fixture`] so every
/// versioned entry carries explicit identity, scope, and consumption mode.
pub fn serialize_llm_mock(mock: LlmMock) -> Result<String, String> {
    serde_json::to_string(&serialize_llm_mock_value(mock, None)?)
        .map_err(|error| format!("failed to serialize recorded fixture: {error}"))
}

/// Serialize the canonical v1 fixture document emitted by CLI record mode.
///
/// Entry IDs derive from their deterministic document position, not provider
/// response IDs, so a recorded fixture is portable and self-contained.
pub fn serialize_llm_mock_fixture(mocks: Vec<LlmMock>) -> Result<String, String> {
    let mut lines = vec![serde_json::to_string(&serde_json::json!({
        "schemaVersion": 1,
        "strictScopes": false,
    }))
    .map_err(|error| format!("failed to serialize fixture header: {error}"))?];
    for (entry_index, mock) in mocks.into_iter().enumerate() {
        let entry = serialize_llm_mock_value(mock, Some(format!("record-{entry_index}")))?;
        lines.push(
            serde_json::to_string(&entry)
                .map_err(|error| format!("failed to serialize fixture entry: {error}"))?,
        );
    }
    Ok(format!("{}\n", lines.join("\n")))
}

fn serialize_llm_mock_value(
    mock: LlmMock,
    v1_entry_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let versioned = v1_entry_id.is_some();
    let mut object = serde_json::Map::new();
    if let Some(match_pattern) = mock.match_pattern {
        object.insert(
            "match".to_string(),
            serde_json::Value::String(match_pattern),
        );
    }
    if let Some(entry_id) = v1_entry_id {
        object.insert("id".to_string(), serde_json::Value::String(entry_id));
        object.insert("scope".to_string(), serde_json::Value::String(mock.scope));
        object.insert(
            "consume".to_string(),
            serde_json::Value::String(if mock.sticky { "sticky" } else { "once" }.to_string()),
        );
    } else {
        // Preserve byte-compatible v0 serialization for direct legacy callers.
        if mock.scope != DEFAULT_MOCK_SCOPE {
            object.insert("scope".to_string(), serde_json::Value::String(mock.scope));
        }
        if mock.sticky {
            object.insert(
                "consume".to_string(),
                serde_json::Value::String("sticky".to_string()),
            );
        }
    }
    if !mock.text.is_empty() {
        object.insert("text".to_string(), serde_json::Value::String(mock.text));
    }
    if !mock.stream_chunks.is_empty() {
        object.insert(
            "stream_chunks".to_string(),
            serde_json::Value::Array(
                mock.stream_chunks
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    if !mock.tool_calls.is_empty() {
        let tool_calls = mock
            .tool_calls
            .into_iter()
            .map(|tool_call| serialize_llm_mock_tool_call(tool_call, versioned))
            .collect::<Result<Vec<_>, String>>()?;
        object.insert(
            "tool_calls".to_string(),
            serde_json::Value::Array(tool_calls),
        );
    }
    if !mock.raw_tool_calls.is_empty() {
        object.insert(
            "raw_tool_calls".to_string(),
            serde_json::Value::Array(
                mock.raw_tool_calls
                    .into_iter()
                    .map(RawProviderToolCall::into_value)
                    .collect(),
            ),
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
        if !versioned {
            object.insert(
                "cache_creation_input_tokens".to_string(),
                serde_json::Value::Number(cache_write_tokens.into()),
            );
        }
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
        let mut error_object = serde_json::Map::new();
        error_object.insert(
            "category".to_string(),
            serde_json::Value::String(error.category.as_str().to_string()),
        );
        if !error.message.is_empty() {
            error_object.insert(
                "message".to_string(),
                serde_json::Value::String(error.message),
            );
        }
        if let Some(status) = error.status {
            error_object.insert(
                "status".to_string(),
                serde_json::Value::Number(status.into()),
            );
        }
        if let Some(kind) = error.kind {
            error_object.insert("kind".to_string(), serde_json::Value::String(kind));
        }
        if let Some(reason) = error.reason {
            error_object.insert("reason".to_string(), serde_json::Value::String(reason));
        }
        if let Some(retry_after_ms) = error.retry_after_ms {
            error_object.insert(
                "retry_after_ms".to_string(),
                serde_json::Value::Number(retry_after_ms.into()),
            );
        }
        object.insert("error".to_string(), serde_json::Value::Object(error_object));
    }
    Ok(serde_json::Value::Object(object))
}

fn serialize_llm_mock_tool_call(
    tool_call: serde_json::Value,
    versioned: bool,
) -> Result<serde_json::Value, String> {
    let object = tool_call
        .as_object()
        .ok_or_else(|| "recorded tool call must be an object".to_string())?;
    let name = object
        .get("name")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "recorded tool call is missing `name`".to_string())?;
    let arguments = object
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if !versioned {
        // Preserve the legacy v0 serialization byte shape.
        return Ok(serde_json::json!({"name": name, "args": arguments}));
    }

    let mut serialized = serde_json::Map::new();
    for field in ["id", "type"] {
        if let Some(value) = object.get(field) {
            serialized.insert(field.to_string(), value.clone());
        }
    }
    serialized.insert(
        "name".to_string(),
        serde_json::Value::String(name.to_string()),
    );
    serialized.insert("arguments".to_string(), arguments);
    if let Some(provider_metadata) = serialize_tool_call_provider_metadata(object)? {
        serialized.insert("provider_metadata".to_string(), provider_metadata);
    }
    Ok(serde_json::Value::Object(serialized))
}

fn serialize_tool_call_provider_metadata(
    tool_call: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<serde_json::Value>, String> {
    let mut metadata = match tool_call.get("provider_metadata") {
        None => serde_json::Map::new(),
        Some(serde_json::Value::Object(metadata)) => metadata.clone(),
        Some(_) => return Err("recorded provider_metadata must be an object".to_string()),
    };
    let call = serde_json::Value::Object(tool_call.clone());
    if let Some(signature) = crate::llm::providers::gemini_tool_call_thought_signature(&call) {
        let gemini = metadata
            .entry("gemini".to_string())
            .or_insert_with(|| serde_json::json!({}));
        let gemini = gemini
            .as_object_mut()
            .ok_or_else(|| "recorded provider_metadata.gemini must be an object".to_string())?;
        gemini
            .entry("thought_signature".to_string())
            .or_insert_with(|| serde_json::Value::String(signature.to_string()));
    }
    Ok((!metadata.is_empty()).then_some(serde_json::Value::Object(metadata)))
}

/// Resolve `(scope, entry_id, sticky)` for an entry under the given contract
/// version. Under v0 the scope is forced to the default and the sticky policy
/// is derived from the legacy `match`/`consume_match` shape; under v1 the
/// required `scope`, `id`, and `consume` fields are honored.
fn resolve_scope_consume(
    object: &serde_json::Map<String, serde_json::Value>,
    schema_version: u32,
    entry_index: usize,
    has_match: bool,
    consume_match: bool,
) -> Result<(String, String, bool), String> {
    if schema_version == 0 {
        // Legacy: reusable glob unless `consume_match`; FIFO always consumed.
        let sticky = has_match && !consume_match;
        return Ok((
            DEFAULT_MOCK_SCOPE.to_string(),
            entry_index.to_string(),
            sticky,
        ));
    }
    let scope = required_v1_string_field(object, "scope")?;
    let entry_id = required_v1_string_field(object, "id")?;
    let sticky = match object.get("consume") {
        None => return Err("`consume` is required for v1 fixture entries".to_string()),
        Some(serde_json::Value::String(mode)) => match mode.as_str() {
            "once" => false,
            "sticky" => true,
            other => {
                return Err(format!(
                    "`consume` must be \"once\" or \"sticky\", got {other:?}"
                ))
            }
        },
        Some(_) => return Err("`consume` must be a string \"once\" or \"sticky\"".to_string()),
    };
    Ok((scope, entry_id, sticky))
}

/// Read a required v1 identifier-like field. Explicit identity prevents a
/// versioned fixture from silently changing meaning when rows are reordered.
fn required_v1_string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<String, String> {
    match object.get(key) {
        None => Err(format!("`{key}` is required for v1 fixture entries")),
        Some(serde_json::Value::String(value)) if value.is_empty() || value.trim() != value => {
            Err(format!("`{key}` must be a non-empty trimmed string"))
        }
        Some(serde_json::Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(format!("`{key}` must be a non-empty trimmed string")),
    }
}

const V1_ENTRY_FIELDS: &[&str] = &[
    "id",
    "scope",
    "consume",
    "match",
    "text",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "thinking",
    "thinking_summary",
    "stop_reason",
    "model",
    "provider",
    "blocks",
    "logprobs",
    "tool_calls",
    "raw_tool_calls",
    "error",
    "stream_chunks",
];

const V1_TOOL_CALL_FIELDS: &[&str] = &["id", "type", "name", "arguments", "provider_metadata"];
const V1_ERROR_FIELDS: &[&str] = &[
    "category",
    "message",
    "status",
    "kind",
    "reason",
    "retry_after_ms",
];

/// Validate the closed v1 authoring surface before reusing the frozen parser.
/// Nested provider-native response blocks remain opaque by design; the fixture
/// contract owns its envelope and normalized tool-call shape, not any one
/// provider's raw response schema.
fn validate_v1_entry(object: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    validate_v1_object_fields(object, "fixture entry", V1_ENTRY_FIELDS)?;
    validate_v1_tool_calls(object.get("tool_calls"))?;
    if let Some(error) = object.get("error") {
        let error = error
            .as_object()
            .ok_or_else(|| "v1 `error` must be an object when present".to_string())?;
        validate_v1_object_fields(error, "fixture error", V1_ERROR_FIELDS)?;
    }
    Ok(())
}

fn validate_v1_tool_calls(value: Option<&serde_json::Value>) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    let items = value
        .as_array()
        .ok_or_else(|| "tool_calls must be an array".to_string())?;
    for (index, item) in items.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| format!("tool_calls[{index}] must be a JSON object"))?;
        validate_v1_object_fields(object, &format!("tool_calls[{index}]"), V1_TOOL_CALL_FIELDS)?;
        if !object.contains_key("arguments") {
            return Err(format!(
                "tool_calls[{index}] requires canonical `arguments`; legacy `args` is v0-only"
            ));
        }
        for field in ["id", "type"] {
            if object.get(field).is_some_and(|value| !value.is_string()) {
                return Err(format!("tool_calls[{index}].{field} must be a string"));
            }
        }
        if object
            .get("provider_metadata")
            .is_some_and(|value| !value.is_object())
        {
            return Err(format!(
                "tool_calls[{index}].provider_metadata must be an object"
            ));
        }
    }
    Ok(())
}

fn validate_v1_object_fields(
    object: &serde_json::Map<String, serde_json::Value>,
    label: &str,
    allowed_fields: &[&str],
) -> Result<(), String> {
    for key in object.keys() {
        if !allowed_fields.contains(&key.as_str()) {
            return Err(format!("unknown {label} field `{key}`"));
        }
    }
    Ok(())
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

fn parse_raw_provider_tool_calls(
    value: Option<&serde_json::Value>,
) -> Result<Vec<RawProviderToolCall>, String> {
    match value {
        None => Ok(Vec::new()),
        Some(value) => RawProviderToolCall::array_from_value(value),
    }
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

    // V0 inline fixtures historically carried provider-specific normalized
    // metadata (for example Gemini's `thought_signature`) alongside the
    // portable call fields. Canonicalize only the legacy arguments alias; v1
    // validation owns the closed authoring surface before this function runs.
    let mut normalized = object.clone();
    normalized.insert(
        "name".to_string(),
        serde_json::Value::String(name.to_string()),
    );
    normalized.insert("arguments".to_string(), arguments);
    normalized.remove("args");
    Ok(serde_json::Value::Object(normalized))
}

fn parse_llm_mock_error(value: Option<&serde_json::Value>) -> Result<Option<MockError>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let object = value.as_object().ok_or_else(|| {
        "error must be an object {category?, message?, status?, kind?, reason?, retry_after_ms?}"
            .to_string()
    })?;
    let category = object
        .get("category")
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "error.category must be a string".to_string())
        })
        .transpose()?;
    let message = object
        .get("message")
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "error.message must be a string".to_string())
        })
        .transpose()?;
    let status = match object.get("status") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Number(n)) => match n.as_i64() {
            Some(v) => Some(mock::validate_mock_error_status(v)?),
            None => return Err("error.status must be an HTTP status code".to_string()),
        },
        Some(_) => return Err("error.status must be an HTTP status code".to_string()),
    };
    let kind = object
        .get("kind")
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "error.kind must be a string".to_string())
        })
        .transpose()?;
    let reason = object
        .get("reason")
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "error.reason must be a string".to_string())
        })
        .transpose()?;
    let retry_after_ms = match object.get("retry_after_ms") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Number(n)) => match n.as_u64() {
            Some(v) => Some(v),
            None => return Err("error.retry_after_ms must be a non-negative integer".to_string()),
        },
        Some(_) => return Err("error.retry_after_ms must be a non-negative integer".to_string()),
    };
    mock::build_mock_error(category, message, status, kind, reason, retry_after_ms).map(Some)
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
    fn roundtrip_preserves_raw_tool_calls() {
        let mock = parse_llm_mock_value(&serde_json::json!({
            "text": "hello",
            "model": "mock",
            "raw_tool_calls": [
                {
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "tool_call",
                        "arguments": "{\"cmd\":\"test\"}"
                    }
                }
            ]
        }))
        .expect("parse");
        let line = serialize_llm_mock(mock).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&line).expect("reparse json");
        let reparsed = parse_llm_mock_value(&value).expect("reparse mock");
        assert_eq!(
            reparsed.raw_tool_calls[0]["function"]["name"].as_str(),
            Some("tool_call")
        );
        assert_eq!(
            reparsed.raw_tool_calls[0]["function"]["arguments"].as_str(),
            Some("{\"cmd\":\"test\"}")
        );
    }

    #[test]
    fn parse_does_not_synthesize_raw_tool_calls_from_normalized_calls() {
        let mock = parse_llm_mock_value(&serde_json::json!({
            "text": "hello",
            "model": "mock",
            "tool_calls": [
                {
                    "name": "search",
                    "arguments": {"query": "rust"}
                }
            ]
        }))
        .expect("parse");

        assert!(
            mock.raw_tool_calls.is_empty(),
            "normalized tool_calls must not be promoted to provider-native raw_tool_calls"
        );
    }

    #[test]
    fn v0_tool_call_normalization_preserves_provider_metadata() {
        let mock = parse_llm_mock_value(&serde_json::json!({
            "tool_calls": [{
                "id": "gemini_call_1",
                "type": "function",
                "name": "lookup",
                "args": {"query": "harn"},
                "thought_signature": "opaque-gemini-signature"
            }]
        }))
        .expect("parse legacy provider metadata");

        let call = &mock.tool_calls[0];
        assert_eq!(call["id"], "gemini_call_1");
        assert_eq!(call["type"], "function");
        assert_eq!(call["name"], "lookup");
        assert_eq!(call["arguments"], serde_json::json!({"query": "harn"}));
        assert_eq!(call["thought_signature"], "opaque-gemini-signature");
        assert!(call.get("args").is_none(), "legacy alias is canonicalized");
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

    #[test]
    fn parses_explicit_generic_error_category() {
        let mock = parse_llm_mock_value(&serde_json::json!({
            "error": { "category": "generic", "message": "x" }
        }))
        .expect("parse generic error");
        let error = mock.error.expect("error");
        assert_eq!(error.category.as_str(), "generic");
        assert_eq!(error.message, "x");
    }

    #[test]
    fn parses_provider_error_envelope() {
        let mock = parse_llm_mock_value(&serde_json::json!({
            "error": {
                "status": 503,
                "kind": "transient",
                "reason": "upstream_unavailable",
                "message": "upstream unavailable",
                "retry_after_ms": 250
            }
        }))
        .expect("parse provider envelope");
        let error = mock.error.expect("error");
        assert_eq!(error.category.as_str(), "overloaded");
        assert_eq!(error.status, Some(503));
        assert_eq!(error.kind.as_deref(), Some("transient"));
        assert_eq!(error.reason.as_deref(), Some("upstream_unavailable"));
        assert_eq!(error.retry_after_ms, Some(250));
    }

    #[test]
    fn roundtrip_preserves_provider_error_envelope() {
        let mock = parse_llm_mock_value(&serde_json::json!({
            "match": "*retry*",
            "error": {
                "status": 503,
                "kind": "transient",
                "reason": "upstream_unavailable",
                "retry_after_ms": 250
            }
        }))
        .expect("parse provider envelope");
        let line = serialize_llm_mock(mock).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&line).expect("reparse json");
        let reparsed = parse_llm_mock_value(&value).expect("reparse mock");
        let error = reparsed.error.expect("error");
        assert_eq!(reparsed.match_pattern.as_deref(), Some("*retry*"));
        assert_eq!(error.category.as_str(), "overloaded");
        assert_eq!(error.status, Some(503));
        assert_eq!(error.kind.as_deref(), Some("transient"));
        assert_eq!(error.reason.as_deref(), Some("upstream_unavailable"));
        assert_eq!(error.retry_after_ms, Some(250));
    }

    #[test]
    fn parse_rejects_unknown_error_kind() {
        let result = parse_llm_mock_value(&serde_json::json!({
            "error": { "status": 503, "kind": "maybe" }
        }));
        match result {
            Err(err) => assert!(err.contains("unknown error kind"), "{err}"),
            Ok(_) => panic!("expected parse failure for unknown error kind"),
        }
    }

    // --- Versioned mock-fixture contract (bc#4969) ---

    #[test]
    fn header_detects_v0_when_no_schema_version() {
        assert_eq!(
            parse_fixture_header(&serde_json::json!({"text": "hi"})).expect("header"),
            None,
            "an ordinary entry is not a header"
        );
    }

    #[test]
    fn header_reads_version_and_strict_scopes() {
        assert_eq!(
            parse_fixture_header(&serde_json::json!({"schemaVersion": 1, "strictScopes": true}))
                .expect("header"),
            Some((1, true))
        );
    }

    #[test]
    fn v1_header_is_closed_and_requires_strict_scopes() {
        let missing = parse_fixture_header(&serde_json::json!({"schemaVersion": 1}))
            .expect_err("v1 header must declare strictScopes");
        assert!(missing.contains("strictScopes"), "{missing}");

        let unknown = parse_fixture_header(&serde_json::json!({
            "schemaVersion": 1,
            "strictScopes": false,
            "strictScope": false,
        }))
        .expect_err("v1 header must reject misspelled fields");
        assert!(unknown.contains("unknown field"), "{unknown}");
    }

    #[test]
    fn header_rejects_unsupported_schema_version() {
        let err =
            parse_fixture_header(&serde_json::json!({"schemaVersion": 2, "strictScopes": false}))
                .expect_err("unsupported version must fail");
        assert!(err.contains("unsupported schemaVersion"), "{err}");
    }

    #[test]
    fn v0_parse_pins_default_scope_and_legacy_consume() {
        // FIFO entry: always consumed (not sticky).
        let fifo = parse_llm_mock_value(&serde_json::json!({"text": "x"})).expect("fifo");
        assert_eq!(fifo.scope, DEFAULT_MOCK_SCOPE);
        assert!(!fifo.sticky);
        // Glob entry: reusable (sticky) unless consume_match.
        let glob = parse_llm_mock_value(&serde_json::json!({"match": "*"})).expect("glob");
        assert!(glob.sticky, "v0 reusable glob is sticky");
        let glob_once =
            parse_llm_mock_value(&serde_json::json!({"match": "*", "consume_match": true}))
                .expect("glob once");
        assert!(!glob_once.sticky, "consume_match makes a glob one-shot");
    }

    #[test]
    fn v1_parse_reads_scope_consume_and_id() {
        let entry = parse_llm_mock_value_versioned(
            &serde_json::json!({"scope": "judge", "consume": "sticky", "id": "j1", "text": "Y"}),
            1,
            7,
        )
        .expect("v1 entry");
        assert_eq!(entry.scope, "judge");
        assert!(entry.sticky);
        assert_eq!(entry.entry_id, "j1");
    }

    #[test]
    fn v1_rejects_missing_metadata_and_legacy_aliases() {
        for (field, value) in [
            ("id", serde_json::json!({"scope":"main","consume":"once"})),
            ("scope", serde_json::json!({"id":"main-1","consume":"once"})),
            ("consume", serde_json::json!({"id":"main-1","scope":"main"})),
        ] {
            let error = parse_llm_mock_value_versioned(&value, 1, 0)
                .expect_err("v1 metadata must be explicit");
            assert!(error.contains(field), "{error}");
        }

        let error = parse_llm_mock_value_versioned(
            &serde_json::json!({
                "id":"main-1",
                "scope":"main",
                "consume":"once",
                "tool_calls":[{"name":"look","args":{}}]
            }),
            1,
            0,
        )
        .expect_err("legacy args alias is v0-only");
        assert!(
            error.contains("unknown tool_calls[0] field `args`"),
            "{error}"
        );

        let error = parse_llm_mock_value_versioned(
            &serde_json::json!({
                "id":"main-1",
                "scope":"main",
                "consume":"once",
                "tool_calls":[{
                    "name":"look",
                    "arguments":{},
                    "thought_signature":"opaque-gemini-signature"
                }]
            }),
            1,
            0,
        )
        .expect_err("provider-specific metadata is v0-only until the v1 contract owns it");
        assert!(
            error.contains("unknown tool_calls[0] field `thought_signature`"),
            "{error}"
        );

        let entry = parse_llm_mock_value_versioned(
            &serde_json::json!({
                "id":"main-1",
                "scope":"main",
                "consume":"once",
                "tool_calls":[{
                    "id":"gemini_call_1",
                    "type":"function",
                    "name":"look",
                    "arguments":{},
                    "provider_metadata": {
                        "gemini": {"thought_signature":"opaque-gemini-signature"}
                    }
                }]
            }),
            1,
            0,
        )
        .expect("v1 accepts namespaced provider metadata");
        assert_eq!(entry.tool_calls[0]["id"], "gemini_call_1");
        assert_eq!(
            entry.tool_calls[0]["provider_metadata"]["gemini"]["thought_signature"],
            "opaque-gemini-signature"
        );

        let error = parse_llm_mock_value_versioned(
            &serde_json::json!({
                "id":"main-1",
                "scope":"main",
                "consume":"once",
                "text":"ok",
                "texte":"typo"
            }),
            1,
            0,
        )
        .expect_err("v1 fixture entries are closed");
        assert!(
            error.contains("unknown fixture entry field `texte`"),
            "{error}"
        );
    }

    #[test]
    fn v1_parse_rejects_unknown_consume_mode() {
        let err = parse_llm_mock_value_versioned(
            &serde_json::json!({"id":"main-1", "scope":"main", "consume": "maybe"}),
            1,
            0,
        )
        .expect_err("unknown consume must fail");
        assert!(err.contains("consume"), "{err}");
    }

    #[test]
    fn v1_serializer_emits_a_closed_document_that_reparses() {
        let mock = parse_llm_mock_value(&serde_json::json!({
            "text": "hello",
            "tool_calls": [{
                "id": "gemini_call_1",
                "type": "function",
                "name": "look",
                "args": {"file": "src/lib.harn"},
                "thought_signature": "opaque-gemini-signature"
            }],
        }))
        .expect("parse legacy source entry");

        let document = serialize_llm_mock_fixture(vec![mock]).expect("serialize v1 document");
        let serialized_entry: serde_json::Value = serde_json::from_str(
            document
                .lines()
                .nth(1)
                .expect("fixture contains one serialized entry"),
        )
        .expect("serialized entry is JSON");
        assert_eq!(serialized_entry["tool_calls"][0]["id"], "gemini_call_1");
        assert_eq!(serialized_entry["tool_calls"][0]["type"], "function");
        assert_eq!(
            serialized_entry["tool_calls"][0]["provider_metadata"]["gemini"]["thought_signature"],
            "opaque-gemini-signature"
        );
        let fixture = parse_llm_mocks_jsonl(&document).expect("reparse v1 document");
        assert_eq!(fixture.schema_version, 1);
        assert!(!fixture.strict_scopes);
        assert_eq!(fixture.mocks.len(), 1);
        assert_eq!(fixture.mocks[0].entry_id, "record-0");
        assert_eq!(fixture.mocks[0].scope, DEFAULT_MOCK_SCOPE);
        assert!(!fixture.mocks[0].sticky);
        assert_eq!(
            fixture.mocks[0].tool_calls[0]["arguments"]["file"].as_str(),
            Some("src/lib.harn")
        );
        assert_eq!(fixture.mocks[0].tool_calls[0]["id"], "gemini_call_1");
        assert_eq!(
            fixture.mocks[0].tool_calls[0]["provider_metadata"]["gemini"]["thought_signature"],
            "opaque-gemini-signature"
        );
    }

    #[test]
    fn load_rejects_unsupported_schema_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fixture.jsonl");
        std::fs::write(
            &path,
            "{\"schemaVersion\": 2, \"strictScopes\": false}\n{\"scope\": \"main\", \"text\": \"MAIN\"}\n",
        )
        .expect("write fixture");
        let err = load_llm_mocks_jsonl(&path).expect_err("unsupported version must fail at load");
        assert!(err.contains("unsupported schemaVersion"), "{err}");
    }

    #[test]
    fn load_parses_v1_header_and_scoped_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fixture.jsonl");
        std::fs::write(
            &path,
            "{\"schemaVersion\": 1, \"strictScopes\": true}\n\
             {\"id\": \"main-1\", \"scope\": \"main\", \"consume\": \"once\", \"text\": \"MAIN\"}\n\
             {\"id\": \"judge-1\", \"scope\": \"judge\", \"consume\": \"sticky\", \"match\": \"*\", \"text\": \"JUDGE\"}\n",
        )
        .expect("write fixture");
        let fixture = load_llm_mocks_jsonl(&path).expect("load v1");
        assert_eq!(fixture.schema_version, 1);
        assert!(fixture.strict_scopes);
        assert_eq!(fixture.mocks.len(), 2);
        assert_eq!(fixture.mocks[0].scope, "main");
        assert_eq!(fixture.mocks[0].entry_id, "main-1");
        assert!(!fixture.mocks[0].sticky);
        assert_eq!(fixture.mocks[1].scope, "judge");
        assert_eq!(fixture.mocks[1].entry_id, "judge-1");
        assert!(fixture.mocks[1].sticky);
    }

    #[test]
    fn load_v0_fixture_has_no_header_and_default_scope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fixture.jsonl");
        std::fs::write(&path, "{\"text\": \"first\"}\n{\"text\": \"second\"}\n")
            .expect("write fixture");
        let fixture = load_llm_mocks_jsonl(&path).expect("load v0");
        assert_eq!(fixture.schema_version, 0);
        assert!(!fixture.strict_scopes);
        assert_eq!(fixture.mocks.len(), 2);
        assert!(fixture.mocks.iter().all(|m| m.scope == DEFAULT_MOCK_SCOPE));
    }

    #[test]
    fn text_parser_is_the_file_parser_contract_owner() {
        let text = "{\"schemaVersion\":1,\"strictScopes\":true}\n\
                    {\"id\":\"main-1\",\"scope\":\"main\",\"consume\":\"once\",\"text\":\"MAIN\"}\n";
        let from_text = parse_llm_mocks_jsonl(text).expect("parse text");

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fixture.jsonl");
        std::fs::write(&path, text).expect("write fixture");
        let from_file = load_llm_mocks_jsonl(&path).expect("parse file");

        assert_eq!(from_text.schema_version, from_file.schema_version);
        assert_eq!(from_text.strict_scopes, from_file.strict_scopes);
        assert_eq!(from_text.mocks[0].entry_id, from_file.mocks[0].entry_id);
        assert_eq!(from_text.mocks[0].scope, from_file.mocks[0].scope);
    }

    #[test]
    fn text_parser_rejects_duplicate_v1_entry_ids_before_installation() {
        let err = parse_llm_mocks_jsonl(
            "{\"schemaVersion\":1,\"strictScopes\":false}\n\
             {\"id\":\"same\",\"scope\":\"main\",\"consume\":\"once\",\"text\":\"one\"}\n\
             {\"id\":\"same\",\"scope\":\"main\",\"consume\":\"once\",\"text\":\"two\"}\n",
        )
        .expect_err("duplicate ids must fail");
        assert!(err.contains("duplicate fixture entry id"), "{err}");
    }

    #[test]
    fn text_parser_rejects_late_header_and_untrimmed_v1_identity() {
        let late_header = parse_llm_mocks_jsonl(
            "{\"text\":\"first\"}\n{\"schemaVersion\":1,\"strictScopes\":false}\n",
        )
        .expect_err("late header must fail");
        assert!(
            late_header.contains("first non-empty line"),
            "{late_header}"
        );

        let untrimmed_id = parse_llm_mocks_jsonl("{\"schemaVersion\":1,\"strictScopes\":false}\n{\"id\":\" judge \",\"scope\":\"main\",\"consume\":\"once\"}\n")
            .expect_err("untrimmed v1 id must fail");
        assert!(
            untrimmed_id.contains("non-empty trimmed string"),
            "{untrimmed_id}"
        );
    }
}
