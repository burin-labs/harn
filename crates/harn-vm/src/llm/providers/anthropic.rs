//! Anthropic Messages API provider (Claude models).

use std::cell::RefCell;
use std::collections::HashSet;

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult, ReasoningEffort, ThinkingConfig};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::llm::providers::common::parse_major_minor_tail;
use crate::llm::providers::schema_compat::{
    sanitize_schema_for_provider, SchemaCompatProfile, SchemaSurface,
};
use crate::value::VmError;

pub(crate) const ANTHROPIC_INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";

/// Anthropic beta header value that unlocks the native `computer_20251124`
/// computer-use tool. Requested via the `anthropic-beta` header whenever a
/// `computer` tool is present in the request surface on a `native_anthropic`
/// route. See [`crate::llm::api::options::LlmCallOptions::anthropic_beta_features_for_request`].
pub(crate) const COMPUTER_USE_BETA: &str = "computer-use-2025-11-24";

thread_local! {
    static ANTHROPIC_PREFILL_WARN_ONCE: RefCell<HashSet<String>> =
        RefCell::new(HashSet::new());
    static ANTHROPIC_SAMPLING_WARN_ONCE: RefCell<HashSet<String>> =
        RefCell::new(HashSet::new());
    static ANTHROPIC_ADAPTIVE_WARN_ONCE: RefCell<HashSet<String>> =
        RefCell::new(HashSet::new());
    static ANTHROPIC_FORCED_JSON_WARN_ONCE: RefCell<HashSet<String>> =
        RefCell::new(HashSet::new());
}

/// Parse the (major, minor) generation out of a Claude model ID. Handles
/// both dash-separated names like `claude-opus-4-7` / `claude-sonnet-4-6`
/// and dotted variants like `claude-opus-4.7` (OpenRouter, some proxies),
/// plus dated IDs like `claude-haiku-4-5-20251001` and single-component
/// generations like `claude-fable-5` → (5, 0).
///
/// Returns `None` if the ID isn't a known Claude shape (e.g. `gpt-4o`),
/// including non-numeric tails like `claude-mythos-preview`.
pub(crate) fn claude_generation(model: &str) -> Option<(u32, u32)> {
    let lower = model.to_lowercase();
    if !is_claude_model_id(&lower) {
        return None;
    }
    if let Some(tail) = lower.split("claude-").nth(1) {
        if tail.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            return parse_major_minor_tail(tail);
        }
    }
    // fable/mythos (the Mythos-class tier above Opus, generation 5+) share
    // the Opus 4.7+ request surface, so the >= (4, 6) / (4, 7) guards below
    // must fire for them too.
    for family in ["opus", "sonnet", "haiku", "fable", "mythos"] {
        let needle = format!("{family}-");
        if let Some(idx) = lower.find(&needle) {
            return parse_major_minor_tail(&lower[idx + needle.len()..]);
        }
    }
    None
}

fn is_claude_model_id(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.starts_with("claude-") || lower.contains("/claude-") || lower.contains(".claude-")
}

/// Canonical message-level keys the Anthropic Messages API accepts on an
/// entry of `messages[]`. Any other key (e.g. the storage-only `reasoning`
/// metadata that `build_assistant_response_message` stamps onto durable
/// assistant turns, or OpenAI-shape `tool_calls`) triggers a non-retryable
/// HTTP 400: `messages.N.<key>: Extra inputs are not permitted`. Anthropic
/// tool turns are projected as `tool_use` content blocks, not a top-level
/// `tool_calls` array, so this set is intentionally minimal. `cache_control`
/// is permitted at the message level for prompt caching.
const ANTHROPIC_MESSAGE_KEYS: &[&str] = &["role", "content", "cache_control"];

/// True for Claude 4.6 and later — the generation where Anthropic
/// deprecated the assistant-prefill feature. Opus 4.7, Sonnet 4.6/4.7,
/// any future -4.8+ model all return 400 when the last message has
/// role=assistant.
fn is_claude_4_6_or_later(model: &str) -> bool {
    matches!(claude_generation(model), Some((major, minor)) if (major, minor) >= (4, 6))
}

/// True for Opus 4.7+ — the generation where Anthropic made non-default
/// `temperature`, `top_p`, and `top_k` return HTTP 400. Sonnet/Haiku 4.7
/// will inherit this restriction if they ship with the same API surface.
fn model_rejects_sampling_params(model: &str) -> bool {
    let lower = model.to_lowercase();
    // Apply to every 4.7+ Claude. The migration guide scopes this to Opus
    // 4.7 today, but the family-wide pattern has been consistent and we'd
    // rather drop a non-default sampling param than hit a 400 in prod.
    matches!(claude_generation(&lower), Some((major, minor)) if (major, minor) >= (4, 7))
}

/// True for Opus 4.7+ — the generation where extended thinking was
/// replaced by adaptive thinking. Passing `thinking.type = "enabled"` to
/// one of these models is a 400. We transparently rewrite the payload to
/// `{type: "adaptive"}` and emit a one-time warning.
fn model_requires_adaptive_thinking(model: &str) -> bool {
    let lower = model.to_lowercase();
    matches!(claude_generation(&lower), Some((major, minor)) if (major, minor) >= (4, 7))
}

/// True for Claude models whose adaptive thinking is on by default. These
/// models don't need a `thinking: {type:"adaptive"}` request field when
/// `output_config.effort` is enough to steer the default-on reasoning.
fn model_defaults_to_adaptive_thinking(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.contains("claude-fable-")
        || lower.contains("claude-mythos-")
        || lower.contains("claude-sonnet-5")
}

/// Fable/Mythos always think and reject an explicit disabled thinking config.
/// Sonnet 5 also defaults thinking on, but it accepts
/// `thinking: {type:"disabled"}` as the explicit off switch.
fn model_rejects_disabled_thinking(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.contains("claude-fable-") || lower.contains("claude-mythos-")
}

fn model_supports_anthropic_effort(model: &str) -> bool {
    crate::llm::capabilities::lookup("anthropic", model).reasoning_effort_supported
}

fn anthropic_effort_value(level: ReasoningEffort) -> Option<&'static str> {
    match level {
        ReasoningEffort::None => None,
        // Harn's provider-neutral policy has a `minimal` notch for OpenAI.
        // Anthropic's current public effort surface starts at low, so floor
        // any direct minimal request to the lowest accepted Anthropic level.
        ReasoningEffort::Minimal | ReasoningEffort::Low => Some("low"),
        ReasoningEffort::Medium => Some("medium"),
        ReasoningEffort::High => Some("high"),
        ReasoningEffort::XHigh => Some("xhigh"),
        ReasoningEffort::Max => Some("max"),
    }
}

fn set_output_config_effort(body: &mut serde_json::Value, effort: &str) {
    let Some(body_object) = body.as_object_mut() else {
        return;
    };
    let output_config = body_object
        .entry("output_config")
        .or_insert_with(|| serde_json::json!({}));
    if !output_config.is_object() {
        *output_config = serde_json::json!({});
    }
    output_config["effort"] = serde_json::json!(effort);
}

fn model_supports_anthropic_prefill(model: &str) -> bool {
    !is_claude_4_6_or_later(model)
}

/// True for Claude models that support the `tool_search_tool_*_20251119`
/// server-side tools and the `defer_loading: true` flag on tool definitions.
/// Per Anthropic's tool-search docs: Claude Mythos Preview, Sonnet 4.0+,
/// Opus 4.0+, Haiku 4.5+.
#[allow(dead_code)]
pub(crate) fn claude_model_supports_tool_search(model: &str) -> bool {
    let lower = model.to_lowercase();
    match claude_generation(&lower) {
        Some((major, minor)) => {
            if lower.contains("haiku-") {
                // Haiku needs 4.5+.
                (major, minor) >= (4, 5)
            } else {
                // Opus and Sonnet: 4.0+.
                major >= 4
            }
        }
        None => false,
    }
}

fn warn_anthropic_prefill_skipped(model: &str) {
    ANTHROPIC_PREFILL_WARN_ONCE.with(|seen| {
        let mut seen = seen.borrow_mut();
        if seen.insert(model.to_string()) {
            crate::events::log_warn(
                "llm.prefill",
                &format!(
                    "assistant prefill requested for {model}, but Anthropic 4.6+ \
                     deprecated prefill; sending without it",
                ),
            );
        }
    });
}

fn warn_sampling_stripped(model: &str) {
    ANTHROPIC_SAMPLING_WARN_ONCE.with(|seen| {
        let mut seen = seen.borrow_mut();
        if seen.insert(model.to_string()) {
            crate::events::log_warn(
                "llm.sampling",
                &format!(
                    "temperature/top_p/top_k supplied for {model}, but this Anthropic \
                     request surface rejects non-default sampling params on newer \
                     Claude models or when thinking is active; stripping them from \
                     the request",
                ),
            );
        }
    });
}

/// Remove Anthropic sampling parameters when the resolved Claude request
/// surface rejects them. This is the single egress policy used by the primary
/// Messages builder and by legacy/override paths that can otherwise reinsert
/// provider options after provider-specific body construction.
pub(crate) fn strip_unsupported_sampling_params(
    body: &mut serde_json::Value,
    model: &str,
    thinking: &ThinkingConfig,
) {
    let strip_sampling = model_rejects_sampling_params(model)
        || !thinking.is_disabled()
        || body_activates_anthropic_thinking(body);
    if !strip_sampling {
        return;
    }

    let Some(object) = body.as_object_mut() else {
        return;
    };
    let had_sampling = object.contains_key("temperature")
        || object.contains_key("top_p")
        || object.contains_key("top_k");
    if had_sampling {
        warn_sampling_stripped(model);
        object.remove("temperature");
        object.remove("top_p");
        object.remove("top_k");
    }
}

/// Bedrock Converse nests sampling parameters under `inferenceConfig`, but
/// Bedrock-hosted Claude follows the same Anthropic sampling restrictions as
/// direct Claude request surfaces.
pub(crate) fn strip_unsupported_bedrock_converse_sampling_params(
    body: &mut serde_json::Value,
    model: &str,
    thinking: &ThinkingConfig,
) {
    if !is_claude_model_id(model) {
        return;
    }
    let strip_sampling = model_rejects_sampling_params(model)
        || !thinking.is_disabled()
        || body_activates_anthropic_thinking(body);
    if !strip_sampling {
        return;
    }

    let Some(body_object) = body.as_object_mut() else {
        return;
    };
    let Some(inference) = body_object
        .get_mut("inferenceConfig")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    let had_temperature = inference.remove("temperature").is_some();
    let had_top_p = inference.remove("topP").is_some();
    let had_top_k = inference.remove("topK").is_some();
    let had_sampling = had_temperature || had_top_p || had_top_k;
    if had_sampling {
        warn_sampling_stripped(model);
    }
    if inference.is_empty() {
        body_object.remove("inferenceConfig");
    }
}

