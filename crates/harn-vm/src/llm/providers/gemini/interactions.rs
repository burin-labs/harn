//! Google Gemini Interactions API (`POST /v1beta/interactions`).
//!
//! The second live wire shape Google serves the Gemini models over, selected by
//! the `live_endpoint_family = "gemini_interactions"` capability. It is *not* a
//! replacement for `:generateContent`, which stays in
//! [`super`](super::GeminiProvider): Gemini Batch accepts `generateContent`
//! bodies only, and Vertex delegates to the `generateContent` builder for its
//! own URL/auth envelope. Keeping the two families in separate modules behind
//! one typed capability is what stops a change to one from silently re-shaping
//! the other.
//!
//! Where `generateContent` models a turn as `contents[].parts[]`, Interactions
//! models it as an ordered list of *steps* — `user_input`, `thought`,
//! `model_output`, `function_call`, `function_result` — and can keep that list
//! server-side under an interaction id. Harn projects both onto the same
//! neutral transcript blocks, so scripts see one contract.
//!
//! Streaming is assembled back into the same interaction envelope the
//! non-streaming route returns, so [`parse_response`] is the single owner of
//! step → transcript mapping and the SSE path only owns event → step assembly.

use crate::llm::api::{
    DeltaSender, LlmRequestPayload, LlmResult, OutputFormat, ProviderTelemetry,
    RawProviderToolCall, ReasoningEffort, ThinkingConfig,
};
use crate::llm::providers::common::{
    apply_provider_overrides, maybe_emit_delta, output_text_block, reasoning_block,
    tool_call_block, vm_err,
};
use crate::llm::providers::gemini::interactions_stream::{
    InteractionStream, StreamAction, DONE_SENTINEL,
};
use crate::llm::providers::schema_compat::{
    sanitize_schema_for_provider, SchemaCompatProfile, SchemaSurface,
};
use crate::value::VmError;
use serde_json::{json, Map, Value};

/// Step `type` discriminants Harn reads and writes. Interactions may add more;
/// unknown step types are ignored by the parser rather than failing a turn.
pub(crate) const STEP_USER_INPUT: &str = "user_input";
pub(crate) const STEP_MODEL_OUTPUT: &str = "model_output";
pub(crate) const STEP_THOUGHT: &str = "thought";
pub(crate) const STEP_FUNCTION_CALL: &str = "function_call";
pub(crate) const STEP_FUNCTION_RESULT: &str = "function_result";

/// The Interactions request/response builder and parser.
pub(crate) struct GeminiInteractions;

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

impl GeminiInteractions {
    pub(crate) fn parse_response(
        json: &Value,
        request: &LlmRequestPayload,
    ) -> Result<LlmResult, VmError> {
        parse_response(json, request)
    }

    /// Build a `POST /v1beta/interactions` body from a neutral Harn payload.
    ///
    /// `stream` is left to the transport so the dry-run request audit and the
    /// live call share one builder.
    pub(crate) fn build_request_body(opts: &LlmRequestPayload) -> Value {
        let wire_model = crate::llm_config::wire_model_id(&opts.model);
        let model = wire_model.strip_prefix("models/").unwrap_or(&wire_model);

        let InputSteps {
            steps,
            system_instruction,
        } = build_input_steps(opts);

        let mut body = json!({
            "model": model,
            "input": steps,
        });

        let mut system = system_instruction;
        if let Some(leading) = opts.system.as_deref().filter(|value| !value.is_empty()) {
            system.insert(0, leading.to_string());
        }
        if !system.is_empty() {
            body["system_instruction"] = json!(system.join("\n\n"));
        }

        if let Some(tools) = interactions_tools(opts) {
            body["tools"] = tools;
        }
        if let Some(generation_config) = generation_config(opts) {
            body["generation_config"] = Value::Object(generation_config);
        }
        if let Some(response_format) = response_format(opts) {
            body["response_format"] = response_format;
        }

        // Provider-side conversation state. Harn owns transcripts, so an
        // interaction is NOT persisted on Google's servers unless the caller
        // asked for state — either explicitly with `store`, or implicitly by
        // chaining from a `previous_response_id`, which is only resolvable
        // while the prior interaction is stored.
        if let Some(previous) = opts
            .previous_response_id
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            body["previous_interaction_id"] = json!(previous);
        }
        body["store"] = json!(opts
            .store
            .unwrap_or_else(|| opts.previous_response_id.is_some()));
        if let Some(background) = opts.background {
            body["background"] = json!(background);
        }

