//! Anthropic Messages API provider (Claude models).

use std::cell::RefCell;
use std::collections::HashSet;

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult, ReasoningEffort, ThinkingConfig};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::value::VmError;
pub(crate) use harn_provider_catalog::capabilities::claude_generation;
use harn_provider_catalog::capabilities::is_claude_model_id;

pub(crate) use harn_llm::wire::anthropic::{
    forced_tool_choice_allowed, tool_choice_forces_tool_use,
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
    // Sonnet 5.5's `between_tools` off switch has the same `high` ceiling.
    let thinking_disabled = is_thinking_off_type(
        body.get("thinking")
            .and_then(|thinking| thinking.get("type"))
            .and_then(serde_json::Value::as_str),
    );
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

/// Models whose thinking is always on (Fable, Mythos, Opus 5.5+) reject an
/// explicit `thinking: {type:"disabled"}` with a 400. The catalog owns that
/// fact as `reasoning_disable_supported`; Sonnet 5 and Opus 5 default thinking
/// on but still accept the explicit off switch.
fn model_rejects_disabled_thinking(model: &str) -> bool {
    !crate::llm::capabilities::lookup("anthropic", model).reasoning_disable_supported
}

/// The `thinking.type` this model's catalog row names as its off switch:
/// `disabled` through Sonnet 5, `between_tools` on Sonnet 5.5.
fn thinking_off_type(model: &str) -> crate::llm::capabilities::ThinkingOffType {
    crate::llm::capabilities::lookup("anthropic", model).thinking_off_type
}

/// Whether `thinking.type` is an off switch rather than a thinking mode.
fn is_thinking_off_type(value: Option<&str>) -> bool {
    use crate::llm::capabilities::ThinkingOffType;
    [ThinkingOffType::Disabled, ThinkingOffType::BetweenTools]
        .iter()
        .any(|off| value == Some(off.as_str()))
}

/// Lower a thinking-off request onto the wire.
///
/// Through Opus 4.8 an omitted `thinking` field is the off switch, so nothing
/// is written. Generation-5 models think when the field is omitted, so the
/// ones that accept an off switch get it explicitly, in the shape their
/// catalog row names (`disabled`, or `between_tools` on Sonnet 5.5, where
/// `disabled` is a 400). The always-on models reject both, and their closest
/// legal request is the lowest effort: without it they think at the API
/// default (`medium` on Opus 5.5, `high` on Fable), which is the opposite of
/// what the caller asked for. An effort the caller already set is left alone.
fn lower_thinking_off(body: &mut serde_json::Value, model: &str) {
    if !model_defaults_to_adaptive_thinking(model) {
        // Omitted is already off here; drop an off switch these models do
        // not know (a `between_tools` override is a 400 on them).
        let off_type = body
            .get("thinking")
            .and_then(|thinking| thinking.get("type"))
            .and_then(serde_json::Value::as_str);
        if off_type == Some(crate::llm::capabilities::ThinkingOffType::BetweenTools.as_str()) {
            if let Some(object) = body.as_object_mut() {
                object.remove("thinking");
            }
        }
        return;
    }
    if !model_rejects_disabled_thinking(model) {
        body["thinking"] = serde_json::json!({ "type": thinking_off_type(model).as_str() });
        return;
    }
    if let Some(object) = body.as_object_mut() {
        object.remove("thinking");
    }
    let has_effort = body
        .get("output_config")
        .and_then(|config| config.get("effort"))
        .is_some();
    if !has_effort && model_supports_anthropic_effort(model) {
        set_output_config_effort(body, ANTHROPIC_EFFORT_LADDER[0]);
    }
}

/// A caller override merged after the builder can still carry an off switch
/// the model rejects: `{type:"disabled"}` to an always-on model or to Sonnet
/// 5.5, or `{type:"between_tools"}` to a model that only knows `disabled`.
/// Each is a 400. Re-lower it the same way the builder lowers a thinking-off
/// request.
fn relower_disabled_thinking_override(body: &mut serde_json::Value, model: &str) {
    let requested = body
        .get("thinking")
        .and_then(|thinking| thinking.get("type"))
        .and_then(serde_json::Value::as_str);
    if !is_thinking_off_type(requested) {
        return;
    }
    if model_rejects_disabled_thinking(model)
        || requested != Some(thinking_off_type(model).as_str())
    {
        lower_thinking_off(body, model);
    }
}

/// The thinking budget when the caller enabled thinking without naming one.
///
/// Anthropic refuses `budget_tokens >= max_tokens` with a 400, and a fixed
/// 10,000 default made `thinking: true` fail on every call capped at or below
/// it. Keep the budget under the cap, at Anthropic's 1,024 floor at least.
pub(super) fn default_thinking_budget(max_tokens: i64) -> i64 {
    (max_tokens - 1).clamp(1024, 10_000)
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
    provider: &str,
    fallback_model: &str,
    thinking: &ThinkingConfig,
    provider_contract_probe: Option<crate::llm::capabilities::PortableOption>,
) {
    // Provider overrides are merged before this seam and may replace `model`
    // alongside messages. Every model-dependent guard must therefore read the
    // value that will actually be sent. An invalid non-string override maps to
    // the empty unknown model and fails closed instead of inheriting an older,
    // more permissive catalog row.
    let wire_model = body.get("model").map_or_else(
        || fallback_model.to_owned(),
        |value| value.as_str().unwrap_or_default().to_owned(),
    );
    remove_unsupported_assistant_prefill(body, provider, &wire_model);
    strip_unsupported_sampling_params(body, &wire_model, thinking, provider_contract_probe);
    if crate::llm::catalog_may_shape_requested_reasoning() {
        relower_disabled_thinking_override(body, &wire_model);
        clamp_effort_for_disabled_thinking(body, &wire_model);
    }
}

fn remove_unsupported_assistant_prefill(body: &mut serde_json::Value, provider: &str, model: &str) {
    let supports_prefill =
        crate::llm::capabilities::lookup(provider, model).supports_assistant_prefill;
    let uses_native_schema = body
        .pointer("/output_config/format/type")
        .and_then(serde_json::Value::as_str)
        == Some("json_schema");
    // Anthropic also rejects response prefill whenever thinking is on. Treat
    // an authored but unknown thinking shape as enabled so a future wire mode
    // cannot silently bypass this compatibility boundary.
    let thinking_on = body.get("thinking").is_some_and(|thinking| {
        thinking.get("type").and_then(serde_json::Value::as_str) != Some("disabled")
    });
    if supports_prefill && !uses_native_schema && !thinking_on {
        return;
    }
    let Some(messages) = body
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    let original_len = messages.len();
    // At the Messages API boundary, every trailing assistant turn is a
    // response prefill regardless of how it entered durable history. Remove
    // the complete trailing run, not just a caller's `prefill` option: loop
    // continuation, replay, direct messages, and provider overrides can all
    // produce the same wire shape. The durable transcript remains unchanged.
    while messages.last().and_then(|message| message["role"].as_str()) == Some("assistant") {
        messages.pop();
    }
    if messages.len() == original_len {
        return;
    }
    let reason = if uses_native_schema {
        "Anthropic native structured output is incompatible with prefill"
    } else if thinking_on {
        "Anthropic thinking is incompatible with prefill"
    } else {
        "this Anthropic model does not support prefill"
    };
    warn_anthropic_prefill_skipped(model, reason);
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
    ) -> crate::llm::provider::ProviderChatFuture<'a> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

impl AnthropicProvider {
    /// Build the Anthropic-style pre-override request body.
    ///
    /// Every live sender must apply caller/provider overrides and then call
    /// [`reconcile_request_body`] before serialization. That final seam owns
    /// lossy capability admission against the actual wire model and shape.
    pub(crate) fn build_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let caps = crate::llm::capabilities::lookup(&opts.provider, &opts.model);
        let wire_model = crate::llm_config::wire_model_id(&opts.model);
        let context = harn_llm::wire::anthropic::AnthropicWireContext {
            capabilities: &caps,
            wire_model: &wire_model,
            prompt_cache: crate::llm::prompt_cache::for_request(opts),
            tool_search_extensions:
                crate::llm::provider::anthropic_tool_search_wire_extensions_enabled(
                    &opts.provider,
                    &opts.model,
                    opts.native_tools.as_deref().unwrap_or(&[]),
                ),
        };
        let mut encoded = harn_llm::wire::anthropic::encode_request(opts, &context);
        strip_unsupported_sampling_params(
            &mut encoded.body,
            &opts.model,
            &opts.thinking,
            opts.provider_contract_probe,
        );
        for effect in encoded.effects {
            match effect {
                harn_llm::wire::anthropic::AnthropicWireEffect::SchemaAudit(metadata) => {
                    crate::llm::providers::schema_compat::publish_audit(metadata);
                }
                harn_llm::wire::anthropic::AnthropicWireEffect::ForcedJsonOverridesTools => {
                    warn_forced_json_overrides_tools(&opts.model);
                }
            }
        }
        let mut body = encoded.body;
        let anthropic_max = body["max_tokens"]
            .as_i64()
            .expect("encoder writes an integer token cap");
        match &opts.thinking {
            // Claude Opus 4.7+ replaced extended thinking with adaptive
            // thinking; `type: enabled` returns HTTP 400. Rewrite the
            // payload transparently rather than fighting the deprecation.
            // Omitting `thinking` is only an off switch through Opus 4.8. On
            // generation-5 models the field defaults to adaptive, so a
            // `Disabled` config has to say so explicitly or the model thinks
            // anyway (and bills for it).
            ThinkingConfig::Disabled => lower_thinking_off(&mut body, &opts.model),
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
                } else {
                    lower_thinking_off(&mut body, &opts.model);
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
                    "budget_tokens": budget_tokens
                        .map(i64::from)
                        .unwrap_or_else(|| default_thinking_budget(anthropic_max)),
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
