use super::*;

/// Read the `provider_overrides.force_native_tool_search` escape hatch
/// (bool). Set to true when a user is pointed at a proxied OpenAI-compat
/// endpoint (self-hosted router, enterprise gateway) whose model ID
/// Harn cannot parse but that is known to forward `tool_search` +
/// `defer_loading` unchanged.
pub(super) fn provider_overrides_force_native(
    options: Option<&BTreeMap<String, VmValue>>,
    provider: &str,
) -> bool {
    let Some(options) = options else { return false };
    let Some(VmValue::Dict(overrides)) = options.get(provider) else {
        return false;
    };
    matches!(
        overrides.get("force_native_tool_search"),
        Some(VmValue::Bool(true))
    )
}

/// Decide which wire shape this (provider, model) pair should emit for
/// the native tool-search meta-tool.
pub(super) fn classify_native_shape(
    provider: &str,
    model: &str,
) -> crate::llm::provider::NativeToolSearchShape {
    crate::llm::provider::provider_native_tool_search_shape(provider, model)
}

pub(super) fn parse_api_mode_option(
    options: Option<&BTreeMap<String, VmValue>>,
) -> Result<crate::llm::api::LlmApiMode, VmError> {
    let Some(raw) = options.and_then(|o| o.get("api_mode").or_else(|| o.get("api"))) else {
        return Ok(crate::llm::api::LlmApiMode::ChatCompletions);
    };
    match raw {
        VmValue::Nil => Ok(crate::llm::api::LlmApiMode::ChatCompletions),
        VmValue::String(value) => {
            let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
            match normalized.as_str() {
                "chat" | "chat_completions" | "chat_completion" | "completions" => {
                    Ok(crate::llm::api::LlmApiMode::ChatCompletions)
                }
                "responses" | "response" => Ok(crate::llm::api::LlmApiMode::Responses),
                other => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    format!(
                        "api_mode: expected \"chat_completions\" or \"responses\", got \"{other}\""
                    ),
                )))),
            }
        }
        other => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            format!("api_mode: expected a string, got {}", other.type_name()),
        )))),
    }
}

pub(super) fn enforce_responses_provider_gate(
    mode: crate::llm::api::LlmApiMode,
    provider: &str,
) -> bool {
    mode == crate::llm::api::LlmApiMode::Responses && provider != "openai" && provider != "mock"
}

pub(super) fn parse_provider_tools_option(
    options: Option<&BTreeMap<String, VmValue>>,
) -> Result<Vec<serde_json::Value>, VmError> {
    let Some(raw) = options.and_then(|o| o.get("provider_tools").or_else(|| o.get("hosted_tools")))
    else {
        return Ok(Vec::new());
    };
    match raw {
        VmValue::Nil | VmValue::Bool(false) => Ok(Vec::new()),
        VmValue::Dict(_) => Ok(vec![vm_value_to_json(raw)]),
        VmValue::List(list) => list
            .iter()
            .map(|value| match value {
                VmValue::String(kind) => Ok(serde_json::json!({"type": kind.as_ref()})),
                VmValue::Dict(_) => Ok(vm_value_to_json(value)),
                other => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    format!(
                        "provider_tools: expected each entry to be a dict or string, got {}",
                        other.type_name()
                    ),
                )))),
            })
            .collect(),
        other => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            format!(
                "provider_tools: expected a list or dict, got {}",
                other.type_name()
            ),
        )))),
    }
}

pub(super) fn opt_bool_field(
    options: Option<&BTreeMap<String, VmValue>>,
    key: &str,
) -> Result<Option<bool>, VmError> {
    match options.and_then(|o| o.get(key)) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            format!("{key}: expected a bool, got {}", other.type_name()),
        )))),
    }
}

pub(super) fn opt_responses_store_field(
    options: Option<&BTreeMap<String, VmValue>>,
) -> Result<Option<bool>, VmError> {
    if let Some(value) = opt_bool_field(options, "response_store")? {
        return Ok(Some(value));
    }
    if let Some(value) = opt_bool_field(options, "responses_store")? {
        return Ok(Some(value));
    }
    match options.and_then(|o| o.get("store")) {
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        _ => Ok(None),
    }
}

pub(super) fn parse_schema_value(
    raw: Option<&VmValue>,
    field: &str,
) -> Result<Option<serde_json::Value>, VmError> {
    match raw {
        None | Some(VmValue::Nil) => Ok(None),
        Some(value) => value
            .as_dict()
            .map(vm_value_dict_to_json)
            .map(Some)
            .ok_or_else(|| {
                VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                    "{field}: expected a JSON Schema object"
                ))))
            }),
    }
}