        apply_provider_overrides(&mut body, opts.provider_overrides.as_ref());
        body
    }
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

impl GeminiInteractions {
    /// Execute one Interactions turn.
    ///
    /// Streaming and non-streaming converge on [`parse_response`]: the SSE path
    /// reassembles the same interaction envelope before parsing it, so the two
    /// cannot produce different transcripts for the same turn.
    pub(crate) async fn chat(
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        let dialect = crate::llm::api::DialectContract::for_request(request);
        let mut body = dialect.build_request_body(request);
        if request.stream {
            body["stream"] = json!(true);
        }
        let pdef = crate::llm_config::provider_config(&request.provider);
        let base_url = pdef
            .as_ref()
            .map(crate::llm_config::resolve_base_url)
            .unwrap_or_else(|| "https://generativelanguage.googleapis.com".to_string());
        let url = format!("{base_url}/v1beta/interactions");
        let client = crate::llm::blocking_client_for_base_url(&base_url);
        let http = client
            .post(url)
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(request.resolve_timeout()))
            .json(&body);
        let http = crate::llm::api::apply_auth_headers(http, &request.api_key, pdef.as_ref());
        let response = http.send().await.map_err(|error| {
            vm_err(format!(
                "gemini API error: {}",
                crate::egress::redact_reqwest_error(&error)
            ))
        })?;
        if !response.status().is_success() {
            return Err(crate::llm::api::err_for_non_success_with_dialect(
                dialect, "gemini", response,
            )
            .await);
        }

        if request.stream {
            let envelope = read_interaction_stream(response, delta_tx, dialect).await?;
            return dialect.parse_response(&envelope, request, false);
        }

        let json: Value = response
            .json()
            .await
            .map_err(|error| vm_err(format!("gemini response parse error: {error}")))?;
        let result = dialect.parse_response(&json, request, false)?;
        maybe_emit_delta(delta_tx, &result.text);
        Ok(result)
    }
}

async fn read_interaction_stream(
    response: reqwest::Response,
    delta_tx: Option<DeltaSender>,
    dialect: crate::llm::api::DialectContract,
) -> Result<Value, VmError> {
    use tokio_stream::StreamExt;

    let stream = response
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(stream));
    consume_interaction_sse(reader, delta_tx, dialect).await
}

/// Drive the [`InteractionStream`] assembler from SSE lines.
///
/// Generic over the reader so the assembly path is exercised from an in-memory
/// transcript in tests, exactly as the shared OpenAI/Anthropic SSE reader is.
pub(crate) async fn consume_interaction_sse<R: tokio::io::AsyncBufRead + Unpin>(
    reader: R,
    delta_tx: Option<DeltaSender>,
    dialect: crate::llm::api::DialectContract,
) -> Result<Value, VmError> {
    use tokio::io::AsyncBufReadExt;

    if dialect.stream_protocol() != crate::llm::api::StreamProtocol::GeminiInteractionsSse {
        return Err(vm_err(
            "Gemini Interactions stream received a mismatched dialect",
        ));
    }

    let mut lines = reader.lines();
    let mut stream = InteractionStream::new();
    let mut saw_provider_frame = false;
    let mut saw_terminal_event = false;
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| vm_err(format!("gemini stream read error: {error}")))?
    {
        let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if payload.is_empty() {
            continue;
        }
        // `[DONE]` only closes the SSE framing. A completed interaction carries
        // the status and usage needed to turn streamed deltas into a response.
        if payload == DONE_SENTINEL {
            break;
        }
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        saw_provider_frame = true;
        match stream.push(&event) {
            StreamAction::Text(text) => maybe_emit_delta(delta_tx.clone(), &text),
            StreamAction::Done => {
                saw_terminal_event = true;
                break;
            }
            StreamAction::None => {}
        }
    }

    if !saw_terminal_event {
        return Err(crate::llm::api::premature_stream_eof(
            "gemini",
            if saw_provider_frame {
                crate::value::ProviderStreamPhase::Streaming
            } else {
                crate::value::ProviderStreamPhase::AwaitingFirstChunk
            },
            saw_provider_frame,
            "interaction.completed",
        ));
    }

    Ok(stream.finish())
}