fn body_activates_anthropic_thinking(body: &serde_json::Value) -> bool {
    value_activates_anthropic_thinking(body.get("thinking"))
        || output_config_activates_thinking(body.get("output_config"))
        || body
            .get("additionalModelRequestFields")
            .is_some_and(|fields| {
                value_activates_anthropic_thinking(fields.get("thinking"))
                    || output_config_activates_thinking(fields.get("output_config"))
            })
}

fn value_activates_anthropic_thinking(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(enabled)) => *enabled,
        Some(serde_json::Value::String(mode)) => {
            !mode.eq_ignore_ascii_case("disabled") && !mode.eq_ignore_ascii_case("none")
        }
        Some(serde_json::Value::Object(object)) => object
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|mode| {
                !mode.eq_ignore_ascii_case("disabled") && !mode.eq_ignore_ascii_case("none")
            }),
        Some(serde_json::Value::Null) | None => false,
        Some(_) => true,
    }
}

fn output_config_activates_thinking(value: Option<&serde_json::Value>) -> bool {
    value
        .and_then(|config| config.get("effort"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|effort| !effort.eq_ignore_ascii_case("none") && !effort.is_empty())
}

fn warn_adaptive_thinking_rewrite(model: &str) {
    ANTHROPIC_ADAPTIVE_WARN_ONCE.with(|seen| {
        let mut seen = seen.borrow_mut();
        if seen.insert(model.to_string()) {
            crate::events::log_warn(
                "llm.thinking",
                &format!(
                    "extended-thinking payload supplied for {model}, but Anthropic \
                     Opus 4.7+ removed that surface; rewriting to \
                     `thinking: {{type: adaptive}}` (budget_tokens ignored)",
                ),
            );
        }
    });
}

/// Zero-cost unit struct for the Anthropic provider.
pub(crate) struct AnthropicProvider;

impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn is_anthropic_style(&self) -> bool {
        true
    }

    fn supports_cache(&self) -> bool {
        true
    }

    fn supports_thinking(&self, model: &str) -> bool {
        !crate::llm::capabilities::lookup(self.name(), model)
            .thinking_modes
            .is_empty()
    }

    // `supports_defer_loading` and `native_tool_search_variants` are
    // served by the default trait impl, which reads the data-driven
    // capability matrix in `capabilities.toml`. The old model-gate
    // logic (Claude 4.0+ for Opus/Sonnet, 4.5+ for Haiku) is now one
    // row per family in that file.
}

impl LlmProviderChat for AnthropicProvider {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResult, VmError>> + 'a>> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

impl AnthropicProvider {
    pub(crate) fn classify_http_error(
        status: reqwest::StatusCode,
        retry_after: Option<&str>,
        body: &str,
    ) -> crate::llm::api::LlmErrorInfo {
        crate::llm::api::classify_provider_http_error("anthropic", status, retry_after, body)
    }

    /// Build the Anthropic-style request body.
    pub(crate) fn build_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let anthropic_max = if opts.max_tokens > 0 {
            opts.max_tokens
        } else {
            8192
        };
        let messages: Vec<serde_json::Value> = opts
            .messages
            .iter()
            .cloned()
            // Cross-provider escalation choke point: a cheap OpenAI/Ollama-dialect
            // primary records tool results as top-level `role:"tool"` messages
            // (`{role:"tool", tool_call_id, content}`). When escalation switches
            // the provider to Anthropic and replays that history, Anthropic 400s
            // with `messages: Unexpected role "tool"` — it represents a tool
            // result as a `role:"user"` message carrying a `tool_result` content
            // block, never a top-level `role:"tool"`. Translate at the egress
            // boundary so BOTH carried-forward primary results and any synthesized
            // ones are covered, regardless of which path produced them. Runs
            // before the retain/`anthropic_content` map below so the id survives
            // (it lives in `tool_call_id`, which is NOT an `ANTHROPIC_MESSAGE_KEYS`
            // member and would otherwise be stripped) and before
            // `enforce_tool_result_adjacency` so the real result pairs with its
            // `tool_use` block instead of being masked by a placeholder.
            .map(anthropic_translate_tool_role_message)
            // ASSISTANT half of the same escalation bridge: render the primary's
            // OpenAI-dialect top-level `tool_calls` as Anthropic `tool_use`
            // content blocks with the SAME ids, so every translated `tool_result`
            // has its corresponding `tool_use`. Must also run before the retain
            // below, which strips the top-level `tool_calls` key. Without this,
            // Anthropic 400s: `unexpected tool_use_id found in tool_result blocks
            // ... Each tool_result block must have a corresponding tool_use`.
            .map(anthropic_translate_assistant_tool_calls)
            .filter_map(|mut message| {
                if let Some(object) = message.as_object_mut() {
                    if let Some(content) = object.get("content").cloned() {
                        let content = drop_anthropic_whitespace_text_blocks(
                            crate::llm::content::anthropic_content(&content),
                        );
                        object.insert("content".to_string(), content);
                    }
                    // Durable transcript turns carry storage-only metadata
                    // keys (e.g. `reasoning` from
                    // `build_assistant_response_message`, OpenAI-shape
                    // `tool_calls`, `private_reasoning`). The Anthropic
                    // Messages API rejects ANY non-canonical message key with
                    // a non-retryable HTTP 400 (`messages.N.reasoning: Extra
                    // inputs are not permitted`). Strip everything except the
                    // canonical message-level fields at this egress boundary
                    // ONLY — the persisted transcript shape is unchanged, so
                    // replay and other providers' adapters still see
                    // `message.reasoning`.
                    object.retain(|key, _| ANTHROPIC_MESSAGE_KEYS.contains(&key.as_str()));
                }
                if is_empty_anthropic_message(&message) {
                    None
                } else {
                    Some(message)
                }
            })
            .collect();
        let mut messages = enforce_tool_result_adjacency(messages);
        if let Some(ref prefill) = opts.prefill {
            // Claude 4.6+ deprecated the assistant-prefill feature and
            // returns HTTP 400 when the final message is role=assistant.
            // Skip the prefill for those models with a one-time warning
            // rather than fighting the deprecation.
            if model_supports_anthropic_prefill(&opts.model) {
                messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": prefill,
                }));
            } else {
                warn_anthropic_prefill_skipped(&opts.model);
            }
        }
        let wire_model = crate::llm_config::wire_model_id(&opts.model);
        let mut body = serde_json::json!({
            "model": wire_model,
            "messages": messages,
            "max_tokens": anthropic_max,
        });
        if opts.cache {
            // Anthropic automatic prompt caching now applies at the
            // top-level request and caches the stable prefix across
            // tools, system, and messages for multi-turn conversations.
            body["cache_control"] = serde_json::json!({"type": "ephemeral"});
        }
        if let Some(ref sys) = opts.system {
            body["system"] = serde_json::json!(sys);
        }
        if let Some(temp) = opts.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(top_p) = opts.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(top_k) = opts.top_k {
            body["top_k"] = serde_json::json!(top_k);
        }
        strip_unsupported_sampling_params(&mut body, &opts.model, &opts.thinking);
        if let Some(ref stop) = opts.stop {
            body["stop_sequences"] = serde_json::json!(stop);
        }
        if let Some(ref tools) = opts.native_tools {
            if !tools.is_empty() {
                let sanitized: Vec<serde_json::Value> = tools
                    .iter()
                    .map(|tool| {
                        sanitize_anthropic_tool_for_request(&opts.provider, &opts.model, tool)
                    })
                    .collect();
                body["tools"] = serde_json::json!(sanitized);
            }
        }
        // Provider-native tools (e.g. the projected `computer_20251124` tool
        // from `crate::llm::computer_use::project_computer_tools`) ride in
        // `provider_tools`. The Messages API expects them in the SAME `tools`
        // array as function tools, so fold them in here. Unlike OpenAI
        // Responses, Anthropic has no separate provider-tool channel.
        if !opts.provider_tools.is_empty() {
            let mut tools = body["tools"].as_array().cloned().unwrap_or_default();
            for tool in &opts.provider_tools {
                tools.push(sanitize_anthropic_tool_for_request(
                    &opts.provider,
                    &opts.model,
                    tool,
                ));
            }
            body["tools"] = serde_json::json!(tools);
        }
        if let Some(ref tc) = opts.tool_choice {
            // Anthropic requires `tool_choice` to be an OBJECT (e.g.
            // `{"type":"auto"}`); a bare string like `"auto"` — which is the
            // OpenAI wire shape that most callers and the harn agent loop emit —
            // returns HTTP 400 (`tool_choice: Input should be an object`).
            // Normalize harn's internal tool-choice modes to Anthropic's shape.
            if let Some(normalized) = normalize_anthropic_tool_choice(tc) {
                body["tool_choice"] = normalized;
            }
        }
        match &opts.output_format {
            crate::llm::api::OutputFormat::Text => {}
            crate::llm::api::OutputFormat::JsonObject => {
                force_json_via_tool_use(
                    &mut body,
                    &serde_json::json!({
                        "type": "object",
                        "additionalProperties": true
                    }),
                    &opts.model,
                );
            }
            crate::llm::api::OutputFormat::JsonSchema { schema, .. } => {
                force_json_via_tool_use(&mut body, schema, &opts.model);
            }
        }
        match &opts.thinking {
            // Claude Opus 4.7+ replaced extended thinking with adaptive
            // thinking; `type: enabled` returns HTTP 400. Rewrite the
            // payload transparently rather than fighting the deprecation.
            ThinkingConfig::Disabled => {
                if model_defaults_to_adaptive_thinking(&opts.model)
                    && !model_rejects_disabled_thinking(&opts.model)
                {
                    body["thinking"] = serde_json::json!({ "type": "disabled" });
                }
            }
            ThinkingConfig::Adaptive => {
                body["thinking"] = serde_json::json!({ "type": "adaptive" });
            }
            ThinkingConfig::Effort { level } => {
                if let Some(effort) = anthropic_effort_value(*level) {
                    if model_supports_anthropic_effort(&opts.model) {
                        set_output_config_effort(&mut body, effort);
                    }
                    if !model_defaults_to_adaptive_thinking(&opts.model) {
                        body["thinking"] = serde_json::json!({ "type": "adaptive" });
                    }
                } else if model_defaults_to_adaptive_thinking(&opts.model)
                    && !model_rejects_disabled_thinking(&opts.model)
                {
                    body["thinking"] = serde_json::json!({ "type": "disabled" });
                }
            }
            ThinkingConfig::Enabled { budget_tokens }
                if model_requires_adaptive_thinking(&opts.model) =>
            {
                warn_adaptive_thinking_rewrite(&opts.model);
                body["thinking"] = serde_json::json!({ "type": "adaptive" });
            }
            ThinkingConfig::Enabled { budget_tokens } => {
                body["thinking"] = serde_json::json!({
                    "type": "enabled",
                    "budget_tokens": budget_tokens.unwrap_or(10000),
                });
            }
        }
        crate::llm::serving_tiers::apply_fast_request_knob(&mut body, &opts.model, opts.fast);
        body
    }

    /// The actual chat implementation. Delegates to the shared transport in
    /// `api.rs` after building the provider-specific request body.
    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        crate::llm::api::vm_call_llm_api_with_body(
            request,
            delta_tx,
            Self::build_request_body(request),
            crate::llm::capabilities::WireDialect::Anthropic,
        )
        .await
    }
}

