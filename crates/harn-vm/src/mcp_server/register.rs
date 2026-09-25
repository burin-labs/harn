use std::cell::RefCell;
use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use crate::mcp_progress::{current_context as current_progress_context, is_valid_progress_token};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::defs::{
    McpCompletionSource, McpPromptArgDef, McpPromptDef, McpResourceDef, McpResourceTemplateDef,
    McpServerMetadata,
};

thread_local! {
    /// Stores the tool registry set by `mcp_tools` / `mcp_serve`.
    static MCP_SERVE_REGISTRY: RefCell<Option<VmValue>> = const { RefCell::new(None) };
    /// Static resources registered by `mcp_resource`.
    static MCP_SERVE_RESOURCES: RefCell<Vec<McpResourceDef>> = const { RefCell::new(Vec::new()) };
    /// Resource templates registered by `mcp_resource_template`.
    static MCP_SERVE_RESOURCE_TEMPLATES: RefCell<Vec<McpResourceTemplateDef>> = const { RefCell::new(Vec::new()) };
    /// Prompts registered by `mcp_prompt`.
    static MCP_SERVE_PROMPTS: RefCell<Vec<McpPromptDef>> = const { RefCell::new(Vec::new()) };
    /// Optional metadata registered by `mcp_server_metadata`.
    static MCP_SERVE_METADATA: RefCell<Option<McpServerMetadata>> = const { RefCell::new(None) };
    /// A failed declaration invalidates the candidate even if the script catches the error.
    static MCP_SERVE_REJECTION: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn reject_failed_registration(result: Result<VmValue, VmError>) -> Result<VmValue, VmError> {
    if let Err(error) = &result {
        MCP_SERVE_REJECTION.with(|cell| {
            let mut rejection = cell.borrow_mut();
            if rejection.is_none() {
                *rejection = Some(error.to_string());
            }
        });
    }
    result
}

fn register_fail_closed<F>(vm: &mut Vm, method: &str, declaration: F)
where
    F: Fn(&[VmValue], &mut String) -> Result<VmValue, VmError> + Send + Sync + 'static,
{
    vm.register_capability_method(
        harn_builtin_meta::CapabilityId::Tools,
        method,
        move |args, output| reject_failed_registration(declaration(args, output)),
    );
}

pub fn take_mcp_serve_rejection() -> Option<String> {
    MCP_SERVE_REJECTION.with(|cell| cell.borrow_mut().take())
}

/// Discard a prior script's unpublished declarations before executing another.
/// Serving adapters consume the same slots after successful publication.
pub fn reset_mcp_serve_publication() {
    let _ = take_mcp_serve_registry();
    let _ = take_mcp_serve_resources();
    let _ = take_mcp_serve_resource_templates();
    let _ = take_mcp_serve_prompts();
    let _ = take_mcp_serve_metadata();
    let _ = take_mcp_serve_rejection();
}

/// Register all MCP server builtins on a VM.
pub fn register_mcp_server_builtins(vm: &mut Vm) {
    fn register_tools_impl(args: &[VmValue]) -> Result<VmValue, VmError> {
        let registry = args.first().cloned().ok_or_else(|| {
            VmError::Runtime("mcp_tools: requires a tool_registry argument".into())
        })?;
        if let VmValue::Dict(d) = &registry {
            match d.get("_type") {
                Some(VmValue::String(t)) if &**t == "tool_registry" => {}
                _ => {
                    return Err(VmError::Runtime(
                        "mcp_tools: argument must be a tool registry (created with tool_registry())"
                            .into(),
                    ));
                }
            }
        } else {
            return Err(VmError::Runtime(
                "mcp_tools: argument must be a tool registry".into(),
            ));
        }
        crate::tool_registry::tool_registry_catalog(&registry)?;
        MCP_SERVE_REGISTRY.with(|cell| {
            *cell.borrow_mut() = Some(registry);
        });
        Ok(VmValue::Nil)
    }

    register_fail_closed(vm, "mcp_tools", |args, _out| register_tools_impl(args));

    // mcp_server_metadata({name?, version?, instructions?}) -> nil
    register_fail_closed(vm, "mcp_server_metadata", |args, _out| {
        let dict = match args.first() {
            Some(VmValue::Dict(d)) => d,
            _ => {
                return Err(VmError::Runtime(
                    "mcp_server_metadata: argument must be a dict".into(),
                ));
            }
        };
        closed_fields(
            dict,
            &["name", "version", "instructions"],
            "mcp_server_metadata",
        )?;

        let metadata = McpServerMetadata {
            name: optional_non_empty_string(dict, "name", "mcp_server_metadata")?,
            version: optional_non_empty_string(dict, "version", "mcp_server_metadata")?,
            instructions: optional_non_empty_string(dict, "instructions", "mcp_server_metadata")?,
        };
        if metadata == McpServerMetadata::default() {
            return Err(VmError::Runtime(
                "mcp_server_metadata: at least one of name, version, or instructions is required"
                    .into(),
            ));
        }

        MCP_SERVE_METADATA.with(|cell| {
            *cell.borrow_mut() = Some(metadata);
        });
        Ok(VmValue::Nil)
    });

    // mcp_resource({uri, name, text, description?, mime_type?, meta?}) -> nil
    register_fail_closed(vm, "mcp_resource", |args, _out| {
        let dict = match args.first() {
            Some(VmValue::Dict(d)) => d,
            _ => {
                return Err(VmError::Runtime(
                    "mcp_resource: argument must be a dict with {uri, name, text}".into(),
                ));
            }
        };
        closed_fields(
            dict,
            &[
                "uri",
                "name",
                "title",
                "description",
                "mime_type",
                "meta",
                "text",
            ],
            "mcp_resource",
        )?;

        let uri = required_non_empty_string(dict, "uri", "mcp_resource")?;
        url::Url::parse(&uri)
            .map_err(|error| VmError::Runtime(format!("mcp_resource: invalid 'uri': {error}")))?;
        let name = required_non_empty_string(dict, "name", "mcp_resource")?;
        let title = optional_non_empty_string(dict, "title", "mcp_resource")?;
        let description = optional_non_empty_string(dict, "description", "mcp_resource")?;
        let mime_type = optional_non_empty_string(dict, "mime_type", "mcp_resource")?;
        let meta = match dict.get("meta") {
            Some(VmValue::Dict(value)) => Some(super::convert::vm_value_to_json(&VmValue::Dict(
                value.clone(),
            ))),
            Some(VmValue::Nil) | None => None,
            Some(_) => {
                return Err(VmError::Runtime(
                    "mcp_resource: 'meta' must be a dict".into(),
                ));
            }
        };
        let text = required_string(dict, "text", "mcp_resource")?;

        MCP_SERVE_RESOURCES.with(|cell| {
            let mut resources = cell.borrow_mut();
            if resources.iter().any(|resource| resource.uri == uri) {
                return Err(VmError::Runtime(format!(
                    "mcp_resource: duplicate URI '{uri}'"
                )));
            }
            resources.push(McpResourceDef {
                uri,
                name,
                title,
                description,
                mime_type,
                meta,
                text,
            });
            Ok::<(), VmError>(())
        })?;

        Ok(VmValue::Nil)
    });

    // mcp_resource_template({uri_template, name, handler, description?, mime_type?}) -> nil
    // The handler receives a dict of URI template arguments and returns a string.
    register_fail_closed(vm, "mcp_resource_template", |args, _out| {
        let dict = match args.first() {
            Some(VmValue::Dict(d)) => d,
            _ => {
                return Err(VmError::Runtime(
                    "mcp_resource_template: argument must be a dict".into(),
                ));
            }
        };
        closed_fields(
            dict,
            &[
                "uri_template",
                "name",
                "title",
                "description",
                "mime_type",
                "handler",
                "completions",
                "suggestions",
            ],
            "mcp_resource_template",
        )?;

        let uri_template =
            required_non_empty_string(dict, "uri_template", "mcp_resource_template")?;
        let name = required_non_empty_string(dict, "name", "mcp_resource_template")?;
        let title = optional_non_empty_string(dict, "title", "mcp_resource_template")?;
        let description = optional_non_empty_string(dict, "description", "mcp_resource_template")?;
        let mime_type = optional_non_empty_string(dict, "mime_type", "mcp_resource_template")?;
        let handler = match dict.get("handler") {
            Some(VmValue::Closure(c)) => (**c).clone(),
            _ => {
                return Err(VmError::Runtime(
                    "mcp_resource_template: 'handler' closure is required".into(),
                ));
            }
        };
        if dict.get("completions").is_some() && dict.get("suggestions").is_some() {
            return Err(VmError::Runtime(
                "mcp_resource_template: declare either 'completions' or 'suggestions', not both"
                    .into(),
            ));
        }
        let completions = completion_sources_from_dict(
            dict.get("completions").or_else(|| dict.get("suggestions")),
        )?;
        let variables = super::uri::validated_uri_template_variables(&uri_template)
            .map_err(|error| VmError::Runtime(format!("mcp_resource_template: {error}")))?;
        for name in completions.keys() {
            if !variables.contains(name) {
                return Err(VmError::Runtime(format!(
                        "mcp_resource_template: completion variable '{name}' is not declared by uri_template"
                    )));
            }
        }

        MCP_SERVE_RESOURCE_TEMPLATES.with(|cell| {
            let mut templates = cell.borrow_mut();
            if templates
                .iter()
                .any(|template| template.uri_template == uri_template)
            {
                return Err(VmError::Runtime(format!(
                    "mcp_resource_template: duplicate URI template '{uri_template}'"
                )));
            }
            templates.push(McpResourceTemplateDef {
                uri_template,
                name,
                title,
                description,
                mime_type,
                completions,
                handler,
            });
            Ok::<(), VmError>(())
        })?;

        Ok(VmValue::Nil)
    });

    // mcp_prompt({name, handler, description?, arguments?}) -> nil
    register_fail_closed(vm, "mcp_prompt", |args, _out| {
        let dict = match args.first() {
            Some(VmValue::Dict(d)) => d,
            _ => {
                return Err(VmError::Runtime(
                    "mcp_prompt: argument must be a dict with {name, handler}".into(),
                ));
            }
        };
        closed_fields(
            dict,
            &["name", "title", "description", "handler", "arguments"],
            "mcp_prompt",
        )?;

        let name = required_non_empty_string(dict, "name", "mcp_prompt")?;
        let title = optional_non_empty_string(dict, "title", "mcp_prompt")?;
        let description = optional_non_empty_string(dict, "description", "mcp_prompt")?;

        let handler = match dict.get("handler") {
            Some(VmValue::Closure(c)) => (**c).clone(),
            _ => {
                return Err(VmError::Runtime(
                    "mcp_prompt: 'handler' closure is required".into(),
                ));
            }
        };

        let arguments = prompt_arguments(dict)?;

        MCP_SERVE_PROMPTS.with(|cell| {
            let mut prompts = cell.borrow_mut();
            if prompts.iter().any(|prompt| prompt.name == name) {
                return Err(VmError::Runtime(format!(
                    "mcp_prompt: duplicate name '{name}'"
                )));
            }
            prompts.push(McpPromptDef {
                name,
                title,
                description,
                arguments,
                handler,
            });
            Ok::<(), VmError>(())
        })?;

        Ok(VmValue::Nil)
    });

    // mcp_elicit({message, requestedSchema}) -> {action, content?}
    //
    // Return a stable input_required round and resolve the client response
    // when the original handler is re-entered.
    //
    // The client may respond with one of:
    //   - {action: "accept", content: <validated against requestedSchema>}
    //   - {action: "decline"}
    //   - {action: "cancel"}
    //
    // Spec: https://modelcontextprotocol.io/specification/2026-07-28/client/elicitation
    vm.register_async_capability_method(
        harn_builtin_meta::CapabilityId::Tools,
        "mcp_elicit",
        |_ctx, args| async move {
            let dict = match args.first() {
                Some(VmValue::Dict(d)) => d.clone(),
                _ => {
                    return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                        "mcp_elicit: argument must be a dict with {message, requestedSchema}",
                    ))));
                }
            };
            let message = dict.get("message").map(VmValue::display).ok_or_else(|| {
                VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    "mcp_elicit: 'message' is required",
                )))
            })?;
            let requested_schema = dict.get("requestedSchema").or_else(|| dict.get("schema"));
            let requested_schema = requested_schema.ok_or_else(|| {
                VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    "mcp_elicit: 'requestedSchema' is required",
                )))
            })?;
            let requested_schema_json: JsonValue = crate::mcp::vm_value_to_serde(requested_schema);

            crate::mcp_elicit::elicit_form(message, requested_schema_json)
        },
    );

    // Ask the MCP client for roots through a stable input_required round.
    vm.register_async_capability_method(
        harn_builtin_meta::CapabilityId::Tools,
        "mcp_client_roots",
        |_ctx, args| async move {
            if !args.is_empty() {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    "mcp_client_roots: takes no arguments",
                ))));
            }
            crate::mcp_client_roots::request_client_roots().await
        },
    );
    // mcp_report_progress(progress, opts?) -> bool
    //
    // Emit a `notifications/progress` notification for the in-flight
    // tool call. Returns `true` when the notification was sent and
    // `false` when it was dropped (no client opt-in via
    // `_meta.progressToken`, or progress would not strictly increase).
    //
    // `opts` is an optional dict supporting:
    //   - `total`: optional ceiling so the client can render a bar
    //   - `message`: human-readable status string
    //   - `token`: override the ambient request token (rarely needed;
    //     useful only when manually fanning out progress for nested work)
    //
    // Spec:
    //   https://modelcontextprotocol.io/specification/2026-07-28/basic/utilities/progress
    vm.register_capability_method(
        harn_builtin_meta::CapabilityId::Tools,
        "mcp_report_progress",
        |args, _out| {
            let progress = match args.first() {
                Some(value) => coerce_progress_number(value).ok_or_else(|| {
                    VmError::Runtime(format!(
                        "mcp_report_progress: 'progress' must be a number (got {})",
                        value.display()
                    ))
                })?,
                None => {
                    return Err(VmError::Runtime(
                        "mcp_report_progress: 'progress' is required".into(),
                    ));
                }
            };

            let mut total: Option<f64> = None;
            let mut message: Option<String> = None;
            let mut explicit_token: Option<JsonValue> = None;
            if let Some(VmValue::Dict(opts)) = args.get(1) {
                if let Some(value) = opts.get("total") {
                    match value {
                        VmValue::Nil => {}
                        other => {
                            total = Some(coerce_progress_number(other).ok_or_else(|| {
                                VmError::Runtime(format!(
                                    "mcp_report_progress: 'total' must be a number (got {})",
                                    other.display()
                                ))
                            })?);
                        }
                    }
                }
                if let Some(value) = opts.get("message") {
                    match value {
                        VmValue::String(s) => message = Some(s.to_string()),
                        VmValue::Nil => {}
                        other => message = Some(other.display()),
                    }
                }
                if let Some(value) = opts.get("token") {
                    let candidate = crate::mcp::vm_value_to_serde(value);
                    if !is_valid_progress_token(&candidate) {
                        return Err(VmError::Runtime(
                            "mcp_report_progress: 'token' must be a string or number".into(),
                        ));
                    }
                    explicit_token = Some(candidate);
                }
            }

            let Some(ctx) = current_progress_context() else {
                // No active per-call context — either the client didn't opt
                // in with `_meta.progressToken` or the call originates
                // outside an MCP tool handler. Either way, silently drop:
                // scripts can sprinkle this builtin liberally without
                // checking.
                return Ok(VmValue::Bool(false));
            };

            let sent = if let Some(token) = explicit_token {
                ctx.bus.report(&token, progress, total, message)
            } else {
                ctx.report(progress, total, message)
            };
            Ok(VmValue::Bool(sent))
        },
    );
}

