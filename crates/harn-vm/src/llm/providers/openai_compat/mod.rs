//! OpenAI-compatible provider — covers OpenAI, OpenRouter, Together, Groq,
//! DeepSeek, Fireworks, HuggingFace, local vLLM/SGLang, and any server that
//! speaks the `/v1/chat/completions` protocol.
//!
//! This module owns the provider type itself and the assembly of a
//! `/v1/chat/completions` request body; each wire concern the body needs is
//! delegated to a sibling module:
//!
//! - [`messages`] — transcript history to an OpenAI-legal `messages` array.
//! - [`tools`] — tool definitions and `tool_choice` on the request.
//! - [`reasoning`] — per-dialect reasoning/thinking request shapes.
//! - [`prompt_cache`] — prompt-cache breakpoint placement.
//! - [`openrouter`] — OpenRouter upstream-routing directives.
//! - [`streaming`] — live-delta canonicalization for reserved-token models.

mod messages;
mod openrouter;
mod prompt_cache;
mod reasoning;
mod streaming;
mod tools;

#[cfg(test)]
mod tests;

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult, ThinkingConfig};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::llm::providers::common::parse_major_minor_tail;
use crate::llm::providers::schema_compat::{
    sanitize_schema_for_provider, SchemaCompatProfile, SchemaSurface,
};
use crate::value::VmError;

use self::messages::{
    drop_orphan_tool_result_messages, enforce_single_tool_call_history,
    enforce_tool_result_adjacency, maybe_remap_tool_call_text,
    relocate_tool_message_images_to_user, sanitize_openai_message_for_request,
};
use self::prompt_cache::apply_prompt_cache_breakpoint;
use self::reasoning::{
    enabled_reasoning_config, is_openrouter_reasoning_disable, minimax_thinking_config,
    model_declares_reasoning, openrouter_reasoning_config, zai_thinking_config,
};
use self::streaming::canonicalizing_delta_tx;
use self::tools::{normalize_tool_choice_for_capabilities, provider_request_tools};

pub(crate) use self::openrouter::{
    apply_openrouter_provider_order, apply_openrouter_route_denylist,
    ensure_openrouter_require_parameters,
};

/// Parse the (major, minor) version out of a GPT model ID. Handles dotted
/// forms like `gpt-5.4`, `gpt-5.4-preview`, `gpt-5.4-turbo-20260115`, and
/// dashed forms like `gpt-5-4`. Also strips OpenRouter-style prefixes
/// (`openai/gpt-5.4`, `azure/gpt-5.4`) so the same parser can gate
/// capabilities regardless of which OpenAI-compatible provider is routing.
///
/// Returns `None` for non-GPT shapes (`claude-opus-4-7`, `llama-3.1`, …).
pub(crate) fn gpt_generation(model: &str) -> Option<(u32, u32)> {
    let lower = model.to_lowercase();
    let stripped = match lower.rsplit_once('/') {
        Some((_, tail)) => tail,
        None => lower.as_str(),
    };
    let idx = stripped.find("gpt-")?;
    parse_major_minor_tail(&stripped[idx + "gpt-".len()..])
}

/// True for GPT models that expose OpenAI's Responses-API `tool_search` meta-tool
/// and the `defer_loading: true` flag on user tool definitions. Per OpenAI's
/// docs, the feature is gated on GPT 5.4+ (hosted + client-executed modes).
/// We intentionally ignore legacy `gpt-4*`, `gpt-3.5*`, and any non-GPT model;
/// those fall back to the client-executed path from harn#70.
///
/// Retained only as a pure-parse helper for `capabilities::lookup` callers
/// that want to ask the model-ID question without loading the full rule
/// table. The authoritative gate is
/// `capabilities::lookup(provider, model).defer_loading`.
#[allow(dead_code)]
pub(crate) fn gpt_model_supports_tool_search(model: &str) -> bool {
    match gpt_generation(model) {
        Some((major, minor)) => (major, minor) >= (5, 4),
        None => false,
    }
}

/// OpenAI-compatible provider parameterized by name. A single struct handles
/// all OpenAI-style backends — the provider name is used to resolve config
/// (base URL, auth, etc.) from `llm_config`.
pub(crate) struct OpenAiCompatibleProvider {
    provider_name: String,
}

impl OpenAiCompatibleProvider {
    pub(crate) fn new(name: String) -> Self {
        Self {
            provider_name: name,
        }
    }

    pub(crate) fn classify_http_error(
        provider: &str,
        status: reqwest::StatusCode,
        retry_after: Option<&str>,
        body: &str,
    ) -> crate::llm::api::LlmErrorInfo {
        crate::llm::api::classify_provider_http_error(provider, status, retry_after, body)
    }
}

