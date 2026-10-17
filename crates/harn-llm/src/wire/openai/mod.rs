//! OpenAI-compatible wire encoding over an explicitly captured route.

pub mod messages;
mod openrouter;
mod reasoning;
mod tools;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

#[cfg(test)]
mod openrouter_tests;
#[cfg(test)]
mod tests;

use super::sanitize_schema_for_provider;
use crate::request::{LlmRequestPayload, ThinkingConfig};
use crate::schema_compat::{SchemaCompatProfile, SchemaSurface};
use harn_provider_catalog::capabilities::Capabilities;
use harn_provider_catalog::ServingTierRequestDef;
use messages::{
    enforce_single_tool_call_history, maybe_remap_tool_call_text,
    relocate_tool_message_images_to_user, sanitize_openai_message_for_request,
};
pub use openrouter::ensure_openrouter_require_parameters;
use openrouter::{apply_openrouter_provider_order, apply_openrouter_route_denylist};
use reasoning::{
    enabled_reasoning_config, groq_qwen_reasoning_effort, is_openrouter_reasoning_disable,
    minimax_thinking_config, model_declares_reasoning, openrouter_reasoning_config,
    typed_thinking_config,
};
use tools::{normalize_tool_choice_for_capabilities, provider_request_tools};

/// Host-owned catalog and launch policy captured before request encoding.
pub struct OpenAiWireContext<'a> {
    pub capabilities: &'a Capabilities,
    pub wire_model: &'a str,
    pub may_shape_reasoning: bool,
    pub tool_search_extensions: bool,
    /// Present only when fast mode was requested and the tier is usable.
    pub fast_request: Option<&'a ServingTierRequestDef>,
}

