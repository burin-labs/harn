use super::*;

/// Read the `provider_overrides.force_native_tool_search` escape hatch
/// (bool). Set to true when a user is pointed at a proxied OpenAI-compat
/// endpoint (self-hosted router, enterprise gateway) whose model ID
/// Harn cannot parse but that is known to forward `tool_search` +
/// `defer_loading` unchanged.
pub(super) fn provider_overrides_force_native(
    options: Option<&crate::value::DictMap>,
    provider: &str,
) -> bool {
    let Some(options) = options else { return false };
    let Some(VmValue::Dict(namespaced)) = options.get("provider_options") else {
        return false;
    };
    let Some(VmValue::Dict(overrides)) = namespaced.get(provider) else {
        return false;
    };
    matches!(
        overrides.get(crate::llm::provider::FORCE_NATIVE_TOOL_SEARCH_OVERRIDE),
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
    options: Option<&crate::value::DictMap>,
) -> Result<crate::llm::api::LlmApiMode, VmError> {
    let Some(raw) = options.and_then(|o| o.get("api_mode")) else {
        return Ok(crate::llm::api::LlmApiMode::ChatCompletions);
    };
    match raw {
        VmValue::Nil => Ok(crate::llm::api::LlmApiMode::ChatCompletions),
        VmValue::String(value) => match value.as_str() {
            "chat_completions" => Ok(crate::llm::api::LlmApiMode::ChatCompletions),
            "responses" => Ok(crate::llm::api::LlmApiMode::Responses),
            other => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!(
                    "api_mode: expected \"chat_completions\" or \"responses\", got \"{other}\""
                ),
            )))),
        },
        other => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("api_mode: expected a string, got {}", other.type_name()),
        )))),
    }
}

pub(super) fn enforce_responses_provider_gate(
    mode: crate::llm::api::LlmApiMode,
    provider: &str,
) -> bool {
    mode == crate::llm::api::LlmApiMode::Responses
        && provider != "openai"
        && provider != "mock"
        && !crate::llm_config::provider_has_feature(provider, "responses_api")
}

pub(super) fn parse_provider_tools_option(
    options: Option<&crate::value::DictMap>,
) -> Result<Vec<serde_json::Value>, VmError> {
    let Some(raw) = options.and_then(|o| o.get("provider_tools")) else {
        return Ok(Vec::new());
    };
    match raw {
        VmValue::Nil | VmValue::Bool(false) => Ok(Vec::new()),
        VmValue::Dict(_) => Ok(vec![vm_value_to_json(raw)]),
        VmValue::List(list) => list
            .iter()
            .map(|value| match value {
                VmValue::String(kind) => Ok(serde_json::json!({"type": kind.as_str()})),
                VmValue::Dict(_) => Ok(vm_value_to_json(value)),
                other => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    format!(
                        "provider_tools: expected each entry to be a dict or string, got {}",
                        other.type_name()
                    ),
                )))),
            })
            .collect(),
        other => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!(
                "provider_tools: expected a list or dict, got {}",
                other.type_name()
            ),
        )))),
    }
}

pub(super) fn opt_bool_field(
    options: Option<&crate::value::DictMap>,
    key: &str,
) -> Result<Option<bool>, VmError> {
    match options.and_then(|o| o.get(key)) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("{key}: expected a bool, got {}", other.type_name()),
        )))),
    }
}

pub(super) fn opt_responses_store_field(
    options: Option<&crate::value::DictMap>,
) -> Result<Option<bool>, VmError> {
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
        Some(value @ VmValue::Dict(_)) => {
            let mut schema = vm_value_to_json(value);
            crate::schema::normalize_json_schema_type_names(&mut schema);
            Ok(Some(schema))
        }
        Some(_) => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("{field}: expected a JSON Schema object"),
        )))),
    }
}

pub(super) fn output_format_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(message.into())))
}

pub(super) fn unsupported_option_error(option: &str, provider: &str, model: &str) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
        "option `{option}` is not supported by `{model}` (provider `{provider}`). See `harn provider catalog matrix` for compatibility."
    ))))
}

pub(super) fn option_is_enabled(options: Option<&crate::value::DictMap>, key: &str) -> bool {
    options
        .and_then(|o| o.get(key))
        .is_some_and(|value| value.is_truthy())
}

