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
    let mut warned_scopes = std::collections::BTreeSet::new();
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
        if fixture.schema_version > 0
            && !mock::KNOWN_MOCK_SCOPES.contains(&mock.scope.as_str())
            && !mock.scope.contains('.')
            && warned_scopes.insert(mock.scope.clone())
        {
            fixture.warnings.push(format!(
                "unknown LLM mock scope {:?}; known Harn purposes: {}",
                mock.scope,
                mock::KNOWN_MOCK_SCOPES.join(", ")
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
    if schema_version > mock::MAX_MOCK_SCHEMA_VERSION {
        return Err(format!(
            "unsupported schemaVersion {schema_version}; this build supports 0..={}",
            mock::MAX_MOCK_SCHEMA_VERSION
        ));
    }
    let object = value
        .as_object()
        .ok_or_else(|| "fixture line must be a JSON object".to_string())?;
    if schema_version > 0 {
        validate_v1_entry(object)?;
    } else {
        validate_v0_entry(object)?;
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
    // A `usage` object is the shape every provider reports and the shape
    // fixture authors reach for. Read it as an alias for the flat fields
    // rather than dropping it: a scripted zero-token completion that silently
    // became the default thirty is indistinguishable from a fixture that was
    // never applied, and it takes the response off the empty path the author
    // meant to exercise. Flat fields win when both are present.
    let usage = parse_llm_mock_usage(object.get("usage"))?;
    let input_tokens = optional_i64_field(object, "input_tokens")?.or(usage.input_tokens);
    let output_tokens = optional_i64_field(object, "output_tokens")?.or(usage.output_tokens);
    let cache_read_tokens =
        optional_i64_field(object, "cache_read_tokens")?.or(usage.cache_read_tokens);
    let cache_write_tokens = optional_i64_field(object, "cache_write_tokens")?
        .or(optional_i64_field(object, "cache_creation_input_tokens")?)
        .or(usage.cache_write_tokens);
    let simulated_cost_usd = optional_nonnegative_f64_field(object, "simulated_cost_usd")?;
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
        simulated_cost_usd,
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
    if let Some(simulated_cost_usd) = mock.simulated_cost_usd {
        object.insert(
            "simulated_cost_usd".to_string(),
            serde_json::json!(simulated_cost_usd),
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

/// The fields the v0 parser above actually reads.
///
/// Deliberately not `V1_ENTRY_FIELDS`: `consume_match` and
/// `cache_creation_input_tokens` are v0-only spellings the versioned contract
/// dropped, and `id` / `scope` / `consume` are v1-only — `resolve_scope_consume`
/// returns before reading them at v0, so authoring one here would be the very
/// silent drop this check exists to stop.
const V0_ENTRY_FIELDS: &[&str] = &[
    "match",
    "consume_match",
    "text",
    "usage",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "cache_creation_input_tokens",
    "simulated_cost_usd",
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

/// Fields that only mean something once a fixture declares `schemaVersion`.
/// Naming the header beats a nearest-spelling guess: the author did not
/// misspell anything, they wrote a v1 entry into a v0 document.
const V1_ONLY_ENTRY_FIELDS: &[&str] = &["id", "scope", "consume"];

/// Close the v0 authoring surface at the top level.
///
/// Nested shapes stay open at v0 on purpose: legacy tool calls accept `args`
/// alongside `arguments`, and provider-native response blocks are opaque by
/// contract. The defect this closes is a top-level key that is read by nobody
/// and reported to nobody, which makes a fixture that was never applied
/// indistinguishable from one that was.
fn validate_v0_entry(object: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    for key in object.keys() {
        if V0_ENTRY_FIELDS.contains(&key.as_str()) {
            continue;
        }
        if V1_ONLY_ENTRY_FIELDS.contains(&key.as_str()) {
            return Err(format!(
                "unknown fixture entry field `{key}`; it is a v1 field and this \
                 document has no `schemaVersion` header, so it would be dropped"
            ));
        }
        return Err(unknown_field_message("fixture entry", key, V0_ENTRY_FIELDS));
    }
    Ok(())
}

/// One message shape for both contract versions: name the key, and name the
/// field the author probably meant when one is close enough to guess.
fn unknown_field_message(label: &str, key: &str, allowed_fields: &[&str]) -> String {
    match crate::value::closest_match(key, allowed_fields.iter().copied()) {
        Some(suggestion) => {
            format!("unknown {label} field `{key}`; did you mean `{suggestion}`?")
        }
        None => format!("unknown {label} field `{key}`"),
    }
}

const V1_ENTRY_FIELDS: &[&str] = &[
    "id",
    "scope",
    "consume",
    "match",
    "text",
    "usage",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "simulated_cost_usd",
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
            return Err(unknown_field_message(label, key, allowed_fields));
        }
    }
    Ok(())
}

/// Token counts read from a fixture entry's `usage` object.
///
/// Accepts both the OpenAI-compat spellings (`prompt_tokens` /
/// `completion_tokens`) and the flat Harn names, because fixture authors copy
/// whichever their provider prints. Every field stays optional so a `usage`
/// carrying only one count leaves the others to their defaults.
#[derive(Default)]
struct LlmMockUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    cache_write_tokens: Option<i64>,
}

const USAGE_FIELDS: &[&str] = &[
    "input_tokens",
    "output_tokens",
    "prompt_tokens",
    "completion_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "cache_creation_input_tokens",
    "total_tokens",
];

fn parse_llm_mock_usage(value: Option<&serde_json::Value>) -> Result<LlmMockUsage, String> {
    let Some(value) = value else {
        return Ok(LlmMockUsage::default());
    };
    if value.is_null() {
        return Ok(LlmMockUsage::default());
    }
    let object = value
        .as_object()
        .ok_or_else(|| "`usage` must be an object".to_string())?;
    validate_v1_object_fields(object, "usage", USAGE_FIELDS)?;
    let input_tokens = optional_i64_field(object, "input_tokens")?
        .or(optional_i64_field(object, "prompt_tokens")?);
    let output_tokens = optional_i64_field(object, "output_tokens")?
        .or(optional_i64_field(object, "completion_tokens")?);
    // `total_tokens` has no slot on the mock: the total is derived from the
    // input and output counts downstream. Accepting it silently would drop a
    // number the author wrote, so honour it as a constraint instead. A total
    // that cannot be checked against a scripted split is rejected rather than
    // ignored, which keeps the failure loud at the fixture that wrote it.
    if let Some(total) = optional_i64_field(object, "total_tokens")? {
        match (input_tokens, output_tokens) {
            (Some(input), Some(output)) if input + output != total => {
                return Err(format!(
                    "`usage.total_tokens` is {total} but `input_tokens` + `output_tokens` is {}",
                    input + output
                ));
            }
            (Some(_), Some(_)) => {}
            _ => {
                return Err(
                    "`usage.total_tokens` needs both `input_tokens` and `output_tokens`; \
                     the mock derives the total and cannot split it"
                        .to_string(),
                );
            }
        }
    }
    Ok(LlmMockUsage {
        input_tokens,
        output_tokens,
        cache_read_tokens: optional_i64_field(object, "cache_read_tokens")?,
        cache_write_tokens: optional_i64_field(object, "cache_write_tokens")?
            .or(optional_i64_field(object, "cache_creation_input_tokens")?),
    })
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

fn optional_nonnegative_f64_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<f64>, String> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|number| number.is_finite() && *number >= 0.0)
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a finite non-negative number")),
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
#[path = "jsonl_tests.rs"]
mod tests;
