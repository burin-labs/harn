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

mod tool_history;

use tool_history::{
    assistant_tool_use_ids, normalize_tool_call_ids, preserve_orphan_results_as_text,
};

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
    static ANTHROPIC_DISABLED_EFFORT_WARN_ONCE: RefCell<HashSet<String>> =
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
#[expect(
    clippy::string_slice,
    reason = "idx comes from find() of the ASCII needle on the sliced string"
)]
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

fn anthropic_cache_control(ttl: Option<crate::llm::api::PromptCacheTtl>) -> serde_json::Value {
    let mut cache_control = serde_json::json!({"type": "ephemeral"});
    if let Some(ttl) = ttl.and_then(crate::llm::api::PromptCacheTtl::anthropic_ttl_field) {
        cache_control["ttl"] = serde_json::json!(ttl);
    }
    cache_control
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
pub(crate) fn model_requires_adaptive_thinking(model: &str) -> bool {
    let lower = model.to_lowercase();
    matches!(claude_generation(&lower), Some((major, minor)) if (major, minor) >= (4, 7))
}

/// True for Claude models whose adaptive thinking is on by default. These
/// models don't need a `thinking: {type:"adaptive"}` request field when
/// `output_config.effort` is enough to steer the default-on reasoning — and,
/// conversely, an omitted `thinking` field is *not* an off switch on them, so
/// a `Disabled` config has to be sent explicitly.
///
/// Generation 5 is the dividing line: Fable 5, Mythos 5, Sonnet 5, and
/// Opus 5 all think when `thinking` is omitted, while Opus 4.8 and every
/// earlier model do not. Keying off the parsed generation rather than a list
/// of ids means the next gen-5 family lands correctly instead of silently
/// inheriting the 4.x "omitted means off" assumption.
pub(super) fn model_defaults_to_adaptive_thinking(model: &str) -> bool {
    matches!(claude_generation(model), Some((major, _)) if major >= 5)
}

/// Anthropic's `output_config.effort` ladder, lowest first. Used to compare
/// two effort strings without threading the provider-neutral
/// [`ReasoningEffort`] enum through the raw-body path.
const ANTHROPIC_EFFORT_LADDER: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Clamp `output_config.effort` to the highest level that is legal alongside an
/// explicit `thinking: {type: "disabled"}`.
///
/// Generation-5 Claude models reject the pair above `high`:
/// `output_config.effort 'xhigh' is not supported when thinking is disabled on
/// this model. Use effort 'high' or below, or enable thinking.` The two halves
/// of that pair are set independently — thinking by the request builder,
/// effort possibly by a caller override merged in afterwards — so the check
/// reads both off the final body rather than off the [`ThinkingConfig`].
fn clamp_effort_for_disabled_thinking(body: &mut serde_json::Value, model: &str) {
    const CEILING: &str = "high";
    if !matches!(claude_generation(model), Some((major, _)) if major >= 5) {
        return;
    }
    let thinking_disabled = body
        .get("thinking")
        .and_then(|thinking| thinking.get("type"))
        .and_then(serde_json::Value::as_str)
        == Some("disabled");
    if !thinking_disabled {
        return;
    }
    let Some(effort) = body
        .get("output_config")
        .and_then(|config| config.get("effort"))
        .and_then(serde_json::Value::as_str)
    else {
        return;
    };
    let rank = |level: &str| ANTHROPIC_EFFORT_LADDER.iter().position(|&e| e == level);
    if rank(effort) <= rank(CEILING) {
        return;
    }
    warn_disabled_thinking_effort_clamped(model, effort);
    set_output_config_effort(body, CEILING);
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
    set_output_config_field(body, "effort", serde_json::json!(effort));
}

fn set_output_config_field(body: &mut serde_json::Value, key: &str, value: serde_json::Value) {
    let Some(body_object) = body.as_object_mut() else {
        return;
    };
    let output_config = body_object
        .entry("output_config")
        .or_insert_with(|| serde_json::json!({}));
    if !output_config.is_object() {
        *output_config = serde_json::json!({});
    }
    output_config[key] = value;
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

fn warn_anthropic_prefill_skipped(model: &str, reason: &str) {
    ANTHROPIC_PREFILL_WARN_ONCE.with(|seen| {
        let mut seen = seen.borrow_mut();
        if seen.insert(model.to_string()) {
            crate::events::log_warn(
                "llm.prefill",
                &format!(
                    "assistant prefill requested for {model}, but {reason}; sending without it",
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

/// Reconcile a fully-merged Anthropic request body against the resolved
/// model's constraints, immediately before egress.
///
/// This is the one seam that sees the *final* body — after
/// `apply_provider_wire_overrides` has merged caller-supplied fields on top of
/// whatever the provider builder produced. Guards that must not be defeated by
/// a caller override belong here rather than in `build_request_body`.
pub(crate) fn reconcile_request_body(
    body: &mut serde_json::Value,
    model: &str,
    thinking: &ThinkingConfig,
    provider_contract_probe: Option<crate::llm::capabilities::PortableOption>,
) {
    strip_unsupported_sampling_params(body, model, thinking, provider_contract_probe);
    if crate::llm::catalog_may_shape_requested_reasoning() {
        clamp_effort_for_disabled_thinking(body, model);
    }
}

/// Remove Anthropic sampling parameters when the resolved Claude request
/// surface rejects them. Call [`reconcile_request_body`] instead from egress
/// paths; this stays separate so the sampling policy is testable on its own.
pub(crate) fn strip_unsupported_sampling_params(
    body: &mut serde_json::Value,
    model: &str,
    thinking: &ThinkingConfig,
    provider_contract_probe: Option<crate::llm::capabilities::PortableOption>,
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
    let mut had_sampling = false;
    for (field, option) in [
        (
            "temperature",
            crate::llm::capabilities::PortableOption::Temperature,
        ),
        ("top_p", crate::llm::capabilities::PortableOption::TopP),
        ("top_k", crate::llm::capabilities::PortableOption::TopK),
    ] {
        if crate::llm::provider_contract_probe::catalog_may_shape_requested_portable_option(
            provider_contract_probe,
            option,
        ) {
            had_sampling = object.remove(field).is_some() || had_sampling;
        }
    }
    if had_sampling {
        warn_sampling_stripped(model);
    }
}

/// Bedrock Converse nests sampling parameters under `inferenceConfig`, but
/// Bedrock-hosted Claude follows the same Anthropic sampling restrictions as
/// direct Claude request surfaces.
pub(crate) fn strip_unsupported_bedrock_converse_sampling_params(
    body: &mut serde_json::Value,
    model: &str,
    thinking: &ThinkingConfig,
    provider_contract_probe: Option<crate::llm::capabilities::PortableOption>,
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

    let had_temperature =
        crate::llm::provider_contract_probe::catalog_may_shape_requested_portable_option(
            provider_contract_probe,
            crate::llm::capabilities::PortableOption::Temperature,
        ) && inference.remove("temperature").is_some();
    let had_top_p = crate::llm::provider_contract_probe::catalog_may_shape_requested_portable_option(
        provider_contract_probe,
        crate::llm::capabilities::PortableOption::TopP,
    ) && inference.remove("topP").is_some();
    let had_top_k = crate::llm::provider_contract_probe::catalog_may_shape_requested_portable_option(
        provider_contract_probe,
        crate::llm::capabilities::PortableOption::TopK,
    ) && inference.remove("topK").is_some();
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

fn warn_disabled_thinking_effort_clamped(model: &str, requested: &str) {
    ANTHROPIC_DISABLED_EFFORT_WARN_ONCE.with(|seen| {
        let mut seen = seen.borrow_mut();
        if seen.insert(model.to_string()) {
            crate::events::log_warn(
                "llm.thinking",
                &format!(
                    "effort `{requested}` is rejected for {model} while thinking is \
                     disabled; clamping to `high` (enable thinking to use \
                     `{requested}`)",
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
    /// Build the Anthropic-style request body.
    pub(crate) fn build_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let caps = crate::llm::capabilities::lookup(&opts.provider, &opts.model);
        let anthropic_max = if opts.max_tokens > 0 {
            opts.max_tokens
        } else {
            8192
        };
        let mut messages: Vec<serde_json::Value> = opts
            .messages
            .iter()
            .cloned()
            .map(|mut message| {
                crate::llm::reasoning_history::restore_anthropic_continuation(
                    &mut message,
                    caps.reasoning_round_trip,
                );
                message
            })
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
                            crate::llm::content::anthropic_content_for_request(
                                &content,
                                caps.reasoning_round_trip,
                            ),
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
        normalize_tool_call_ids(&mut messages);
        let mut messages = enforce_tool_result_adjacency(messages);
        preserve_orphan_results_as_text(&mut messages);
        if let Some(ref prefill) = opts.prefill {
            let uses_native_schema = matches!(
                &opts.output_format,
                crate::llm::api::OutputFormat::JsonSchema { .. }
            ) && caps.structured_output.as_deref() == Some("native");
            // Anthropic rejects message prefill both on models that removed
            // the feature and on requests using native structured output.
            // The capability catalog is the semantic owner of model support;
            // the request shape adds the one per-call incompatibility.
            if caps.supports_assistant_prefill && !uses_native_schema {
                messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": prefill,
                }));
            } else if uses_native_schema {
                warn_anthropic_prefill_skipped(
                    &opts.model,
                    "Anthropic native structured output is incompatible with prefill",
                );
            } else {
                warn_anthropic_prefill_skipped(
                    &opts.model,
                    "this Anthropic model does not support prefill",
                );
            }
        }
        let wire_model = crate::llm_config::wire_model_id(&opts.model);
        let mut body = serde_json::json!({
            "model": wire_model,
            "messages": messages,
            "max_tokens": anthropic_max,
        });
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
        strip_unsupported_sampling_params(
            &mut body,
            &opts.model,
            &opts.thinking,
            opts.provider_contract_probe,
        );
        if let Some(ref stop) = opts.stop {
            body["stop_sequences"] = serde_json::json!(stop);
        }
        crate::llm::prompt_cache::apply_prompt_cache_breakpoint(
            &mut body,
            opts.cache,
            &caps,
            anthropic_cache_control(opts.prompt_cache_ttl),
        );
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
        if body.get("tools").is_some() {
            if let Some(parallel) = opts.parallel_tool_calls {
                if body.get("tool_choice").is_none() {
                    body["tool_choice"] = serde_json::json!({"type": "auto"});
                }
                body["tool_choice"]["disable_parallel_tool_use"] = serde_json::json!(!parallel);
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
                if caps.structured_output.as_deref() == Some("native") {
                    set_native_json_schema_output(&mut body, schema, &opts.model);
                } else {
                    force_json_via_tool_use(&mut body, schema, &opts.model);
                }
            }
        }
        match &opts.thinking {
            // Claude Opus 4.7+ replaced extended thinking with adaptive
            // thinking; `type: enabled` returns HTTP 400. Rewrite the
            // payload transparently rather than fighting the deprecation.
            // Omitting `thinking` is only an off switch through Opus 4.8. On
            // generation-5 models the field defaults to adaptive, so a
            // `Disabled` config has to say so explicitly or the model thinks
            // anyway (and bills for it).
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
                    if model_supports_anthropic_effort(&opts.model)
                        || !crate::llm::catalog_may_shape_requested_reasoning()
                    {
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
        let dialect = crate::llm::api::DialectContract::for_request(request);
        crate::llm::api::vm_call_llm_api_with_body(
            request,
            delta_tx,
            dialect.build_request_body(request),
            dialect,
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
        .or_else(|| message.get("call_id"))
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
        let input = match function
            .and_then(|f| f.get("arguments"))
            .or_else(|| call.get("arguments"))
        {
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

/// Whether Anthropic's normalized tool choice requires a tool call.
pub(crate) fn tool_choice_forces_tool_use(value: &serde_json::Value) -> bool {
    normalize_anthropic_tool_choice(value).is_some_and(|choice| {
        matches!(
            choice.get("type").and_then(serde_json::Value::as_str),
            Some("any") | Some("tool")
        )
    })
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

/// Lower Harn's provider-neutral schema contract to Anthropic's native JSON
/// output grammar. Unlike the legacy synthetic-tool fallback, this keeps both
/// thinking and the caller's tool surface available on the same turn.
fn set_native_json_schema_output(
    body: &mut serde_json::Value,
    schema: &serde_json::Value,
    model: &str,
) {
    let schema = sanitize_schema_for_provider(
        "anthropic",
        model,
        SchemaCompatProfile::AnthropicStrict,
        SchemaSurface::StructuredOutput,
        schema,
    );
    set_output_config_field(
        body,
        "format",
        serde_json::json!({"type": "json_schema", "schema": schema}),
    );
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
mod tests;