fn drop_anthropic_whitespace_text_blocks(content: serde_json::Value) -> serde_json::Value {
    match content {
        serde_json::Value::Array(blocks) => serde_json::Value::Array(
            blocks
                .into_iter()
                .filter(|block| !is_whitespace_text_block(block))
                .collect(),
        ),
        other => other,
    }
}

fn is_whitespace_text_block(block: &serde_json::Value) -> bool {
    block.get("type").and_then(|value| value.as_str()) == Some("text")
        && block
            .get("text")
            .and_then(|value| value.as_str())
            .is_some_and(|text| text.trim().is_empty())
}

fn is_empty_anthropic_message(message: &serde_json::Value) -> bool {
    match message.get("content") {
        Some(serde_json::Value::String(text)) => text.trim().is_empty(),
        Some(serde_json::Value::Array(blocks)) => blocks.is_empty(),
        _ => false,
    }
}

/// Translate a top-level tool-result-role message into Anthropic's shape: a
/// `role:"user"` message whose `content` is a single `tool_result` block keyed
/// by `tool_use_id`. Any other message is returned unchanged.
///
/// Two source shapes are covered because both are top-level roles Anthropic's
/// Messages API rejects with HTTP 400 (`messages: Unexpected role "..."`):
///
///   - `role:"tool"` — the OpenAI/Ollama native dialect a cheap primary records
///     (`{role:"tool", tool_call_id, content}`), carried forward on escalation.
///   - `role:"tool_result"` — the shape `tool_result_message_for_provider`
///     emits for the Anthropic branch; if such a synthesized message is ever
///     appended as a top-level message (rather than as a `tool_result` block
///     inside a `role:"user"` message), it would 400 the same way.
///
/// The source id lives in `tool_call_id` (OpenAI) or `tool_use_id` (Anthropic
/// native). The original `content` becomes the `tool_result` block's `content` —
/// Anthropic accepts a plain string there, and list-of-blocks content (e.g. an
/// image tool result) survives the later `anthropic_content` pass because that
/// pass recurses into the block's content.
///
/// This is intentionally provider-agnostic at the call site: the quirk (Anthropic
/// has no top-level tool-result role) is encoded here in the Anthropic adapter,
/// so homogeneous Anthropic and homogeneous OpenAI/Ollama runs are byte-identical
/// to before — only a message that literally carries one of those top-level roles
/// on the Anthropic egress path (i.e. a cross-dialect escalation) is rewritten.
fn anthropic_translate_tool_role_message(message: serde_json::Value) -> serde_json::Value {
    let role = message.get("role").and_then(|role| role.as_str());
    if role != Some("tool") && role != Some("tool_result") {
        return message;
    }
    let tool_use_id = message
        .get("tool_call_id")
        .or_else(|| message.get("tool_use_id"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let content = message
        .get("content")
        .cloned()
        .unwrap_or(serde_json::Value::String(String::new()));
    let mut tool_result = serde_json::Map::new();
    tool_result.insert("type".to_string(), serde_json::json!("tool_result"));
    tool_result.insert("tool_use_id".to_string(), serde_json::json!(tool_use_id));
    tool_result.insert("content".to_string(), content);
    // Preserve an is_error flag if the primary marked the observation an error,
    // so Anthropic sees the same result semantics the OpenAI dialect carried.
    if let Some(is_error) = message.get("is_error") {
        tool_result.insert("is_error".to_string(), is_error.clone());
    }
    serde_json::json!({
        "role": "user",
        "content": [serde_json::Value::Object(tool_result)],
    })
}

/// Translate an assistant message carrying an OpenAI/Ollama-style top-level
/// `tool_calls` array into Anthropic's shape: `tool_use` content blocks with the
/// SAME ids, merged into (or forming) the message's `content` block list. Any
/// message without `tool_calls` — or already in Anthropic shape (`tool_use`
/// blocks inline in `content`) — is returned unchanged.
///
/// This is the ASSISTANT half of the cross-provider escalation dialect bridge.
/// `anthropic_translate_tool_role_message` translates the tool-RESULT half
/// (`role:"tool"` → `role:"user"` + `tool_result` block keyed by the OpenAI call
/// id). But the primary's assistant turn carries its calls as a top-level
/// `tool_calls` array, which the canonical-key retain in `build_request_body`
/// STRIPS (it is not an `ANTHROPIC_MESSAGE_KEYS` member) — leaving no `tool_use`
/// block to pair with the translated `tool_result`. Anthropic then 400s:
/// `messages.N.content.M: unexpected tool_use_id found in tool_result blocks:
/// <id>. Each tool_result block must have a corresponding tool_use.` Rendering
/// the calls as `tool_use` blocks with the same ids closes the pairing.
///
/// General case handled: an assistant message may carry BOTH text `content` AND
/// `tool_calls`. Text (string content, or existing content blocks) is preserved
/// first, then a `tool_use` block per call is appended — the OpenAI wire order
/// (assistant text precedes its tool calls). The OpenAI `arguments` field is a
/// JSON *string*; Anthropic wants a JSON *object* `input`, so it is parsed
/// (falling back to an empty object on non-JSON). The call name is read from
/// `function.name` (OpenAI) with a top-level `name` fallback, mirroring
/// `assistant_tool_use_blocks`.
///
/// Provider-decoupled: only an assistant message that literally carries a
/// top-level `tool_calls` array on the Anthropic egress path (a cross-dialect
/// escalation) is rewritten. Homogeneous-Anthropic runs (whose assistant turns
/// carry `tool_use` blocks inline in `content`, no top-level `tool_calls`) are
/// byte-identical.
fn anthropic_translate_assistant_tool_calls(message: serde_json::Value) -> serde_json::Value {
    if message.get("role").and_then(|role| role.as_str()) != Some("assistant") {
        return message;
    }
    let Some(tool_calls) = message.get("tool_calls").and_then(|value| value.as_array()) else {
        return message;
    };
    if tool_calls.is_empty() {
        return message;
    }

    // Preserve existing text/content first (OpenAI order: assistant text, then
    // its tool calls). A plain-string content becomes a single `text` block; an
    // existing block list is carried through as-is; null/absent content yields
    // no leading block.
    let mut blocks: Vec<serde_json::Value> = match message.get("content") {
        Some(serde_json::Value::String(text)) if !text.is_empty() => {
            vec![serde_json::json!({"type": "text", "text": text})]
        }
        Some(serde_json::Value::Array(existing)) => existing.clone(),
        _ => Vec::new(),
    };

    for call in tool_calls {
        let id = call
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let function = call.get("function");
        let name = function
            .and_then(|f| f.get("name"))
            .and_then(|value| value.as_str())
            .or_else(|| call.get("name").and_then(|value| value.as_str()))
            .unwrap_or_default();
        // OpenAI `arguments` is a JSON string; Anthropic `input` is a JSON
        // object. Parse it, tolerating an already-object form and falling back
        // to an empty object on non-JSON so a malformed primary call still
        // produces a valid (if empty-input) tool_use rather than dropping the
        // pairing.
        let input = match function.and_then(|f| f.get("arguments")) {
            Some(serde_json::Value::String(raw)) => serde_json::from_str::<serde_json::Value>(raw)
                .unwrap_or_else(|_| serde_json::json!({})),
            Some(other) if other.is_object() => other.clone(),
            _ => serde_json::json!({}),
        };
        blocks.push(serde_json::json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        }));
    }

    let mut out = message;
    if let Some(object) = out.as_object_mut() {
        object.remove("tool_calls");
        object.insert("content".to_string(), serde_json::Value::Array(blocks));
    }
    out
}

fn enforce_tool_result_adjacency(messages: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut normalized = Vec::with_capacity(messages.len());
    let mut cursor = 0;
    while cursor < messages.len() {
        let message = messages[cursor].clone();
        let Some(mut pending_ids) = assistant_tool_use_ids(&message) else {
            normalized.push(message);
            cursor += 1;
            continue;
        };

        normalized.push(message);
        cursor += 1;

        let mut results = Vec::new();
        let mut deferred = Vec::new();
        while cursor < messages.len() && !pending_ids.is_empty() {
            let next = messages[cursor].clone();
            let matching_ids = matching_tool_result_ids(&next, &pending_ids);
            if !matching_ids.is_empty() {
                for id in matching_ids {
                    pending_ids.remove(&id);
                }
                results.push(next);
                cursor += 1;
                continue;
            }
            if is_user_message_without_tool_result(&next) {
                deferred.push(next);
                cursor += 1;
                continue;
            }
            break;
        }

        normalized.extend(results);
        if !pending_ids.is_empty() {
            // Backfill: any tool_use id that never received a tool_result —
            // a skip path that persisted the assistant turn but never closed
            // out its calls (pre-dispatch interrupt, suspension, a future
            // path we haven't met yet) — would make Anthropic reject the
            // WHOLE request with HTTP 400 "tool_use ids were found without
            // tool_result blocks". Synthesize a placeholder result per
            // orphaned id so the transcript degrades gracefully instead.
            // Ids are sorted so the egress body stays deterministic.
            let mut missing: Vec<String> = pending_ids.into_iter().collect();
            missing.sort_unstable();
            let placeholders: Vec<serde_json::Value> = missing
                .into_iter()
                .map(|id| {
                    serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": "result unavailable (interrupted before dispatch)",
                        "is_error": true,
                    })
                })
                .collect();
            normalized.push(serde_json::json!({
                "role": "user",
                "content": placeholders,
            }));
        }
        normalized.extend(deferred);
    }
    normalized
}

