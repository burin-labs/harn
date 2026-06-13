//! `LlmResult` and the Harn-facing dict builder for `llm_call` return
//! values, plus the mock-provider completion response.

use crate::value::VmDictExt;

use super::telemetry::ProviderTelemetry;
use crate::value::VmValue;

fn default_true() -> bool {
    true
}

/// `skip_serializing_if` for `cache_supported`: omit the field in the common
/// (supported) case so recordings/replay tapes/transcripts only carry it for the
/// rare unsupported (native-Ollama) result. Keeps serialized output byte-stable
/// with pre-existing goldens; `default_true` restores `true` on deserialize.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_true(value: &bool) -> bool {
    *value
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct LlmResult {
    pub text: String,
    pub tool_calls: Vec<serde_json::Value>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Prompt tokens served from the provider's cache (when supported).
    /// Anthropic: `usage.cache_read_input_tokens`.
    /// OpenAI: `usage.prompt_tokens_details.cached_tokens`.
    /// OpenRouter passthrough for Anthropic: `usage.cache_read_input_tokens`.
    /// Defaults to 0 when the provider doesn't report it.
    pub cache_read_tokens: i64,
    /// Prompt tokens written to the provider's cache on this request
    /// (Anthropic `usage.cache_creation_input_tokens`). Helps distinguish
    /// "warm-up" calls from cache hits.
    pub cache_write_tokens: i64,
    /// Whether the provider reports prompt-cache accounting at all. Native
    /// Ollama (`/api/chat`, `/api/generate`, the completion endpoint) sends no
    /// cache field in its done frame — and the `/v1` shim on these hosts also
    /// omits `prompt_tokens_details` — so `cache_read_tokens: 0` there means
    /// "unknown", NOT a real 100% cache miss. When `false`, cache hit-ratio is
    /// surfaced as `cache_visibility: "unsupported"` with a null ratio rather
    /// than scoring a local model as a 0.0-ratio total miss. Defaults to `true`
    /// for every provider that does report cache counts.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub cache_supported: bool,
    pub model: String,
    pub provider: String,
    pub thinking: Option<String>,
    pub thinking_summary: Option<String>,
    pub stop_reason: Option<String>,
    /// True when the provider confirmed it served this request at the
    /// accelerated ("fast mode") tier — it echoes the knob (`speed` /
    /// `service_tier`) in the response. Drives premium-tier billing. A
    /// `fast: true` request that the provider downgraded under capacity
    /// pressure echoes a different value and bills at standard rates.
    #[serde(default)]
    pub served_fast: bool,
    pub blocks: Vec<serde_json::Value>,
    pub logprobs: Vec<serde_json::Value>,
    /// Server-side timings and runtime accounting captured from this
    /// response. Empty for mocks and providers that report nothing usable.
    #[serde(default, skip_serializing_if = "ProviderTelemetry::is_empty")]
    pub telemetry: ProviderTelemetry,
}

fn build_usage_dict(result: &LlmResult) -> crate::value::DictMap {
    let cache_hit_ratio = crate::llm::cost::cache_hit_ratio(
        result.input_tokens,
        result.cache_read_tokens,
        result.cache_write_tokens,
    );
    let cache_savings_usd = crate::llm::cost::cache_savings_usd_for_provider(
        &result.provider,
        &result.model,
        result.cache_read_tokens,
        result.cache_write_tokens,
    );

    let mut usage = crate::value::DictMap::new();
    usage.insert(
        "input_tokens".to_string(),
        VmValue::Int(result.input_tokens),
    );
    usage.insert(
        "output_tokens".to_string(),
        VmValue::Int(result.output_tokens),
    );
    usage.insert(
        "cache_read_tokens".to_string(),
        VmValue::Int(result.cache_read_tokens),
    );
    usage.insert(
        "cache_write_tokens".to_string(),
        VmValue::Int(result.cache_write_tokens),
    );
    usage.insert(
        "cache_creation_input_tokens".to_string(),
        VmValue::Int(result.cache_write_tokens),
    );
    if result.cache_supported {
        usage.insert(
            "cache_hit_ratio".to_string(),
            VmValue::Float(cache_hit_ratio),
        );
        usage.insert("cache_visibility".to_string(), VmValue::Nil);
    } else {
        // Native local runtimes report no cache field; a 0.0 ratio here would
        // mislabel a local model as a 100% cache miss. Surface the unknown
        // explicitly instead of fabricating a number.
        usage.insert("cache_hit_ratio".to_string(), VmValue::Nil);
        usage.put_str("cache_visibility", "unsupported");
    }
    usage.insert(
        "cache_savings_usd".to_string(),
        VmValue::Float(cache_savings_usd),
    );
    usage.insert("served_fast".to_string(), VmValue::Bool(result.served_fast));
    usage
}