fn completion_sources_from_dict(
    value: Option<&VmValue>,
) -> Result<BTreeMap<String, McpCompletionSource>, VmError> {
    let sources = match value {
        None | Some(VmValue::Nil) => return Ok(BTreeMap::new()),
        Some(VmValue::Dict(sources)) => sources,
        Some(_) => {
            return Err(VmError::Runtime(
                "mcp_resource_template: 'completions' must be an object".into(),
            ));
        }
    };
    let mut parsed = BTreeMap::new();
    for (name, value) in sources.iter() {
        if name.trim().is_empty() {
            return Err(VmError::Runtime(
                "mcp_resource_template: completion variable must be non-empty".into(),
            ));
        }
        let label = format!("mcp_resource_template: completions.{name}");
        parsed.insert(
            name.to_string(),
            completion_source_from_value(value, &label)?,
        );
    }
    Ok(parsed)
}

fn completion_source_from_argument_dict(
    dict: &crate::value::DictMap,
    label: &str,
) -> Result<Option<McpCompletionSource>, VmError> {
    let mut source = McpCompletionSource::default();
    for key in ["suggestions", "completions", "values"] {
        if let Some(value) = dict.get(key) {
            source.values.extend(completion_values_from_value(
                value,
                &format!("{label}.{key}"),
            )?);
        }
    }
    for key in ["complete", "completion", "handler"] {
        match dict.get(key) {
            None | Some(VmValue::Nil) => {}
            Some(VmValue::Closure(closure)) if source.handler.is_none() => {
                source.handler = Some((**closure).clone());
            }
            Some(VmValue::Closure(_)) => {
                return Err(VmError::Runtime(format!(
                    "{label}: only one completion handler may be declared"
                )));
            }
            Some(_) => {
                return Err(VmError::Runtime(format!(
                    "{label}.{key} must be a handler closure"
                )));
            }
        }
    }
    Ok((!source.values.is_empty() || source.handler.is_some()).then_some(source))
}

