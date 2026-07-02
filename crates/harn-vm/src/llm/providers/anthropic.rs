//! Anthropic Messages API provider (Claude models).

use std::cell::RefCell;
use std::collections::HashSet;

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult, ReasoningEffort, ThinkingConfig};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::llm::providers::common::parse_major_minor_tail;
use crate::value::VmError;

pub(crate) const ANTHROPIC_INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";

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
    if !lower.starts_with("claude-") && !lower.contains("/claude-") {
        return None;
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
                    "temperature/top_p/top_k supplied for {model}, but Anthropic \
                     Opus 4.7+ rejects non-default sampling params with HTTP 400; \
                     stripping them from the request",
                ),
            );
        }
    });
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
        // Claude Opus 4.7+ rejects non-default sampling parameters with
        // HTTP 400. We strip them transparently and warn once per model
        // so pipeline authors don't have to special-case each release.
        //
        // Anthropic also rejects `temperature != 1` when thinking is
        // active (adaptive, effort, or enabled): "temperature may only
        // be set to 1 when thinking is enabled". Strip sampling params
        // in that case too so callers can default to temperature=0 for
        // determinism without having to know which models silently
        // auto-enable thinking.
        let thinking_active = !matches!(opts.thinking, ThinkingConfig::Disabled);
        let strip_sampling = model_rejects_sampling_params(&opts.model) || thinking_active;
        let any_sampling_supplied =
            opts.temperature.is_some() || opts.top_p.is_some() || opts.top_k.is_some();
        if strip_sampling && any_sampling_supplied {
            warn_sampling_stripped(&opts.model);
        }
        if !strip_sampling {
            if let Some(temp) = opts.temperature {
                body["temperature"] = serde_json::json!(temp);
            }
            if let Some(top_p) = opts.top_p {
                body["top_p"] = serde_json::json!(top_p);
            }
            if let Some(top_k) = opts.top_k {
                body["top_k"] = serde_json::json!(top_k);
            }
        }
        if let Some(ref stop) = opts.stop {
            body["stop_sequences"] = serde_json::json!(stop);
        }
        if let Some(ref tools) = opts.native_tools {
            if !tools.is_empty() {
                let sanitized: Vec<serde_json::Value> = tools
                    .iter()
                    .map(sanitize_anthropic_tool_for_request)
                    .collect();
                body["tools"] = serde_json::json!(sanitized);
            }
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
        crate::llm::fast_mode::apply_request_knob(&mut body, &opts.model, opts.fast);
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
            true,  // is_anthropic_style
            false, // is_ollama
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
fn sanitize_anthropic_tool_for_request(tool: &serde_json::Value) -> serde_json::Value {
    let mut tool = tool.clone();
    if let Some(object) = tool.as_object_mut() {
        object.remove("x-harn-output-schema");
        object.remove("defer_loading");
        object.remove("namespace");
        object.remove("namespaces");
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
        }
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
    fn fast_mode_injects_speed_knob_and_beta_header() {
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
    fn fast_mode_knob_absent_when_off() {
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
    fn native_tools_strip_harn_internal_extensions() {
        let mut payload = base_payload();
        payload.native_tools = Some(vec![serde_json::json!({
            "name": "read_file",
            "description": "Read a file",
            "input_schema": {"type": "object"},
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
    }

    #[test]
    fn output_format_json_schema_forces_anthropic_tool_use() {
        let mut payload = base_payload();
        payload.output_format = crate::llm::api::OutputFormat::JsonSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {"answer": {"type": "string"}},
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
