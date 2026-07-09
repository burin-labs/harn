//! Native OpenAI Responses API provider path.
//!
//! This is intentionally separate from the OpenAI-compatible chat-completions
//! adapter. Responses has first-class hosted tools, provider-managed
//! conversation state, and different output items, so keeping the wire builder
//! distinct prevents compatibility shims from leaking into generic
//! OpenAI-compatible providers.

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult, OutputFormat, ThinkingConfig};
use crate::llm::providers::schema_compat::{
    sanitize_schema_for_provider, SchemaCompatProfile, SchemaSurface,
};
use crate::value::{VmError, VmValue};

const RESPONSES_ENDPOINT: &str = "/responses";
const RESPONSES_COMPACT_ENDPOINT: &str = "/responses/compact";

pub(crate) struct OpenAiResponsesProvider;

impl OpenAiResponsesProvider {
    pub(crate) async fn call(
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        if request.provider != "openai" {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "api_mode: \"responses\" is only supported by provider \"openai\"; got provider \"{}\"",
                request.provider
            )))));
        }

        let clock = harn_clock::RealClock::arc();
        let started_ms = clock.monotonic_ms();
        let compact = request.compact.unwrap_or(false);
        let mut body = if compact {
            Self::build_compact_request_body(request)
        } else {
            Self::build_request_body(request)
        };
        if let Some(ref overrides) = request.provider_overrides {
            if let Some(object) = overrides.as_object() {
                for (key, value) in object {
                    body[key] = value.clone();
                }
            }
        }

        let mut resolved = crate::llm::helpers::ResolvedProvider::resolve(&request.provider);
        resolved.endpoint = if compact {
            RESPONSES_COMPACT_ENDPOINT
        } else {
            RESPONSES_ENDPOINT
        }
        .to_string();
        let client = crate::llm::shared_blocking_client().clone();
        let req = client
            .post(resolved.url())
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(request.resolve_timeout()))
            .json(&body);
        let req = resolved.apply_headers(req, &request.api_key);
        let response = req.send().await.map_err(|error| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "openai Responses API error: {}",
                crate::egress::redact_reqwest_error(&error)
            ))))
        })?;

        if !response.status().is_success() {
            let status = response.status();
            let retry_after = crate::llm::api::retry_after_header(response.headers());
            let body = response.text().await.unwrap_or_default();
            let msg = crate::llm::providers::OpenAiCompatibleProvider::classify_http_error(
                "openai",
                status,
                retry_after.as_deref(),
                &body,
            )
            .message;
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(msg))));
        }

        let json: serde_json::Value = response.json().await.map_err(|error| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "openai Responses API response parse error: {error}"
            ))))
        })?;

        let mut result =
            crate::llm::api::parse_openai_responses_response(&json, "openai", &request.model)?;
        if result.telemetry.client_wall_ms.is_none() {
            result.telemetry.client_wall_ms = Some(elapsed_ms(&*clock, started_ms));
        }
        if result.telemetry.source.is_empty() {
            result.telemetry.source = crate::llm::api::telemetry_source::UNKNOWN.to_string();
        }
        if let Some(tx) = delta_tx {
            if !result.text.is_empty() {
                let _ = tx.send(result.text.clone());
            }
        }
        Ok(result)
    }

    pub(crate) fn build_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let wire_model = crate::llm_config::wire_model_id(&opts.model);
        let mut body = serde_json::json!({
            "model": wire_model,
            "input": responses_input_items(opts),
        });

        if let Some(ref system) = opts.system {
            body["instructions"] = serde_json::json!(system);
        }
        if opts.max_tokens > 0 {
            body["max_output_tokens"] = serde_json::json!(opts.max_tokens);
        }
        if let Some(temp) = opts.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(top_p) = opts.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(ref stop) = opts.stop {
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
        if let Some(ref previous_response_id) = opts.previous_response_id {
            body["previous_response_id"] = serde_json::json!(previous_response_id);
        }
        if let Some(store) = opts.store {
            body["store"] = serde_json::json!(store);
        }
        if let Some(background) = opts.background {
            body["background"] = serde_json::json!(background);
        }
        if let Some(ref truncation) = opts.truncation {
            body["truncation"] = serde_json::json!(truncation);
        }
        if let Some(ref include) = opts.include {
            body["include"] = serde_json::json!(include);
        }
        if let Some(max_tool_calls) = opts.max_tool_calls {
            body["max_tool_calls"] = serde_json::json!(max_tool_calls);
        }
        if let Some(ref tool_choice) = opts.tool_choice {
            body["tool_choice"] = tool_choice.clone();
        }
        if let Some(reasoning) = responses_reasoning_config(&opts.thinking) {
            body["reasoning"] = reasoning;
        }
        match &opts.output_format {
            OutputFormat::Text => {}
            OutputFormat::JsonObject => {
                body["text"] = serde_json::json!({
                    "format": {"type": "json_object"}
                });
            }
            OutputFormat::JsonSchema { schema, strict } => {
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
                body["text"] = serde_json::json!({
                    "format": {
                        "type": "json_schema",
                        "name": "response",
                        "schema": schema,
                        "strict": strict,
                    }
                });
            }
        }

        let tools = responses_tools(opts);
        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(tools);
        }

        crate::llm::serving_tiers::apply_fast_request_knob(&mut body, &opts.model, opts.fast);
        body
    }

    pub(crate) fn build_compact_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let wire_model = crate::llm_config::wire_model_id(&opts.model);
        let mut body = serde_json::json!({
            "model": wire_model,
            "input": responses_input_items(opts),
        });
        if let Some(ref system) = opts.system {
            body["instructions"] = serde_json::json!(system);
        }
        if let Some(ref previous_response_id) = opts.previous_response_id {
            body["previous_response_id"] = serde_json::json!(previous_response_id);
        }
        body
    }
}