fn completion_source_from_value(
    value: &VmValue,
    label: &str,
) -> Result<McpCompletionSource, VmError> {
    match value {
        VmValue::Closure(closure) => Ok(McpCompletionSource {
            values: Vec::new(),
            handler: Some((**closure).clone()),
        }),
        VmValue::Dict(dict) => {
            closed_fields(
                dict,
                &[
                    "suggestions",
                    "completions",
                    "values",
                    "complete",
                    "completion",
                    "handler",
                ],
                label,
            )?;
            completion_source_from_argument_dict(dict, label)?
                .ok_or_else(|| VmError::Runtime(format!("{label} declares no completion source")))
        }
        _ => {
            let values = completion_values_from_value(value, label)?;
            if values.is_empty() {
                return Err(VmError::Runtime(format!(
                    "{label} declares no completion source"
                )));
            }
            Ok(McpCompletionSource {
                values,
                handler: None,
            })
        }
    }
}

fn completion_values_from_value(value: &VmValue, label: &str) -> Result<Vec<String>, VmError> {
    match value {
        VmValue::List(items) => items
            .iter()
            .enumerate()
            .map(|(index, value)| match value {
                VmValue::String(value) if !value.trim().is_empty() => Ok(value.to_string()),
                _ => Err(VmError::Runtime(format!(
                    "{label}[{index}] must be a non-empty string"
                ))),
            })
            .collect(),
        VmValue::String(value) if !value.trim().is_empty() => Ok(vec![value.to_string()]),
        _ => Err(VmError::Runtime(format!(
            "{label} must be a non-empty string or a list of non-empty strings"
        ))),
    }
}

