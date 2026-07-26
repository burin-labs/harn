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

impl LlmResult {
    /// Price this exact provider result without collapsing unknown pricing to
    /// zero. VM-return and provider-observability projections use this contract
    /// so their usage costs cannot disagree.
    pub(crate) fn priced_cost_usd(&self) -> Option<f64> {
        let detail = crate::llm::cost::pricing_detail_for_tier(
            &self.provider,
            &self.model,
            self.served_fast,
            self.input_tokens,
        )?;
        Some(crate::llm::cost::project_call_cost(
            &detail,
            self.input_tokens,
            self.output_tokens,
            self.cache_read_tokens,
            self.cache_write_tokens,
        ))
    }

    /// True when the completion carries nothing the agent loop can act on: no
    /// visible text (whitespace-only counts as empty), no tool calls, and no
    /// thinking. This is the single definition of "committed nothing usable"
    /// shared by the dispatch-outcome booking (`resolved_dispatch`) and the
    /// empty-completion retry (`agent_observe`), so a whitespace-only or
    /// echoed-stop-sequence completion cannot book as `served` in one place
    /// while the retry misses it in another (harn#4744). Trimming is what
    /// distinguishes this from a raw `text.is_empty()`: a provider that bills
    /// tokens for whitespace still committed nothing usable.
    pub(crate) fn committed_nothing_usable(&self) -> bool {
        self.text.trim().is_empty()
            && self.tool_calls.is_empty()
            && self.thinking.as_deref().unwrap_or("").trim().is_empty()
    }
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
        crate::value::intern_key("cost_usd"),
        result
            .priced_cost_usd()
            .map_or(VmValue::Nil, VmValue::Float),
    );
    usage.insert(
        crate::value::intern_key("cache_read_tokens"),
        VmValue::Int(result.cache_read_tokens),
    );
    usage.insert(
        crate::value::intern_key("cache_write_tokens"),
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
/// `text`/`visible_text` carry only the answer, and return the
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

/// Canonical caller-facing classification of a completed (non-thrown) LLM
/// call. This is the typed answer to "what did this call actually produce?"
/// so no consumer ever re-derives it from stop-reason vocabularies or
/// token-count probing (the class of bug behind harn#4744 and the
/// billed-noncommittal S2 failure in the tool-calling north star).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LlmOutcomeKind {
    /// The model committed a normal answer and stopped cleanly.
    Complete,
    /// The response's actionable content is one or more tool calls.
    ToolUse,
    /// The provider cut generation on an output-token limit; text and
    /// especially tool-call arguments must be treated as suspect.
    Truncated,
    /// The provider refused or filtered the completion.
    Refused,
    /// The provider paused the turn (e.g. Anthropic `pause_turn`); the call
    /// should be resumed, not judged.
    Paused,
    /// The call committed nothing usable: no visible text, no tool calls,
    /// no thinking. `billed` distinguishes the paid-for flavor.
    Empty,
}

impl LlmOutcomeKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::ToolUse => "tool_use",
            Self::Truncated => "truncated",
            Self::Refused => "refused",
            Self::Paused => "paused",
            Self::Empty => "empty",
        }
    }
}

/// Single owner of the "provider cut generation on an output-token limit"
/// stop-reason vocabulary. `canonical_provider_stop_reason` /
/// `is_length_truncation` (session plane) and the response parsers all
/// delegate here so a new provider spelling is added exactly once.
pub(crate) fn stop_reason_is_length(stop_reason: &str) -> bool {
    // OpenAI-compat "length", Anthropic/Bedrock "max_tokens" (Bedrock wire
    // camelCase is normalized before it reaches LlmResult), Gemini
    // "MAX_TOKENS" (case-insensitive compare covers it).
    stop_reason.eq_ignore_ascii_case("length")
        || stop_reason.eq_ignore_ascii_case("max_tokens")
        || stop_reason.eq_ignore_ascii_case("max_output_tokens")
        || stop_reason.eq_ignore_ascii_case("model_length")
}