pub(crate) fn vm_build_llm_result(
    result: &LlmResult,
    parsed_json: Option<VmValue>,
    transcript: Option<VmValue>,
    tools_val: Option<&VmValue>,
) -> VmValue {
    use crate::stdlib::json_to_vm_value;

    let mut dict = crate::value::DictMap::new();
    dict.put_str("text", result.text.as_str());
    dict.put_str("model", result.model.as_str());
    dict.put_str("provider", result.provider.as_str());
    dict.insert(
        "input_tokens".to_string(),
        VmValue::Int(result.input_tokens),
    );
    dict.insert(
        "output_tokens".to_string(),
        VmValue::Int(result.output_tokens),
    );
    // Cache accounting (0 when provider doesn't report cache info).
    dict.insert(
        "cache_read_tokens".to_string(),
        VmValue::Int(result.cache_read_tokens),
    );
    dict.insert(
        "cache_write_tokens".to_string(),
        VmValue::Int(result.cache_write_tokens),
    );
    dict.insert(
        "cache_creation_input_tokens".to_string(),
        VmValue::Int(result.cache_write_tokens),
    );
    dict.insert("served_fast".to_string(), VmValue::Bool(result.served_fast));
    let usage = build_usage_dict(result);
    if let Some(value) = usage.get("cache_hit_ratio") {
        dict.insert("cache_hit_ratio".to_string(), value.clone());
    }
    if let Some(value) = usage.get("cache_visibility") {
        dict.insert("cache_visibility".to_string(), value.clone());
    }
    if let Some(value) = usage.get("cache_savings_usd") {
        dict.insert("cache_savings_usd".to_string(), value.clone());
    }
    // Surface provider-side timings (Ollama load_duration, prompt_eval_duration,
    // eval_duration; OpenAI usage; llama.cpp `timings`). Evals key off
    // `provider_telemetry` for cold-vs-steady-state and prefill-vs-generation
    // breakdowns; absent fields stay absent rather than collapsing to zero.
    let telemetry_dict = result.telemetry.as_vm_dict();
    let mut usage = usage;
    if let Some(ref telemetry_dict) = telemetry_dict {
        usage.insert("provider_telemetry".to_string(), telemetry_dict.clone());
    }
    dict.insert("usage".to_string(), VmValue::dict(usage));
    if let Some(telemetry_dict) = telemetry_dict {
        dict.insert("provider_telemetry".to_string(), telemetry_dict);
    }

    if let Some(json_val) = parsed_json {
        dict.insert("data".to_string(), json_val);
    }

    let has_tagged_blocks = [
        "<assistant_prose>",
        "<user_response>",
        "<done>",
        "<tool_call>",
    ]
    .iter()
    .any(|tag| result.text.contains(tag));
    let has_text_tool_protocol =
        tools_val.is_some() || !result.tool_calls.is_empty() || has_tagged_blocks;
    // Keep parsing available for tool-calling responses so llm_call can
    // expose canonical/prose/tool metadata, but do not surface tagged-protocol
    // violations for ordinary plain-text completions with no tools.
    let tagged = has_text_tool_protocol
        .then(|| crate::llm::tools::parse_text_tool_calls_with_tools(&result.text, tools_val));

    let merged_tool_calls: Vec<serde_json::Value> = if !result.tool_calls.is_empty() {
        result.tool_calls.clone()
    } else if let Some(parse) = tagged.as_ref() {
        parse.calls.clone()
    } else {
        Vec::new()
    };
    if !merged_tool_calls.is_empty() {
        let calls: Vec<VmValue> = merged_tool_calls.iter().map(json_to_vm_value).collect();
        dict.insert(
            "tool_calls".to_string(),
            VmValue::List(std::sync::Arc::new(calls)),
        );
    }
    // Expose native_tool_calls separately so the agent loop can distinguish
    // provider-native tool calls from text-parsed ones for native_tool_fallback
    // detection. `tool_calls` (above) merges both sources for callers that
    // just want the unified view.
    let native_calls: Vec<VmValue> = result.tool_calls.iter().map(json_to_vm_value).collect();
    dict.insert(
        "native_tool_calls".to_string(),
        VmValue::List(std::sync::Arc::new(native_calls)),
    );

    if let Some(parse) = tagged.as_ref() {
        if !parse.violations.is_empty() {
            let violations: Vec<VmValue> = parse
                .violations
                .iter()
                .map(|v| VmValue::String(std::sync::Arc::from(v.as_str())))
                .collect();
            dict.insert(
                "protocol_violations".to_string(),
                VmValue::List(std::sync::Arc::new(violations)),
            );
        }
        if !parse.errors.is_empty() {
            let errors: Vec<VmValue> = parse
                .errors
                .iter()
                .map(|e| VmValue::String(std::sync::Arc::from(e.as_str())))
                .collect();
            dict.insert(
                "tool_parse_errors".to_string(),
                VmValue::List(std::sync::Arc::new(errors)),
            );
        }
        if let Some(ref body) = parse.done_marker {
            dict.put_str("done_marker", body.as_str());
        }
        if !parse.canonical.is_empty() {
            dict.put_str("canonical_text", parse.canonical.as_str());
        }
        // Always emit `prose` (fall back to raw text) so callers have a
        // single reliable "the answer" key regardless of whether the model
        // used the tagged protocol.
        let prose = if parse.prose.is_empty() {
            result.text.clone()
        } else {
            parse.prose.clone()
        };
        dict.put_str("prose", prose.as_str());
    } else {
        dict.put_str("prose", result.text.as_str());
    }

    if let Some(ref thinking) = result.thinking {
        dict.put_str("thinking", thinking.as_str());
        dict.put_str("private_reasoning", thinking.as_str());
    }
    if let Some(ref summary) = result.thinking_summary {
        dict.put_str("thinking_summary", summary.as_str());
    }

    if let Some(ref stop_reason) = result.stop_reason {
        dict.put_str("stop_reason", stop_reason.as_str());
    }
    if let Some(ref request_id) = result.telemetry.request_id {
        dict.put_str("provider_response_id", request_id.as_str());
    }

    if let Some(transcript) = transcript {
        dict.insert("transcript".to_string(), transcript);
    }

    // Prose with fenceless TS tool-call expressions stripped. Agent_loop
    // applies the same semantics on its final iteration.
    let visible_text = if tools_val.is_some() && result.tool_calls.is_empty() {
        let parse_result =
            crate::llm::tools::parse_text_tool_calls_with_tools(&result.text, tools_val);
        parse_result.prose
    } else {
        crate::visible_text::sanitize_visible_assistant_text(&result.text, false)
    };
    dict.put_str("visible_text", visible_text.as_str());
    dict.insert(
        "blocks".to_string(),
        VmValue::List(std::sync::Arc::new(
            result
                .blocks
                .iter()
                .map(json_to_vm_value)
                .collect::<Vec<_>>(),
        )),
    );
    if !result.logprobs.is_empty() {
        dict.insert(
            "logprobs".to_string(),
            VmValue::List(std::sync::Arc::new(
                result
                    .logprobs
                    .iter()
                    .map(json_to_vm_value)
                    .collect::<Vec<_>>(),
            )),
        );
    }

    VmValue::dict(dict)
}