/// The parsed single `output` contract key.
///
/// Grammar (the ONE spelling for structured output):
/// * absent / nil          -> plain text
/// * "text"                -> plain text (explicit)
/// * "json"                -> provider JSON mode, no schema
/// * <schema dict>         -> schema-constrained output (strict)
/// * {schema, strict?, validation?, stream_abort?} -> full config form
///
/// The config form is recognized by the presence of a `schema` key; any other
/// dict is treated as the JSON Schema itself.
pub(super) struct ParsedOutput {
    pub format: crate::llm::api::OutputFormat,
    pub validation: Option<String>,
    /// Explicit `stream_abort` override; `None` defaults to
    /// "abort when a schema is present".
    pub stream_abort: Option<bool>,
}

pub(super) fn parse_output_option(
    options: Option<&crate::value::DictMap>,
) -> Result<ParsedOutput, VmError> {
    use crate::llm::api::OutputFormat;

    let text = |validation: Option<String>, stream_abort: Option<bool>| ParsedOutput {
        format: OutputFormat::Text,
        validation,
        stream_abort,
    };
    let Some(raw) = options.and_then(|o| o.get("output")) else {
        return Ok(text(None, None));
    };
    match raw {
        VmValue::Nil => Ok(text(None, None)),
        VmValue::String(kind) => match kind.as_str() {
            "text" => Ok(text(None, None)),
            "json" => Ok(ParsedOutput {
                format: OutputFormat::JsonObject,
                validation: None,
                stream_abort: None,
            }),
            other => Err(output_format_error(format!(
                "output: expected \"text\", \"json\", a schema, or {{schema, ...}}, got \"{other}\""
            ))),
        },
        VmValue::Dict(d) if d.get("schema").is_some() => {
            let schema = match d.get("schema") {
                Some(VmValue::Nil) | None => None,
                other => parse_schema_value(other, "output.schema")?,
            };
            let strict = match d.get("strict") {
                None | Some(VmValue::Nil) => true,
                Some(VmValue::Bool(value)) => *value,
                Some(other) => {
                    return Err(output_format_error(format!(
                        "output.strict: expected a bool, got {}",
                        other.type_name()
                    )))
                }
            };
            let validation = match d.get("validation") {
                None | Some(VmValue::Nil) => None,
                Some(VmValue::String(value)) => Some(value.to_string()),
                Some(other) => {
                    return Err(output_format_error(format!(
                        "output.validation: expected a string, got {}",
                        other.type_name()
                    )))
                }
            };
            let stream_abort = match d.get("stream_abort") {
                None | Some(VmValue::Nil) => None,
                Some(VmValue::Bool(value)) => Some(*value),
                Some(other) => {
                    return Err(output_format_error(format!(
                        "output.stream_abort: expected a bool, got {}",
                        other.type_name()
                    )))
                }
            };
            match schema {
                Some(schema) => Ok(ParsedOutput {
                    format: OutputFormat::JsonSchema { schema, strict },
                    validation,
                    stream_abort,
                }),
                None => Err(output_format_error(
                    "output: the {schema, ...} form requires a non-nil `schema`",
                )),
            }
        }
        value @ VmValue::Dict(_) => {
            let schema =
                parse_schema_value(Some(value), "output")?.expect("dict schema parses to Some");
            Ok(ParsedOutput {
                format: OutputFormat::JsonSchema {
                    schema,
                    strict: true,
                },
                validation: None,
                stream_abort: None,
            })
        }
        other => Err(output_format_error(format!(
            "output: expected \"text\", \"json\", a schema, or {{schema, ...}}, got {}",
            other.type_name()
        ))),
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
                Err(unsupported_option_error("output", provider, model))
            }
        }
        OutputFormat::JsonSchema { .. } => {
            match caps.structured_output.as_deref() {
                Some("native" | "tool_use" | "format_kw") => Ok(()),
                Some(other) => Err(output_format_error(format!(
                    "output: provider \"{provider}\" model \"{model}\" declares unsupported structured_output strategy \"{other}\""
                ))),
                None => Err(unsupported_option_error("output", provider, model)),
            }
        }
    }
}

#[cfg(test)]
mod responses_provider_gate_tests {
    use super::enforce_responses_provider_gate;
    use crate::llm::api::LlmApiMode;

    #[test]
    fn responses_gate_preserves_openai_and_allows_feature_declared_gateways() {
        assert!(!enforce_responses_provider_gate(
            LlmApiMode::Responses,
            "openai"
        ));
        assert!(!enforce_responses_provider_gate(
            LlmApiMode::Responses,
            "vercel_ai_gateway"
        ));
        assert!(enforce_responses_provider_gate(
            LlmApiMode::Responses,
            "anthropic"
        ));
    }
}