fn stop_reason_is_refusal(stop_reason: &str) -> bool {
    // Anthropic "refusal", OpenAI "content_filter", Bedrock
    // "content_filtered", Gemini safety finish reasons.
    [
        "refusal",
        "content_filter",
        "content_filtered",
        "safety",
        "recitation",
        "prohibited_content",
        "blocklist",
        "image_safety",
    ]
    .iter()
    .any(|reason| stop_reason.eq_ignore_ascii_case(reason))
}

/// Classify the canonical `outcome` for a completed call. `has_tool_calls`
/// covers BOTH provider-native and text-protocol-parsed calls (the struct's
/// own `tool_calls` only carries native ones), and `has_visible_text` /
/// `has_thinking` reflect the post-projection channels the caller can act on.
pub(crate) fn classify_llm_outcome(
    result: &LlmResult,
    has_tool_calls: bool,
    has_visible_text: bool,
    has_thinking: bool,
) -> LlmOutcomeKind {
    let stop_reason = result.stop_reason.as_deref().unwrap_or("");
    if !has_visible_text && !has_tool_calls && !has_thinking {
        return LlmOutcomeKind::Empty;
    }
    if stop_reason_is_refusal(stop_reason) {
        return LlmOutcomeKind::Refused;
    }
    if stop_reason.eq_ignore_ascii_case("pause_turn") {
        return LlmOutcomeKind::Paused;
    }
    if stop_reason_is_length(stop_reason) {
        return LlmOutcomeKind::Truncated;
    }
    if has_tool_calls {
        return LlmOutcomeKind::ToolUse;
    }
    LlmOutcomeKind::Complete
}

fn build_outcome_dict(kind: LlmOutcomeKind, result: &LlmResult) -> crate::value::DictMap {
    let mut outcome = crate::value::DictMap::new();
    outcome.put_str("kind", kind.as_str());
    // `billed` is what turns an `empty` outcome into the S2
    // "billed-noncommittal" signal; carried on every outcome so callers
    // never re-derive it from usage.
    outcome.insert(
        crate::value::intern_key("billed"),
        VmValue::Bool(result.input_tokens > 0 || result.output_tokens > 0),
    );
    outcome
}

/// Text-channel projection of one provider result: the visible text with
/// inline reasoning split out, plus the text-tool parse of it.
///
/// Built ONCE, at the async boundary, and threaded into the sync assembly
/// below and the observability sink. Which tags and markers mean "tool call"
/// is a dialect question answered in the Harn stdlib, so the parse can only
/// happen where an `AsyncBuiltinCtx` exists. Re-deriving it in three sync
/// helpers is also what made one response text get parsed three times per
/// provider call.
pub(crate) struct LlmTextProjection {
    pub(crate) visible_text_src: String,
    pub(crate) inline_reasoning: Option<String>,
    pub(crate) parsed: Option<crate::llm::tools::TextToolParseResult>,
    pub(crate) public_text: String,
    pub(crate) visible_text: String,
}

impl LlmTextProjection {
    /// The merged tool-call view: provider-native calls when present,
    /// otherwise the calls recovered from the text channel.
    pub(crate) fn merged_tool_calls(&self, result: &LlmResult) -> Vec<serde_json::Value> {
        if !result.tool_calls.is_empty() {
            return result.tool_calls.clone();
        }
        self.parsed
            .as_ref()
            .map(|parse| parse.calls.clone())
            .unwrap_or_default()
    }
}

pub(crate) async fn build_llm_text_projection(
    _ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    result: &LlmResult,
    tools_val: Option<&VmValue>,
) -> LlmTextProjection {
    let (visible_text_src, inline_reasoning) = split_inline_reasoning_if_capable(result);
    let parsed = parse_visible_text_tools(&visible_text_src, tools_val, &result.tool_calls);

    let has_native_tool_calls = !result.tool_calls.is_empty();
    let parsed_has_calls = parsed.as_ref().is_some_and(|parse| !parse.calls.is_empty());

    let public_text = match parsed.as_ref() {
        Some(parse) if !parse.prose.is_empty() => parse.prose.clone(),
        Some(_) if parsed_has_calls || has_native_tool_calls => String::new(),
        _ => visible_text_src.clone(),
    };
    let visible_text = if parsed_has_calls || has_native_tool_calls || tools_val.is_some() {
        public_text.clone()
    } else {
        crate::visible_text::sanitize_visible_assistant_text(&visible_text_src, false)
    };

    LlmTextProjection {
        visible_text_src,
        inline_reasoning,
        parsed,
        public_text,
        visible_text,
    }
}