pub(super) fn mock_completion_response(prefix: &str, suffix: Option<&str>) -> LlmResult {
    let suffix = suffix.unwrap_or_default();
    let text = format!(
        "Mock completion after {} chars{}",
        prefix.chars().count(),
        if suffix.is_empty() {
            String::new()
        } else {
            format!(" before {} chars", suffix.chars().count())
        }
    );
    LlmResult {
        served_fast: false,
        text: text.clone(),
        tool_calls: Vec::new(),
        input_tokens: (prefix.len() + suffix.len()) as i64,
        output_tokens: 16,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_supported: true,
        model: "mock".to_string(),
        provider: "mock".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: Some("stop".to_string()),
        blocks: vec![serde_json::json!({
            "type": "output_text",
            "text": text,
            "visibility": "public",
        })],
        logprobs: Vec::new(),
        telemetry: ProviderTelemetry::default(),
    }
}

#[cfg(test)]
mod cache_supported_serde_tests {
    use super::mock_completion_response;
    use super::LlmResult;

    #[test]
    fn cache_supported_true_is_omitted_from_serialization() {
        // The common (cache-supported) case must serialize byte-identically to
        // pre-`cache_supported` recordings / replay tapes, so the field is
        // omitted when true and restored to true on deserialize via
        // `default_true`. This keeps the testbench replay-fidelity golden stable.
        let result = mock_completion_response("hi", None);
        assert!(result.cache_supported);
        let json = serde_json::to_value(&result).expect("serialize");
        assert!(
            json.get("cache_supported").is_none(),
            "cache_supported=true must be omitted from serialized output, got: {json}"
        );
        let back: LlmResult = serde_json::from_value(json).expect("deserialize");
        assert!(
            back.cache_supported,
            "missing field must default back to true"
        );
    }

    #[test]
    fn cache_supported_false_is_serialized_and_round_trips() {
        let mut result = mock_completion_response("hi", None);
        result.cache_supported = false;
        let json = serde_json::to_value(&result).expect("serialize");
        assert_eq!(
            json.get("cache_supported")
                .and_then(serde_json::Value::as_bool),
            Some(false),
            "the meaningful unsupported case must serialize the field"
        );
        let back: LlmResult = serde_json::from_value(json).expect("deserialize");
        assert!(!back.cache_supported);
    }
}