/// Steps for `input`, plus the system text lifted out of the message list.
struct InputSteps {
    steps: Vec<Value>,
    system_instruction: Vec<String>,
}

/// Project Harn's neutral message list onto Interactions input steps.
///
/// When the caller chains from a `previous_response_id`, the provider already
/// holds every step up to and including the last assistant turn, so replaying
/// them would double the history *and* the bill. Only the messages after the
/// final assistant turn — the new user input or tool results — go on the wire.
fn build_input_steps(opts: &LlmRequestPayload) -> InputSteps {
    let mut system_instruction = Vec::new();
    let mut steps = Vec::new();

    let chained = opts
        .previous_response_id
        .as_deref()
        .is_some_and(|value| !value.is_empty());
    let start = if chained {
        opts.messages
            .iter()
            .rposition(|message| matches!(message_role(message), "assistant" | "model"))
            .map(|index| index + 1)
            .unwrap_or(0)
    } else {
        0
    };

    for message in &opts.messages[start.min(opts.messages.len())..] {
        match message_role(message) {
            "system" => {
                let text = crate::llm::providers::common::request_text_content(message);
                if !text.is_empty() {
                    system_instruction.push(text);
                }
            }
            "tool" | "tool_result" => {
                if let Some(step) = function_result_step(message) {
                    steps.push(step);
                }
            }
            "assistant" | "model" => push_assistant_steps(message, &mut steps),
            _ => {
                let content = interactions_content(&message["content"]);
                if !content.is_empty() {
                    steps.push(json!({"type": STEP_USER_INPUT, "content": content}));
                }
            }
        }
    }

    InputSteps {
        steps,
        system_instruction,
    }
}

fn message_role(message: &Value) -> &str {
    message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user")
}

/// Emit the `thought` / `model_output` / `function_call` steps for one
/// assistant turn, in the order Interactions validates.
///
/// The opaque thought signature is mandatory on a stateless replay: dropping it
/// makes the follow-up request fail with `invalid_request` rather than
/// degrading, which is why it is emitted before any function call rather than
/// attached to one.
fn push_assistant_steps(message: &Value, steps: &mut Vec<Value>) {
    let parts = message
        .get("content")
        .map(crate::llm::content::gemini_parts)
        .unwrap_or_default();

    let mut calls: Vec<Value> = Vec::new();
    let mut content = Vec::new();
    let mut signature: Option<String> = None;

    let mut remember_signature = |value: &Value| {
        if signature.is_none() {
            signature = super::gemini_tool_call_thought_signature(value).map(str::to_string);
        }
    };

    for part in &parts {
        remember_signature(part);
        if let Some(call) = part.get("functionCall") {
            if let Some(step) = function_call_step(call, part) {
                calls.push(step);
            }
            continue;
        }
        if let Some(value) = content_from_gemini_part(part) {
            content.push(value);
        }
    }
    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        remember_signature(call);
        if let Some(step) = function_call_step(call.get("function").unwrap_or(call), call) {
            calls.push(step);
        }
    }

    if let Some(signature) = signature {
        steps.push(json!({"type": STEP_THOUGHT, "signature": signature}));
    }
    if !content.is_empty() {
        steps.push(json!({"type": STEP_MODEL_OUTPUT, "content": content}));
    }
    steps.extend(calls);
}