/// Parse one derived candidate string under the text-tool grammar.
///
/// Separate from [`build_llm_text_projection`] because the structured-output
/// extractor parses strings the provider never sent (tool arguments, public
/// blocks), so there is no projection to reuse. This and
/// [`parse_visible_text_tools`] are the two seams the dialect layer is reached
/// through.
pub(crate) async fn parse_candidate_text_tools(
    _ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    text: &str,
    tools_val: Option<&VmValue>,
) -> crate::llm::tools::TextToolParseResult {
    crate::llm::tools::parse_text_tool_calls_with_tools(text, tools_val)
}

fn parse_visible_text_tools(
    visible_text_src: &str,
    tools_val: Option<&VmValue>,
    native_tool_calls: &[serde_json::Value],
) -> Option<crate::llm::tools::TextToolParseResult> {
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
    has_text_tool_protocol
        .then(|| crate::llm::tools::parse_text_tool_calls_with_tools(visible_text_src, tools_val))
}

/// Build a projection for tests that assemble a result dict directly.
///
/// The parse is pure computation, so driving the async producer with a bare
/// executor is sound here and keeps tests on the same seam production uses.
#[cfg(test)]
pub(crate) fn test_text_projection(
    result: &LlmResult,
    tools_val: Option<&VmValue>,
) -> LlmTextProjection {
    futures::executor::block_on(build_llm_text_projection(None, result, tools_val))
}

