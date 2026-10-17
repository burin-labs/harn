//! Anthropic Messages encoding over resolved host policy.

mod messages;
mod tool_history;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

#[cfg(test)]
mod tests;

use crate::request::{LlmRequestPayload, OutputFormat, PromptCacheTtl};
use crate::schema_compat::{SchemaCompatProfile, SchemaSurface};
use harn_provider_catalog::capabilities::{Capabilities, PortableOption};
use serde_json::{json, Value};

/// Native catalog and cache decisions captured before encoding.
pub struct AnthropicWireContext<'a> {
    pub capabilities: &'a Capabilities,
    pub wire_model: &'a str,
    pub prompt_cache: super::prompt_cache::PromptCacheBreakpoint,
    pub tool_search_extensions: bool,
}

/// Encoded body and deferred effects; the host owns warning deduplication.
pub struct AnthropicEncodedRequest {
    pub body: Value,
    pub effects: Vec<AnthropicWireEffect>,
}

/// Ordered audit and warning delivery, interpreted by the native host.
pub enum AnthropicWireEffect {
    SchemaAudit(std::collections::BTreeMap<String, Value>),
    ForcedJsonOverridesTools,
}

/// Encode history, tools, caching, and structured output without native state.
/// The host applies thinking policy and final caller overrides afterward.
pub fn encode_request(
    opts: &LlmRequestPayload,
    context: &AnthropicWireContext<'_>,
) -> AnthropicEncodedRequest {
    let mut audits = Vec::new();
    let mut body = json!({
        "model": context.wire_model,
        "messages": messages::encode_messages(opts, context.capabilities.reasoning_round_trip),
        "max_tokens": if opts.max_tokens > 0 {opts.max_tokens} else {8192},
    });
    if let Some(system) = &opts.system {
        body["system"] = json!(system);
    }
    if let Some(value) = opts.temperature {
        body["temperature"] = json!(value);
    }
    if let Some(value) = opts.top_p {
        body["top_p"] = json!(value);
    }
    if let Some(value) = opts.top_k {
        body["top_k"] = json!(value);
    }
    if let Some(value) = &opts.stop {
        body["stop_sequences"] = json!(value);
    }
    // Contract probes must send the selected unsupported option itself.
    match opts.provider_contract_probe {
        Some(PortableOption::Seed) => {
            if let Some(value) = opts.seed {
                body["seed"] = json!(value);
            }
        }
        Some(PortableOption::FrequencyPenalty) => {
            if let Some(value) = opts.frequency_penalty {
                body["frequency_penalty"] = json!(value);
            }
        }
        Some(PortableOption::PresencePenalty) => {
            if let Some(value) = opts.presence_penalty {
                body["presence_penalty"] = json!(value);
            }
        }
        _ => {}
    }
    let mut cache_control = json!({"type":"ephemeral"});
    if let Some(ttl) = opts
        .prompt_cache_ttl
        .and_then(PromptCacheTtl::anthropic_ttl_field)
    {
        cache_control["ttl"] = json!(ttl);
    }
    super::prompt_cache::apply_prompt_cache_breakpoint(
        &mut body,
        context.prompt_cache,
        cache_control,
    );
    let tools: Vec<Value> = opts
        .native_tools
        .iter()
        .flatten()
        .chain(&opts.provider_tools)
        .map(|tool| sanitize_tool(opts, context.tool_search_extensions, tool, &mut audits))
        .collect();
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    if let Some(choice) = opts.tool_choice.as_ref().and_then(normalize_tool_choice) {
        body["tool_choice"] = choice;
    }
    if body.get("tools").is_some() {
        if let Some(parallel) = opts.parallel_tool_calls {
            if body.get("tool_choice").is_none() {
                body["tool_choice"] = json!({"type":"auto"});
            }
            body["tool_choice"]["disable_parallel_tool_use"] = json!(!parallel);
        }
    }
    let schema = match &opts.output_format {
        OutputFormat::Text => None,
        OutputFormat::JsonObject => Some(std::borrow::Cow::Owned(
            json!({"type":"object","additionalProperties":true}),
        )),
        OutputFormat::JsonSchema { schema, .. } => Some(std::borrow::Cow::Borrowed(schema)),
    };
    let mut effects: Vec<_> = audits
        .drain(..)
        .map(AnthropicWireEffect::SchemaAudit)
        .collect();
    if let Some(schema) = schema {
        let native = context.capabilities.structured_output.as_deref() == Some("native")
            && (!matches!(opts.output_format, OutputFormat::JsonObject)
                || !forced_tool_choice_allowed(context.capabilities));
        if !native
            && (body
                .get("tools")
                .and_then(Value::as_array)
                .is_some_and(|tools| !tools.is_empty())
                || body.get("tool_choice").is_some())
        {
            effects.push(AnthropicWireEffect::ForcedJsonOverridesTools);
        }
        let schema = super::sanitize_schema_for_provider(
            "anthropic",
            &opts.model,
            SchemaCompatProfile::AnthropicStrict,
            SchemaSurface::StructuredOutput,
            &schema,
            &mut audits,
        );
        if native {
            body["output_config"] = json!({"format":{"type":"json_schema","schema":schema}});
        } else {
            let mut tools = body["tools"].as_array().cloned().unwrap_or_default();
            tools.push(json!({"name":"json_response", "description":"Return a structured JSON response matching the schema.","input_schema":schema}));
            body["tools"] = Value::Array(tools);
            body["tool_choice"] = json!({"type":"tool","name":"json_response"});
        }
    }
    effects.extend(audits.into_iter().map(AnthropicWireEffect::SchemaAudit));
    AnthropicEncodedRequest { body, effects }
}

