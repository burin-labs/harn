//! Responses request encoding over resolved host policy, without native runtime state.

use crate::request::{LlmRequestPayload, OutputFormat, ReasoningEffort, ThinkingConfig};
use crate::schema_compat::{SchemaCompatProfile, SchemaSurface};
use harn_provider_catalog::{ReasoningModeRequestDef, ServingTierRequestDef};

#[cfg(test)]
mod tests;

/// Catalog decisions captured by the host before encoding a Responses request.
pub struct ResponsesWireContext<'a> {
    pub wire_model: &'a str,
    pub tool_search_extensions: bool,
    pub reasoning_request: Option<&'a ReasoningModeRequestDef>,
    pub fast_request: Option<&'a ServingTierRequestDef>,
}

/// Encode the full request and return schema audits for the host to publish.
pub fn encode_request(
    opts: &LlmRequestPayload,
    context: &ResponsesWireContext<'_>,
) -> super::EncodedRequest {
    let mut audits = Vec::new();
    let mut body = encode_compact_request(opts, context.wire_model);
    if opts.max_tokens > 0 {
        body["max_output_tokens"] = serde_json::json!(opts.max_tokens);
    }
    if let Some(temp) = opts.temperature {
        body["temperature"] = serde_json::json!(temp);
    }
    if let Some(top_p) = opts.top_p {
        body["top_p"] = serde_json::json!(top_p);
    }
    if let Some(logprobs) = opts.logprobs {
        body["top_logprobs"] = serde_json::json!(logprobs.top.unwrap_or(0));
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
        body["tool_choice"] = responses_tool_choice(tool_choice);
    }
    if let Some(reasoning) = responses_reasoning_config(&opts.thinking) {
        body["reasoning"] = reasoning;
    }
    // Merge mode after effort so the two fields share the reasoning object.
    if let Some(request) = context.reasoning_request {
        super::apply_reasoning_mode(&mut body, request);
    }
    match &opts.output_format {
        OutputFormat::Text => {}
        OutputFormat::JsonObject => {
            body["text"] = serde_json::json!({"format": {"type": "json_object"}});
        }
        OutputFormat::JsonSchema { schema, strict } => {
            let schema_profile = if *strict {
                SchemaCompatProfile::OpenAiStrict
            } else {
                SchemaCompatProfile::OpenAiLenient
            };
            let schema = super::sanitize_schema_for_provider(
                &opts.provider,
                &opts.model,
                schema_profile,
                SchemaSurface::StructuredOutput,
                schema,
                &mut audits,
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
    if let Some(verbosity) = opts.verbosity {
        if body.get("text").is_none() {
            body["text"] = serde_json::json!({});
        }
        body["text"]["verbosity"] = serde_json::json!(verbosity.as_str());
    }
    let tools = responses_tools(opts, context.tool_search_extensions, &mut audits);
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(tools);
        if let Some(parallel) = opts.parallel_tool_calls {
            body["parallel_tool_calls"] = serde_json::json!(parallel);
        }
    }
    if let Some(request) = context.fast_request {
        body.as_object_mut().expect("request is an object").insert(
            request.param.clone(),
            serde_json::Value::String(request.value.clone()),
        );
    }
    super::EncodedRequest {
        body,
        schema_audits: audits,
    }
}

/// Compaction uses the same history projection with only compaction controls.
pub fn encode_compact_request(opts: &LlmRequestPayload, wire_model: &str) -> serde_json::Value {
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

fn responses_tool_choice(choice: &serde_json::Value) -> serde_json::Value {
    let Some(name) = choice
        .get("function")
        .and_then(|function| function.get("name"))
        .and_then(serde_json::Value::as_str)
    else {
        return choice.clone();
    };
    serde_json::json!({"type": "function", "name": name})
}

fn responses_reasoning_config(thinking: &ThinkingConfig) -> Option<serde_json::Value> {
    match thinking {
        ThinkingConfig::Effort {
            level: ReasoningEffort::None,
        } => None,
        ThinkingConfig::Effort { level } => Some(serde_json::json!({"effort": level.as_str()})),
        _ => None,
    }
}

fn responses_tools(
    opts: &LlmRequestPayload,
    tool_search_extensions: bool,
    audits: &mut Vec<std::collections::BTreeMap<String, serde_json::Value>>,
) -> Vec<serde_json::Value> {
    let mut tools = opts.provider_tools.clone();
    let provider_tool_count = tools.len();
    if let Some(native_tools) = opts.native_tools.as_ref() {
        tools.extend(native_tools.iter().cloned());
    }
    tools
        .into_iter()
        .enumerate()
        .map(|(index, tool)| {
            if index < provider_tool_count {
                tool
            } else {
                responses_function_tool(
                    &opts.provider,
                    &opts.model,
                    &tool,
                    tool_search_extensions,
                    audits,
                )
            }
        })
        .collect()
}

fn responses_function_tool(
    provider: &str,
    model: &str,
    tool: &serde_json::Value,
    tool_search_extensions: bool,
    audits: &mut Vec<std::collections::BTreeMap<String, serde_json::Value>>,
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
            super::sanitize_schema_for_provider(
                provider,
                model,
                schema_profile,
                SchemaSurface::ToolParameters,
                parameters,
                audits,
            ),
        );
    }
    // Namespace is storage metadata; defer_loading belongs only to tool-search routes.
    if tool_search_extensions {
        if let Some(value) = tool
            .get("defer_loading")
            .or_else(|| function.get("defer_loading"))
        {
            out.insert("defer_loading".to_string(), value.clone());
        }
    }
    serde_json::Value::Object(out)
}

fn responses_input_items(opts: &LlmRequestPayload) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    // Buffer tool-result images until the end of the parallel result batch.
    let mut pending_images = Vec::new();
    for message in &opts.messages {
        let is_tool = matches!(
            message.get("role").and_then(serde_json::Value::as_str),
            Some("tool" | "tool_result")
        );
        if !is_tool {
            flush_responses_pending_images(&mut items, &mut pending_images);
        }
        append_responses_message_items(message, &mut items, &mut pending_images);
    }
    flush_responses_pending_images(&mut items, &mut pending_images);
    if let Some(ref prefill) = opts.prefill {
        items.push(serde_json::json!({"role": "assistant", "content": prefill}));
    }
    items
}

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
    if matches!(role, "tool" | "tool_result") {
        let call_id = message
            .get("tool_call_id")
            .or_else(|| message.get("call_id"))
            .or_else(|| message.get("tool_use_id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let content = message
            .get("content")
            .map(crate::content::provider_visible_content);
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
        items.push(serde_json::json!({"type": "function_call_output", "call_id": call_id, "output": output}));
        pending_images.extend(images);
        return;
    }
    if let Some(content) = message.get("content") {
        let content = responses_message_content(role, content);
        let is_empty = match &content {
            serde_json::Value::String(text) => text.is_empty(),
            serde_json::Value::Array(parts) => parts.is_empty(),
            serde_json::Value::Null => true,
            _ => false,
        };
        if !is_empty {
            items.push(serde_json::json!({"role": role, "content": content}));
        }
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
            let arguments = match arguments {
                serde_json::Value::String(text) => text,
                value => serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string()),
            };
            items.push(serde_json::json!({"type": "function_call", "call_id": call_id, "name": name, "arguments": arguments}));
        }
    }
}

fn responses_message_content(role: &str, content: &serde_json::Value) -> serde_json::Value {
    let visible = crate::content::provider_visible_content(content);
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

fn responses_image_input_item(item: &serde_json::Value) -> Option<serde_json::Value> {
    if let Ok(screen) = crate::content::ScreenImage::try_from(item) {
        return Some(serde_json::json!({"type": "input_image", "image_url": screen.to_data_url()}));
    }
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
        // Private continuation blocks and unknown storage metadata stay off the wire.
        _ => None,
    }
}