fn coerce_progress_number(value: &VmValue) -> Option<f64> {
    match value {
        VmValue::Int(n) => Some(*n as f64),
        VmValue::Float(n) => Some(*n),
        _ => None,
    }
}

// Thread-local accessors used by the CLI after pipeline execution.

pub fn take_mcp_serve_registry() -> Option<VmValue> {
    MCP_SERVE_REGISTRY.with(|cell| cell.borrow_mut().take())
}

pub fn take_mcp_serve_resources() -> Vec<McpResourceDef> {
    MCP_SERVE_RESOURCES.with(|cell| cell.borrow_mut().drain(..).collect())
}

pub fn take_mcp_serve_resource_templates() -> Vec<McpResourceTemplateDef> {
    MCP_SERVE_RESOURCE_TEMPLATES.with(|cell| cell.borrow_mut().drain(..).collect())
}

pub fn take_mcp_serve_prompts() -> Vec<McpPromptDef> {
    MCP_SERVE_PROMPTS.with(|cell| cell.borrow_mut().drain(..).collect())
}

pub fn take_mcp_serve_metadata() -> Option<McpServerMetadata> {
    MCP_SERVE_METADATA.with(|cell| cell.borrow_mut().take())
}

fn closed_fields(
    dict: &crate::value::DictMap,
    allowed: &[&str],
    label: &str,
) -> Result<(), VmError> {
    for (key, _) in dict.iter() {
        let key = key.to_string();
        if !allowed.contains(&key.as_str()) {
            return Err(VmError::Runtime(format!("{label}: unknown field '{key}'")));
        }
    }
    Ok(())
}