fn assistant_tool_use_ids(message: &serde_json::Value) -> Option<HashSet<String>> {
    if message.get("role").and_then(|role| role.as_str()) != Some("assistant") {
        return None;
    }
    let blocks = message.get("content")?.as_array()?;
    let ids: HashSet<String> = blocks
        .iter()
        .filter_map(|block| {
            if block.get("type").and_then(|value| value.as_str()) == Some("tool_use") {
                block
                    .get("id")
                    .and_then(|value| value.as_str())
                    .map(ToString::to_string)
            } else {
                None
            }
        })
        .collect();
    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

fn matching_tool_result_ids(
    message: &serde_json::Value,
    pending_ids: &HashSet<String>,
) -> HashSet<String> {
    if message.get("role").and_then(|role| role.as_str()) != Some("user") {
        return HashSet::new();
    }
    message
        .get("content")
        .and_then(|content| content.as_array())
        .into_iter()
        .flatten()
        .filter_map(|block| {
            let block_type = block.get("type").and_then(|value| value.as_str());
            let id = block.get("tool_use_id").and_then(|value| value.as_str());
            match (block_type, id) {
                (Some("tool_result"), Some(id)) if pending_ids.contains(id) => Some(id.to_string()),
                _ => None,
            }
        })
        .collect()
}

fn is_user_message_without_tool_result(message: &serde_json::Value) -> bool {
    if message.get("role").and_then(|role| role.as_str()) != Some("user") {
        return false;
    }
    !message
        .get("content")
        .and_then(|content| content.as_array())
        .into_iter()
        .flatten()
        .any(|block| block.get("type").and_then(|value| value.as_str()) == Some("tool_result"))
}

/// Strip Harn-internal extensions that Anthropic's strict request validator
/// rejects with HTTP 400 (`Extra inputs are not permitted`). Mirrors the
/// equivalent helper in `openai_compat.rs`. Anthropic's native-tools shape
/// keeps tool fields at the root (no `function` wrapper), so we strip
/// only at that level.
fn sanitize_anthropic_tool_for_request(
    provider: &str,
    model: &str,
    tool: &serde_json::Value,
) -> serde_json::Value {
    let mut tool = tool.clone();
    if let Some(object) = tool.as_object_mut() {
        object.remove("x-harn-output-schema");
        object.remove("defer_loading");
        object.remove("namespace");
        object.remove("namespaces");
        if object
            .get("strict")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            if let Some(schema) = object.get("input_schema").cloned() {
                object.insert(
                    "input_schema".to_string(),
                    sanitize_schema_for_provider(
                        provider,
                        model,
                        SchemaCompatProfile::AnthropicStrict,
                        SchemaSurface::ToolParameters,
                        &schema,
                    ),
                );
            }
        }
    }
    tool
}

/// Map a caller-supplied `tool_choice` value to Anthropic's object form.
///
/// Anthropic's Messages API requires `tool_choice` to be an object with a
/// `type` of `auto` | `any` | `tool` | `none` (and `name` for `tool`); a bare
/// string returns HTTP 400. Callers and the harn agent loop, however, speak the
/// OpenAI dialect (bare strings `"auto"` / `"none"` / `"required"`, or a
/// `{"type":"function","function":{"name":...}}` object), so we translate here.
///
/// Mapping (mirrors how the OpenAI providers interpret the same modes):
/// - `"auto"` / `{"type":"auto"}` → `{"type":"auto"}`
/// - `"required"` / `"any"` / `{"type":"required"}` / `{"type":"any"}` → `{"type":"any"}`
/// - `"none"` / `{"type":"none"}` → `{"type":"none"}` (tools stay in the request
///   but Claude won't call them — same semantics as OpenAI's `"none"`)
/// - a specific tool: bare name string, `{"type":"tool","name":N}`, or the
///   OpenAI `{"type":"function","function":{"name":N}}` → `{"type":"tool","name":N}`
///
/// Returns `None` only when the value can't be interpreted at all (e.g. a JSON
/// `null`), in which case the caller leaves `tool_choice` unset. Any
/// `disable_parallel_tool_use` flag on an object input is preserved.
fn normalize_anthropic_tool_choice(value: &serde_json::Value) -> Option<serde_json::Value> {
    let attach_parallel = |mut obj: serde_json::Value, src: &serde_json::Value| {
        if let Some(flag) = src.get("disable_parallel_tool_use") {
            if let Some(map) = obj.as_object_mut() {
                map.insert("disable_parallel_tool_use".to_string(), flag.clone());
            }
        }
        obj
    };

    match value {
        serde_json::Value::String(s) => match s.as_str() {
            "auto" => Some(serde_json::json!({"type": "auto"})),
            "any" | "required" => Some(serde_json::json!({"type": "any"})),
            "none" => Some(serde_json::json!({"type": "none"})),
            // A bare, non-keyword string names a specific tool to force.
            other => Some(serde_json::json!({"type": "tool", "name": other})),
        },
        serde_json::Value::Object(_) => {
            let ty = value.get("type").and_then(|t| t.as_str());
            match ty {
                Some("auto") => Some(attach_parallel(serde_json::json!({"type": "auto"}), value)),
                Some("any") | Some("required") => {
                    Some(attach_parallel(serde_json::json!({"type": "any"}), value))
                }
                Some("none") => Some(attach_parallel(serde_json::json!({"type": "none"}), value)),
                // Anthropic native: `{"type":"tool","name":...}`.
                Some("tool") => {
                    let name = value.get("name").and_then(|n| n.as_str());
                    name.map(|name| {
                        attach_parallel(serde_json::json!({"type": "tool", "name": name}), value)
                    })
                }
                // OpenAI native: `{"type":"function","function":{"name":...}}`.
                Some("function") => {
                    let name = value
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str());
                    name.map(|name| {
                        attach_parallel(serde_json::json!({"type": "tool", "name": name}), value)
                    })
                }
                // Unknown / unset `type` on an object: fall back to letting
                // Claude decide rather than forwarding a shape Anthropic rejects.
                _ => Some(serde_json::json!({"type": "auto"})),
            }
        }
        // Null or any other JSON scalar: treat as "no tool_choice".
        _ => None,
    }
}

fn force_json_via_tool_use(body: &mut serde_json::Value, schema: &serde_json::Value, model: &str) {
    // Forced structured output and open tool use are mutually exclusive on
    // Anthropic: we pin `tool_choice` to the synthetic `json_response` tool,
    // which overrides any caller-supplied `tool_choice` and makes the caller's
    // own tools unreachable this turn. Structured output deliberately wins (the
    // caller explicitly requested an `output_format`), but warn once per model
    // so the override isn't silent — it used to clobber both with no signal.
    let had_native_tools = body
        .get("tools")
        .and_then(|tools| tools.as_array())
        .is_some_and(|tools| !tools.is_empty());
    let had_tool_choice = body.get("tool_choice").is_some();
    if had_native_tools || had_tool_choice {
        warn_forced_json_overrides_tools(model);
    }
    body["tools"] = {
        let mut tools = body["tools"].as_array().cloned().unwrap_or_default();
        let schema = sanitize_schema_for_provider(
            "anthropic",
            model,
            SchemaCompatProfile::AnthropicStrict,
            SchemaSurface::StructuredOutput,
            schema,
        );
        tools.push(serde_json::json!({
            "name": "json_response",
            "description": "Return a structured JSON response matching the schema.",
            "input_schema": schema,
        }));
        serde_json::json!(tools)
    };
    body["tool_choice"] = serde_json::json!({"type": "tool", "name": "json_response"});
}