fn sanitize_tool(
    opts: &LlmRequestPayload,
    tool_search: bool,
    tool: &Value,
    audits: &mut Vec<std::collections::BTreeMap<String, Value>>,
) -> Value {
    let mut tool = tool.clone();
    if let Some(object) = tool.as_object_mut() {
        object.remove("x-harn-output-schema");
        if !tool_search {
            object.remove("defer_loading");
        }
        object.remove("namespace");
        object.remove("namespaces");
        if let Some(schema) = object.get("input_schema").cloned() {
            object.insert(
                "input_schema".to_owned(),
                super::sanitize_schema_for_provider(
                    &opts.provider,
                    &opts.model,
                    SchemaCompatProfile::AnthropicStrict,
                    SchemaSurface::ToolParameters,
                    &schema,
                    audits,
                ),
            );
        }
    }
    tool
}

/// Normalize OpenAI and Anthropic tool choice forms to the Messages object.
fn normalize_tool_choice(value: &Value) -> Option<Value> {
    let attach_parallel = |mut object: Value| {
        if let Some(flag) = value.get("disable_parallel_tool_use") {
            object["disable_parallel_tool_use"] = flag.clone();
        }
        object
    };
    match value {
        Value::String(value) => Some(match value.as_str() {
            "auto" => json!({"type":"auto"}),
            "any" | "required" => json!({"type":"any"}),
            "none" => json!({"type":"none"}),
            name => json!({"type":"tool","name":name}),
        }),
        Value::Object(_) => match value.get("type").and_then(Value::as_str) {
            Some("auto") => Some(attach_parallel(json!({"type":"auto"}))),
            Some("any" | "required") => Some(attach_parallel(json!({"type":"any"}))),
            Some("none") => Some(attach_parallel(json!({"type":"none"}))),
            Some("tool") => value
                .get("name")
                .and_then(Value::as_str)
                .map(|name| attach_parallel(json!({"type":"tool","name":name}))),
            Some("function") => value
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .map(|name| attach_parallel(json!({"type":"tool","name":name}))),
            _ => Some(json!({"type":"auto"})),
        },
        _ => None,
    }
}

/// Whether the normalized tool choice requires a tool call.
pub fn tool_choice_forces_tool_use(value: &Value) -> bool {
    normalize_tool_choice(value).is_some_and(|choice| {
        matches!(
            choice.get("type").and_then(Value::as_str),
            Some("any" | "tool")
        )
    })
}

/// An empty allowed-mode list means the catalog declares no restriction.
pub fn forced_tool_choice_allowed(caps: &Capabilities) -> bool {
    caps.allowed_tool_choice_modes.is_empty()
        || caps
            .allowed_tool_choice_modes
            .iter()
            .any(|mode| mode == "required")
}