fn required_string(
    dict: &crate::value::DictMap,
    key: &str,
    builtin: &str,
) -> Result<String, VmError> {
    match dict.get(key) {
        Some(VmValue::String(value)) => Ok(value.to_string()),
        None | Some(VmValue::Nil) => {
            Err(VmError::Runtime(format!("{builtin}: '{key}' is required")))
        }
        Some(_) => Err(VmError::Runtime(format!(
            "{builtin}: '{key}' must be a string"
        ))),
    }
}

fn required_non_empty_string(
    dict: &crate::value::DictMap,
    key: &str,
    builtin: &str,
) -> Result<String, VmError> {
    let value = required_string(dict, key, builtin)?;
    if value.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{builtin}: '{key}' must be a non-empty string"
        )));
    }
    Ok(value)
}

fn prompt_arguments(dict: &crate::value::DictMap) -> Result<Option<Vec<McpPromptArgDef>>, VmError> {
    let values = match dict.get("arguments") {
        None | Some(VmValue::Nil) => return Ok(None),
        Some(VmValue::List(values)) => values,
        Some(_) => {
            return Err(VmError::Runtime(
                "mcp_prompt: 'arguments' must be a list".into(),
            ));
        }
    };
    let mut arguments = Vec::with_capacity(values.len());
    let mut names = std::collections::BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let VmValue::Dict(argument) = value else {
            return Err(VmError::Runtime(format!(
                "mcp_prompt: 'arguments[{index}]' must be an object"
            )));
        };
        let label = format!("mcp_prompt: arguments[{index}]");
        closed_fields(
            argument,
            &[
                "name",
                "title",
                "description",
                "required",
                "suggestions",
                "completions",
                "values",
                "complete",
                "completion",
                "handler",
            ],
            &label,
        )?;
        let name = required_non_empty_string(argument, "name", &label)?;
        if !names.insert(name.clone()) {
            return Err(VmError::Runtime(format!(
                "mcp_prompt: duplicate argument name '{name}'"
            )));
        }
        let required = match argument.get("required") {
            None | Some(VmValue::Nil) => false,
            Some(VmValue::Bool(value)) => *value,
            Some(_) => {
                return Err(VmError::Runtime(format!(
                    "{label}: 'required' must be a boolean"
                )));
            }
        };
        arguments.push(McpPromptArgDef {
            name,
            title: optional_non_empty_string(argument, "title", &label)?,
            description: optional_non_empty_string(argument, "description", &label)?,
            required,
            completion: completion_source_from_argument_dict(argument, &label)?,
        });
    }
    Ok((!arguments.is_empty()).then_some(arguments))
}

