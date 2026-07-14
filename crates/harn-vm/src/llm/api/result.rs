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

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct RawProviderToolCall(serde_json::Value);

impl RawProviderToolCall {
    pub(crate) fn new(value: serde_json::Value) -> Result<Self, String> {
        if value.is_object() {
            Ok(Self(value))
        } else {
            Err("raw provider tool call must be a JSON object".to_string())
        }
    }

    pub(crate) fn array_from_value(value: &serde_json::Value) -> Result<Vec<Self>, String> {
        match value {
            serde_json::Value::Null => Ok(Vec::new()),
            serde_json::Value::Array(items) => items.iter().cloned().map(Self::new).collect(),
            _ => Err("raw_tool_calls must be an array".to_string()),
        }
    }

    pub(crate) fn into_value(self) -> serde_json::Value {
        self.0
    }
}

impl std::ops::Deref for RawProviderToolCall {
    type Target = serde_json::Value;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<serde_json::Value> for RawProviderToolCall {
    type Error = String;

    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> serde::Deserialize<'de> for RawProviderToolCall {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl From<RawProviderToolCall> for serde_json::Value {
    fn from(value: RawProviderToolCall) -> Self {
        value.into_value()
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct LlmResult {
    pub text: String,
    pub tool_calls: Vec<serde_json::Value>,
    /// Provider-native tool-call envelopes before Harn normalizes names and
    /// arguments for dispatch. Transcript-only receipt for format/adapter
    /// forensics; dispatch must keep using `tool_calls`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_tool_calls: Vec<RawProviderToolCall>,
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
        result.input_tokens,
        result.cache_read_tokens,
        result.cache_write_tokens,
    );

    let mut usage = crate::value::DictMap::new();
    usage.insert(
        crate::value::intern_key("input_tokens"),
        VmValue::Int(result.input_tokens),
    );
    usage.insert(
        crate::value::intern_key("output_tokens"),
        VmValue::Int(result.output_tokens),
    );
    usage.insert(
        crate::value::intern_key("cache_read_tokens"),
        VmValue::Int(result.cache_read_tokens),
    );
    usage.insert(
        crate::value::intern_key("cache_write_tokens"),
        VmValue::Int(result.cache_write_tokens),
    );
    usage.insert(
        crate::value::intern_key("cache_creation_input_tokens"),
        VmValue::Int(result.cache_write_tokens),
    );
    if result.cache_supported {
        usage.insert(
            crate::value::intern_key("cache_hit_ratio"),
            VmValue::Float(cache_hit_ratio),
        );
        usage.insert(crate::value::intern_key("cache_visibility"), VmValue::Nil);
    } else {
        // Native local runtimes report no cache field; a 0.0 ratio here would
        // mislabel a local model as a 100% cache miss. Surface the unknown
        // explicitly instead of fabricating a number.
        usage.insert(crate::value::intern_key("cache_hit_ratio"), VmValue::Nil);
        usage.put_str("cache_visibility", "unsupported");
    }
    usage.insert(
        crate::value::intern_key("cache_savings_usd"),
        VmValue::Float(cache_savings_usd),
    );
    usage.insert(
        crate::value::intern_key("served_fast"),
        VmValue::Bool(result.served_fast),
    );
    usage
}

/// Some local/open-weight routes emit their reasoning INLINE in the text
/// channel as `<think>...</think>` blocks (Qwen3 via vLLM, local Ollama /
/// llama.cpp reasoning models, Kimi) instead of in a separate provider
/// reasoning field. When the route's capability matrix marks it as an
/// inline-reasoning emitter, split those blocks out of the visible text so
/// `text`/`prose`/`visible_text` carry only the answer, and return the
/// extracted reasoning so it can be folded into the reasoning channel —
/// mirroring how hosted-provider thinking is already surfaced.
///
/// Gated on the `emits_inline_reasoning` capability (data-driven, never a
/// provider-name match): a route that never emits inline `<think>` (Anthropic,
/// OpenAI) passes any literal `<think>` in its output through untouched.
///
/// Malformed-tag rule (inherited from [`split_openai_thinking_blocks`]): a
/// leading/embedded well-formed `<think>...</think>` block is moved to the
/// reasoning channel; a `<think>` with no matching `</think>` consumes the
/// remainder as reasoning (visible becomes empty), which is the safe reading
/// for a truncated reasoning trace with no committed answer. Text with no
/// `<think>` is returned verbatim. The real streaming and openai-compat/ollama
/// batch paths already strip inline think upstream, so for those routes this is
/// an idempotent no-op (the text arrives with the tags already gone).
fn split_inline_reasoning_if_capable(result: &LlmResult) -> (String, Option<String>) {
    if !result.text.contains("<think>") {
        return (result.text.clone(), None);
    }
    let caps = crate::llm::capabilities::lookup(&result.provider, &result.model);
    if !caps.emits_inline_reasoning {
        return (result.text.clone(), None);
    }
    let (visible, thinking) = crate::llm::api::split_openai_thinking_blocks(&result.text);
    let thinking = (!thinking.trim().is_empty()).then_some(thinking);
    (visible, thinking)
}

/// Merge an existing provider reasoning field with reasoning extracted from
/// inline `<think>` blocks. In practice only one side is populated for any
/// given call (a route either surfaces a separate reasoning field or emits
/// inline think, not both), but merge defensively so neither is dropped.
fn merge_reasoning_channels(existing: &Option<String>, inline: Option<String>) -> Option<String> {
    match (existing.as_deref(), inline) {
        (None, inline) => inline,
        (Some(existing), None) => Some(existing.to_string()),
        (Some(existing), Some(inline)) => {
            if existing.trim().is_empty() {
                Some(inline)
            } else if inline.trim().is_empty() {
                Some(existing.to_string())
            } else {
                Some(format!("{existing}\n{inline}"))
            }
        }
    }
}

struct TextToolProjection {
    parsed: Option<crate::llm::tools::TextToolParseResult>,
    public_text: String,
    visible_text: String,
}

fn build_text_tool_projection(
    visible_text_src: &str,
    tools_val: Option<&VmValue>,
    native_tool_calls: &[serde_json::Value],
) -> TextToolProjection {
    let has_tagged_blocks = [
        "<assistant_prose>",
        "<user_response>",
        "<done>",
        "<tool_call>",
    ]
    .iter()
    .any(|tag| visible_text_src.contains(tag));
    let has_text_tool_protocol =
        tools_val.is_some() || !native_tool_calls.is_empty() || has_tagged_blocks;
    let parsed = has_text_tool_protocol
        .then(|| crate::llm::tools::parse_text_tool_calls_with_tools(visible_text_src, tools_val));

    let has_native_tool_calls = !native_tool_calls.is_empty();
    let parsed_has_calls = parsed.as_ref().is_some_and(|parse| !parse.calls.is_empty());

    let public_text = match parsed.as_ref() {
        Some(parse) if !parse.prose.is_empty() => parse.prose.clone(),
        Some(_) if parsed_has_calls || has_native_tool_calls => String::new(),
        _ => visible_text_src.to_string(),
    };
    let visible_text = if parsed_has_calls || has_native_tool_calls || tools_val.is_some() {
        public_text.clone()
    } else {
        crate::visible_text::sanitize_visible_assistant_text(visible_text_src, false)
    };

    TextToolProjection {
        parsed,
        public_text,
        visible_text,
    }
}

pub(crate) fn vm_build_llm_result(
    result: &LlmResult,
    parsed_json: Option<VmValue>,
    transcript: Option<VmValue>,
    tools_val: Option<&VmValue>,
) -> VmValue {
    use crate::stdlib::json_to_vm_value;

    // Capability-gated split of inline `<think>` reasoning out of the visible
    // text channel. `visible_text_src` is the answer with inline reasoning
    // removed (or the original text unchanged for non-inline routes);
    // `inline_reasoning` is the extracted reasoning to fold into the channel.
    let (visible_text_src, inline_reasoning) = split_inline_reasoning_if_capable(result);

    let mut dict = crate::value::DictMap::new();
    dict.put_str("model", result.model.as_str());
    dict.put_str("provider", result.provider.as_str());
    dict.insert(
        crate::value::intern_key("input_tokens"),
        VmValue::Int(result.input_tokens),
    );
    dict.insert(
        crate::value::intern_key("output_tokens"),
        VmValue::Int(result.output_tokens),
    );
    // Cache accounting (0 when provider doesn't report cache info).
    dict.insert(
        crate::value::intern_key("cache_read_tokens"),
        VmValue::Int(result.cache_read_tokens),
    );
    dict.insert(
        crate::value::intern_key("cache_write_tokens"),
        VmValue::Int(result.cache_write_tokens),
    );
    dict.insert(
        crate::value::intern_key("cache_creation_input_tokens"),
        VmValue::Int(result.cache_write_tokens),
    );
    dict.insert(
        crate::value::intern_key("served_fast"),
        VmValue::Bool(result.served_fast),
    );
    let usage = build_usage_dict(result);
    if let Some(value) = usage.get("cache_hit_ratio") {
        dict.insert(crate::value::intern_key("cache_hit_ratio"), value.clone());
    }
    if let Some(value) = usage.get("cache_visibility") {
        dict.insert(crate::value::intern_key("cache_visibility"), value.clone());
    }
    if let Some(value) = usage.get("cache_savings_usd") {
        dict.insert(crate::value::intern_key("cache_savings_usd"), value.clone());
    }
    // Surface provider-side timings (Ollama load_duration, prompt_eval_duration,
    // eval_duration; OpenAI usage; llama.cpp `timings`). Evals key off
    // `provider_telemetry` for cold-vs-steady-state and prefill-vs-generation
    // breakdowns; absent fields stay absent rather than collapsing to zero.
    let telemetry_dict = result.telemetry.as_vm_dict();
    let mut usage = usage;
    if let Some(ref telemetry_dict) = telemetry_dict {
        usage.insert(
            crate::value::intern_key("provider_telemetry"),
            telemetry_dict.clone(),
        );
    }
    dict.insert(crate::value::intern_key("usage"), VmValue::dict(usage));
    if let Some(telemetry_dict) = telemetry_dict {
        dict.insert(
            crate::value::intern_key("provider_telemetry"),
            telemetry_dict,
        );
    }

    if let Some(json_val) = parsed_json {
        dict.insert(crate::value::intern_key("data"), json_val);
    }

    // Keep parsing available for tool-calling responses so llm_call can
    // expose canonical/prose/tool metadata, but do not surface tagged-protocol
    // violations for ordinary plain-text completions with no tools.
    let projection = build_text_tool_projection(&visible_text_src, tools_val, &result.tool_calls);
    dict.put_str("raw_text", visible_text_src.as_str());
    dict.put_str("text", projection.public_text.as_str());

    let merged_tool_calls: Vec<serde_json::Value> = if !result.tool_calls.is_empty() {
        result.tool_calls.clone()
    } else if let Some(parse) = projection.parsed.as_ref() {
        parse.calls.clone()
    } else {
        Vec::new()
    };
    if !merged_tool_calls.is_empty() {
        let calls: Vec<VmValue> = merged_tool_calls.iter().map(json_to_vm_value).collect();
        dict.insert(
            crate::value::intern_key("tool_calls"),
            VmValue::List(std::sync::Arc::new(calls)),
        );
    }
    // Expose native_tool_calls separately so the agent loop can distinguish
    // provider-native tool calls from text-parsed ones for native_tool_fallback
    // detection. `tool_calls` (above) merges both sources for callers that
    // just want the unified view.
    let native_calls: Vec<VmValue> = result.tool_calls.iter().map(json_to_vm_value).collect();
    dict.insert(
        crate::value::intern_key("native_tool_calls"),
        VmValue::List(std::sync::Arc::new(native_calls)),
    );

    if let Some(parse) = projection.parsed.as_ref() {
        if !parse.violations.is_empty() {
            let violations: Vec<VmValue> = parse
                .violations
                .iter()
                .map(|v| VmValue::String(arcstr::ArcStr::from(v.as_str())))
                .collect();
            dict.insert(
                crate::value::intern_key("protocol_violations"),
                VmValue::List(std::sync::Arc::new(violations)),
            );
        }
        if !parse.errors.is_empty() {
            let errors: Vec<VmValue> = parse
                .errors
                .iter()
                .map(|e| VmValue::String(arcstr::ArcStr::from(e.as_str())))
                .collect();
            dict.insert(
                crate::value::intern_key("tool_parse_errors"),
                VmValue::List(std::sync::Arc::new(errors)),
            );
        }
        if let Some(ref body) = parse.done_marker {
            dict.put_str("done_marker", body.as_str());
            dict.put_str("parsed_done_marker", body.as_str());
        }
        if !parse.canonical.is_empty() {
            dict.put_str("canonical_text", parse.canonical.as_str());
        }
        // Always emit `prose` (fall back to raw text) so callers have a
        // single reliable "the answer" key regardless of whether the model
        // used the tagged protocol.
        dict.put_str("prose", projection.public_text.as_str());
    } else {
        dict.put_str("prose", visible_text_src.as_str());
    }

    if let Some(thinking) = merge_reasoning_channels(&result.thinking, inline_reasoning) {
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
        dict.insert(crate::value::intern_key("transcript"), transcript);
    }

    dict.put_str("visible_text", projection.visible_text.as_str());
    dict.insert(
        crate::value::intern_key("blocks"),
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
            crate::value::intern_key("logprobs"),
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
        raw_tool_calls: Vec::new(),
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
    use crate::value::VmValue;

    use super::{mock_completion_response, vm_build_llm_result, LlmResult, RawProviderToolCall};
    use std::collections::BTreeMap;

    fn run_tool_registry() -> VmValue {
        let parameters = VmValue::dict(BTreeMap::from([
            ("type".to_string(), VmValue::string("object")),
            (
                "properties".to_string(),
                VmValue::dict(Vec::<(String, VmValue)>::new()),
            ),
        ]));
        let tool = VmValue::dict(BTreeMap::from([
            ("name".to_string(), VmValue::string("run")),
            (
                "description".to_string(),
                VmValue::string("Run a shell command."),
            ),
            ("parameters".to_string(), parameters),
        ]));
        VmValue::dict(BTreeMap::from([(
            "tools".to_string(),
            VmValue::List(std::sync::Arc::new(vec![tool])),
        )]))
    }

    #[test]
    fn raw_provider_tool_call_rejects_non_object() {
        let constructed = RawProviderToolCall::new(serde_json::json!("tool_call"));
        let deserialized =
            serde_json::from_value::<RawProviderToolCall>(serde_json::json!("tool_call"));

        assert!(constructed.is_err());
        assert!(deserialized.is_err());
    }

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

    #[test]
    fn provider_reasoning_text_is_not_parsed_as_executable_tool_calls() {
        let mut result = mock_completion_response("hi", None);
        result.text = "<user_response>Done.</user_response>".to_string();
        result.thinking = Some(
            "<tool_call>\nrun({ command: \"echo should-not-run\" })\n</tool_call>".to_string(),
        );

        let value = vm_build_llm_result(&result, None, None, None);
        let dict = value.as_dict().expect("result dict");

        assert!(
            dict.get("tool_calls").is_none(),
            "provider reasoning must not become executable tool calls"
        );
        assert_eq!(
            dict.get("prose").map(VmValue::display).as_deref(),
            Some("Done.")
        );
        assert_eq!(
            dict.get("thinking").map(VmValue::display).as_deref(),
            result.thinking.as_deref()
        );
        assert_eq!(
            dict.get("private_reasoning")
                .map(VmValue::display)
                .as_deref(),
            result.thinking.as_deref()
        );
    }

    #[test]
    fn text_tool_protocol_action_only_response_does_not_leak_as_public_text() {
        let mut result = mock_completion_response("hi", None);
        result.provider = "fireworks".to_string();
        result.model = "accounts/fireworks/models/gpt-oss-120b".to_string();
        result.text = concat!(
            "<|start|>assistant<|channel|>commentary<|message|>",
            "<tool_call>\n",
            "run({ command: \"cargo test\" })\n",
            "</tool_call>",
            "<|end|>"
        )
        .to_string();

        let tools = run_tool_registry();
        let value = vm_build_llm_result(&result, None, None, Some(&tools));
        let dict = value.as_dict().expect("result dict");

        assert_eq!(dict.get("text").map(VmValue::display).as_deref(), Some(""));
        assert_eq!(dict.get("prose").map(VmValue::display).as_deref(), Some(""));
        assert_eq!(
            dict.get("visible_text").map(VmValue::display).as_deref(),
            Some("")
        );
        assert!(
            dict.get("raw_text")
                .map(VmValue::display)
                .is_some_and(|text| text.contains("<tool_call>")),
            "raw parser text must preserve the text-tool protocol source"
        );

        let Some(VmValue::List(tool_calls)) = dict.get("tool_calls") else {
            panic!("missing dispatchable tool call: {dict:?}");
        };
        assert_eq!(tool_calls.len(), 1);
        let call = tool_calls[0].as_dict().expect("tool call dict");
        assert_eq!(
            call.get("name").map(VmValue::display).as_deref(),
            Some("run")
        );
        let args = call
            .get("arguments")
            .and_then(VmValue::as_dict)
            .expect("tool call arguments");
        assert_eq!(
            args.get("command").map(VmValue::display).as_deref(),
            Some("cargo test")
        );

        let canonical = dict
            .get("canonical_text")
            .map(VmValue::display)
            .expect("canonical replay text");
        assert!(canonical.contains("<tool_call>"));
        assert!(!canonical.contains("<|channel|>"));
    }

    #[test]
    fn text_tool_protocol_done_marker_survives_clean_public_projection() {
        let mut result = mock_completion_response("hi", None);
        result.text = concat!(
            "<assistant_prose>Finished.</assistant_prose>\n",
            "<done>##DONE##</done>"
        )
        .to_string();

        let tools = run_tool_registry();
        let value = vm_build_llm_result(&result, None, None, Some(&tools));
        let dict = value.as_dict().expect("result dict");

        assert_eq!(
            dict.get("text").map(VmValue::display).as_deref(),
            Some("Finished.")
        );
        assert_eq!(
            dict.get("visible_text").map(VmValue::display).as_deref(),
            Some("Finished.")
        );
        assert!(
            dict.get("raw_text")
                .map(VmValue::display)
                .is_some_and(|text| text.contains("##DONE##")),
            "raw parser text must preserve the done sentinel"
        );
        assert_eq!(
            dict.get("parsed_done_marker")
                .map(VmValue::display)
                .as_deref(),
            Some("##DONE##")
        );
        assert_eq!(
            dict.get("done_marker").map(VmValue::display).as_deref(),
            Some("##DONE##")
        );
    }

    #[test]
    fn native_tool_call_action_only_response_does_not_leak_wrapper_text() {
        let mut result = mock_completion_response("hi", None);
        result.text = concat!(
            "<|start|>assistant<|channel|>commentary<|message|>",
            "<tool_call>\n",
            "run({ command: \"cargo test\" })\n",
            "</tool_call>",
            "<|end|>"
        )
        .to_string();
        result.tool_calls = vec![serde_json::json!({
            "id": "call_run",
            "type": "tool_call",
            "name": "run",
            "arguments": {"command": "cargo test"},
        })];

        let value = vm_build_llm_result(&result, None, None, None);
        let dict = value.as_dict().expect("result dict");

        assert_eq!(dict.get("text").map(VmValue::display).as_deref(), Some(""));
        assert_eq!(dict.get("prose").map(VmValue::display).as_deref(), Some(""));
        assert_eq!(
            dict.get("visible_text").map(VmValue::display).as_deref(),
            Some("")
        );
        let Some(VmValue::List(tool_calls)) = dict.get("tool_calls") else {
            panic!("missing native tool call: {dict:?}");
        };
        assert_eq!(tool_calls.len(), 1);
        let call = tool_calls[0].as_dict().expect("tool call dict");
        assert_eq!(
            call.get("name").map(VmValue::display).as_deref(),
            Some("run")
        );
    }
}