/// Encode without reading ambient catalog state or publishing native events.
pub fn encode_request(
    opts: &LlmRequestPayload,
    context: &OpenAiWireContext<'_>,
) -> super::EncodedRequest {
    let caps = context.capabilities;
    let mut audits = Vec::new();
    // Models that reserve `<tool_call>` as a special token collapse when
    // they meet it as instructional/wrapper text. Remap the colliding
    // delimiters to a non-special wire form on every outgoing message
    // (system + full history + prefill); the response is mapped back in
    // the native transport. This boundary covers
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
            caps.reasoning_round_trip,
            caps.reasoning_history_wire_field,
            has_native_tools,
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
    msgs = super::openai_normalize::normalize_openai_style_messages(msgs, false);
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

    let wire_model = context.wire_model;
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
    if let Some(temp) = opts.temperature {
        body["temperature"] = serde_json::json!(temp);
    }
    if let Some(top_p) = opts.top_p {
        body["top_p"] = serde_json::json!(top_p);
    }
    if let Some(top_k) = opts.top_k {
        body["top_k"] = serde_json::json!(top_k);
    }
    if let Some(logprobs) = opts.logprobs {
        body["logprobs"] = serde_json::json!(true);
        if let Some(top_logprobs) = logprobs.top {
            body["top_logprobs"] = serde_json::json!(top_logprobs);
        }
    }
    if !opts.logit_bias.is_empty() {
        body["logit_bias"] = serde_json::Value::Object(
            opts.logit_bias
                .iter()
                .map(|entry| (entry.token_id.to_string(), serde_json::json!(entry.bias)))
                .collect(),
        );
    }
    if let Some(min_p) = opts.min_p {
        body["min_p"] = serde_json::json!(min_p);
    }
    if let Some(repetition_penalty) = opts.repetition_penalty {
        body["repetition_penalty"] = serde_json::json!(repetition_penalty);
    }
    if let Some(prediction) = &opts.prediction {
        body["prediction"] = serde_json::json!({
            "type": "content",
            "content": prediction,
        });
    }
    if let Some(verbosity) = opts.verbosity {
        body["verbosity"] = serde_json::json!(verbosity.as_str());
    }
    if let Some(stop) = opts.stop.as_ref() {
        body["stop"] = serde_json::json!(stop);
    }
    if let Some(seed) = opts.seed {
        body["seed"] = serde_json::json!(seed);
    }
    if let Some(fp) = opts.frequency_penalty {
        body["frequency_penalty"] = serde_json::json!(fp);
    }
    if let Some(pp) = opts.presence_penalty {
        body["presence_penalty"] = serde_json::json!(pp);
    }
    let may_shape = context.may_shape_reasoning;
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
                //
                // That skip is a catalog claim, so the probe stands it down.
                let skip_disable = may_shape
                    && is_openrouter_reasoning_disable(&reasoning)
                    && (!model_declares_reasoning(caps) || !caps.reasoning_disable_supported);
                if !skip_disable {
                    body["reasoning"] = reasoning;
                }
            }
        }
        Some("enabled") => {
            if let Some(reasoning) = enabled_reasoning_config(&opts.thinking, caps) {
                body["reasoning"] = reasoning;
            }
        }
        Some("groq_qwen") => {
            if let Some(effort) = groq_qwen_reasoning_effort(&opts.thinking) {
                body["reasoning_effort"] = serde_json::json!(effort);
            }
        }
        Some("minimax") => {
            if let Some(thinking) = minimax_thinking_config(&opts.thinking) {
                let thinking_enabled =
                    thinking.get("type").and_then(serde_json::Value::as_str) != Some("disabled");
                body["thinking"] = thinking;
                if thinking_enabled {
                    body["reasoning_split"] = serde_json::json!(true);
                }
            }
        }
        Some("thinking_type") => {
            if let Some(thinking) = typed_thinking_config(&opts.thinking) {
                // Some models require thinking on every request. An omitted
                // caller setting reaches us as Disabled, but sending the
                // explicit disable makes those endpoints reject a healthy
                // plain call. An explicit unsupported disable is rejected
                // during option extraction before it reaches this builder.
                let implicit_disable = matches!(opts.thinking, ThinkingConfig::Disabled);
                if !implicit_disable || caps.reasoning_disable_supported || !may_shape {
                    body["thinking"] = thinking;
                }
            }
        }
        _ => {}
    }
    // Both conditions are catalog claims, so an effort probe suspends them
    // and sends the rung it was asked to measure. Otherwise a route whose
    // row says `none` is unsupported answers 200 to a request that carried
    // no effort at all, and the probe reads that as the route accepting
    // `none`.
    //
    // OpenRouter already encodes the rung as nested `reasoning.effort`
    // (or `reasoning.enabled = false` for `none`). The top-level
    // OpenAI-style field is a different dialect; emitting it here would
    // change the wire shape of every OpenRouter route whose modes contain
    // `effort`, including rows that never set the legacy flag.
    let openrouter_owns_effort = caps.reasoning_wire_format.as_deref() == Some("openrouter");
    if !openrouter_owns_effort && (caps.reasoning_effort_supported || !may_shape) {
        if let ThinkingConfig::Effort { level } = &opts.thinking {
            if *level != crate::request::ReasoningEffort::None
                || caps.reasoning_none_supported
                || !may_shape
            {
                body["reasoning_effort"] = serde_json::json!(level.as_str());
            }
        }
    }
    match &opts.output_format {
        crate::request::OutputFormat::Text => {}
        crate::request::OutputFormat::JsonObject => {
            body["response_format"] = serde_json::json!({"type": "json_object"});
        }
        crate::request::OutputFormat::JsonSchema { schema, strict } => {
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
                &mut audits,
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
        && (opts.provider_contract_probe.is_some()
            || body.get("response_format").is_some()
            || body.get("top_k").is_some())
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
            body["tools"] = serde_json::Value::Array(provider_request_tools(
                opts,
                tools,
                context.tool_search_extensions,
                &mut audits,
            ));
        }
    }
    if has_native_tools {
        if let Some(parallel) = opts.parallel_tool_calls {
            if caps.supports_parallel_tool_calls {
                body["parallel_tool_calls"] = serde_json::json!(parallel);
            }
        }
        if caps.requires_parallel_tool_calls_false {
            body["parallel_tool_calls"] = serde_json::json!(false);
        }
    }
    if let Some(ref tc) = opts.tool_choice {
        if let Some(tool_choice) =
            normalize_tool_choice_for_capabilities(tc, caps, has_native_tools)
        {
            body["tool_choice"] = tool_choice;
        }
    }
    let mut chat_template_kwargs = if caps.honors_chat_template_kwargs {
        // Always set explicitly for compatible Qwen/DeepSeek
        // templates: some default thinking on when absent, making
        // fast tool-call turns waste budget on reasoning.
        // When prefill is present, continue the final assistant
        // message instead of starting a fresh assistant turn.
        let mut kwargs = serde_json::json!({
            "enable_thinking": opts.thinking.is_enabled(),
        });
        if opts.prefill.is_some() {
            kwargs["add_generation_prompt"] = serde_json::json!(false);
            kwargs["continue_final_message"] = serde_json::json!(true);
        }
        Some(kwargs)
    } else {
        None
    };
    // Model support and wire support are separate facts. A route may
    // preserve thinking in its model while rejecting template kwargs.
    if caps.preserve_thinking && caps.honors_preserve_thinking_kwarg {
        chat_template_kwargs.get_or_insert_with(|| serde_json::json!({}))["preserve_thinking"] =
            serde_json::json!(true);
    }
    if let Some(chat_template_kwargs) = chat_template_kwargs {
        let field = chat_template_options_field(caps);
        body[field] = chat_template_kwargs;
    }
    super::prompt_cache::apply_prompt_cache_breakpoint(
        &mut body,
        super::prompt_cache::PromptCacheBreakpoint::resolve(opts, caps),
        serde_json::json!({"type": "ephemeral"}),
    );
    if let Some(knob) = context.fast_request {
        if let Some(object) = body.as_object_mut() {
            object.insert(
                knob.param.clone(),
                serde_json::Value::String(knob.value.clone()),
            );
        }
    }
    super::EncodedRequest {
        body,
        schema_audits: audits,
    }
}

pub fn chat_template_options_field(
    caps: &harn_provider_catalog::capabilities::Capabilities,
) -> &str {
    caps.chat_template_options_field
        .as_deref()
        .unwrap_or("chat_template_kwargs")
}