fn responses_reasoning_config(thinking: &ThinkingConfig) -> Option<serde_json::Value> {
    match thinking {
        ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::None,
        } => None,
        ThinkingConfig::Effort { level } => Some(serde_json::json!({
            "effort": level.as_str(),
        })),
        _ => None,
    }
}

fn responses_tools(opts: &LlmRequestPayload) -> Vec<serde_json::Value> {
    let mut tools = opts.provider_tools.clone();
    if let Some(native_tools) = opts.native_tools.as_ref() {
        tools.extend(
            native_tools
                .iter()
                .map(|tool| responses_function_tool(&opts.provider, &opts.model, tool)),
        );
    }
    tools
}

fn responses_function_tool(
    provider: &str,
    model: &str,
    tool: &serde_json::Value,
) -> serde_json::Value {
    if tool.get("type").and_then(serde_json::Value::as_str) != Some("function") {
        return tool.clone();
    }
    let Some(function) = tool.get("function").and_then(serde_json::Value::as_object) else {
        return tool.clone();
    };

    let mut out = serde_json::Map::new();
    out.insert("type".to_string(), serde_json::json!("function"));
    for key in ["name", "description", "strict"] {
        if let Some(value) = function.get(key) {
            out.insert(key.to_string(), value.clone());
        }
    }
    if let Some(parameters) = function.get("parameters") {
        let schema_profile = if function
            .get("strict")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            SchemaCompatProfile::OpenAiStrict
        } else {
            SchemaCompatProfile::OpenAiLenient
        };
        out.insert(
            "parameters".to_string(),
            sanitize_schema_for_provider(
                provider,
                model,
                schema_profile,
                SchemaSurface::ToolParameters,
                parameters,
            ),
        );
    }
    for key in ["defer_loading", "namespace", "namespaces"] {
        if let Some(value) = tool.get(key).or_else(|| function.get(key)) {
            out.insert(key.to_string(), value.clone());
        }
    }
    serde_json::Value::Object(out)
}

fn responses_input_items(opts: &LlmRequestPayload) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    // Images stripped off `function_call_output`s (a screenshot rides back on a
    // tool result, but Responses only accepts images on a `user` item). They are
    // buffered across the contiguous run of tool results and flushed as one
    // `user` item when the run ends, so an image never lands *between* two
    // `function_call_output` items in a parallel tool-call batch.
    let mut pending_images: Vec<serde_json::Value> = Vec::new();
    for message in &opts.messages {
        let is_tool = message.get("role").and_then(serde_json::Value::as_str) == Some("tool");
        if !is_tool {
            flush_responses_pending_images(&mut items, &mut pending_images);
        }
        append_responses_message_items(message, &mut items, &mut pending_images);
    }
    flush_responses_pending_images(&mut items, &mut pending_images);
    if let Some(ref prefill) = opts.prefill {
        items.push(serde_json::json!({
            "role": "assistant",
            "content": prefill,
        }));
    }
    items
}

/// Emit the buffered tool-result images (if any) as one `user` item; called when
/// a run of tool results ends so the images land after the whole run, never
/// between two `function_call_output` items.
fn flush_responses_pending_images(
    items: &mut Vec<serde_json::Value>,
    pending: &mut Vec<serde_json::Value>,
) {
    if pending.is_empty() {
        return;
    }
    let mut user_content = vec![serde_json::json!({
        "type": "input_text",
        "text": "Screenshot(s) from the preceding tool result(s):",
    })];
    user_content.append(pending);
    items.push(serde_json::json!({"role": "user", "content": user_content}));
}