pub(crate) fn vm_build_llm_result(
    result: &LlmResult,
    parsed_json: Option<VmValue>,
    transcript: Option<VmValue>,
    projection: &LlmTextProjection,
) -> VmValue {
    use crate::stdlib::json_to_vm_value;

    // Capability-gated split of inline `<think>` reasoning out of the visible
    // text channel. `visible_text_src` is the answer with inline reasoning
    // removed (or the original text unchanged for non-inline routes);
    // `inline_reasoning` is the extracted reasoning to fold into the channel.
    let visible_text_src = projection.visible_text_src.as_str();
    let inline_reasoning = projection.inline_reasoning.clone();

    let mut dict = crate::value::DictMap::new();
    dict.put_str("model", result.model.as_str());
    dict.put_str("provider", result.provider.as_str());
    // `usage` is the single owner of ALL accounting (tokens, cache, cost,
    // served tier, provider timings). No accounting key is duplicated at the
    // top level: one spelling, one place.
    let mut usage = build_usage_dict(result);
    // Provider-side timings (Ollama load_duration, prompt_eval_duration,
    // eval_duration; OpenAI usage; llama.cpp `timings`). Evals key off
    // `usage.provider_telemetry` for cold-vs-steady-state and
    // prefill-vs-generation breakdowns; absent fields stay absent rather than
    // collapsing to zero.
    if let Some(telemetry_dict) = result.telemetry.as_vm_dict() {
        usage.insert(
            crate::value::intern_key("provider_telemetry"),
            telemetry_dict,
        );
    }
    dict.insert(crate::value::intern_key("usage"), VmValue::dict(usage));

    if let Some(json_val) = parsed_json {
        dict.insert(crate::value::intern_key("data"), json_val);
    }

    // Keep parsing available for tool-calling responses so llm_call can
    // expose canonical/tool metadata, but do not surface tagged-protocol
    // violations for ordinary plain-text completions with no tools.
    dict.put_str("raw_text", visible_text_src);
    dict.put_str("text", projection.public_text.as_str());

    let merged_tool_calls: Vec<serde_json::Value> = projection.merged_tool_calls(result);
    // Always present (possibly empty) so consumers never branch on key
    // existence to mean "no tool calls".
    let calls: Vec<VmValue> = merged_tool_calls.iter().map(json_to_vm_value).collect();
    dict.insert(
        crate::value::intern_key("tool_calls"),
        VmValue::List(std::sync::Arc::new(calls)),
    );
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
        }
        if !parse.canonical.is_empty() {
            dict.put_str("canonical_text", parse.canonical.as_str());
        }
    }

    let merged_thinking = merge_reasoning_channels(&result.thinking, inline_reasoning);
    if let Some(ref thinking) = merged_thinking {
        dict.put_str("thinking", thinking.as_str());
    }
    if let Some(ref summary) = result.thinking_summary {
        dict.put_str("thinking_summary", summary.as_str());
    }

    if let Some(ref stop_reason) = result.stop_reason {
        dict.put_str("stop_reason", stop_reason.as_str());
    }

    // Canonical outcome classification. "Usable content" mirrors
    // `LlmResult::committed_nothing_usable` (trim-based, harn#4744) but also
    // counts text-protocol-parsed tool calls, which only exist
    // post-projection.
    let outcome_kind = classify_llm_outcome(
        result,
        !merged_tool_calls.is_empty(),
        !visible_text_src.trim().is_empty(),
        merged_thinking
            .as_deref()
            .is_some_and(|thinking| !thinking.trim().is_empty()),
    );
    dict.insert(
        crate::value::intern_key("outcome"),
        VmValue::dict(build_outcome_dict(outcome_kind, result)),
    );
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
    fn committed_nothing_usable_trims_whitespace() {
        // harn#4744: empty AND whitespace-only completions committed nothing the
        // loop can use — trimming is what catches the whitespace / echoed-stop
        // case a raw `text.is_empty()` would miss. (mock_completion_response
        // always synthesizes non-empty text, so override it for the empty cases.)
        let mut empty = mock_completion_response("x", None);
        empty.text = String::new();
        assert!(empty.committed_nothing_usable());
        let mut whitespace = mock_completion_response("x", None);
        whitespace.text = "   \n\t".to_string();
        assert!(whitespace.committed_nothing_usable());

        // Real visible content, a tool call, or thinking all count as usable.
        assert!(!mock_completion_response("hello", None).committed_nothing_usable());
        let mut with_tool = mock_completion_response("x", None);
        with_tool.text = String::new();
        with_tool.tool_calls = vec![serde_json::json!({"id": "t1", "name": "run"})];
        assert!(!with_tool.committed_nothing_usable());
        let mut with_thinking = mock_completion_response("x", None);
        with_thinking.text = String::new();
        with_thinking.thinking = Some("reasoning".to_string());
        assert!(!with_thinking.committed_nothing_usable());
    }

    #[test]
    fn envelope_top_level_keys_are_canonical() {
        // THE canonical envelope guard: exactly one spelling per field, no
        // accounting outside `usage`, no alias keys. A new top-level key is a
        // contract change and must be added here deliberately.
        let allowed: std::collections::BTreeSet<&str> = [
            "model",
            "provider",
            "usage",
            "outcome",
            "data",
            "text",
            "raw_text",
            "visible_text",
            "canonical_text",
            "thinking",
            "thinking_summary",
            "stop_reason",
            "tool_calls",
            "native_tool_calls",
            "protocol_violations",
            "tool_parse_errors",
            "done_marker",
            "provider_response_id",
            "transcript",
            "blocks",
            "logprobs",
            // Attached post-build by `call.rs::attach_routing_block`.
            "routing",
        ]
        .into_iter()
        .collect();

        let mut result = mock_completion_response("hi", None);
        result.thinking = Some("plan".to_string());
        result.thinking_summary = Some("summary".to_string());
        result.logprobs = vec![serde_json::json!({"token": "x"})];
        let value = vm_build_llm_result(
            &result,
            None,
            None,
            &crate::llm::api::test_text_projection(&result, None),
        );
        let dict = value.as_dict().expect("result dict");
        for key in dict.keys() {
            assert!(
                allowed.contains(key.as_str()),
                "non-canonical top-level envelope key: {key}"
            );
        }
        // Single-owner accounting: usage holds tokens, nothing else does.
        let usage = dict
            .get("usage")
            .and_then(VmValue::as_dict)
            .expect("usage dict");
        for key in [
            "input_tokens",
            "output_tokens",
            "cost_usd",
            "cache_read_tokens",
            "cache_write_tokens",
            "cache_hit_ratio",
            "cache_visibility",
            "cache_savings_usd",
            "served_fast",
        ] {
            assert!(usage.get(key).is_some(), "usage must own {key}");
            assert!(
                dict.get(key).is_none(),
                "{key} must not be duplicated at the top level"
            );
        }
        assert!(
            usage.get("cache_creation_input_tokens").is_none(),
            "the cache_write_tokens alias must not exist"
        );
        for alias in ["prose", "private_reasoning", "parsed_done_marker"] {
            assert!(dict.get(alias).is_none(), "alias key {alias} must be dead");
        }
    }

    #[test]
    fn outcome_classification_covers_the_vocabulary() {
        use super::{classify_llm_outcome, LlmOutcomeKind};

        let served = mock_completion_response("hi", None);
        assert_eq!(
            classify_llm_outcome(&served, false, true, false),
            LlmOutcomeKind::Complete
        );
        assert_eq!(
            classify_llm_outcome(&served, true, true, false),
            LlmOutcomeKind::ToolUse
        );

        let mut truncated = mock_completion_response("hi", None);
        truncated.stop_reason = Some("max_tokens".to_string());
        assert_eq!(
            classify_llm_outcome(&truncated, false, true, false),
            LlmOutcomeKind::Truncated
        );
        // Length-stop with tool calls is still truncated: partial tool-call
        // arguments must not be trusted as a clean tool_use.
        assert_eq!(
            classify_llm_outcome(&truncated, true, true, false),
            LlmOutcomeKind::Truncated
        );
        truncated.stop_reason = Some("MAX_TOKENS".to_string());
        assert_eq!(
            classify_llm_outcome(&truncated, false, true, false),
            LlmOutcomeKind::Truncated
        );

        let mut refused = mock_completion_response("hi", None);
        refused.stop_reason = Some("content_filter".to_string());
        assert_eq!(
            classify_llm_outcome(&refused, false, true, false),
            LlmOutcomeKind::Refused
        );

        let mut paused = mock_completion_response("hi", None);
        paused.stop_reason = Some("pause_turn".to_string());
        assert_eq!(
            classify_llm_outcome(&paused, false, true, false),
            LlmOutcomeKind::Paused
        );

        // Nothing usable at all: empty wins over every stop_reason reading.
        let mut empty = mock_completion_response("hi", None);
        empty.stop_reason = Some("end_turn".to_string());
        assert_eq!(
            classify_llm_outcome(&empty, false, false, false),
            LlmOutcomeKind::Empty
        );
        // Thinking-only responses committed something usable.
        assert_eq!(
            classify_llm_outcome(&empty, false, false, true),
            LlmOutcomeKind::Complete
        );
    }

    #[test]
    fn billed_empty_outcome_is_surfaced_on_the_envelope() {
        let mut result = mock_completion_response("x", None);
        result.text = "   \n".to_string();
        result.input_tokens = 100;
        result.output_tokens = 3;
        let value = vm_build_llm_result(
            &result,
            None,
            None,
            &crate::llm::api::test_text_projection(&result, None),
        );
        let dict = value.as_dict().expect("result dict");
        let outcome = dict
            .get("outcome")
            .and_then(VmValue::as_dict)
            .expect("outcome dict");
        assert_eq!(
            outcome.get("kind").map(VmValue::display).as_deref(),
            Some("empty")
        );
        assert!(
            matches!(outcome.get("billed"), Some(VmValue::Bool(true))),
            "billed must be true, got: {:?}",
            outcome.get("billed")
        );
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
    fn usage_cost_preserves_priced_and_unpriced_results() {
        let _guard = crate::llm::env_guard();
        crate::llm_config::clear_user_overrides();

        let mut priced = mock_completion_response("hi", None);
        priced.provider = "anthropic".to_string();
        priced.model = "claude-sonnet-4-20250514".to_string();
        priced.input_tokens = 1_000;
        priced.output_tokens = 1_000;
        let uncached_cost = priced.priced_cost_usd().expect("catalog-priced result");
        priced.cache_read_tokens = 800;
        let expected_cost = priced.priced_cost_usd().expect("cache-priced result");
        assert!(expected_cost < uncached_cost);
        let priced_value = vm_build_llm_result(
            &priced,
            None,
            None,
            &crate::llm::api::test_text_projection(&priced, None),
        );
        let priced_dict = priced_value.as_dict().expect("result dict");
        let Some(VmValue::Dict(priced_usage)) = priced_dict.get("usage") else {
            panic!("missing usage dict: {priced_dict:?}");
        };
        let priced_usage_json = crate::llm::vm_value_to_json(&VmValue::Dict(priced_usage.clone()));
        assert_eq!(
            priced_usage_json["cost_usd"],
            serde_json::json!(expected_cost)
        );
        assert_eq!(priced_usage_json["input_tokens"], serde_json::json!(1_000));
        assert_eq!(priced_usage_json["output_tokens"], serde_json::json!(1_000));

        let mut unpriced = priced;
        unpriced.provider = "nonexistent_provider".to_string();
        unpriced.model = "ghost-model".to_string();
        assert_eq!(unpriced.priced_cost_usd(), None);
        let unpriced_value = vm_build_llm_result(
            &unpriced,
            None,
            None,
            &crate::llm::api::test_text_projection(&unpriced, None),
        );
        let unpriced_dict = unpriced_value.as_dict().expect("result dict");
        let Some(VmValue::Dict(unpriced_usage)) = unpriced_dict.get("usage") else {
            panic!("missing usage dict: {unpriced_dict:?}");
        };
        let unpriced_usage_json =
            crate::llm::vm_value_to_json(&VmValue::Dict(unpriced_usage.clone()));
        assert_eq!(
            unpriced_usage_json
                .as_object()
                .and_then(|usage| usage.get("cost_usd")),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(
            unpriced_usage_json["cache_savings_usd"],
            serde_json::json!(0.0)
        );

        let mut zero_priced = unpriced;
        zero_priced.provider = "local".to_string();
        zero_priced.model = "no-such-local-model".to_string();
        assert_eq!(zero_priced.priced_cost_usd(), Some(0.0));
        let zero_priced_value = vm_build_llm_result(
            &zero_priced,
            None,
            None,
            &crate::llm::api::test_text_projection(&zero_priced, None),
        );
        let zero_priced_dict = zero_priced_value.as_dict().expect("result dict");
        let Some(VmValue::Dict(zero_priced_usage)) = zero_priced_dict.get("usage") else {
            panic!("missing usage dict: {zero_priced_dict:?}");
        };
        let zero_priced_usage_json =
            crate::llm::vm_value_to_json(&VmValue::Dict(zero_priced_usage.clone()));
        assert_eq!(zero_priced_usage_json["cost_usd"], serde_json::json!(0.0));
    }

    #[test]
    fn provider_reasoning_text_is_not_parsed_as_executable_tool_calls() {
        let mut result = mock_completion_response("hi", None);
        result.text = "<user_response>Done.</user_response>".to_string();
        result.thinking = Some(
            "<tool_call>\nrun({ command: \"echo should-not-run\" })\n</tool_call>".to_string(),
        );

        let value = vm_build_llm_result(
            &result,
            None,
            None,
            &crate::llm::api::test_text_projection(&result, None),
        );
        let dict = value.as_dict().expect("result dict");

        let Some(VmValue::List(tool_calls)) = dict.get("tool_calls") else {
            panic!("tool_calls must always be present as a list: {dict:?}");
        };
        assert!(
            tool_calls.is_empty(),
            "provider reasoning must not become executable tool calls"
        );
        assert_eq!(
            dict.get("text").map(VmValue::display).as_deref(),
            Some("Done.")
        );
        assert_eq!(
            dict.get("thinking").map(VmValue::display).as_deref(),
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
        let value = vm_build_llm_result(
            &result,
            None,
            None,
            &crate::llm::api::test_text_projection(&result, Some(&tools)),
        );
        let dict = value.as_dict().expect("result dict");

        assert_eq!(dict.get("text").map(VmValue::display).as_deref(), Some(""));
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
        let value = vm_build_llm_result(
            &result,
            None,
            None,
            &crate::llm::api::test_text_projection(&result, Some(&tools)),
        );
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

        let value = vm_build_llm_result(
            &result,
            None,
            None,
            &crate::llm::api::test_text_projection(&result, None),
        );
        let dict = value.as_dict().expect("result dict");

        assert_eq!(dict.get("text").map(VmValue::display).as_deref(), Some(""));
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