impl LlmProvider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        &self.provider_name
    }

    /// Apply provider-specific request body transformations.
    fn transform_request(&self, body: &mut serde_json::Value) {
        let model = body
            .get("model")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let caps = crate::llm::capabilities::lookup(&self.provider_name, model);
        if let Some(object) = body.as_object_mut() {
            let allowed_field = if caps.honors_chat_template_kwargs {
                Some(chat_template_options_field(&caps))
            } else {
                None
            };
            for field in ["chat_template_kwargs", "chat_template_args"] {
                if allowed_field != Some(field) {
                    object.remove(field);
                }
            }
        }
    }

    // `supports_defer_loading` and `native_tool_search_variants` are
    // served by the default trait impl, which reads `capabilities.toml`.
    // The `gpt_model_supports_tool_search` helper below is retained for
    // shape detection in `helpers/options.rs::classify_native_shape`
    // (deciding Anthropic- vs OpenAI-wire shape for the mock provider).
}

impl LlmProviderChat for OpenAiCompatibleProvider {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResult, VmError>> + 'a>> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

impl OpenAiCompatibleProvider {
    /// Build the OpenAI-compatible request body.
    pub(crate) fn build_request_body(
        opts: &LlmRequestPayload,
        force_string_content: bool,
    ) -> serde_json::Value {
        let caps = crate::llm::capabilities::lookup(&opts.provider, &opts.model);
        // Models that reserve `<tool_call>` as a special token collapse when
        // they meet it as instructional/wrapper text. Remap the colliding
        // delimiters to a non-special wire form on every outgoing message
        // (system + full history + prefill); the response is mapped back in
        // `chat_impl`. This is the single, comprehensive boundary — it covers
        // every prompt fragment that references the text tool-call paradigm
        // because it operates on the assembled wire bytes, not per-template.
        let remap_tool_call = caps.reserved_tool_call_token;
        let has_native_tools = opts
            .native_tools
            .as_ref()
            .is_some_and(|tools| !tools.is_empty());
        let mut msgs = Vec::new();
        if let Some(ref sys) = opts.system {
            let sys = maybe_remap_tool_call_text(sys, remap_tool_call);
            msgs.push(serde_json::json!({"role": "system", "content": sys}));
        }
        msgs.extend(opts.messages.iter().cloned().map(|mut message| {
            sanitize_openai_message_for_request(
                &mut message,
                remap_tool_call,
                caps.reasoning_history_wire_field,
            );
            message
        }));
        if let Some(ref prefill) = opts.prefill {
            let prefill = maybe_remap_tool_call_text(prefill, remap_tool_call);
            msgs.push(serde_json::json!({
                "role": "assistant",
                "content": prefill,
            }));
        }
        msgs = crate::llm::api::normalize_openai_style_messages(msgs, force_string_content);
        if has_native_tools {
            msgs = drop_orphan_tool_result_messages(msgs);
        }
        if caps.requires_tool_result_adjacency {
            msgs = enforce_tool_result_adjacency(msgs);
        }
        if !caps.supports_parallel_tool_calls {
            msgs = enforce_single_tool_call_history(msgs, has_native_tools);
        }
        // OpenAI rejects image content on a `role:"tool"` message ("Image URLs
        // are only allowed for messages with role 'user'"). A tool that returns
        // a screenshot (computer use) or any other image rides it back on the
        // tool result, so relocate those image parts onto a following user
        // message — the text stays on the tool result, the image lands where
        // OpenAI accepts it. Applies to any image-bearing tool result, not just
        // computer use.
        msgs = relocate_tool_message_images_to_user(msgs);

        let wire_model = crate::llm_config::wire_model_id(&opts.model);
        let mut body = serde_json::json!({
            "model": wire_model,
            "messages": msgs,
        });
        if opts.max_tokens > 0 {
            let token_limit_field = if caps.requires_completion_tokens {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            body[token_limit_field] = serde_json::json!(opts.max_tokens);
        }
        if let Some(temp) = opts.temperature.filter(|_| caps.temperature_supported) {
            body["temperature"] = serde_json::json!(clamp_temperature(temp));
        }
        if let Some(top_p) = opts.top_p.filter(|_| caps.top_p_supported) {
            body["top_p"] = serde_json::json!(clamp_probability(top_p));
        }
        if let Some(top_k) = opts.top_k.filter(|_| caps.top_k_supported) {
            body["top_k"] = serde_json::json!(top_k);
        }
        if opts.logprobs {
            body["logprobs"] = serde_json::json!(true);
            if let Some(top_logprobs) = opts.top_logprobs.filter(|value| *value > 0) {
                body["top_logprobs"] = serde_json::json!(top_logprobs);
            }
        }
        if let Some(stop) = opts.stop.as_ref().filter(|_| caps.stop_supported) {
            body["stop"] = serde_json::json!(stop);
        }
        if let Some(seed) = opts.seed.filter(|_| caps.seed_supported) {
            body["seed"] = serde_json::json!(seed);
        }
        if let Some(fp) = opts
            .frequency_penalty
            .filter(|_| caps.frequency_penalty_supported)
        {
            body["frequency_penalty"] = serde_json::json!(fp);
        }
        if let Some(pp) = opts
            .presence_penalty
            .filter(|_| caps.presence_penalty_supported)
        {
            body["presence_penalty"] = serde_json::json!(pp);
        }
        match caps.reasoning_wire_format.as_deref() {
            Some("openrouter") => {
                if let Some(reasoning) = openrouter_reasoning_config(&opts.thinking) {
                    // OpenRouter excludes every endpoint that doesn't support the
                    // `reasoning` param when `require_parameters: true` is set
                    // (which structured/top_k calls force below). For models that
                    // declare NO reasoning capability, emitting a reasoning DISABLE
                    // directive shrinks the candidate set to zero -> 404 "No
                    // endpoints found" (e.g. qwen/qwen3-coder json_object calls).
                    // The disable is a no-op for these models anyway, so skip it.
                    // A smaller set of routes (for example Step 3.7 Flash)
                    // support reasoning but reject explicit disable directives;
                    // omit the field there and let the endpoint's mandatory
                    // reasoning default apply.
                    let skip_disable = is_openrouter_reasoning_disable(&reasoning)
                        && (!model_declares_reasoning(&caps) || !caps.reasoning_disable_supported);
                    if !skip_disable {
                        body["reasoning"] = reasoning;
                    }
                }
            }
            Some("enabled") => {
                if let Some(reasoning) = enabled_reasoning_config(&opts.thinking, &caps) {
                    body["reasoning"] = reasoning;
                }
            }
            Some("minimax") => {
                if let Some(thinking) = minimax_thinking_config(&opts.thinking) {
                    let thinking_enabled = thinking.get("type").and_then(serde_json::Value::as_str)
                        != Some("disabled");
                    body["thinking"] = thinking;
                    if thinking_enabled {
                        body["reasoning_split"] = serde_json::json!(true);
                    }
                }
            }
            Some("zai") => {
                if let Some(thinking) = zai_thinking_config(&opts.thinking) {
                    body["thinking"] = thinking;
                }
            }
            _ => {}
        }
        if caps.reasoning_effort_supported {
            if let ThinkingConfig::Effort { level } = &opts.thinking {
                if *level != crate::llm::api::ReasoningEffort::None || caps.reasoning_none_supported
                {
                    body["reasoning_effort"] = serde_json::json!(level.as_str());
                }
            }
        }
        match &opts.output_format {
            crate::llm::api::OutputFormat::Text => {}
            crate::llm::api::OutputFormat::JsonObject => {
                body["response_format"] = serde_json::json!({"type": "json_object"});
            }
            crate::llm::api::OutputFormat::JsonSchema { schema, strict } => {
                let schema_profile = if *strict {
                    SchemaCompatProfile::OpenAiStrict
                } else {
                    SchemaCompatProfile::OpenAiLenient
                };
                let schema = sanitize_schema_for_provider(
                    &opts.provider,
                    &opts.model,
                    schema_profile,
                    SchemaSurface::StructuredOutput,
                    schema,
                );
                body["response_format"] = serde_json::json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": "response",
                        "schema": schema,
                        "strict": strict,
                    }
                });
            }
        }
        if has_native_tools && caps.tools_exclude_response_format {
            body.as_object_mut()
                .expect("request body is object")
                .remove("response_format");
        }
        if opts.provider == "openrouter"
            && (body.get("response_format").is_some() || body.get("top_k").is_some())
        {
            ensure_openrouter_require_parameters(&mut body);
        }
        // Data-driven OpenRouter upstream route-around. The capability row may
        // deny specific upstream providers that serve this (provider, model)
        // route incorrectly (e.g. billing reasoning tokens then finishing with
        // empty tool_calls) while still advertising the model. We merge those
        // names into the request body's `provider.ignore` so OpenRouter reroutes
        // to a healthy upstream. Pure capability lookup — no model-name branch.
        // Data-driven OpenRouter upstream pin (allowlist). When the row pins a
        // closed set of known-clean upstreams, route ONLY to them in order with
        // `allow_fallbacks:false` so OpenRouter never silently falls back to a
        // sketchier upstream that mis-serializes the route. The canonical case
        // is `openai/gpt-oss-*`, pinned to Cerebras/Groq (clean for Harmony
        // tool calls). A closed allowlist already excludes everything not on it,
        // so the pin takes precedence over `provider_route_denylist` when both
        // are present. Pure capability lookup — no model-name branch.
        if opts.provider == "openrouter" {
            if !caps.openrouter_provider_order.is_empty() {
                apply_openrouter_provider_order(&mut body, &caps.openrouter_provider_order);
            } else if !caps.provider_route_denylist.is_empty() {
                apply_openrouter_route_denylist(&mut body, &caps.provider_route_denylist);
            }
        }
        if let Some(ref tools) = opts.native_tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::Value::Array(provider_request_tools(opts, tools));
            }
        }
        if has_native_tools && !caps.supports_parallel_tool_calls {
            body["parallel_tool_calls"] = serde_json::json!(false);
        }
        if let Some(ref tc) = opts.tool_choice {
            if let Some(tool_choice) =
                normalize_tool_choice_for_capabilities(tc, &caps, has_native_tools)
            {
                body["tool_choice"] = tool_choice;
            }
        }
        if caps.honors_chat_template_kwargs {
            // Always set explicitly for compatible Qwen/DeepSeek
            // templates: some default thinking on when absent, making
            // fast tool-call turns waste budget on reasoning.
            // When prefill is present, continue the final assistant
            // message instead of starting a fresh assistant turn.
            let mut chat_template_kwargs = serde_json::json!({
                "enable_thinking": opts.thinking.is_enabled(),
            });
            if opts.prefill.is_some() {
                chat_template_kwargs["add_generation_prompt"] = serde_json::json!(false);
                chat_template_kwargs["continue_final_message"] = serde_json::json!(true);
            }
            // Qwen3.6 introduced `preserve_thinking`. When the capability
            // matrix says the current (provider, model) pair honours it,
            // emit the flag so the chat template carries `<think>` blocks
            // across turns.
            if caps.preserve_thinking {
                chat_template_kwargs["preserve_thinking"] = serde_json::json!(true);
            }
            let field = chat_template_options_field(&caps);
            body[field] = chat_template_kwargs;
        }
        apply_prompt_cache_breakpoint(&mut body, opts.cache, &caps);
        crate::llm::serving_tiers::apply_fast_request_knob(&mut body, &opts.model, opts.fast);
        body
    }

    /// The actual chat implementation.
    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        // Responses-API routing (explicit `api_mode: "responses"` and the
        // `*-codex` responses-only auto-route) is owned by the shared
        // `vm_call_llm_api` transport funnel, which every OpenAI path passes
        // through (built-in `openai` is not `provider_register`-ed, so it takes
        // the unregistered fallback and never reaches here). This guard stays
        // as defense-in-depth for any registered OpenAI-family provider.
        if request.api_mode == crate::llm::api::LlmApiMode::Responses
            || crate::llm::capabilities::lookup(&request.provider, &request.model)
                .chat_completions_unsupported
        {
            return crate::llm::providers::OpenAiResponsesProvider::call(request, delta_tx).await;
        }

        let mut body = Self::build_request_body(request, false);
        self.transform_request(&mut body);

        // For reserved-tool-call-token models the prompt was sent with the
        // delimiters remapped (see `build_request_body`). The streamed live
        // deltas are canonicalized here (across chunk boundaries) so the live
        // display never shows the wire form; the assembled `result.text` is
        // mapped back to canonical in the shared transport funnel
        // (`vm_call_llm_api_with_body`), which is the single boundary covering
        // every route — registered and unregistered, streaming and not.
        let remap_tool_call = crate::llm::capabilities::lookup(&request.provider, &request.model)
            .reserved_tool_call_token;
        let delta_tx = if remap_tool_call {
            delta_tx.map(canonicalizing_delta_tx)
        } else {
            delta_tx
        };
        let result = crate::llm::api::vm_call_llm_api_with_body(
            request,
            delta_tx,
            body,
            crate::llm::capabilities::WireDialect::OpenAiCompat,
        )
        .await?;
        Ok(result)
    }
}

fn clamp_temperature(value: f64) -> f64 {
    if !value.is_finite() {
        return 1.0;
    }
    value.clamp(0.0, 2.0)
}

fn clamp_probability(value: f64) -> f64 {
    if !value.is_finite() {
        return 1.0;
    }
    value.clamp(0.0, 1.0)
}

fn chat_template_options_field(caps: &crate::llm::capabilities::Capabilities) -> &str {
    caps.chat_template_options_field
        .as_deref()
        .unwrap_or("chat_template_kwargs")
}
