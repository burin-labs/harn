//! OpenAI-compatible provider — covers OpenAI, OpenRouter, Together, Groq,
//! DeepSeek, Fireworks, HuggingFace, local vLLM/SGLang, and any server that
//! speaks the `/v1/chat/completions` protocol.

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult, ThinkingConfig};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::llm::providers::common::parse_major_minor_tail;
use crate::value::VmError;

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
        let mut msgs = Vec::new();
        if let Some(ref sys) = opts.system {
            let sys = maybe_remap_tool_call_text(sys, remap_tool_call);
            msgs.push(serde_json::json!({"role": "system", "content": sys}));
        }
        msgs.extend(opts.messages.iter().cloned().map(|mut message| {
            if let Some(object) = message.as_object_mut() {
                // The durable transcript stores a prior assistant turn's
                // private reasoning as a top-level `reasoning` field (see
                // `build_assistant_response_message`). That field is for
                // host/run-record storage only — no provider consumes a prior
                // assistant message's `reasoning` on the chat-completions wire.
                // Strict OpenAI-compat providers (e.g. Fireworks) reject any
                // unknown top-level message field with HTTP 400 `Extra inputs
                // are not permitted, field: 'messages[N].reasoning'`, which is
                // terminal and non-retryable. Tolerant providers (Cerebras,
                // groq, OpenRouter, DeepInfra, SambaNova) silently ignore it.
                // Drop it here at the single, comprehensive wire boundary so the
                // request is portable across every strict provider; reasoning
                // continuity that DOES matter rides separate, typed channels
                // (Gemini `thoughtSignature`, Anthropic signed thinking blocks,
                // the OpenAI Responses reasoning items API).
                object.remove("reasoning");
                if let Some(content) = object.get("content").cloned() {
                    let content = if remap_tool_call {
                        remap_tool_call_content(&content)
                    } else {
                        content
                    };
                    object.insert(
                        "content".to_string(),
                        crate::llm::content::openai_content(&content),
                    );
                }
            }
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
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(top_p) = opts.top_p.filter(|_| caps.top_p_supported) {
            body["top_p"] = serde_json::json!(top_p);
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
        if let Some(ref stop) = opts.stop {
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
                body["tools"] = serde_json::Value::Array(provider_request_tools(
                    &opts.provider,
                    &opts.model,
                    tools,
                ));
            }
        }
        if let Some(ref tc) = opts.tool_choice {
            if let Some(tool_choice) = normalize_tool_choice_for_capabilities(tc, &caps) {
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
        crate::llm::fast_mode::apply_request_knob(&mut body, &opts.model, opts.fast);
        body
    }

    /// The actual chat implementation.
    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        if request.api_mode == crate::llm::api::LlmApiMode::Responses {
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
            request, delta_tx, body, false, // is_anthropic_style
            false, // is_ollama
        )
        .await?;
        Ok(result)
    }
}

/// Remap canonical `<tool_call>` delimiters to the non-special wire form for a
/// reserved-token model (no-op when `remap` is false). Applied to every outgoing
/// message; see [`crate::llm::tool_delimiter`].
fn maybe_remap_tool_call_text(text: &str, remap: bool) -> String {
    if remap {
        crate::llm::tool_delimiter::canonical_to_wire(text)
    } else {
        text.to_string()
    }
}

/// Apply the tool-call delimiter remap to an OpenAI `content` value, which may
/// be a bare string or an array of typed parts (`{type:"text", text:"…"}`).
fn remap_tool_call_content(content: &serde_json::Value) -> serde_json::Value {
    use crate::llm::tool_delimiter::canonical_to_wire;
    match content {
        serde_json::Value::String(s) => serde_json::Value::String(canonical_to_wire(s)),
        serde_json::Value::Array(parts) => serde_json::Value::Array(
            parts
                .iter()
                .map(|part| {
                    let mut part = part.clone();
                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        let remapped = canonical_to_wire(text);
                        if let Some(obj) = part.as_object_mut() {
                            obj.insert("text".to_string(), serde_json::Value::String(remapped));
                        }
                    }
                    part
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Stateful canonicalizer for a streamed completion. The wire delimiter
/// (`[[CALL]]`) usually arrives split across token deltas (`[[`, `CALL`, `]]`),
/// so a per-chunk replace would miss it and leak the wire form into the
/// transcript / live display. `push` returns the text safe to emit now —
/// everything except a trailing tail that could still be the start of a
/// delimiter — and `flush` returns the remainder at end-of-stream.
#[derive(Default)]
struct DeltaCanonicalizer {
    buf: String,
}

impl DeltaCanonicalizer {
    fn push(&mut self, chunk: &str) -> String {
        use crate::llm::tool_delimiter::{
            wire_to_canonical, WIRE_TOOL_CALL_CLOSE, WIRE_TOOL_CALL_OPEN,
        };
        self.buf.push_str(chunk);
        let max = WIRE_TOOL_CALL_OPEN.len().max(WIRE_TOOL_CALL_CLOSE.len());
        let blen = self.buf.len();
        // The wire delimiters are pure ASCII, so any partial-delimiter tail is
        // ASCII and lands on a char boundary — byte slicing is safe here.
        let mut hold = 0;
        for k in (1..=max.min(blen)).rev() {
            let tail = &self.buf.as_bytes()[blen - k..];
            if WIRE_TOOL_CALL_OPEN.as_bytes().starts_with(tail)
                || WIRE_TOOL_CALL_CLOSE.as_bytes().starts_with(tail)
            {
                hold = k;
                break;
            }
        }
        let safe_end = blen - hold;
        if safe_end == 0 {
            return String::new();
        }
        let emit = wire_to_canonical(&self.buf[..safe_end]);
        self.buf.drain(..safe_end);
        emit
    }

    fn flush(&mut self) -> String {
        if self.buf.is_empty() {
            return String::new();
        }
        let out = crate::llm::tool_delimiter::wire_to_canonical(&self.buf);
        self.buf.clear();
        out
    }
}

/// Wrap a delta sender so streamed chunks are mapped from the wire tool-call
/// delimiter back to canonical (across chunk boundaries) before reaching the
/// live display / ACP text stream.
fn canonicalizing_delta_tx(orig: DeltaSender) -> DeltaSender {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut canon = DeltaCanonicalizer::default();
        while let Some(chunk) = rx.recv().await {
            let emit = canon.push(&chunk);
            if !emit.is_empty() {
                let _ = orig.send(emit);
            }
        }
        let tail = canon.flush();
        if !tail.is_empty() {
            let _ = orig.send(tail);
        }
    });
    tx
}

fn chat_template_options_field(caps: &crate::llm::capabilities::Capabilities) -> &str {
    caps.chat_template_options_field
        .as_deref()
        .unwrap_or("chat_template_kwargs")
}

pub(crate) fn ensure_openrouter_require_parameters(body: &mut serde_json::Value) {
    match body.get_mut("provider") {
        Some(serde_json::Value::Object(provider)) => {
            provider
                .entry("require_parameters".to_string())
                .or_insert_with(|| serde_json::json!(true));
        }
        Some(_) => {}
        None => {
            body["provider"] = serde_json::json!({"require_parameters": true});
        }
    }
}

/// Merge `deny` into the OpenRouter request body's `provider.ignore` array,
/// preserving any entries already present and de-duplicating. Creates the
/// `provider` object and/or `ignore` array when absent. This is the wire
/// materialization of the capability-row `provider_route_denylist`; it is
/// provider-agnostic data plumbing with no model-specific logic — the caller
/// decides whether a denylist applies by consulting the capability matrix.
pub(crate) fn apply_openrouter_route_denylist(body: &mut serde_json::Value, deny: &[String]) {
    if deny.is_empty() {
        return;
    }
    if !body.is_object() {
        return;
    }
    let provider = body
        .as_object_mut()
        .expect("body is an object")
        .entry("provider".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(provider_obj) = provider.as_object_mut() else {
        return;
    };
    let ignore = provider_obj
        .entry("ignore".to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let Some(ignore_arr) = ignore.as_array_mut() else {
        return;
    };
    for name in deny {
        let already_present = ignore_arr
            .iter()
            .any(|existing| existing.as_str() == Some(name.as_str()));
        if !already_present {
            ignore_arr.push(serde_json::Value::String(name.clone()));
        }
    }
}

/// Pin the OpenRouter request body to a closed, ordered allowlist of upstream
/// providers: sets `provider.order` to `order` and `provider.allow_fallbacks`
/// to `false`, so OpenRouter routes the model only to those upstreams (in
/// preference order) and never silently falls back to one not on the list.
/// This is the wire materialization of the capability-row
/// `openrouter_provider_order` — provider-agnostic data plumbing with no
/// model-specific logic; the caller decides whether a pin applies by consulting
/// the capability matrix. A pre-existing `provider.order` (e.g. a caller
/// override) is left untouched; `allow_fallbacks` is always forced to `false`
/// so the pin is genuinely closed. No-op when `order` is empty.
pub(crate) fn apply_openrouter_provider_order(body: &mut serde_json::Value, order: &[String]) {
    if order.is_empty() {
        return;
    }
    if !body.is_object() {
        return;
    }
    let provider = body
        .as_object_mut()
        .expect("body is an object")
        .entry("provider".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(provider_obj) = provider.as_object_mut() else {
        return;
    };
    // Only set the order when the caller has not already pinned one; respect an
    // explicit caller override rather than clobbering it.
    if !provider_obj.contains_key("order") {
        provider_obj.insert(
            "order".to_string(),
            serde_json::Value::Array(
                order
                    .iter()
                    .map(|name| serde_json::Value::String(name.clone()))
                    .collect(),
            ),
        );
    }
    // A closed allowlist must not fall back off-list.
    provider_obj.insert(
        "allow_fallbacks".to_string(),
        serde_json::Value::Bool(false),
    );
}

fn normalize_tool_choice_for_capabilities(
    tool_choice: &serde_json::Value,
    caps: &crate::llm::capabilities::Capabilities,
) -> Option<serde_json::Value> {
    if caps.allowed_tool_choice_modes.is_empty() {
        return Some(tool_choice.clone());
    }

    let mode = tool_choice_mode(tool_choice);
    if mode.as_deref().is_some_and(|mode| {
        caps.allowed_tool_choice_modes
            .iter()
            .any(|allowed| allowed == mode)
    }) {
        return Some(tool_choice.clone());
    }

    if caps
        .allowed_tool_choice_modes
        .iter()
        .any(|mode| mode == "auto")
    {
        return Some(serde_json::Value::String("auto".to_string()));
    }
    if caps
        .allowed_tool_choice_modes
        .iter()
        .any(|mode| mode == "none")
    {
        return Some(serde_json::Value::String("none".to_string()));
    }
    None
}

fn tool_choice_mode(tool_choice: &serde_json::Value) -> Option<String> {
    match tool_choice {
        serde_json::Value::String(mode) => Some(mode.to_ascii_lowercase()),
        serde_json::Value::Object(object) => match object.get("type").and_then(|v| v.as_str()) {
            Some("function") | Some("tool") => Some("required".to_string()),
            Some(other) => Some(other.to_ascii_lowercase()),
            None => None,
        },
        _ => None,
    }
}

fn provider_request_tools(
    provider: &str,
    model: &str,
    tools: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let caps = crate::llm::capabilities::lookup(provider, model);
    let has_openai_tool_search = tools
        .iter()
        .any(|tool| tool.get("type").and_then(serde_json::Value::as_str) == Some("tool_search"));
    let supports_openai_tool_search_extensions = has_openai_tool_search
        && caps.defer_loading
        && caps.tool_search.iter().any(|variant| variant == "hosted");

    tools
        .iter()
        .map(|tool| sanitize_openai_tool_for_request(tool, supports_openai_tool_search_extensions))
        .collect()
}

fn sanitize_openai_tool_for_request(
    tool: &serde_json::Value,
    supports_openai_tool_search_extensions: bool,
) -> serde_json::Value {
    let mut tool = tool.clone();
    let Some(object) = tool.as_object_mut() else {
        return tool;
    };

    object.remove("x-harn-output-schema");
    if !supports_openai_tool_search_extensions {
        object.remove("defer_loading");
        object.remove("namespace");
        object.remove("namespaces");
    }

    if let Some(function) = object
        .get_mut("function")
        .and_then(serde_json::Value::as_object_mut)
    {
        function.remove("x-harn-output-schema");
        function.remove("namespace");
    }

    tool
}

fn apply_prompt_cache_breakpoint(
    body: &mut serde_json::Value,
    cache_requested: bool,
    caps: &crate::llm::capabilities::Capabilities,
) {
    if !cache_requested || !caps.prompt_caching || body_contains_cache_control(body) {
        return;
    }
    match caps.cache_breakpoint_style.as_str() {
        "top_level" => {
            body["cache_control"] = serde_json::json!({"type": "ephemeral"});
        }
        "last_block" => {
            insert_last_message_cache_control(body);
        }
        _ => {}
    }
}

fn body_contains_cache_control(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            object.contains_key("cache_control") || object.values().any(body_contains_cache_control)
        }
        serde_json::Value::Array(values) => values.iter().any(body_contains_cache_control),
        _ => false,
    }
}

fn insert_last_message_cache_control(body: &mut serde_json::Value) -> bool {
    let Some(messages) = body
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    for message in messages.iter_mut().rev() {
        if insert_message_cache_control(message) {
            return true;
        }
    }
    false
}

fn insert_message_cache_control(message: &mut serde_json::Value) -> bool {
    let Some(content) = message
        .as_object_mut()
        .and_then(|object| object.get_mut("content"))
    else {
        return false;
    };
    match content {
        serde_json::Value::String(text) => {
            if text.is_empty() {
                return false;
            }
            let text = text.clone();
            *content = serde_json::json!([{
                "type": "text",
                "text": text,
                "cache_control": {"type": "ephemeral"},
            }]);
            true
        }
        serde_json::Value::Array(blocks) => {
            for block in blocks.iter_mut().rev() {
                let Some(object) = block.as_object_mut() else {
                    continue;
                };
                if object.contains_key("cache_control") {
                    return true;
                }
                object.insert(
                    "cache_control".to_string(),
                    serde_json::json!({"type": "ephemeral"}),
                );
                return true;
            }
            false
        }
        serde_json::Value::Object(object) => {
            object
                .entry("cache_control".to_string())
                .or_insert_with(|| serde_json::json!({"type": "ephemeral"}));
            true
        }
        _ => false,
    }
}

/// True when the OpenRouter `reasoning` body explicitly disables reasoning
/// (`{"enabled": false}`). These are the directives that, on a model with no
/// reasoning support, cause OpenRouter to drop every endpoint under
/// `require_parameters: true`.
fn is_openrouter_reasoning_disable(reasoning: &serde_json::Value) -> bool {
    reasoning
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
}

/// True when the (provider, model) capability row advertises ANY reasoning
/// support: a non-empty `thinking_modes` list, `reasoning_effort_supported`,
/// or `reasoning_none_supported`. When all three are absent the model declares
/// no reasoning capability and the `reasoning` request param must be omitted on
/// OpenRouter to avoid the empty-endpoint 404.
fn model_declares_reasoning(caps: &crate::llm::capabilities::Capabilities) -> bool {
    !caps.thinking_modes.is_empty()
        || caps.reasoning_effort_supported
        || caps.reasoning_none_supported
}

fn openrouter_reasoning_config(thinking: &ThinkingConfig) -> Option<serde_json::Value> {
    match thinking {
        // Explicitly disable on the wire. The previous `None` return left
        // the request silent, which on Qwen3 thinking variants caused the
        // model to fall through to its trained-default unbounded thinking
        // budget. OpenRouter universally honors `reasoning.enabled: false`
        // (verified empirically on qwen/qwen3.6-35b-a3b: 358ms with the
        // disable directive vs 1300ms+ without), so emit it.
        ThinkingConfig::Disabled => Some(serde_json::json!({
            "enabled": false
        })),
        ThinkingConfig::Enabled {
            budget_tokens: None,
        } => Some(serde_json::json!({
            "enabled": true
        })),
        ThinkingConfig::Enabled {
            budget_tokens: Some(max_tokens),
        } => Some(serde_json::json!({
            "max_tokens": max_tokens
        })),
        ThinkingConfig::Adaptive => Some(serde_json::json!({
            "enabled": true
        })),
        ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::None,
        } => Some(serde_json::json!({
            "enabled": false
        })),
        ThinkingConfig::Effort { level } => Some(serde_json::json!({
            "effort": level.as_str()
        })),
    }
}

fn enabled_reasoning_config(
    thinking: &ThinkingConfig,
    caps: &crate::llm::capabilities::Capabilities,
) -> Option<serde_json::Value> {
    let supports_enabled = caps.thinking_modes.iter().any(|mode| mode == "enabled");
    if !supports_enabled {
        return None;
    }
    match thinking {
        ThinkingConfig::Disabled
        | ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::None,
        } => Some(serde_json::json!({ "enabled": false })),
        ThinkingConfig::Enabled { .. } | ThinkingConfig::Adaptive => {
            Some(serde_json::json!({ "enabled": true }))
        }
        ThinkingConfig::Effort { .. } => Some(serde_json::json!({ "enabled": true })),
    }
}

fn minimax_thinking_config(thinking: &ThinkingConfig) -> Option<serde_json::Value> {
    match thinking {
        ThinkingConfig::Disabled
        | ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::None,
        } => Some(serde_json::json!({ "type": "disabled" })),
        ThinkingConfig::Enabled { .. }
        | ThinkingConfig::Adaptive
        | ThinkingConfig::Effort { .. } => Some(serde_json::json!({ "type": "adaptive" })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{
        LlmErrorKind, LlmErrorReason, LlmRequestPayload, ReasoningEffort, ThinkingConfig,
    };
    use serde_json::json;

    #[test]
    fn tool_search_supported_for_gpt_5_4_and_up() {
        assert!(gpt_model_supports_tool_search("gpt-5.4"));
        assert!(gpt_model_supports_tool_search("gpt-5.4-preview"));
        assert!(gpt_model_supports_tool_search("gpt-5.4-turbo"));
        assert!(gpt_model_supports_tool_search("gpt-5-4"));
        assert!(gpt_model_supports_tool_search("gpt-5.5"));
        assert!(gpt_model_supports_tool_search("gpt-6.0"));
    }

    #[test]
    fn fast_mode_injects_service_tier_for_openai() {
        // `fast: true` on GPT-5.5 rides the catalog's `service_tier` knob;
        // OpenAI needs no beta header so none is added.
        let mut payload = base_request_payload();
        payload.provider = "openai".to_string();
        payload.model = "gpt-5.5".to_string();
        payload.fast = true;
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert_eq!(body["service_tier"], json!("fast"));

        payload.fast = false;
        let body_off = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert!(body_off.get("service_tier").is_none());
    }

    #[test]
    fn tool_search_unsupported_for_pre_5_4() {
        assert!(!gpt_model_supports_tool_search("gpt-4o"));
        assert!(!gpt_model_supports_tool_search("gpt-4.1"));
        assert!(!gpt_model_supports_tool_search("gpt-4-turbo"));
        assert!(!gpt_model_supports_tool_search("gpt-3.5-turbo"));
        assert!(!gpt_model_supports_tool_search("gpt-5.0"));
        assert!(!gpt_model_supports_tool_search("gpt-5.3-preview"));
        assert!(!gpt_model_supports_tool_search("gpt-5"));
    }

    #[test]
    fn tool_search_unsupported_for_non_gpt() {
        assert!(!gpt_model_supports_tool_search("claude-opus-4-7"));
        assert!(!gpt_model_supports_tool_search("llama-3.1-70b"));
        assert!(!gpt_model_supports_tool_search(""));
    }

    #[test]
    fn gpt_generation_handles_openrouter_prefix() {
        // OpenRouter model IDs carry an `openai/` prefix. Same capability
        // check must produce the same answer.
        assert_eq!(gpt_generation("openai/gpt-5.4-preview"), Some((5, 4)));
        assert_eq!(gpt_generation("azure/gpt-5.5-turbo"), Some((5, 5)));
        assert!(gpt_model_supports_tool_search("openai/gpt-5.4"));
        assert!(!gpt_model_supports_tool_search("openai/gpt-4o"));
    }

    #[test]
    fn gpt_generation_ignores_date_suffix_as_minor() {
        // `gpt-5-20260115` should parse as generation (5, 0), not (5, 20260115).
        assert_eq!(gpt_generation("gpt-5-20260115"), Some((5, 0)));
        assert!(!gpt_model_supports_tool_search("gpt-5-20260115"));
    }

    #[test]
    fn native_tool_search_variants_lists_hosted_first() {
        let provider = OpenAiCompatibleProvider::new("openai".to_string());
        let variants = provider.native_tool_search_variants("gpt-5.4-preview");
        assert_eq!(variants, vec!["hosted".to_string(), "client".to_string()]);
    }

    #[test]
    fn native_tool_search_variants_empty_for_old_model() {
        let provider = OpenAiCompatibleProvider::new("openai".to_string());
        assert!(provider.native_tool_search_variants("gpt-4o").is_empty());
    }

    #[test]
    fn classifies_openai_context_length_as_terminal_context_overflow() {
        let info = OpenAiCompatibleProvider::classify_http_error(
            "openai",
            reqwest::StatusCode::BAD_REQUEST,
            None,
            r#"{"error":{"code":"context_length_exceeded","message":"maximum context length"}}"#,
        );
        assert_eq!(info.kind, LlmErrorKind::Terminal);
        assert_eq!(info.reason, LlmErrorReason::ContextOverflow);
    }

    #[test]
    fn classifies_openai_rate_limit_as_transient_rate_limit() {
        let info = OpenAiCompatibleProvider::classify_http_error(
            "openai",
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            Some("5"),
            r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#,
        );
        assert_eq!(info.kind, LlmErrorKind::Transient);
        assert_eq!(info.reason, LlmErrorReason::RateLimit);
        assert!(info.message.contains("retry-after: 5"));
    }

    #[test]
    fn supports_defer_loading_matches_tool_search_gate() {
        let provider = OpenAiCompatibleProvider::new("openai".to_string());
        assert!(provider.supports_defer_loading("gpt-5.4"));
        assert!(!provider.supports_defer_loading("gpt-4o"));
    }

    #[test]
    fn cerebras_request_strips_harn_tool_extensions() {
        let mut payload = base_request_payload();
        payload.provider = "cerebras".to_string();
        payload.model = "gpt-oss-120b".to_string();
        payload.native_tools = Some(vec![json!({
            "type": "function",
            "namespace": "ops",
            "defer_loading": true,
            "function": {
                "name": "deploy",
                "description": "Deploy the app",
                "namespace": "ops",
                "x-harn-output-schema": {"type": "object"},
                "parameters": {
                    "type": "object",
                    "properties": {
                        "env": {"type": "string"}
                    },
                    "required": ["env"]
                }
            }
        })]);

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        let tool = &body["tools"][0];
        assert_eq!(tool["type"], "function");
        assert!(tool.get("namespace").is_none());
        assert!(tool.get("defer_loading").is_none());
        assert!(tool["function"].get("namespace").is_none());
        assert!(tool["function"].get("x-harn-output-schema").is_none());
        assert_eq!(
            tool["function"]["parameters"]["properties"]["env"]["type"],
            "string"
        );
        let source_tool = &payload.native_tools.as_ref().expect("source tools")[0];
        assert_eq!(source_tool["namespace"], "ops");
        assert_eq!(
            source_tool["function"]["x-harn-output-schema"]["type"],
            "object"
        );
    }

    #[test]
    fn openai_tool_search_request_keeps_wire_extensions() {
        let mut payload = base_request_payload();
        payload.provider = "openai".to_string();
        payload.model = "gpt-5.4".to_string();
        payload.native_tools = Some(vec![
            json!({
                "type": "tool_search",
                "mode": "hosted",
                "namespaces": ["ops"],
            }),
            json!({
                "type": "function",
                "namespace": "ops",
                "defer_loading": true,
                "function": {
                    "name": "deploy",
                    "description": "Deploy the app",
                    "x-harn-output-schema": {"type": "object"},
                    "parameters": {"type": "object"}
                }
            }),
        ]);

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert_eq!(body["tools"][0]["namespaces"], json!(["ops"]));
        assert_eq!(body["tools"][1]["namespace"], "ops");
        assert_eq!(body["tools"][1]["defer_loading"], true);
        assert!(
            body["tools"][1]["function"]
                .get("x-harn-output-schema")
                .is_none(),
            "Harn output schemas stay in transcripts, not provider payloads"
        );
    }

    #[test]
    fn openai_regular_request_strips_tool_search_extensions_without_meta_tool() {
        let mut payload = base_request_payload();
        payload.provider = "openai".to_string();
        payload.model = "gpt-5.4".to_string();
        payload.native_tools = Some(vec![json!({
            "type": "function",
            "namespace": "ops",
            "defer_loading": true,
            "function": {
                "name": "deploy",
                "description": "Deploy the app",
                "parameters": {"type": "object"}
            }
        })]);

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert!(body["tools"][0].get("namespace").is_none());
        assert!(body["tools"][0].get("defer_loading").is_none());
    }

    #[test]
    fn openrouter_thinking_enabled_maps_to_reasoning_enabled() {
        let provider = OpenAiCompatibleProvider::new("openrouter".to_string());
        let mut payload = base_request_payload();
        payload.thinking = ThinkingConfig::Enabled {
            budget_tokens: None,
        };
        let mut body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        provider.transform_request(&mut body);

        assert_eq!(body["reasoning"]["enabled"], true);
        assert!(body.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn openrouter_no_reasoning_model_omits_reasoning_on_structured_disable() {
        // Regression: qwen/qwen3-coder declares no reasoning capability. With
        // a structured (json_object) call, `require_parameters: true` is set,
        // which makes OpenRouter exclude any endpoint lacking the `reasoning`
        // param. Emitting `reasoning: {enabled: false}` then drops every
        // candidate -> 404 "No endpoints found". The disable must be omitted.
        let mut payload = base_request_payload();
        payload.provider = "openrouter".to_string();
        payload.model = "qwen/qwen3-coder".to_string();
        payload.thinking = ThinkingConfig::Disabled;
        payload.output_format = crate::llm::api::OutputFormat::JsonObject;
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert!(
            body.get("reasoning").is_none(),
            "reasoning disable must be omitted for a no-reasoning model: {body}"
        );
        // The structured directive and require_parameters must still be present.
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["provider"]["require_parameters"], true);
    }

    #[test]
    fn openrouter_reasoning_capable_model_still_disables_on_directive() {
        // Control: a reasoning-capable OpenRouter model (qwen/qwen3.6* declares
        // thinking_modes + reasoning_none_supported) must keep the explicit
        // disable so it doesn't fall back to unbounded thinking.
        let mut payload = base_request_payload();
        payload.provider = "openrouter".to_string();
        payload.model = "qwen/qwen3.6-35b-a3b".to_string();
        payload.thinking = ThinkingConfig::Disabled;
        payload.output_format = crate::llm::api::OutputFormat::JsonObject;
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(
            body["reasoning"]["enabled"], false,
            "reasoning-capable model must keep the explicit disable: {body}"
        );
    }

    #[test]
    fn openrouter_mandatory_reasoning_model_omits_unsupported_disable() {
        let mut payload = base_request_payload();
        payload.provider = "openrouter".to_string();
        payload.model = "stepfun/step-3.7-flash".to_string();
        payload.thinking = ThinkingConfig::Disabled;
        payload.output_format = crate::llm::api::OutputFormat::JsonObject;
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert!(
            body.get("reasoning").is_none(),
            "mandatory-reasoning route must omit unsupported disable: {body}"
        );
    }

    #[test]
    fn openrouter_thinking_budget_maps_to_reasoning_max_tokens() {
        let provider = OpenAiCompatibleProvider::new("openrouter".to_string());
        let mut payload = base_request_payload();
        payload.thinking = ThinkingConfig::Enabled {
            budget_tokens: Some(2048),
        };
        let mut body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        provider.transform_request(&mut body);

        assert_eq!(body["reasoning"]["max_tokens"], 2048);
        assert!(body.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn openrouter_kimi27_code_normalizes_forced_tool_choice_to_auto() {
        let mut payload = base_request_payload();
        payload.provider = "openrouter".to_string();
        payload.model = "moonshotai/kimi-k2.7-code".to_string();
        payload.native_tools = Some(vec![json!({
            "type": "function",
            "function": {
                "name": "add_two",
                "description": "Add two integers.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "a": {"type": "integer"},
                        "b": {"type": "integer"}
                    },
                    "required": ["a", "b"]
                }
            }
        })]);
        payload.tool_choice = Some(json!("required"));

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["tools"][0]["function"]["name"], "add_two");
    }

    #[test]
    fn openrouter_kimi27_code_keeps_allowed_tool_choice_none() {
        let mut payload = base_request_payload();
        payload.provider = "openrouter".to_string();
        payload.model = "moonshotai/kimi-k2.7-code".to_string();
        payload.tool_choice = Some(json!("none"));

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(body["tool_choice"], "none");
    }

    #[test]
    fn openrouter_kimi27_code_strips_fixed_sampling_params() {
        let mut payload = base_request_payload();
        payload.provider = "openrouter".to_string();
        payload.model = "moonshotai/kimi-k2.7-code".to_string();
        payload.temperature = Some(0.2);
        payload.top_p = Some(0.8);
        payload.frequency_penalty = Some(0.1);
        payload.presence_penalty = Some(0.2);

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert!(body.get("frequency_penalty").is_none());
        assert!(body.get("presence_penalty").is_none());
    }

    #[test]
    fn qwen36_emits_preserve_thinking_in_chat_template_kwargs() {
        let mut payload = base_request_payload();
        payload.provider = "local".to_string();
        payload.model = "Qwen/Qwen3.6-35B-A3B".to_string();
        payload.thinking = ThinkingConfig::Enabled {
            budget_tokens: None,
        };
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert_eq!(
            body["chat_template_kwargs"]["preserve_thinking"], true,
            "Qwen3.6 should request preserve_thinking so <think> blocks survive across agentic turns"
        );
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
    }

    #[test]
    fn build_request_body_uses_wire_model_for_catalog_key() {
        let mut payload = base_request_payload();
        payload.provider = "groq".to_string();
        payload.model = "groq/openai/gpt-oss-120b".to_string();

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(body["model"], "openai/gpt-oss-120b");
    }

    #[test]
    fn transform_request_preserves_chat_template_kwargs_when_capability_allows() {
        crate::llm::capabilities::set_user_overrides_toml(
            r#"
[[provider.openrouter]]
model_match = "custom-qwen"
honors_chat_template_kwargs = true
thinking_modes = ["enabled"]
"#,
        )
        .expect("capability override");
        let provider = OpenAiCompatibleProvider::new("openrouter".to_string());
        let mut payload = base_request_payload();
        payload.model = "custom-qwen".to_string();
        payload.thinking = ThinkingConfig::Enabled {
            budget_tokens: None,
        };
        let mut body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert!(body.get("chat_template_kwargs").is_some());

        provider.transform_request(&mut body);

        assert!(body.get("chat_template_kwargs").is_some());
        crate::llm::capabilities::clear_user_overrides();
    }

    #[test]
    fn build_request_body_uses_configured_chat_template_field() {
        crate::llm::capabilities::set_user_overrides_toml(
            r#"
[[provider.baseten]]
model_match = "zai-org/glm-5.2"
honors_chat_template_kwargs = true
chat_template_options_field = "chat_template_args"
thinking_modes = ["enabled"]
"#,
        )
        .expect("capability override");
        let provider = OpenAiCompatibleProvider::new("baseten".to_string());
        let mut payload = base_request_payload();
        payload.provider = "baseten".to_string();
        payload.model = "zai-org/GLM-5.2".to_string();
        payload.thinking = ThinkingConfig::Enabled {
            budget_tokens: None,
        };

        let mut body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert!(body.get("chat_template_args").is_some());
        assert!(body.get("chat_template_kwargs").is_none());
        body["chat_template_kwargs"] = json!({"enable_thinking": false});

        provider.transform_request(&mut body);

        assert!(body.get("chat_template_args").is_some());
        assert!(body.get("chat_template_kwargs").is_none());
        crate::llm::capabilities::clear_user_overrides();
    }

    #[test]
    fn transform_request_strips_chat_template_kwargs_when_capability_denies() {
        let provider = OpenAiCompatibleProvider::new("acme".to_string());
        let mut body = json!({
            "model": "custom-qwen",
            "chat_template_kwargs": {"enable_thinking": true},
            "chat_template_args": {"enable_thinking": true},
        });

        provider.transform_request(&mut body);

        assert!(body.get("chat_template_kwargs").is_none());
        assert!(body.get("chat_template_args").is_none());
    }

    #[test]
    fn ollama_qwen35_does_not_emit_chat_template_kwargs() {
        let mut payload = base_request_payload();
        payload.provider = "ollama".to_string();
        payload.model = "qwen3.5:35b-a3b-coding-nvfp4".to_string();
        payload.thinking = ThinkingConfig::Enabled {
            budget_tokens: None,
        };
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert!(
            body.get("chat_template_kwargs").is_none(),
            "Ollama silently drops chat_template_kwargs today; gate them so strict validation would not break requests"
        );
    }

    #[test]
    fn qwen35_local_disables_thinking_when_absent() {
        let mut payload = base_request_payload();
        payload.provider = "local".to_string();
        payload.model = "Qwen/Qwen3.5-Coder-32B".to_string();
        payload.thinking = ThinkingConfig::Disabled;
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
    }

    #[test]
    fn openai_effort_maps_to_reasoning_effort() {
        let mut payload = base_request_payload();
        payload.provider = "openai".to_string();
        payload.model = "o3".to_string();
        payload.thinking = ThinkingConfig::Effort {
            level: ReasoningEffort::High,
        };
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["max_completion_tokens"], 64);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn openai_none_effort_maps_to_reasoning_effort_none() {
        let mut payload = base_request_payload();
        payload.provider = "openai".to_string();
        payload.model = "gpt-5.5".to_string();
        payload.thinking = ThinkingConfig::Effort {
            level: ReasoningEffort::None,
        };
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(body["reasoning_effort"], "none");
    }

    #[test]
    fn together_hybrid_reasoning_uses_reasoning_enabled() {
        let mut payload = base_request_payload();
        payload.provider = "together".to_string();
        payload.model = "moonshotai/Kimi-K2.5".to_string();
        payload.thinking = ThinkingConfig::Enabled {
            budget_tokens: None,
        };
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(body["reasoning"]["enabled"], true);
        assert!(body.get("chat_template_kwargs").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn minimax_m3_uses_adaptive_thinking_and_completion_tokens() {
        let mut payload = base_request_payload();
        payload.provider = "minimax".to_string();
        payload.model = "MiniMax-M3".to_string();
        payload.thinking = ThinkingConfig::Enabled {
            budget_tokens: Some(4096),
        };

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["reasoning_split"], true);
        assert_eq!(body["max_completion_tokens"], 64);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn minimax_m3_disables_thinking_explicitly() {
        let mut payload = base_request_payload();
        payload.provider = "minimax".to_string();
        payload.model = "MiniMax-M3".to_string();
        payload.thinking = ThinkingConfig::Disabled;

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("reasoning_split").is_none());
    }

    #[test]
    fn together_gpt_oss_effort_uses_reasoning_effort() {
        let mut payload = base_request_payload();
        payload.provider = "together".to_string();
        payload.model = "openai/gpt-oss-120b".to_string();
        payload.thinking = ThinkingConfig::Effort {
            level: ReasoningEffort::Medium,
        };
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(body["reasoning_effort"], "medium");
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn openai_non_reasoning_model_uses_legacy_max_tokens() {
        let mut payload = base_request_payload();
        payload.provider = "openai".to_string();
        payload.model = "gpt-4o".to_string();

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(body["max_tokens"], 64);
        assert!(body.get("max_completion_tokens").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn openrouter_effort_maps_to_nested_reasoning_effort() {
        let provider = OpenAiCompatibleProvider::new("openrouter".to_string());
        let mut payload = base_request_payload();
        payload.thinking = ThinkingConfig::Effort {
            level: ReasoningEffort::Medium,
        };
        let mut body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        provider.transform_request(&mut body);

        assert_eq!(body["reasoning"]["effort"], "medium");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn openrouter_disabled_thinking_emits_reasoning_enabled_false() {
        // Qwen3 thinking variants honor explicit `{enabled: false}` but may
        // otherwise use their trained-default thinking budget.
        let provider = OpenAiCompatibleProvider::new("openrouter".to_string());
        let mut payload = base_request_payload();
        payload.thinking = ThinkingConfig::Disabled;
        let mut body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        provider.transform_request(&mut body);

        assert_eq!(body["reasoning"]["enabled"], false);
    }

    #[test]
    fn openrouter_anthropic_cache_uses_top_level_breakpoint() {
        let mut payload = base_request_payload();
        payload.model = "anthropic/claude-sonnet-4-6".to_string();
        payload.cache = true;

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(body["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(cache_control_count(&body), 1);
    }

    #[test]
    fn openrouter_qwen_explicit_cache_uses_last_content_block() {
        let mut payload = base_request_payload();
        payload.model = "qwen/qwen3.6-plus".to_string();
        payload.cache = true;
        payload.messages = vec![json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "stable reference"},
                {"type": "text", "text": "question"}
            ],
        })];

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert!(body.get("cache_control").is_none());
        assert_eq!(
            body["messages"][0]["content"][1]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert_eq!(cache_control_count(&body), 1);
    }

    #[test]
    fn openrouter_gemini_explicit_cache_uses_last_content_block() {
        let mut payload = base_request_payload();
        payload.model = "google/gemini-2.5-flash".to_string();
        payload.cache = true;
        payload.messages = vec![json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "stable reference"},
                {"type": "text", "text": "question"}
            ],
        })];

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(
            body["messages"][0]["content"][1]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert_eq!(cache_control_count(&body), 1);
    }

    #[test]
    fn openrouter_automatic_cache_route_does_not_emit_cache_control() {
        let mut payload = base_request_payload();
        payload.model = "deepseek/deepseek-v3".to_string();
        payload.cache = true;

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(cache_control_count(&body), 0);
    }

    #[test]
    fn openrouter_qwen_open_weight_route_does_not_emit_cache_control() {
        let mut payload = base_request_payload();
        payload.model = "qwen/qwen3.6-35b-a3b".to_string();
        payload.cache = true;

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(cache_control_count(&body), 0);
    }

    #[test]
    fn openrouter_explicit_cache_preserves_existing_message_breakpoint() {
        let mut payload = base_request_payload();
        payload.model = "qwen/qwen3-coder-plus".to_string();
        payload.cache = true;
        payload.messages = vec![json!({
            "role": "user",
            "content": [
                {
                    "type": "text",
                    "text": "stable reference",
                    "cache_control": {"type": "ephemeral"}
                },
                {"type": "text", "text": "question"}
            ],
        })];

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert!(body["messages"][0]["content"][1]
            .get("cache_control")
            .is_none());
        assert_eq!(cache_control_count(&body), 1);
    }

    #[test]
    fn openrouter_cache_preserves_existing_tool_breakpoint() {
        let mut payload = base_request_payload();
        payload.model = "anthropic/claude-sonnet-4-6".to_string();
        payload.cache = true;
        payload.native_tools = Some(vec![json!({
            "type": "function",
            "cache_control": {"type": "ephemeral"},
            "function": {
                "name": "lookup",
                "description": "Lookup stable context",
                "parameters": {"type": "object"}
            }
        })]);

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert!(body.get("cache_control").is_none());
        assert_eq!(
            body["tools"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert_eq!(cache_control_count(&body), 1);
    }

    #[test]
    fn image_content_maps_to_openai_image_url_block() {
        let mut payload = base_request_payload();
        payload.provider = "openai".to_string();
        payload.model = "gpt-4o".to_string();
        payload.messages = vec![json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "caption"},
                {"type": "image", "base64": "iVBORw0KGgo=", "media_type": "image/png", "detail": "low"}
            ],
        })];

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert_eq!(body["messages"][0]["content"][0]["text"], "caption");
        assert_eq!(
            body["messages"][0]["content"][1],
            json!({
                "type": "image_url",
                "image_url": {
                    "url": "data:image/png;base64,iVBORw0KGgo=",
                    "detail": "low",
                }
            })
        );
    }

    #[test]
    fn image_url_content_maps_to_openai_image_url_block() {
        let mut payload = base_request_payload();
        payload.provider = "openai".to_string();
        payload.model = "gpt-4o".to_string();
        payload.messages = vec![json!({
            "role": "user",
            "content": [
                {"type": "image", "url": "https://example.com/image.png", "media_type": "image/png", "detail": "high"}
            ],
        })];

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert_eq!(
            body["messages"][0]["content"][0],
            json!({
                "type": "image_url",
                "image_url": {
                    "url": "https://example.com/image.png",
                    "detail": "high",
                }
            })
        );
    }

    #[test]
    fn video_content_maps_to_openai_video_url_block() {
        let mut payload = base_request_payload();
        payload.provider = "minimax".to_string();
        payload.model = "MiniMax-M3".to_string();
        payload.messages = vec![json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "summarize"},
                {"type": "video", "base64": "AAAA", "media_type": "video/mp4"}
            ],
        })];

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert_eq!(body["messages"][0]["content"][0]["text"], "summarize");
        assert_eq!(
            body["messages"][0]["content"][1],
            json!({
                "type": "video_url",
                "video_url": {
                    "url": "data:video/mp4;base64,AAAA",
                }
            })
        );
    }

    #[test]
    fn output_format_json_schema_maps_to_openai_response_format() {
        let mut payload = base_request_payload();
        payload.output_format = crate::llm::api::OutputFormat::JsonSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {"answer": {"type": "string"}},
                "required": ["answer"],
            }),
            strict: false,
        };

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(
            body["response_format"]["json_schema"]["schema"]["properties"]["answer"]["type"],
            "string"
        );
        assert_eq!(
            body["response_format"]["json_schema"]["strict"],
            serde_json::json!(false)
        );
    }

    #[test]
    fn openrouter_structured_output_requires_supported_parameters() {
        let mut payload = base_request_payload();
        payload.output_format = crate::llm::api::OutputFormat::JsonSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {"answer": {"type": "string"}},
                "required": ["answer"],
            }),
            strict: true,
        };

        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        assert_eq!(body["provider"]["require_parameters"], true);
    }

    #[test]
    fn openrouter_require_parameters_preserves_provider_preferences() {
        let mut body = serde_json::json!({
            "model": "google/gemma-4-26b-a4b-it",
            "messages": [],
            "response_format": {"type": "json_schema"},
            "provider": {"order": ["Fireworks"], "sort": "throughput"},
        });

        ensure_openrouter_require_parameters(&mut body);

        assert_eq!(body["provider"]["order"][0], "Fireworks");
        assert_eq!(body["provider"]["sort"], "throughput");
        assert_eq!(body["provider"]["require_parameters"], true);
    }

    #[test]
    fn openrouter_emits_top_k_only_when_capability_allows() {
        let mut payload = base_request_payload();
        payload.model = "google/gemma-4-26b-a4b-it".to_string();
        payload.top_k = Some(64);
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert_eq!(body["top_k"].as_i64(), Some(64));
        assert_eq!(body["provider"]["require_parameters"], true);

        payload.model = "mistralai/devstral-small".to_string();
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert!(body.get("top_k").is_none());
    }

    fn base_request_payload() -> LlmRequestPayload {
        LlmRequestPayload {
            provider: "openrouter".to_string(),
            model: "google/gemini-2.5-pro".to_string(),
            region: None,
            api_key: String::new(),
            api_mode: crate::llm::api::LlmApiMode::ChatCompletions,
            fallback_chain: Vec::new(),
            route_fallbacks: Vec::new(),
            session_id: None,
            messages: vec![json!({"role": "user", "content": "hello"})],
            system: None,
            max_tokens: 64,
            temperature: Some(0.0),
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
            native_tools: None,
            provider_tools: Vec::new(),
            tool_choice: None,
            cache: false,
            timeout: None,
            stream: false,
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

    fn cache_control_count(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(object) => {
                usize::from(object.contains_key("cache_control"))
                    + object.values().map(cache_control_count).sum::<usize>()
            }
            serde_json::Value::Array(values) => values.iter().map(cache_control_count).sum(),
            _ => 0,
        }
    }

    #[test]
    fn build_request_body_remaps_reserved_tool_call_token() {
        // llamacpp + qwen3.6 is flagged `reserved_tool_call_token` in
        // capabilities.toml, so the colliding delimiters must be remapped off
        // the wire across both system and history messages.
        let mut payload = base_request_payload();
        payload.provider = "llamacpp".to_string();
        payload.model = "qwen3.6-35b-a3b-ud-q4-k-xl".to_string();
        payload.system = Some("Use <tool_call>\nname({})\n</tool_call> blocks.".to_string());
        payload.messages = vec![json!({
            "role": "assistant",
            "content": "<tool_call>\nlook({})\n</tool_call>"
        })];
        let serialized = OpenAiCompatibleProvider::build_request_body(&payload, false).to_string();
        assert!(
            !serialized.contains("<tool_call>") && !serialized.contains("</tool_call>"),
            "canonical delimiters must be remapped off the wire: {serialized}"
        );
        assert!(
            serialized.contains("[[CALL]]") && serialized.contains("[[/CALL]]"),
            "non-special wire delimiters must be present: {serialized}"
        );
    }

    #[test]
    fn build_request_body_strips_prior_assistant_reasoning_field() {
        // The durable transcript carries a prior assistant turn's private
        // reasoning as a top-level `messages[N].reasoning` field. Strict
        // OpenAI-compat providers (Fireworks) reject any unknown top-level
        // message field with a terminal HTTP 400
        // `Extra inputs are not permitted, field: 'messages[N].reasoning'`.
        // No provider consumes this field on the chat-completions wire, so it
        // must be dropped at the request boundary for every provider.
        let mut payload = base_request_payload();
        payload.provider = "fireworks".to_string();
        payload.model = "accounts/fireworks/models/gpt-oss-120b".to_string();
        payload.messages = vec![
            json!({"role": "user", "content": "hello"}),
            json!({
                "role": "assistant",
                "content": "",
                "reasoning": "let me inspect the file before editing",
                "tool_calls": [{
                    "id": "call_001",
                    "type": "function",
                    "function": {"name": "read", "arguments": "{\"path\":\"main.rs\"}"},
                }],
            }),
            json!({"role": "user", "content": "continue"}),
        ];
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        let messages = body["messages"].as_array().expect("messages array");
        for message in messages {
            assert!(
                message.get("reasoning").is_none(),
                "outgoing message must not carry a `reasoning` field: {message}"
            );
        }
        // The rest of the assistant turn (tool_calls + role) must survive intact.
        let assistant = messages
            .iter()
            .find(|message| message["role"] == "assistant")
            .expect("assistant message preserved");
        assert_eq!(assistant["tool_calls"][0]["id"], "call_001");
    }

    #[test]
    fn delta_canonicalizer_reassembles_split_wire_delimiters() {
        // The wire delimiter arrives split across token deltas; the canonical
        // form must still be emitted intact in order.
        let mut c = DeltaCanonicalizer::default();
        let chunks = [
            "[[",
            "CALL",
            "]]",
            "\nlook({ a: 1 })\n",
            "[[",
            "/CALL",
            "]]",
            " done",
        ];
        let mut out = String::new();
        for ch in chunks {
            out.push_str(&c.push(ch));
        }
        out.push_str(&c.flush());
        assert_eq!(out, "<tool_call>\nlook({ a: 1 })\n</tool_call> done");
    }

    #[test]
    fn delta_canonicalizer_matches_whole_string_remap_on_real_response() {
        // Streaming/non-streaming parity at the live-delta boundary: feeding the
        // real captured wire-form completion through the streaming
        // `DeltaCanonicalizer` (arbitrary chunk splits, including inside the
        // `[[CALL]]` opener and the heredoc body) must yield exactly the same
        // canonical text as the non-streaming whole-string `wire_to_canonical`.
        let wire = include_str!("../testdata/qwen36_reserved_token_response.txt");
        let expected = crate::llm::tool_delimiter::wire_to_canonical(wire);

        let mut c = DeltaCanonicalizer::default();
        let mut out = String::new();
        let bytes = wire.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let mut end = (i + 5).min(bytes.len());
            while end < bytes.len() && !wire.is_char_boundary(end) {
                end += 1;
            }
            out.push_str(&c.push(&wire[i..end]));
            i = end;
        }
        out.push_str(&c.flush());

        assert_eq!(
            out, expected,
            "streamed delta canonicalization must equal the non-streaming whole-string remap"
        );
        assert!(out.contains("<tool_call>") && !out.contains("[[CALL]]"));
    }

    #[test]
    fn delta_canonicalizer_passes_through_plain_text() {
        let mut c = DeltaCanonicalizer::default();
        let mut out = String::new();
        for ch in ["Here is ", "some [pro", "se] text."] {
            out.push_str(&c.push(ch));
        }
        out.push_str(&c.flush());
        assert_eq!(out, "Here is some [prose] text.");
    }

    #[test]
    fn build_request_body_keeps_canonical_for_normal_models() {
        // openrouter gemini is not a reserved-token model: leave the canonical
        // text tool-call delimiters exactly as authored.
        let mut payload = base_request_payload();
        payload.system = Some("Use <tool_call>\nname({})\n</tool_call> blocks.".to_string());
        let serialized = OpenAiCompatibleProvider::build_request_body(&payload, false).to_string();
        assert!(
            serialized.contains("<tool_call>"),
            "non-reserved model keeps canonical delimiter: {serialized}"
        );
        assert!(!serialized.contains("[[CALL]]"));
    }

    #[test]
    fn route_denylist_seeds_provider_ignore_on_empty_body() {
        let mut body = json!({"model": "qwen/qwen3.6-35b-a3b"});
        apply_openrouter_route_denylist(&mut body, &["Ambient".to_string()]);
        assert_eq!(body["provider"]["ignore"], json!(["Ambient"]));
    }

    #[test]
    fn route_denylist_merges_and_dedupes_existing_ignore() {
        let mut body = json!({
            "model": "qwen/qwen3.6-35b-a3b",
            "provider": { "ignore": ["X"], "require_parameters": true }
        });
        apply_openrouter_route_denylist(&mut body, &["Ambient".to_string(), "X".to_string()]);
        // Existing entry preserved, new entry appended, duplicate not re-added.
        assert_eq!(body["provider"]["ignore"], json!(["X", "Ambient"]));
        // Unrelated provider keys are left untouched.
        assert_eq!(body["provider"]["require_parameters"], json!(true));
    }

    #[test]
    fn route_denylist_noop_for_empty_deny() {
        let mut body = json!({"model": "qwen/qwen3.6-35b-a3b"});
        apply_openrouter_route_denylist(&mut body, &[]);
        assert!(body.get("provider").is_none());
    }

    #[test]
    fn build_request_body_applies_qwen36_ambient_denylist_for_openrouter_only() {
        // The qwen3.6 openrouter capability row carries
        // provider_route_denylist = ["Ambient"]; build_request_body must
        // materialize it into provider.ignore for the openrouter provider.
        let mut payload = base_request_payload();
        payload.provider = "openrouter".to_string();
        payload.model = "qwen/qwen3.6-35b-a3b".to_string();
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        let ignore = body["provider"]["ignore"]
            .as_array()
            .expect("provider.ignore array present for qwen3.6 openrouter route");
        assert!(
            ignore.iter().any(|v| v.as_str() == Some("Ambient")),
            "qwen3.6 openrouter body must deny the Ambient upstream: {body}"
        );

        // A non-openrouter provider serving the same model id must NOT get a
        // provider.ignore block — the denylist is openrouter-scoped.
        let mut other = base_request_payload();
        other.provider = "vllm".to_string();
        other.model = "qwen/qwen3.6-35b-a3b".to_string();
        let other_body = OpenAiCompatibleProvider::build_request_body(&other, false);
        assert!(
            other_body.get("provider").is_none(),
            "non-openrouter provider must not receive provider.ignore: {other_body}"
        );
    }

    #[test]
    fn provider_order_pins_closed_allowlist() {
        let mut body = json!({"model": "openai/gpt-oss-120b"});
        apply_openrouter_provider_order(&mut body, &["Cerebras".to_string(), "Groq".to_string()]);
        assert_eq!(body["provider"]["order"], json!(["Cerebras", "Groq"]));
        assert_eq!(body["provider"]["allow_fallbacks"], json!(false));
    }

    #[test]
    fn provider_order_respects_caller_order_but_forces_closed() {
        // A caller-supplied order is preserved; allow_fallbacks is still forced
        // false so the pin is genuinely closed.
        let mut body = json!({
            "model": "openai/gpt-oss-120b",
            "provider": { "order": ["Groq"], "allow_fallbacks": true }
        });
        apply_openrouter_provider_order(&mut body, &["Cerebras".to_string(), "Groq".to_string()]);
        assert_eq!(body["provider"]["order"], json!(["Groq"]));
        assert_eq!(body["provider"]["allow_fallbacks"], json!(false));
    }

    #[test]
    fn provider_order_noop_for_empty() {
        let mut body = json!({"model": "openai/gpt-oss-120b"});
        apply_openrouter_provider_order(&mut body, &[]);
        assert!(body.get("provider").is_none());
    }

    #[test]
    fn build_request_body_pins_gpt_oss_openrouter_to_clean_subproviders() {
        // The openrouter openai/gpt-oss-* capability row carries
        // openrouter_provider_order = ["Cerebras", "Groq"]; build_request_body
        // must materialize it into provider.order + allow_fallbacks:false so the
        // sub-provider lottery only lands on known-clean upstreams.
        let mut payload = base_request_payload();
        payload.provider = "openrouter".to_string();
        payload.model = "openai/gpt-oss-120b".to_string();
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert_eq!(
            body["provider"]["order"],
            json!(["Cerebras", "Groq"]),
            "gpt-oss openrouter body must pin the clean upstream order: {body}"
        );
        assert_eq!(
            body["provider"]["allow_fallbacks"],
            json!(false),
            "gpt-oss openrouter pin must be closed (no fallbacks): {body}"
        );
    }

    #[test]
    fn build_request_body_does_not_pin_other_openrouter_models() {
        // A non-gpt-oss openrouter model must not receive a provider.order pin —
        // the allowlist is row-scoped, so unrelated routes keep free routing.
        let mut payload = base_request_payload();
        payload.provider = "openrouter".to_string();
        payload.model = "anthropic/claude-sonnet-4.5".to_string();
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        let order = body
            .get("provider")
            .and_then(|provider| provider.get("order"));
        assert!(
            order.is_none(),
            "non-gpt-oss openrouter route must not be pinned: {body}"
        );
    }

    #[test]
    fn build_request_body_does_not_pin_gpt_oss_on_other_providers() {
        // gpt-oss served by a NON-openrouter provider (groq/cerebras direct)
        // must NOT get a provider.order block — the pin is openrouter-scoped.
        let mut payload = base_request_payload();
        payload.provider = "groq".to_string();
        payload.model = "openai/gpt-oss-120b".to_string();
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
        assert!(
            body.get("provider").is_none(),
            "non-openrouter gpt-oss route must not be pinned: {body}"
        );
    }
}