fn append_responses_message_items(
    message: &serde_json::Value,
    items: &mut Vec<serde_json::Value>,
    pending_images: &mut Vec<serde_json::Value>,
) {
    let role = message
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("user");
    if role == "tool" {
        let call_id = message
            .get("tool_call_id")
            .or_else(|| message.get("call_id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        // The Responses API `function_call_output.output` is a string, and it
        // cannot carry an image. A tool that returns a screenshot rides it back
        // on its result, so split: text -> `output`, images -> a following user
        // message as `input_image` items (where the Responses API accepts them).
        let content = message
            .get("content")
            .map(crate::llm::content::provider_visible_content);
        let content = content.as_ref();
        let images: Vec<serde_json::Value> = content
            .and_then(serde_json::Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(responses_image_input_item)
                    .collect()
            })
            .unwrap_or_default();
        let output = match content {
            Some(serde_json::Value::String(text)) => text.clone(),
            Some(serde_json::Value::Array(parts)) => {
                // Concatenate only the text parts; images move below.
                let text = parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() && !images.is_empty() {
                    "(screenshot returned; see the image in the following message)".to_string()
                } else {
                    text
                }
            }
            Some(other) => other.to_string(),
            None => String::new(),
        };
        items.push(serde_json::json!({
            "type": "function_call_output",
            "call_id": call_id,
            "output": output,
        }));
        // Buffer the images; the caller flushes them as a `user` item once this
        // contiguous run of tool results ends, preserving output adjacency.
        pending_images.extend(images);
        return;
    }

    if let Some(content) = message.get("content") {
        items.push(serde_json::json!({
            "role": role,
            "content": responses_message_content(role, content),
        }));
    }

    if let Some(tool_calls) = message
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
    {
        for tool_call in tool_calls {
            let function = tool_call.get("function").unwrap_or(tool_call);
            let call_id = tool_call
                .get("id")
                .or_else(|| tool_call.get("call_id"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let name = function
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let arguments = function
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            items.push(serde_json::json!({
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": arguments_to_string(arguments),
            }));
        }
    }
}

fn responses_message_content(role: &str, content: &serde_json::Value) -> serde_json::Value {
    let visible = crate::llm::content::provider_visible_content(content);
    match &visible {
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .filter_map(|item| responses_content_item(role, item))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Build a Responses `input_image` item from a tool-result content part that
/// carries an image: the neutral screenshot dict the computer tool returns (via
/// the canonical [`ScreenImage`] definition), or a typed image block
/// (`image`/`input_image`/`image_url`) carrying a url. Returns `None` for
/// non-image parts (text, etc.).
fn responses_image_input_item(item: &serde_json::Value) -> Option<serde_json::Value> {
    // Neutral screenshot dict -> data URL, keyed off the single ScreenImage
    // definition (same signature the rest of the pipeline detects).
    if let Ok(screen) = crate::llm::content::ScreenImage::try_from(item) {
        return Some(serde_json::json!({
            "type": "input_image",
            "image_url": screen.to_data_url(),
        }));
    }
    // Otherwise a typed image block that carries a url directly.
    let type_tag = item.get("type").and_then(serde_json::Value::as_str);
    if !matches!(type_tag, Some("image" | "input_image" | "image_url")) {
        return None;
    }
    let image_url = item
        .get("url")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            item.get("image_url").and_then(|value| {
                value.as_str().map(str::to_string).or_else(|| {
                    value
                        .get("url")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
            })
        })?;
    Some(serde_json::json!({"type": "input_image", "image_url": image_url}))
}

fn responses_content_item(role: &str, item: &serde_json::Value) -> Option<serde_json::Value> {
    let item_type = item.get("type").and_then(serde_json::Value::as_str)?;
    match item_type {
        "text" | "input_text" | "output_text" => {
            let text = item
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let response_type = if role == "assistant" {
                "output_text"
            } else {
                "input_text"
            };
            Some(serde_json::json!({"type": response_type, "text": text}))
        }
        "image" | "input_image" => {
            let image_url = item
                .get("url")
                .or_else(|| item.get("image_url"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    let base64 = item.get("base64").and_then(serde_json::Value::as_str)?;
                    let media_type = item
                        .get("media_type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("image/png");
                    Some(format!("data:{media_type};base64,{base64}"))
                })?;
            Some(serde_json::json!({"type": "input_image", "image_url": image_url}))
        }
        "file" | "input_file" | "pdf" => {
            if let Some(file_id) = item
                .get("file_id")
                .or_else(|| item.get("file"))
                .and_then(serde_json::Value::as_str)
            {
                return Some(serde_json::json!({"type": "input_file", "file_id": file_id}));
            }
            let file_data = item.get("base64").and_then(serde_json::Value::as_str)?;
            Some(serde_json::json!({"type": "input_file", "file_data": file_data}))
        }
        _ => Some(item.clone()),
    }
}

fn arguments_to_string(arguments: serde_json::Value) -> String {
    match arguments {
        serde_json::Value::String(text) => text,
        value => serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string()),
    }
}

fn elapsed_ms(clock: &dyn harn_clock::Clock, started_ms: i64) -> u64 {
    clock.monotonic_ms().saturating_sub(started_ms).max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{LlmApiMode, LlmRequestPayload, OutputFormat};

    #[test]
    fn responses_tool_result_screenshot_relocated_to_user_input_image() {
        // A computer-use tool result carries [text, neutral-screenshot-dict].
        // The Responses `function_call_output.output` is a string, so the image
        // must move to a following `user` input item as an `input_image`, flushed
        // by `responses_input_items` after the tool-result run.
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.messages = vec![serde_json::json!({
            "role": "tool",
            "tool_call_id": "call_1",
            "content": [
                {"type": "text", "text": "Captured screenshot."},
                {"base64": "AAAB", "media_type": "image/png", "scale_factor": 2.0},
            ],
        })];
        let payload = LlmRequestPayload::from(&opts);
        let items = responses_input_items(&payload);
        assert_eq!(
            items.len(),
            2,
            "function_call_output + a user image message"
        );
        assert_eq!(items[0]["type"], "function_call_output");
        assert_eq!(items[0]["output"], "Captured screenshot.");
        // No image leaked into the string output.
        assert!(!items[0]["output"].as_str().unwrap().contains("base64"));
        assert_eq!(items[1]["role"], "user");
        let content = items[1]["content"].as_array().expect("user content");
        let img = content
            .iter()
            .find(|c| c.get("type").and_then(|t| t.as_str()) == Some("input_image"))
            .expect("input_image present");
        assert_eq!(img["image_url"], "data:image/png;base64,AAAB");
    }

    #[test]
    fn responses_input_items_drop_private_reasoning_blocks() {
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "reasoning", "text": "private plan", "visibility": "private"},
                    {"type": "output_text", "text": "visible answer", "visibility": "public"}
                ],
            }),
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "call_1",
                "content": [
                    {"type": "thinking", "text": "hidden tool trace"},
                    {"type": "text", "text": "tool output"}
                ],
            }),
        ];
        let payload = LlmRequestPayload::from(&opts);
        let items = responses_input_items(&payload);
        let serialized = serde_json::to_string(&items).expect("serialize");

        assert!(!serialized.contains("private plan"));
        assert!(!serialized.contains("hidden tool trace"));
        assert!(serialized.contains("visible answer"));
        assert!(serialized.contains("tool output"));
    }

    #[test]
    fn responses_parallel_tool_results_stay_adjacent_with_image() {
        // Parallel tool-call batch whose FIRST result carries a screenshot: the
        // two `function_call_output` items must stay contiguous, with the
        // relocated `user` image landing AFTER the whole run — never between the
        // outputs.
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.messages = vec![
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "c1",
                "content": [
                    {"type": "text", "text": "shot"},
                    {"base64": "AAAB", "media_type": "image/png", "scale_factor": 1.0},
                ],
            }),
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "c2",
                "content": [{"type": "text", "text": "read ok"}],
            }),
        ];
        let payload = LlmRequestPayload::from(&opts);
        let items = responses_input_items(&payload);
        let kinds: Vec<String> = items
            .iter()
            .map(|item| {
                item.get("type")
                    .and_then(|t| t.as_str())
                    .or_else(|| item.get("role").and_then(|r| r.as_str()))
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        assert_eq!(
            kinds,
            ["function_call_output", "function_call_output", "user"],
            "outputs stay adjacent; image flushes after the run: {items:?}"
        );
    }

    #[test]
    fn responses_body_maps_structured_output_and_controls() {
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.model = "gpt-5.4".to_string();
        opts.api_mode = LlmApiMode::Responses;
        opts.previous_response_id = Some("resp_prev".to_string());
        opts.store = Some(false);
        opts.background = Some(true);
        opts.truncation = Some("auto".to_string());
        opts.include = Some(vec!["file_search_call.results".to_string()]);
        opts.max_tool_calls = Some(3);
        opts.output_format = OutputFormat::JsonSchema {
            schema: serde_json::json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {
                    "ok": {
                        "type": "string",
                        "pattern": "^ok$",
                        "minLength": 2,
                        "format": "email",
                        "default": "ok"
                    },
                    "variant": {
                        "oneOf": [
                            {"type": "string"},
                            {"type": "integer"}
                        ]
                    }
                },
                "required": ["ok", "variant"],
            }),
            strict: true,
        };
        opts.provider_tools = vec![serde_json::json!({
            "type": "web_search_preview"
        })];
        opts.native_tools = Some(vec![serde_json::json!({
            "type": "function",
            "defer_loading": true,
            "function": {
                "name": "lookup",
                "description": "Lookup a record",
                "strict": true,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "pattern": "^harn",
                            "minLength": 1,
                            "default": "harn"
                        },
                        "mode": {
                            "oneOf": [
                                {"type": "string"},
                                {"type": "integer"}
                            ]
                        }
                    }
                },
            }
        })]);
        let payload = LlmRequestPayload::from(&opts);

        let body = OpenAiResponsesProvider::build_request_body(&payload);

        assert_eq!(body["model"], "gpt-5.4");
        assert_eq!(body["previous_response_id"], "resp_prev");
        assert_eq!(body["store"], false);
        assert_eq!(body["background"], true);
        assert_eq!(body["truncation"], "auto");
        assert_eq!(body["include"][0], "file_search_call.results");
        assert_eq!(body["max_tool_calls"], 3);
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert_eq!(
            body["text"]["format"]["schema"]["properties"]["ok"]["type"],
            "string"
        );
        assert_eq!(
            body["text"]["format"]["schema"]["additionalProperties"],
            false
        );
        assert_eq!(
            body["text"]["format"]["schema"]["required"],
            serde_json::json!(["ok", "variant"])
        );
        assert!(body["text"]["format"]["schema"]["properties"]["ok"]
            .get("default")
            .is_none());
        assert!(body["text"]["format"]["schema"]["properties"]["ok"]
            .get("pattern")
            .is_none());
        assert!(body["text"]["format"]["schema"]["properties"]["ok"]
            .get("minLength")
            .is_none());
        assert!(body["text"]["format"]["schema"]["properties"]["ok"]
            .get("format")
            .is_none());
        assert!(body["text"]["format"]["schema"]["properties"]["variant"]
            .get("oneOf")
            .is_none());
        assert!(
            body["text"]["format"]["schema"]["properties"]["variant"]["description"]
                .as_str()
                .expect("oneOf compatibility note")
                .contains("Original JSON Schema `oneOf` constraint omitted")
        );
        assert_eq!(body["tools"][0]["type"], "web_search_preview");
        assert_eq!(body["tools"][1]["type"], "function");
        assert_eq!(body["tools"][1]["name"], "lookup");
        assert_eq!(body["tools"][1]["defer_loading"], true);
        assert_eq!(
            body["tools"][1]["parameters"]["additionalProperties"],
            false
        );
        assert_eq!(
            body["tools"][1]["parameters"]["required"],
            serde_json::json!(["mode", "query"])
        );
        assert!(body["tools"][1]["parameters"]["properties"]["query"]
            .get("default")
            .is_none());
        assert!(body["tools"][1]["parameters"]["properties"]["query"]
            .get("pattern")
            .is_none());
        assert!(body["tools"][1]["parameters"]["properties"]["query"]
            .get("minLength")
            .is_none());
        assert!(body["tools"][1]["parameters"]["properties"]["mode"]
            .get("oneOf")
            .is_none());
    }

    #[test]
    fn compact_body_keeps_only_compaction_controls() {
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.model = "gpt-5.4".to_string();
        opts.api_mode = LlmApiMode::Responses;
        opts.system = Some("Preserve actionable state.".to_string());
        opts.previous_response_id = Some("resp_prev".to_string());
        opts.output_format = OutputFormat::JsonObject;
        opts.provider_tools = vec![serde_json::json!({"type": "web_search"})];
        opts.truncation = Some("auto".to_string());
        opts.max_tool_calls = Some(3);
        let payload = LlmRequestPayload::from(&opts);

        let body = OpenAiResponsesProvider::build_compact_request_body(&payload);

        assert_eq!(body["model"], "gpt-5.4");
        assert_eq!(body["instructions"], "Preserve actionable state.");
        assert_eq!(body["previous_response_id"], "resp_prev");
        assert!(body.get("text").is_none());
        assert!(body.get("tools").is_none());
        assert!(body.get("truncation").is_none());
        assert!(body.get("max_tool_calls").is_none());
    }
}