/// Build one `function_call` step. `call` carries name/args (either a Gemini
/// `functionCall` object or an OpenAI-shaped `function`), `owner` carries the
/// id and any thought signature.
fn function_call_step(call: &Value, owner: &Value) -> Option<Value> {
    let name = call
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())?;
    let arguments = call
        .get("args")
        .or_else(|| call.get("arguments"))
        .and_then(|value| {
            value
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .or_else(|| (!value.is_string()).then(|| value.clone()))
        })
        .unwrap_or_else(|| json!({}));
    let mut step = json!({
        "type": STEP_FUNCTION_CALL,
        "name": name,
        "arguments": arguments,
    });
    if let Some(id) = call
        .get("id")
        .or_else(|| owner.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        step["id"] = json!(id);
    }
    Some(step)
}

/// Build one `function_result` step from a Harn tool-result message.
///
/// The payload normalization is shared with the `generateContent` path so a
/// tool result renders identically on both families; only the envelope
/// (`call_id` + typed content list instead of `functionResponse`) differs.
fn function_result_step(message: &Value) -> Option<Value> {
    let name = message
        .get("name")
        .or_else(|| message.get("tool_name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())?;
    let payload = message
        .get("content")
        .map(super::gemini_function_response_payload)
        .unwrap_or_else(|| json!({}));
    let text = match &payload {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let mut step = json!({
        "type": STEP_FUNCTION_RESULT,
        "name": name,
        "result": [{"type": "text", "text": text}],
    });
    if let Some(id) = message
        .get("tool_call_id")
        .or_else(|| message.get("tool_use_id"))
        .or_else(|| message.get("call_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        step["call_id"] = json!(id);
    }
    Some(step)
}

/// Project a neutral content value onto Interactions content blocks.
///
/// Routing through [`crate::llm::content::gemini_parts`] keeps one owner for
/// what a Harn content block *means* (media sniffing, file references,
/// visibility filtering); this function only restates the result in the second
/// wire vocabulary.
fn interactions_content(content: &Value) -> Vec<Value> {
    crate::llm::content::gemini_parts(content)
        .iter()
        .filter_map(content_from_gemini_part)
        .collect()
}

fn content_from_gemini_part(part: &Value) -> Option<Value> {
    if let Some(text) = part.get("text").and_then(Value::as_str) {
        // Interactions requires a text block to carry text. An empty one is
        // refused with `invalid_request: Missing text in content of type
        // text.`, which takes down the whole request rather than degrading.
        // Google itself returns an empty text part beside a tool-calls-only
        // model turn, so a replay that echoes the turn back verbatim would
        // send exactly the block the endpoint refuses.
        if text.is_empty() {
            return None;
        }
        return Some(json!({"type": "text", "text": text}));
    }
    if let Some(inline) = part.get("inline_data").or_else(|| part.get("inlineData")) {
        let mime = media_field(inline, "mime_type", "mimeType")?;
        let data = inline.get("data").and_then(Value::as_str)?;
        return Some(json!({
            "type": media_kind_for_mime(mime),
            "data": data,
            "mime_type": mime,
        }));
    }
    if let Some(file) = part.get("file_data").or_else(|| part.get("fileData")) {
        let mime = media_field(file, "mime_type", "mimeType")?;
        let uri = media_field(file, "file_uri", "fileUri")?;
        return Some(json!({
            "type": media_kind_for_mime(mime),
            "uri": uri,
            "mime_type": mime,
        }));
    }
    // `functionCall` / `functionResponse` parts become steps, not content.
    None
}

fn media_field<'a>(value: &'a Value, snake: &str, camel: &str) -> Option<&'a str> {
    value
        .get(snake)
        .or_else(|| value.get(camel))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

/// Interactions types media by content block rather than by MIME string, so the
/// MIME family picks the block type. Anything that is not audio/image/video is
/// a document (PDF and CSV are the documented members).
fn media_kind_for_mime(mime: &str) -> &'static str {
    match mime.split('/').next().unwrap_or_default() {
        "image" => "image",
        "audio" => "audio",
        "video" => "video",
        _ => "document",
    }
}

/// Restate Harn's native tool definitions as flat Interactions function tools.
///
/// The `generateContent` builder is the owner of Google schema sanitization;
/// this unwraps its `functionDeclarations` envelope rather than sanitizing
/// again, so a schema fix lands on both families at once.
fn interactions_tools(opts: &LlmRequestPayload) -> Option<Value> {
    let declarations = crate::llm::providers::common::google_function_declaration_tools(
        &opts.provider,
        &opts.model,
        opts.native_tools.as_deref(),
    )?;
    let tools: Vec<Value> = declarations
        .get(0)?
        .get("functionDeclarations")?
        .as_array()?
        .iter()
        .map(|declaration| {
            let mut tool = declaration.clone();
            tool["type"] = json!("function");
            tool
        })
        .collect();
    (!tools.is_empty()).then_some(Value::Array(tools))
}

fn generation_config(opts: &LlmRequestPayload) -> Option<Map<String, Value>> {
    let mut config = Map::new();
    // Gemini 3.6 Flash and 3.5 Flash-Lite deprecate these controls and will
    // reject them on future model generations. Capability admission catches
    // explicit portable options; this builder-side guard also protects
    // catalog/model defaults and direct request construction.
    let caps = crate::llm::capabilities::lookup(&opts.provider, &opts.model);
    if opts.max_tokens > 0 {
        config.insert("max_output_tokens".to_string(), json!(opts.max_tokens));
    }
    if caps.temperature_supported
        || !crate::llm::provider_contract_probe::catalog_may_shape_requested_portable_option(
            opts.provider_contract_probe,
            crate::llm::capabilities::PortableOption::Temperature,
        )
    {
        if let Some(temperature) = opts.temperature {
            config.insert("temperature".to_string(), json!(temperature));
        }
    }
    if caps.top_p_supported
        || !crate::llm::provider_contract_probe::catalog_may_shape_requested_portable_option(
            opts.provider_contract_probe,
            crate::llm::capabilities::PortableOption::TopP,
        )
    {
        if let Some(top_p) = opts.top_p {
            config.insert("top_p".to_string(), json!(top_p));
        }
    }
    if caps.top_k_supported
        || !crate::llm::provider_contract_probe::catalog_may_shape_requested_portable_option(
            opts.provider_contract_probe,
            crate::llm::capabilities::PortableOption::TopK,
        )
    {
        if let Some(top_k) = opts.top_k {
            config.insert("top_k".to_string(), json!(top_k));
        }
    }
    if let Some(stop) = &opts.stop {
        config.insert("stop_sequences".to_string(), json!(stop));
    }
    if let Some(seed) = opts.seed {
        config.insert("seed".to_string(), json!(seed));
    }
    if let Some(logprobs) = opts.logprobs {
        config.insert("response_logprobs".to_string(), json!(true));
        if let Some(top) = logprobs.top {
            config.insert("logprobs".to_string(), json!(top));
        }
    }
    // Interactions replaces `generationConfig.thinkingConfig.thinkingBudget`
    // with a coarse level and has no "off" rung. The floor comes from the
    // model capability row because current model generations do not all expose
    // the same ladder. Token budgets have no representation here and are
    // dropped; the request audit reports the omission.
    if let Some(level) = thinking_level(&opts.thinking, &caps) {
        config.insert("thinking_level".to_string(), json!(level));
    }
    // `generateContent` returns thinking as `thought: true` parts for free;
    // Interactions only fills the `thought` step's summary when asked. Ask
    // whenever the caller enabled thinking, so both families surface the same
    // reasoning channel.
    if opts.thinking.is_enabled() {
        config.insert("thinking_summaries".to_string(), json!("auto"));
    }
    if let Some(tool_choice) = tool_choice_config(opts.tool_choice.as_ref()) {
        config.insert("tool_choice".to_string(), tool_choice);
    }
    (!config.is_empty()).then_some(config)
}

/// Map Harn's thinking configuration onto `generation_config.thinking_level`.
///
/// `None` means "send nothing and let the model pick", which is the only honest
/// projection of a token budget onto a four-rung ladder.
pub(crate) fn thinking_level(
    thinking: &ThinkingConfig,
    caps: &crate::llm::capabilities::Capabilities,
) -> Option<String> {
    let floor = || {
        ["minimal", "low", "medium", "high", "xhigh", "max"]
            .into_iter()
            .find(|candidate| {
                caps.reasoning_effort_levels.is_empty()
                    || caps
                        .reasoning_effort_levels
                        .iter()
                        .any(|supported| supported == candidate)
            })
            .unwrap_or("minimal")
            .to_string()
    };
    match thinking {
        ThinkingConfig::Disabled => Some(floor()),
        ThinkingConfig::Enabled { .. } | ThinkingConfig::Adaptive => None,
        ThinkingConfig::Effort { level } => Some(
            match level {
                ReasoningEffort::None | ReasoningEffort::Minimal => return Some(floor()),
                ReasoningEffort::Low => "low",
                ReasoningEffort::Medium => "medium",
                ReasoningEffort::High | ReasoningEffort::XHigh | ReasoningEffort::Max => "high",
            }
            .to_string(),
        ),
    }
}

/// Project Harn's portable tool choice onto the Interactions configuration.
///
/// A named choice uses Google's allowlist form so the model cannot call a
/// different tool. Scalar modes remain scalar because that is the shortest
/// native representation.
fn tool_choice_config(tool_choice: Option<&Value>) -> Option<Value> {
    let choice = tool_choice?;
    if let Some(name) = named_tool_choice(choice) {
        return Some(json!({
            "allowed_tools": {
                "mode": "any",
                "tools": [name],
            }
        }));
    }

    Some(json!(match choice {
        Value::String(value) => match value.as_str() {
            "none" => "none",
            "required" | "any" => "any",
            _ => "auto",
        },
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("none") => "none",
            Some("function" | "tool" | "any" | "required") => "any",
            _ => "auto",
        },
        _ => "auto",
    }))
}

fn named_tool_choice(choice: &Value) -> Option<&str> {
    match choice {
        Value::String(value)
            if !value.is_empty()
                && !matches!(value.as_str(), "auto" | "none" | "any" | "required") =>
        {
            Some(value)
        }
        Value::Object(object)
            if matches!(
                object.get("type").and_then(Value::as_str),
                Some("function" | "tool")
            ) =>
        {
            object
                .get("function")
                .and_then(|function| function.get("name"))
                .or_else(|| object.get("name"))
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
        }
        _ => None,
    }
}

fn response_format(opts: &LlmRequestPayload) -> Option<Value> {
    let schema = match &opts.output_format {
        OutputFormat::Text => None,
        OutputFormat::JsonObject => Some(json!({"type": "object"})),
        OutputFormat::JsonSchema { schema, .. } => Some(sanitize_schema_for_provider(
            &opts.provider,
            &opts.model,
            SchemaCompatProfile::Google,
            SchemaSurface::StructuredOutput,
            schema,
        )),
    }?;
    Some(json!({
        "type": "text",
        "mime_type": "application/json",
        "schema": schema,
    }))
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

/// Parse an Interaction envelope into Harn's provider-neutral result.
///
/// Both the non-streaming response and the envelope reassembled from SSE events
/// land here, so a streamed and an unstreamed turn produce identical transcript
/// blocks.
pub(crate) fn parse_response(
    json: &Value,
    request: &LlmRequestPayload,
) -> Result<LlmResult, VmError> {
    if let Some(message) = json
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        return Err(vm_err(format!("{} API error: {message}", request.provider)));
    }

    let mut text = String::new();
    let mut thinking = String::new();
    let mut blocks = Vec::new();
    let mut tool_calls = Vec::new();
    let mut raw_tool_calls = Vec::new();
    // One `thought` step covers the calls that follow it, so the signature is
    // carried forward onto them — that is the shape a stateless replay has to
    // send back.
    let mut signature: Option<String> = None;

    for (index, step) in json
        .get("steps")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        match step.get("type").and_then(Value::as_str).unwrap_or_default() {
            STEP_THOUGHT => {
                if let Some(value) = step.get("signature").and_then(Value::as_str) {
                    if !value.is_empty() {
                        signature = Some(value.to_string());
                    }
                }
                for fragment in step_text(step, "summary") {
                    thinking.push_str(&fragment);
                    blocks.push(reasoning_block(&fragment));
                }
            }
            STEP_MODEL_OUTPUT => {
                for fragment in step_text(step, "content") {
                    text.push_str(&fragment);
                    let mut block = output_text_block(&fragment);
                    if let Some(signature) = &signature {
                        block["provider_metadata"] = json!({
                            "gemini": {"thought_signature": signature}
                        });
                    }
                    blocks.push(block);
                }
            }
            STEP_FUNCTION_CALL => {
                let Some(name) = step
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                else {
                    continue;
                };
                raw_tool_calls.push(RawProviderToolCall::new(step.clone()).map_err(|message| {
                    vm_err(format!("gemini raw tool call parse error: {message}"))
                })?);
                let arguments = step.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let id = step
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("gemini_tool_{}", tool_calls.len()));
                let mut tool_call = json!({
                    "id": id,
                    "name": name,
                    "arguments": arguments.clone(),
                });
                if let Some(signature) = &signature {
                    tool_call["thought_signature"] = json!(signature);
                }
                tool_calls.push(tool_call.clone());
                let mut block = tool_call_block(tool_call["id"].clone(), name, arguments);
                if let Some(signature) = &signature {
                    block["thought_signature"] = json!(signature);
                }
                block["part_index"] = json!(index);
                blocks.push(block);
            }
            _ => {}
        }
    }

    let usage = &json["usage"];
    let input_tokens = usage["total_input_tokens"].as_i64().unwrap_or(0);
    // Thought tokens are billed as output and reported separately, exactly as
    // `generateContent` splits `candidatesTokenCount` from `thoughtsTokenCount`.
    let output_tokens = usage["total_output_tokens"].as_i64().unwrap_or(0)
        + usage["total_thought_tokens"].as_i64().unwrap_or(0);
    let cache_read_tokens = usage["total_cached_tokens"].as_i64().unwrap_or(0);
    let request_id = json["id"].as_str().filter(|value| !value.is_empty());
    let telemetry = ProviderTelemetry::from_gemini_interactions_usage(usage, request_id);

    Ok(LlmResult {
        attempts: Default::default(),
        text_projection: None,
        served_fast: false,
        text,
        raw_tool_calls,
        tool_calls,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens: 0,
        cache_supported: true,
        model: request.model.clone(),
        provider: request.provider.clone(),
        thinking: (!thinking.is_empty()).then_some(thinking),
        thinking_summary: None,
        stop_reason: interaction_stop_reason(json["status"].as_str()),
        blocks,
        logprobs: Vec::new(),
        telemetry,
    })
}

/// Project an Interaction `status` onto Harn's stop-reason vocabulary.
///
/// Interactions reports terminal state as a lifecycle status rather than a
/// finish reason, and running out of output-token budget shows up only as
/// `incomplete` — there is no separate detail field. Left verbatim it would
/// canonicalize to `end_turn` and silently disable Harn's truncation handling,
/// so it is normalized here, at the boundary that owns this wire shape, into
/// the spelling [`stop_reason_is_length`] already recognizes. Spend exhaustion
/// (`budget_exceeded`) and failures (`failed` / `cancelled`) have their own
/// statuses, so they pass through untouched.
///
/// [`stop_reason_is_length`]: crate::llm::api::result::stop_reason_is_length
fn interaction_stop_reason(status: Option<&str>) -> Option<String> {
    Some(match status? {
        "incomplete" => "max_tokens".to_string(),
        other => other.to_string(),
    })
}

/// Concatenated text of a step's typed content list (`content` on
/// `model_output`, `summary` on `thought`).
fn step_text(step: &Value, field: &str) -> Vec<String> {
    step.get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| {
            entry
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        })
        .collect()
}