fn warn_forced_json_overrides_tools(model: &str) {
    ANTHROPIC_FORCED_JSON_WARN_ONCE.with(|seen| {
        let mut seen = seen.borrow_mut();
        if seen.insert(model.to_string()) {
            crate::events::log_warn(
                "llm.structured_output",
                &format!(
                    "structured output (output_format) requested for {model} alongside \
                     native tools or a tool_choice; forcing the json_response tool, which \
                     overrides tool_choice and makes the other tools unreachable this turn",
                ),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{LlmErrorKind, LlmErrorReason};
    use crate::llm::api::{LlmRequestPayload, ReasoningEffort};

    fn base_payload() -> LlmRequestPayload {
        LlmRequestPayload {
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            region: None,
            api_key: String::new(),
            api_mode: crate::llm::api::LlmApiMode::ChatCompletions,
            fallback_chain: Vec::new(),
            route_fallbacks: Vec::new(),
            session_id: None,
            messages: vec![serde_json::json!({"role": "user", "content": "hello"})],
            system: Some("system prompt".to_string()),
            max_tokens: 64,
            temperature: None,
            top_p: None,
            top_k: None,
            logprobs: false,
            top_logprobs: None,
            stop: None,
            seed: None,
            frequency_penalty: None,
            presence_penalty: None,
            fast: false,
            output_format: crate::llm::api::OutputFormat::Text,
            response_format: None,
            json_schema: None,
            output_schema: None,
            schema_stream_abort: false,
            thinking: ThinkingConfig::Disabled,
            anthropic_beta_features: Vec::new(),
            vision: false,
            native_tools: Some(vec![serde_json::json!({
                "name": "read_file",
                "description": "Read a file",
                "input_schema": {"type": "object"},
            })]),
            provider_tools: Vec::new(),
            tool_choice: None,
            cache: false,
            timeout: None,
            stream: true,
            provider_overrides: None,
            previous_response_id: None,
            store: None,
            background: None,
            truncation: None,
            compact: None,
            include: None,
            max_tool_calls: None,
            prefill: None,
            reminder_lifecycle: Vec::new(),
            cli_llm_mock_scope: None,
        }
    }

    // Full live-path diagnostic: a computer screenshot recorded through the real
    // `record_tool_results` path must reach the Anthropic request body as an
    // image block whose base64 is BYTE-IDENTICAL to the captured PNG. Reproduces
    // the exact flow burin-headless uses (record -> transcript -> egress).
    #[test]
    fn live_computer_screenshot_reaches_anthropic_body_byte_perfect() {
        use base64::Engine;
        // A large synthetic image payload (~300 KB, the size of a real 1024x768
        // screenshot) so the test exercises byte preservation of a big base64
        // string through the whole record -> transcript -> egress path.
        let bytes: Vec<u8> = (0..300_000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 16) as u8)
            .collect();
        let src_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        crate::llm::agent_session_host::reset_agent_session_host_state();
        let session_id = crate::agent_sessions::open_or_create(Some("cu-live-diag".to_string()));
        crate::llm::agent_session_host::seed_host_session_provider_model(
            &session_id,
            "anthropic",
            "claude-opus-4-8",
        );
        // Pair the tool_result with an assistant tool_use so egress keeps it.
        crate::agent_sessions::inject_message(
            &session_id,
            crate::stdlib::json_to_vm_value(&serde_json::json!({
                "role": "user", "content": "take a screenshot"
            })),
        )
        .unwrap();
        crate::agent_sessions::inject_message(
            &session_id,
            crate::stdlib::json_to_vm_value(&serde_json::json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "tc1", "name": "computer", "input": {"action": "screenshot"}}],
            })),
        )
        .unwrap();
        let dispatch = crate::stdlib::json_to_vm_value(&serde_json::json!([{
            "tool_name": "computer",
            "tool_call_id": "tc1",
            "ok": true,
            "observation": "Captured screenshot 1024x768.",
            "result": {
                "ok": true,
                "text": "Captured screenshot 1024x768.",
                "screenshot": {
                    "base64": src_b64,
                    "media_type": "image/png",
                    "width": 1024, "height": 768, "scale_factor": 2.0,
                },
            },
        }]));
        crate::llm::agent_session_host::record_tool_results_for_test(&session_id, dispatch);

        let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
        // Route the recorded transcript messages through the SAME VmValue->json
        // conversion the live `llm_call` uses (`vm_messages_to_json`), not just
        // `build_request_body`. That conversion is the rung that previously
        // stringified a `[text, screenshot]` tool_result block list into ~1MB of
        // base64 TEXT, so a test that fed transcript messages straight to
        // build_request_body passed while the live path dropped the image. This
        // guards the full record -> vm_messages_to_json -> egress chain.
        let message_vms: Vec<crate::value::VmValue> =
            match transcript.as_dict().and_then(|dict| dict.get("messages")) {
                Some(crate::value::VmValue::List(list)) => list.iter().cloned().collect(),
                _ => Vec::new(),
            };
        let messages =
            crate::llm::helpers::vm_messages_to_json(&message_vms).expect("messages json");

        let mut opts = base_payload();
        opts.model = "claude-opus-4-8".to_string();
        opts.messages = messages;
        let body = AnthropicProvider::build_request_body(&opts);

        // Find the image block anywhere in the outgoing body.
        fn find_image_data(v: &serde_json::Value) -> Option<String> {
            match v {
                serde_json::Value::Object(map) => {
                    if map.get("type").and_then(|t| t.as_str()) == Some("image") {
                        if let Some(data) = map
                            .get("source")
                            .and_then(|s| s.get("data"))
                            .and_then(|d| d.as_str())
                        {
                            return Some(data.to_string());
                        }
                    }
                    map.values().find_map(find_image_data)
                }
                serde_json::Value::Array(items) => items.iter().find_map(find_image_data),
                _ => None,
            }
        }
        let out_b64 = find_image_data(&body).expect("an image block in the Anthropic body");
        let out_bytes = base64::engine::general_purpose::STANDARD
            .decode(&out_b64)
            .expect("valid base64");
        assert_eq!(
            out_bytes, bytes,
            "the screenshot in the Anthropic body must be byte-identical to the capture (len {} vs {})",
            out_bytes.len(),
            bytes.len()
        );
    }

    #[test]
    fn stored_reasoning_key_stripped_from_outgoing_messages() {
        // Reproduces the eval-traced HTTP 400: a persisted assistant turn
        // carries a top-level `reasoning` key (stamped by
        // build_assistant_response_message) which, if echoed into the
        // Anthropic request, returns
        // `messages.1.reasoning: Extra inputs are not permitted`.
        let mut opts = base_payload();
        opts.messages = vec![
            serde_json::json!({"role": "user", "content": "do the task"}),
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": "Let me start by understanding..."}],
                "reasoning": "Let me start by understanding the task.",
                // OpenAI-shape leakage and other storage-only metadata must also
                // be stripped at the Anthropic egress boundary.
                "tool_calls": [{"id": "x", "type": "function"}],
                "private_reasoning": "hidden",
            }),
            serde_json::json!({"role": "user", "content": "continue"}),
        ];

        let body = AnthropicProvider::build_request_body(&opts);
        let messages = body["messages"].as_array().expect("messages array");

        // The persisted assistant turn (messages[1]) must carry ONLY
        // canonical Anthropic message keys after projection.
        let assistant = messages[1].as_object().expect("assistant object");
        assert!(
            assistant.get("reasoning").is_none(),
            "non-canonical `reasoning` key rode into the Anthropic request: {assistant:?}"
        );
        assert!(assistant.get("tool_calls").is_none());
        assert!(assistant.get("private_reasoning").is_none());
        assert_eq!(
            assistant.get("role").and_then(|v| v.as_str()),
            Some("assistant")
        );
        assert!(
            assistant.get("content").is_some(),
            "content must be preserved (replay/answer continuity)"
        );

        // Replay-preservation: the SOURCE transcript shape is untouched —
        // build_request_body must not mutate opts.messages in place.
        assert_eq!(
            opts.messages[1].get("reasoning").and_then(|v| v.as_str()),
            Some("Let me start by understanding the task."),
            "persisted transcript shape must be unchanged at the storage layer"
        );

        // Canonical round-trip: plain user/assistant turns with no stored
        // metadata still serialize their content unchanged.
        assert_eq!(
            messages[0].get("content").and_then(|v| v.as_str()),
            Some("do the task")
        );
    }

    #[test]
    fn cross_provider_tool_role_message_translated_to_anthropic_shape() {
        // Reproduces the cross-provider escalation HTTP 400
        // (`messages: Unexpected role "tool"`): a cheap OpenAI/Ollama-dialect
        // primary records tool results as top-level `role:"tool"` messages.
        // When escalation switches the provider to Anthropic and replays that
        // history, Anthropic rejects `role:"tool"` — it wants a `role:"user"`
        // message carrying a `tool_result` content block. Pre-fix, the
        // `role:"tool"` message rides through verbatim (its `tool_call_id` is
        // even stripped by the canonical-key retain), producing the 400.
        let mut opts = base_payload();
        opts.messages = vec![
            serde_json::json!({"role": "user", "content": "read the file"}),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "call_001", "name": "read_file", "input": {}}
                ],
            }),
            // OpenAI dialect tool-result carried forward from the primary.
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "call_001",
                "name": "read_file",
                "content": "fn main() {}",
            }),
        ];

        let body = AnthropicProvider::build_request_body(&opts);
        let messages = body["messages"].as_array().expect("messages array");

        // No top-level `role:"tool"` message may survive to the Anthropic wire.
        assert!(
            messages
                .iter()
                .all(|m| m.get("role").and_then(|r| r.as_str()) != Some("tool")),
            "a top-level role:\"tool\" message rode into the Anthropic request: {messages:?}"
        );

        // The tool result must now be a `role:"user"` message carrying a
        // `tool_result` block keyed by the matching tool_use_id, and the real
        // observation must be preserved (not masked by an interrupted-before-
        // dispatch placeholder).
        let tool_result_block = messages
            .iter()
            .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
            .flat_map(|m| {
                m.get("content")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default()
            })
            .find(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
            .expect("a user message carrying a tool_result block");
        assert_eq!(
            tool_result_block
                .get("tool_use_id")
                .and_then(|v| v.as_str()),
            Some("call_001"),
            "tool_result must key off the original tool_call_id"
        );
        assert_eq!(
            tool_result_block.get("content").and_then(|v| v.as_str()),
            Some("fn main() {}"),
            "the real observation must survive (no placeholder masking)"
        );

        // Replay-preservation: the source transcript shape is untouched.
        assert_eq!(
            opts.messages[2].get("role").and_then(|v| v.as_str()),
            Some("tool"),
            "persisted transcript shape must be unchanged at the storage layer"
        );
    }

    #[test]
    fn cross_provider_tool_use_and_result_pair_both_translated_for_anthropic() {
        // Reproduces the THIRD stacked escalation 400 (downstream of the
        // role:"tool" fix): `messages.N.content.M: unexpected tool_use_id found
        // in tool_result blocks: <id>. Each tool_result block must have a
        // corresponding tool_use.` The primary's OpenAI-dialect assistant turn
        // carries its calls as a top-level `tool_calls` array (with BOTH text
        // content AND the call). The role:"tool" fix translates the RESULT half
        // into a tool_result block keyed by that id — but pre-fix the assistant
        // `tool_calls` were STRIPPED by the canonical-key retain, leaving the
        // tool_result orphaned. Both halves must translate so the pair matches.
        let mut opts = base_payload();
        opts.messages = vec![
            serde_json::json!({"role": "user", "content": "read the file"}),
            // OpenAI-dialect assistant turn: text content + a top-level tool_call.
            serde_json::json!({
                "role": "assistant",
                "content": "I'll read it now.",
                "tool_calls": [{
                    "id": "call_R0hU",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\":\"main.rs\"}",
                    },
                }],
            }),
            // OpenAI-dialect tool result referencing that call id.
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "call_R0hU",
                "name": "read_file",
                "content": "fn main() {}",
            }),
        ];

        let body = AnthropicProvider::build_request_body(&opts);
        let messages = body["messages"].as_array().expect("messages array");

        // The assistant message must carry a tool_use block (id call_R0hU) with
        // the parsed input object, AND preserve its leading text block.
        let assistant = messages
            .iter()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
            .expect("assistant message present");
        assert!(
            assistant.get("tool_calls").is_none(),
            "top-level OpenAI `tool_calls` must not ride into the Anthropic request: {assistant:?}"
        );
        let assistant_blocks = assistant
            .get("content")
            .and_then(|c| c.as_array())
            .expect("assistant content is a block list");
        let tool_use = assistant_blocks
            .iter()
            .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
            .expect("a tool_use block in the assistant message");
        assert_eq!(
            tool_use.get("id").and_then(|v| v.as_str()),
            Some("call_R0hU")
        );
        assert_eq!(
            tool_use.get("name").and_then(|v| v.as_str()),
            Some("read_file")
        );
        assert_eq!(
            tool_use.get("input"),
            Some(&serde_json::json!({"path": "main.rs"})),
            "OpenAI `arguments` string must be parsed into Anthropic `input` object"
        );
        assert!(
            assistant_blocks
                .iter()
                .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("text")),
            "assistant text content must be preserved alongside the tool_use: {assistant_blocks:?}"
        );

        // The matching tool_result must exist keyed by the SAME id — and NOT be
        // the interrupted-before-dispatch placeholder (the real result survives).
        let tool_result = messages
            .iter()
            .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
            .flat_map(|m| {
                m.get("content")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default()
            })
            .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
            .expect("a tool_result block");
        assert_eq!(
            tool_result.get("tool_use_id").and_then(|v| v.as_str()),
            Some("call_R0hU"),
            "tool_result must pair with the assistant tool_use id"
        );
        assert_eq!(
            tool_result.get("content").and_then(|v| v.as_str()),
            Some("fn main() {}"),
            "real observation must survive (no placeholder masking)"
        );
        // Full-pairing invariant: every tool_result id has a corresponding
        // tool_use id — no orphan, which is exactly what Anthropic 400s on.
        let tool_use_ids: std::collections::BTreeSet<String> = messages
            .iter()
            .flat_map(|m| {
                m.get("content")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default()
            })
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
            .filter_map(|b| b.get("id").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        let tool_result_ids: std::collections::BTreeSet<String> = messages
            .iter()
            .flat_map(|m| {
                m.get("content")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default()
            })
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
            .filter_map(|b| {
                b.get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();
        assert!(
            tool_result_ids.is_subset(&tool_use_ids),
            "orphaned tool_result id (no corresponding tool_use): results={tool_result_ids:?} uses={tool_use_ids:?}"
        );
    }

    #[test]
    fn top_level_tool_result_role_message_translated_to_anthropic_shape() {
        // Defense-in-depth: a synthesized tool-result whose role is the
        // Anthropic-native `tool_result` string (what
        // tool_result_message_for_provider emits) would ALSO 400 if it reached
        // egress as a top-level message. The same choke point must fold it into
        // a role:"user" + tool_result block.
        let mut opts = base_payload();
        opts.messages = vec![
            serde_json::json!({"role": "user", "content": "read the file"}),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "toolu_9", "name": "read_file", "input": {}}
                ],
            }),
            serde_json::json!({
                "role": "tool_result",
                "tool_use_id": "toolu_9",
                "content": "fn main() {}",
            }),
        ];

        let body = AnthropicProvider::build_request_body(&opts);
        let messages = body["messages"].as_array().expect("messages array");
        assert!(
            messages.iter().all(|m| {
                let r = m.get("role").and_then(|r| r.as_str());
                r != Some("tool") && r != Some("tool_result")
            }),
            "a top-level tool-result role rode into the Anthropic request: {messages:?}"
        );
        let block = messages
            .iter()
            .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
            .flat_map(|m| {
                m.get("content")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default()
            })
            .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
            .expect("a user message carrying a tool_result block");
        assert_eq!(
            block.get("tool_use_id").and_then(|v| v.as_str()),
            Some("toolu_9")
        );
        assert_eq!(
            block.get("content").and_then(|v| v.as_str()),
            Some("fn main() {}")
        );
    }

    #[test]
    fn homogeneous_anthropic_tool_result_unchanged_by_translation() {
        // Guard: a message history already in Anthropic shape (role:"user"
        // with a tool_result block) must be byte-identical before and after —
        // the translation only touches literal role:"tool" messages.
        let mut opts = base_payload();
        opts.messages = vec![
            serde_json::json!({"role": "user", "content": "read the file"}),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "toolu_001", "name": "read_file", "input": {}}
                ],
            }),
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_001", "content": "fn main() {}"}
                ],
            }),
        ];

        let body = AnthropicProvider::build_request_body(&opts);
        let messages = body["messages"].as_array().expect("messages array");
        // The tool_result user message survives with its id and content intact,
        // and no placeholder/backfill was synthesized (exactly one tool_result).
        let tool_results: Vec<_> = messages
            .iter()
            .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
            .flat_map(|m| {
                m.get("content")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default()
            })
            .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
            .collect();
        assert_eq!(
            tool_results.len(),
            1,
            "no duplicate/placeholder tool_result"
        );
        assert_eq!(
            tool_results[0].get("tool_use_id").and_then(|v| v.as_str()),
            Some("toolu_001")
        );
        assert_eq!(
            tool_results[0].get("content").and_then(|v| v.as_str()),
            Some("fn main() {}")
        );
    }

    #[test]
    fn cache_control_message_key_survives_anthropic_egress() {
        // `cache_control` is a canonical message-level field for prompt
        // caching and must NOT be stripped by the egress sanitizer.
        let mut opts = base_payload();
        opts.messages = vec![serde_json::json!({
            "role": "user",
            "content": "hello",
            "cache_control": {"type": "ephemeral"},
        })];
        let body = AnthropicProvider::build_request_body(&opts);
        let msg = body["messages"][0].as_object().expect("message object");
        assert_eq!(
            msg.get("cache_control"),
            Some(&serde_json::json!({"type": "ephemeral"}))
        );
    }

    #[test]
    fn whitespace_only_text_blocks_are_dropped_before_anthropic_egress() {
        let mut opts = base_payload();
        opts.messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "  \n\t"},
                {"type": "text", "text": "keep me"},
                {
                    "type": "tool_result",
                    "tool_use_id": "toolu_read",
                    "content": "result"
                }
            ],
        })];

        let body = AnthropicProvider::build_request_body(&opts);
        let content = body["messages"][0]["content"]
            .as_array()
            .expect("content blocks");

        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "keep me");
        assert_eq!(content[1]["type"], "tool_result");
        assert_eq!(content[1]["tool_use_id"], "toolu_read");
    }

    #[test]
    fn whitespace_only_messages_are_dropped_before_tool_result_adjacency() {
        let mut opts = base_payload();
        opts.messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_verify",
                    "name": "verify",
                    "input": {},
                }],
            }),
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": "\n   \t"}],
            }),
            serde_json::json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_verify",
                    "content": "ok",
                }],
            }),
        ];

        let body = AnthropicProvider::build_request_body(&opts);
        let messages = body["messages"].as_array().expect("messages array");

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"][0]["type"], "tool_result");
        assert_eq!(messages[1]["content"][0]["tool_use_id"], "toolu_verify");
    }

    #[test]
    fn injected_feedback_deferred_until_after_tool_result() {
        let mut opts = base_payload();
        opts.messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "I will verify."},
                    {
                        "type": "tool_use",
                        "id": "toolu_verify",
                        "name": "verify",
                        "input": {},
                    },
                ],
            }),
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "<runtime_feedback>grounding note</runtime_feedback>"}
                ],
            }),
            serde_json::json!({
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_verify",
                        "content": "tests passed",
                    }
                ],
            }),
        ];

        let body = AnthropicProvider::build_request_body(&opts);
        let messages = body["messages"].as_array().expect("messages array");

        assert_eq!(
            messages[0]["content"][1],
            serde_json::json!({
                "type": "tool_use",
                "id": "toolu_verify",
                "name": "verify",
                "input": {},
            })
        );
        assert_eq!(
            messages[1]["content"][0],
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": "toolu_verify",
                "content": "tests passed",
            })
        );
        assert_eq!(
            messages[2]["content"][0],
            serde_json::json!({
                "type": "text",
                "text": "<runtime_feedback>grounding note</runtime_feedback>",
            })
        );
    }

    #[test]
    fn feedback_deferred_when_tool_use_is_not_final_content_block() {
        let mut opts = base_payload();
        opts.messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_verify",
                        "name": "verify",
                        "input": {},
                    },
                    {"type": "text", "text": "Waiting for the result."},
                ],
            }),
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "<runtime_feedback>late reminder</runtime_feedback>"}
                ],
            }),
            serde_json::json!({
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_verify",
                        "content": "ok",
                    }
                ],
            }),
        ];

        let body = AnthropicProvider::build_request_body(&opts);
        let messages = body["messages"].as_array().expect("messages array");

        assert_eq!(
            messages[1]["content"][0],
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": "toolu_verify",
                "content": "ok",
            })
        );
        assert_eq!(
            messages[2]["content"][0],
            serde_json::json!({
                "type": "text",
                "text": "<runtime_feedback>late reminder</runtime_feedback>",
            })
        );
    }

    #[test]
    fn orphaned_tool_use_gets_placeholder_tool_result_backfill() {
        // A transcript that ends on an assistant tool_use with no recorded
        // tool_result (e.g. an interrupt/suspend path that failed to close
        // out its calls) must not 400 the whole session: the egress
        // normalizer backfills a placeholder result.
        let mut opts = base_payload();
        opts.messages = vec![
            serde_json::json!({"role": "user", "content": "do the thing"}),
            serde_json::json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_orphan",
                    "name": "run",
                    "input": {},
                }],
            }),
        ];

        let body = AnthropicProvider::build_request_body(&opts);
        let messages = body["messages"].as_array().expect("messages array");

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(
            messages[2]["content"][0],
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": "toolu_orphan",
                "content": "result unavailable (interrupted before dispatch)",
                "is_error": true,
            })
        );
    }

    #[test]
    fn orphaned_tool_use_backfill_lands_before_deferred_user_text() {
        // An orphaned tool_use followed by injected user feedback: the
        // placeholder result must sit ADJACENT to the assistant turn, with
        // the deferred user text after it — the same ordering contract the
        // real-result reorder path guarantees.
        let mut opts = base_payload();
        opts.messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_orphan",
                    "name": "run",
                    "input": {},
                }],
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"type": "text", "text": "STOP — user interrupted"}],
            }),
        ];

        let body = AnthropicProvider::build_request_body(&opts);
        let messages = body["messages"].as_array().expect("messages array");

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["content"][0]["type"], "tool_result");
        assert_eq!(messages[1]["content"][0]["tool_use_id"], "toolu_orphan");
        assert_eq!(
            messages[2]["content"][0],
            serde_json::json!({"type": "text", "text": "STOP — user interrupted"})
        );
    }

    #[test]
    fn partially_orphaned_tool_use_backfills_only_missing_ids() {
        // Two parallel tool_use blocks, only one real result: the backfill
        // must cover exactly the missing id (sorted, deterministic) and
        // leave the real result untouched.
        let mut opts = base_payload();
        opts.messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "toolu_b", "name": "run", "input": {}},
                    {"type": "tool_use", "id": "toolu_a", "name": "read", "input": {}},
                ],
            }),
            serde_json::json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_a",
                    "content": "file text",
                }],
            }),
        ];

        let body = AnthropicProvider::build_request_body(&opts);
        let messages = body["messages"].as_array().expect("messages array");

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["content"][0]["tool_use_id"], "toolu_a");
        assert_eq!(messages[1]["content"][0]["content"], "file text");
        let backfill = messages[2]["content"].as_array().expect("backfill blocks");
        assert_eq!(backfill.len(), 1);
        assert_eq!(backfill[0]["tool_use_id"], "toolu_b");
        assert_eq!(backfill[0]["is_error"], true);
    }

    #[test]
    fn tool_search_supported_for_claude_4_opus_and_up() {
        // Per Anthropic's tool-search docs:
        //   Claude Mythos Preview, Sonnet 4.0+, Opus 4.0+, Haiku 4.5+.
        assert!(claude_model_supports_tool_search("claude-opus-4-7"));
        assert!(claude_model_supports_tool_search("claude-opus-4.7"));
        assert!(claude_model_supports_tool_search("claude-opus-4-0"));
        assert!(claude_model_supports_tool_search("claude-sonnet-4-6"));
        assert!(claude_model_supports_tool_search("claude-sonnet-4-0"));
    }

    #[test]
    fn fable_and_mythos_parse_generation_and_inherit_guards() {
        // Fable/Mythos 5 (launched 2026-06-09) share the Opus 4.7+ request
        // surface; the generation parser must recognize the families or none
        // of the >= (4, 6) / (4, 7) guards (prefill removal, sampling strip,
        // adaptive-thinking rewrite) fire for them.
        assert_eq!(claude_generation("claude-fable-5"), Some((5, 0)));
        assert_eq!(claude_generation("claude-mythos-5"), Some((5, 0)));
        assert_eq!(claude_generation("anthropic/claude-fable-5"), Some((5, 0)));
        assert_eq!(
            claude_generation("anthropic.claude-opus-4-7-v1:0"),
            Some((4, 7))
        );
        assert_eq!(
            claude_generation("anthropic.claude-3-5-sonnet-20240620-v1:0"),
            Some((3, 5))
        );
        // Mythos Preview has no numeric generation — stays unrecognized.
        assert_eq!(claude_generation("claude-mythos-preview"), None);
        assert!(claude_model_supports_tool_search("claude-fable-5"));
    }

    #[test]
    fn fable_thinking_payloads_match_always_on_surface() {
        // Extended-thinking budgets are a 400 on Fable — rewritten to adaptive.
        let mut payload = base_payload();
        payload.model = "claude-fable-5".to_string();
        payload.thinking = ThinkingConfig::Enabled {
            budget_tokens: Some(4096),
        };
        let body = AnthropicProvider::build_request_body(&payload);
        assert_eq!(body["thinking"], serde_json::json!({ "type": "adaptive" }));

        // Thinking is always on for Fable, and an explicit
        // `thinking: {type: "disabled"}` is also a 400 — a Disabled config
        // must leave the field out of the payload entirely.
        let mut payload2 = base_payload();
        payload2.model = "claude-fable-5".to_string();
        payload2.thinking = ThinkingConfig::Disabled;
        payload2.temperature = Some(0.0);
        let body2 = AnthropicProvider::build_request_body(&payload2);
        assert!(body2.get("thinking").is_none());
        // Sampling params are rejected on the 4.7+ surface — stripped.
        assert!(
            body2.get("temperature").is_none(),
            "temperature must be stripped for claude-fable-5"
        );
    }

    #[test]
    fn sonnet_5_effort_uses_output_config_and_default_on_thinking() {
        let mut payload = base_payload();
        payload.model = "claude-sonnet-5".to_string();
        payload.thinking = ThinkingConfig::Effort {
            level: ReasoningEffort::High,
        };
        let body = AnthropicProvider::build_request_body(&payload);
        assert_eq!(body["output_config"]["effort"], serde_json::json!("high"));
        assert!(
            body.get("thinking").is_none(),
            "Sonnet 5 defaults adaptive thinking on; effort should not send legacy thinking budgets"
        );

        let mut disabled = base_payload();
        disabled.model = "claude-sonnet-5".to_string();
        disabled.thinking = ThinkingConfig::Disabled;
        let disabled_body = AnthropicProvider::build_request_body(&disabled);
        assert_eq!(
            disabled_body["thinking"],
            serde_json::json!({ "type": "disabled" })
        );
        assert!(
            disabled_body.get("output_config").is_none(),
            "turning Sonnet 5 thinking off should not also send an effort level"
        );
    }

    #[test]
    fn opus_adaptive_effort_uses_output_config_with_adaptive_thinking() {
        let mut payload = base_payload();
        payload.model = "claude-opus-4-7".to_string();
        payload.thinking = ThinkingConfig::Effort {
            level: ReasoningEffort::Max,
        };
        let body = AnthropicProvider::build_request_body(&payload);
        assert_eq!(body["thinking"], serde_json::json!({ "type": "adaptive" }));
        assert_eq!(body["output_config"]["effort"], serde_json::json!("max"));
    }

    #[test]
    fn tool_search_unsupported_for_older_claude() {
        // Opus/Sonnet 3.x predate the feature.
        assert!(!claude_model_supports_tool_search("claude-opus-3-5"));
        assert!(!claude_model_supports_tool_search("claude-sonnet-3-5"));
        assert!(!claude_model_supports_tool_search("claude-haiku-3-5"));
    }

    #[test]
    fn tool_search_haiku_requires_4_5() {
        // Haiku's cutoff is 4.5 (later than Opus/Sonnet's 4.0).
        assert!(!claude_model_supports_tool_search("claude-haiku-4-0"));
        assert!(!claude_model_supports_tool_search("claude-haiku-4-4"));
        assert!(claude_model_supports_tool_search("claude-haiku-4-5"));
        assert!(claude_model_supports_tool_search(
            "claude-haiku-4-5-20251001"
        ));
        assert!(claude_model_supports_tool_search("claude-haiku-5-0"));
    }

    #[test]
    fn tool_search_unsupported_for_non_claude() {
        assert!(!claude_model_supports_tool_search("gpt-5"));
        assert!(!claude_model_supports_tool_search("gpt-5.4-turbo"));
        assert!(!claude_model_supports_tool_search("gemini-2.0"));
        assert!(!claude_model_supports_tool_search(""));
    }

    #[test]
    fn native_tool_search_variants_lists_bm25_first() {
        let provider = AnthropicProvider;
        let variants = provider.native_tool_search_variants("claude-opus-4-7");
        assert_eq!(variants, vec!["bm25".to_string(), "regex".to_string()]);
    }

    #[test]
    fn native_tool_search_variants_empty_for_old_model() {
        let provider = AnthropicProvider;
        assert!(provider
            .native_tool_search_variants("claude-opus-3-5")
            .is_empty());
    }

    #[test]
    fn supports_defer_loading_matches_tool_search_gate() {
        let provider = AnthropicProvider;
        assert!(provider.supports_defer_loading("claude-opus-4-7"));
        assert!(!provider.supports_defer_loading("claude-opus-3-5"));
    }

    #[test]
    fn fast_tier_injects_speed_knob_and_beta_header() {
        // `fast: true` on a model whose catalog tier rides `speed` sets the
        // top-level request knob and the beta header flows through the
        // payload's resolved Anthropic beta features.
        let mut payload = base_payload();
        payload.model = "claude-opus-4-8".to_string();
        payload.fast = true;
        let body = AnthropicProvider::build_request_body(&payload);
        assert_eq!(body["speed"], serde_json::json!("fast"));

        let opts = {
            let mut o = crate::llm::api::options::base_opts("anthropic");
            o.model = "claude-opus-4-8".to_string();
            o.fast = true;
            o
        };
        assert!(
            opts.anthropic_beta_features_for_request()
                .iter()
                .any(|f| f == "fast-mode-2026-02-01"),
            "fast mode must request the fast-mode beta header"
        );
    }

    #[test]
    fn fast_tier_knob_absent_when_off() {
        let mut payload = base_payload();
        payload.model = "claude-opus-4-8".to_string();
        payload.fast = false;
        let body = AnthropicProvider::build_request_body(&payload);
        assert!(body.get("speed").is_none());
    }

    #[test]
    fn temperature_stripped_when_thinking_active() {
        // Anthropic rejects HTTP 400 if `temperature != 1` when thinking is
        // active. Strip the temperature transparently so callers can default
        // to temperature=0 for determinism without having to know which
        // models silently auto-enable thinking.
        let mut payload = base_payload();
        payload.temperature = Some(0.0);
        payload.thinking = ThinkingConfig::Adaptive;
        let body = AnthropicProvider::build_request_body(&payload);
        assert!(
            body.get("temperature").is_none(),
            "temperature must be stripped when thinking is active to avoid HTTP 400"
        );
        // Sanity: temperature is preserved when thinking is disabled.
        let mut payload2 = base_payload();
        payload2.temperature = Some(0.0);
        payload2.thinking = ThinkingConfig::Disabled;
        let body2 = AnthropicProvider::build_request_body(&payload2);
        assert_eq!(body2["temperature"], serde_json::json!(0.0));
    }

    #[test]
    fn sampling_params_stripped_by_shared_helper_for_rejecting_models() {
        let mut body = serde_json::json!({
            "model": "claude-opus-4-7",
            "temperature": 0.2,
            "top_p": 0.9,
            "top_k": 20,
        });

        strip_unsupported_sampling_params(&mut body, "claude-opus-4-7", &ThinkingConfig::Disabled);

        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert!(body.get("top_k").is_none());
        assert_eq!(body["model"], serde_json::json!("claude-opus-4-7"));
    }

    #[test]
    fn sampling_params_stripped_by_shared_helper_when_thinking_active() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "temperature": 0.0,
            "top_p": 0.9,
            "top_k": 20,
        });

        strip_unsupported_sampling_params(
            &mut body,
            "claude-sonnet-4-6",
            &ThinkingConfig::Adaptive,
        );

        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert!(body.get("top_k").is_none());
    }

    #[test]
    fn sampling_params_preserved_by_shared_helper_for_supported_disabled_thinking() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "temperature": 0.2,
            "top_p": 0.9,
            "top_k": 20,
        });

        strip_unsupported_sampling_params(
            &mut body,
            "claude-sonnet-4-6",
            &ThinkingConfig::Disabled,
        );

        assert_eq!(body["temperature"], serde_json::json!(0.2));
        assert_eq!(body["top_p"], serde_json::json!(0.9));
        assert_eq!(body["top_k"], serde_json::json!(20));
    }

    #[test]
    fn sampling_params_stripped_when_body_thinking_override_is_active() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "temperature": 0.2,
            "top_p": 0.9,
            "thinking": {"type": "enabled", "budget_tokens": 1024},
        });

        strip_unsupported_sampling_params(
            &mut body,
            "claude-sonnet-4-6",
            &ThinkingConfig::Disabled,
        );

        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert_eq!(
            body["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 1024})
        );
    }

    #[test]
    fn sampling_params_stripped_when_body_output_config_effort_override_is_active() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "temperature": 0.2,
            "top_p": 0.9,
            "output_config": {"effort": "high"},
        });

        strip_unsupported_sampling_params(
            &mut body,
            "claude-sonnet-4-6",
            &ThinkingConfig::Disabled,
        );

        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert_eq!(body["output_config"], serde_json::json!({"effort": "high"}));
    }

    #[test]
    fn native_tools_strip_harn_internal_extensions() {
        let mut payload = base_payload();
        payload.native_tools = Some(vec![serde_json::json!({
            "name": "read_file",
            "description": "Read a file",
            "strict": true,
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "pattern": "^/",
                        "format": "uri-reference",
                        "minLength": 1
                    }
                }
            },
            "x-harn-output-schema": {"type": "object"},
            "defer_loading": true,
            "namespace": "fs",
        })]);
        let body = AnthropicProvider::build_request_body(&payload);
        let sent = body["tools"][0].as_object().expect("tool object");
        assert!(
            !sent.contains_key("x-harn-output-schema"),
            "Anthropic rejects unknown tool fields with HTTP 400; the x-harn-output-schema \
             extension must be stripped before sending"
        );
        assert!(!sent.contains_key("defer_loading"));
        assert!(!sent.contains_key("namespace"));
        assert!(sent.contains_key("input_schema"));
        assert_eq!(sent["input_schema"]["additionalProperties"], false);
        assert!(sent["input_schema"]["properties"]["path"]
            .get("pattern")
            .is_none());
        assert!(sent["input_schema"]["properties"]["path"]
            .get("format")
            .is_none());
        assert!(sent["input_schema"]["properties"]["path"]
            .get("minLength")
            .is_none());
    }

    #[test]
    fn output_format_json_schema_forces_anthropic_tool_use() {
        let mut payload = base_payload();
        payload.output_format = crate::llm::api::OutputFormat::JsonSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {"answer": {"type": "string", "pattern": "^ok"}},
                "required": ["answer"],
            }),
            strict: true,
        };

        let body = AnthropicProvider::build_request_body(&payload);

        assert_eq!(
            body["tool_choice"],
            serde_json::json!({"type": "tool", "name": "json_response"})
        );
        let tools = body["tools"].as_array().expect("tools array");
        let json_tool = tools
            .iter()
            .find(|tool| tool.get("name").and_then(|value| value.as_str()) == Some("json_response"))
            .expect("json_response tool");
        assert_eq!(
            json_tool["input_schema"]["properties"]["answer"]["type"],
            "string"
        );
        assert_eq!(json_tool["input_schema"]["additionalProperties"], false);
        assert!(json_tool["input_schema"]["properties"]["answer"]
            .get("pattern")
            .is_none());
    }

    #[test]
    fn forced_json_overrides_caller_tools_and_tool_choice() {
        // Caller supplies their own native tool AND tool_choice, then also asks
        // for structured output. Structured output wins (the documented
        // precedence) instead of silently leaving the caller's tool_choice in
        // place: tool_choice is pinned to json_response and that tool is added,
        // while the caller's tool is preserved in the array (just unreachable).
        let mut payload = base_payload();
        payload.native_tools = Some(vec![serde_json::json!({
            "name": "lookup",
            "description": "look something up",
            "input_schema": {"type": "object"},
        })]);
        payload.tool_choice = Some(serde_json::json!({"type": "auto"}));
        payload.output_format = crate::llm::api::OutputFormat::JsonObject;

        let body = AnthropicProvider::build_request_body(&payload);

        assert_eq!(
            body["tool_choice"],
            serde_json::json!({"type": "tool", "name": "json_response"}),
            "structured output must win over the caller's tool_choice"
        );
        let tool_names: Vec<&str> = body["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(|value| value.as_str()))
            .collect();
        assert!(tool_names.contains(&"json_response"));
        assert!(
            tool_names.contains(&"lookup"),
            "the caller's tool is preserved, not dropped: {tool_names:?}"
        );
    }

    #[test]
    fn tool_choice_string_modes_become_anthropic_objects() {
        // The OpenAI/agent-loop wire shape is a bare string. Anthropic 400s on
        // a bare string, so each mode must be rewritten to its object form.
        for (input, expected) in [
            ("auto", serde_json::json!({"type": "auto"})),
            ("any", serde_json::json!({"type": "any"})),
            ("required", serde_json::json!({"type": "any"})),
            ("none", serde_json::json!({"type": "none"})),
        ] {
            let mut payload = base_payload();
            payload.tool_choice = Some(serde_json::json!(input));
            let body = AnthropicProvider::build_request_body(&payload);
            assert_eq!(
                body["tool_choice"], expected,
                "tool_choice \"{input}\" must serialize to an object"
            );
            assert!(
                body["tool_choice"].is_object(),
                "Anthropic rejects a non-object tool_choice"
            );
        }
    }

    #[test]
    fn tool_choice_bare_string_names_a_specific_tool() {
        // A non-keyword bare string is treated as "force this tool by name".
        let mut payload = base_payload();
        payload.tool_choice = Some(serde_json::json!("read_file"));
        let body = AnthropicProvider::build_request_body(&payload);
        assert_eq!(
            body["tool_choice"],
            serde_json::json!({"type": "tool", "name": "read_file"})
        );
    }

    #[test]
    fn tool_choice_openai_function_object_maps_to_anthropic_tool() {
        // OpenAI's specific-tool shape is `{"type":"function","function":{...}}`.
        let mut payload = base_payload();
        payload.tool_choice = Some(serde_json::json!({
            "type": "function",
            "function": {"name": "read_file"},
        }));
        let body = AnthropicProvider::build_request_body(&payload);
        assert_eq!(
            body["tool_choice"],
            serde_json::json!({"type": "tool", "name": "read_file"})
        );
    }

    #[test]
    fn tool_choice_already_anthropic_object_is_preserved() {
        // Callers that already speak Anthropic must pass through unchanged,
        // including the optional disable_parallel_tool_use flag.
        let mut payload = base_payload();
        payload.tool_choice = Some(serde_json::json!({
            "type": "tool",
            "name": "read_file",
            "disable_parallel_tool_use": true,
        }));
        let body = AnthropicProvider::build_request_body(&payload);
        assert_eq!(
            body["tool_choice"],
            serde_json::json!({
                "type": "tool",
                "name": "read_file",
                "disable_parallel_tool_use": true,
            })
        );
    }

    #[test]
    fn tool_choice_null_leaves_field_unset() {
        let mut payload = base_payload();
        payload.tool_choice = Some(serde_json::Value::Null);
        let body = AnthropicProvider::build_request_body(&payload);
        assert!(
            body.get("tool_choice").is_none(),
            "a null tool_choice must not be forwarded"
        );
    }

    #[test]
    fn classifies_anthropic_overloaded_error_as_transient_server_error() {
        let info = AnthropicProvider::classify_http_error(
            reqwest::StatusCode::from_u16(529).unwrap(),
            None,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        );
        assert_eq!(info.kind, LlmErrorKind::Transient);
        assert_eq!(info.reason, LlmErrorReason::ServerError);
    }

    #[test]
    fn classifies_anthropic_auth_error_as_terminal_auth_failure() {
        let info = AnthropicProvider::classify_http_error(
            reqwest::StatusCode::UNAUTHORIZED,
            None,
            r#"{"type":"error","error":{"type":"authentication_error","message":"bad key"}}"#,
        );
        assert_eq!(info.kind, LlmErrorKind::Terminal);
        assert_eq!(info.reason, LlmErrorReason::AuthFailure);
    }

    #[test]
    fn image_content_maps_to_anthropic_source_block() {
        let mut payload = base_payload();
        payload.messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "caption"},
                {"type": "image", "base64": "iVBORw0KGgo=", "media_type": "image/png"}
            ],
        })];

        let body = AnthropicProvider::build_request_body(&payload);
        assert_eq!(body["messages"][0]["content"][0]["text"], "caption");
        assert_eq!(
            body["messages"][0]["content"][1],
            serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": "iVBORw0KGgo=",
                }
            })
        );
    }

    #[test]
    fn image_url_content_maps_to_anthropic_url_source() {
        let mut payload = base_payload();
        payload.messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "image", "url": "https://example.com/image.png", "media_type": "image/png"}
            ],
        })];

        let body = AnthropicProvider::build_request_body(&payload);
        assert_eq!(
            body["messages"][0]["content"][0],
            serde_json::json!({
                "type": "image",
                "source": {
                    "type": "url",
                    "url": "https://example.com/image.png",
                }
            })
        );
    }

    #[test]
    fn pdf_file_id_content_maps_to_anthropic_document_block() {
        let mut payload = base_payload();
        payload.messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "pdf", "file_id": "file_123", "title": "Report"}
            ],
        })];

        let body = AnthropicProvider::build_request_body(&payload);
        assert_eq!(
            body["messages"][0]["content"][0],
            serde_json::json!({
                "type": "document",
                "source": {
                    "type": "file",
                    "file_id": "file_123",
                },
                "title": "Report",
            })
        );
    }

    #[test]
    fn audio_base64_content_maps_to_anthropic_audio_block() {
        let mut payload = base_payload();
        payload.messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "audio", "base64": "UklGRg==", "media_type": "audio/wav"}
            ],
        })];

        let body = AnthropicProvider::build_request_body(&payload);
        assert_eq!(
            body["messages"][0]["content"][0],
            serde_json::json!({
                "type": "audio",
                "source": {
                    "type": "base64",
                    "media_type": "audio/wav",
                    "data": "UklGRg==",
                }
            })
        );
    }

    #[test]
    fn cache_uses_top_level_automatic_prompt_caching() {
        let mut payload = base_payload();
        payload.cache = true;

        let body = AnthropicProvider::build_request_body(&payload);
        assert_eq!(
            body["cache_control"],
            serde_json::json!({"type": "ephemeral"})
        );
        assert_eq!(body["system"].as_str(), Some("system prompt"));
        assert_eq!(
            body["tools"].as_array().map(Vec::len),
            Some(1),
            "tool definitions remain in the top-level cached prefix"
        );
    }
}