pub(super) fn output_format_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(std::sync::Arc::from(message.into())))
}

pub(super) fn unsupported_option_error(option: &str, provider: &str, model: &str) -> VmError {
    VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
        "option `{option}` is not supported by `{model}` (provider `{provider}`). See `harn providers matrix` for compatibility."
    ))))
}

pub(super) fn option_is_enabled(options: Option<&BTreeMap<String, VmValue>>, key: &str) -> bool {
    options
        .and_then(|o| o.get(key))
        .is_some_and(|value| value.is_truthy())
}

pub(super) fn parse_output_format_kind(raw: &str) -> Result<&'static str, VmError> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "text" | "none" | "off" => Ok("text"),
        "json" | "json_object" => Ok("json_object"),
        "json_schema" | "schema" => Ok("json_schema"),
        other => Err(output_format_error(format!(
            "output_format.kind: expected \"text\" | \"json_object\" | \"json_schema\", got \"{other}\""
        ))),
    }
}

pub(super) fn parse_output_format_option(
    options: Option<&BTreeMap<String, VmValue>>,
    legacy_response_format: Option<&str>,
    legacy_json_schema: Option<&serde_json::Value>,
) -> Result<crate::llm::api::OutputFormat, VmError> {
    use crate::llm::api::OutputFormat;

    let Some(raw) = options.and_then(|o| o.get("output_format")) else {
        if let Some(schema) = legacy_json_schema {
            return Ok(OutputFormat::JsonSchema {
                schema: schema.clone(),
                strict: true,
            });
        }
        return match legacy_response_format {
            Some("json") | Some("json_object") => Ok(OutputFormat::JsonObject),
            Some("text") | None => Ok(OutputFormat::Text),
            Some(other) => Err(output_format_error(format!(
                "response_format: expected \"json\", \"json_object\", or \"text\", got \"{other}\""
            ))),
        };
    };

    match raw {
        VmValue::Nil => Ok(OutputFormat::Text),
        VmValue::String(kind) => match parse_output_format_kind(kind)? {
            "text" => Ok(OutputFormat::Text),
            "json_object" => Ok(OutputFormat::JsonObject),
            "json_schema" => {
                let Some(schema) = legacy_json_schema else {
                    return Err(output_format_error(
                        "output_format: kind \"json_schema\" requires a `schema` field",
                    ));
                };
                Ok(OutputFormat::JsonSchema {
                    schema: schema.clone(),
                    strict: true,
                })
            }
            _ => unreachable!(),
        },
        VmValue::Dict(d) => {
            let kind_raw = d
                .get("kind")
                .map(|value| value.display())
                .unwrap_or_else(|| "text".to_string());
            match parse_output_format_kind(&kind_raw)? {
                "text" => Ok(OutputFormat::Text),
                "json_object" => Ok(OutputFormat::JsonObject),
                "json_schema" => {
                    let schema = parse_schema_value(
                        d.get("schema").or_else(|| d.get("json_schema")),
                        "output_format.schema",
                    )?
                    .ok_or_else(|| {
                        output_format_error(
                            "output_format: kind \"json_schema\" requires a `schema` field",
                        )
                    })?;
                    let strict = d.get("strict").map(VmValue::is_truthy).unwrap_or(true);
                    Ok(OutputFormat::JsonSchema { schema, strict })
                }
                _ => unreachable!(),
            }
        }
        _ => Err(output_format_error(
            "output_format: expected string or dict",
        )),
    }
}

pub(super) fn validate_output_format_supported(
    output_format: &crate::llm::api::OutputFormat,
    provider: &str,
    model: &str,
    caps: &crate::llm::capabilities::Capabilities,
) -> Result<(), VmError> {
    use crate::llm::api::OutputFormat;

    match output_format {
        OutputFormat::Text => Ok(()),
        _ if provider == "mock" => Ok(()),
        OutputFormat::JsonObject => {
            if caps.structured_output.is_some() {
                Ok(())
            } else {
                Err(unsupported_option_error("output_format", provider, model))
            }
        }
        OutputFormat::JsonSchema { .. } => {
            match caps.structured_output.as_deref() {
                Some("native" | "tool_use" | "format_kw") => Ok(()),
                Some(other) => Err(output_format_error(format!(
                    "output_format: provider \"{provider}\" model \"{model}\" declares unsupported structured_output strategy \"{other}\""
                ))),
                None => Err(unsupported_option_error("output_format", provider, model)),
            }
        }
    }
}