fn optional_non_empty_string(
    dict: &crate::value::DictMap,
    key: &str,
    builtin: &str,
) -> Result<Option<String>, VmError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(value)) if !value.trim().is_empty() => Ok(Some(value.to_string())),
        Some(VmValue::String(_)) => Err(VmError::Runtime(format!(
            "{builtin}: '{key}' must be a non-empty string"
        ))),
        Some(_) => Err(VmError::Runtime(format!(
            "{builtin}: '{key}' must be a string"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{intern_key, DictMap};

    #[test]
    fn resource_metadata_never_coerces_a_non_string() {
        let mut resource = DictMap::new();
        resource.insert(intern_key("uri"), VmValue::Int(42));
        let error = required_non_empty_string(&resource, "uri", "mcp_resource")
            .expect_err("a numeric URI must not publish");
        assert!(error.to_string().contains("'uri' must be a string"));
        resource.put_str("uri", "docs://readme");
        resource.insert(intern_key("mime_type"), VmValue::Int(42));
        let error = optional_non_empty_string(&resource, "mime_type", "mcp_resource")
            .expect_err("a numeric MIME type must not publish");
        assert!(error.to_string().contains("'mime_type' must be a string"));
    }

    #[test]
    fn malformed_prompt_argument_refuses_the_whole_publication() {
        let mut prompt = DictMap::new();
        prompt.insert(
            intern_key("arguments"),
            VmValue::List(vec![VmValue::Int(7)].into()),
        );
        let error = prompt_arguments(&prompt)
            .err()
            .expect("malformed argument must not disappear");
        assert!(error
            .to_string()
            .contains("'arguments[0]' must be an object"));

        let mut argument = DictMap::new();
        argument.put_str("name", "target");
        argument.insert(intern_key("required"), VmValue::Int(1));
        prompt.insert(
            intern_key("arguments"),
            VmValue::List(vec![VmValue::dict(argument)].into()),
        );
        let error = prompt_arguments(&prompt)
            .err()
            .expect("required must be a boolean");
        assert!(error.to_string().contains("'required' must be a boolean"));

        let mut first = DictMap::new();
        first.put_str("name", "target");
        let mut duplicate = DictMap::new();
        duplicate.put_str("name", "target");
        prompt.insert(
            intern_key("arguments"),
            VmValue::List(vec![VmValue::dict(first), VmValue::dict(duplicate)].into()),
        );
        let error = prompt_arguments(&prompt)
            .err()
            .expect("duplicate names must not publish");
        assert!(error
            .to_string()
            .contains("duplicate argument name 'target'"));
    }

    #[test]
    fn completion_declarations_never_silently_disappear_or_coerce_values() {
        let mut completions = DictMap::new();
        completions.insert(intern_key("path"), VmValue::Int(7));
        let error = completion_sources_from_dict(Some(&VmValue::dict(completions)))
            .err()
            .expect("invalid completion value must not disappear");
        assert!(error.to_string().contains("completions.path must be"));

        let mut argument = DictMap::new();
        argument.put_str("name", "target");
        argument.insert(
            intern_key("values"),
            VmValue::List(vec![VmValue::Int(7)].into()),
        );
        let error = completion_source_from_argument_dict(&argument, "mcp_prompt: arguments[0]")
            .err()
            .expect("numeric completion choice must not become text");
        assert!(error
            .to_string()
            .contains("values[0] must be a non-empty string"));
    }
}
