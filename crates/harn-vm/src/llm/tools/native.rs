use std::rc::Rc;

use super::json_schema::vm_build_json_schema;
use crate::value::{VmError, VmValue};

pub(crate) fn vm_tools_to_native(
    tools_val: &VmValue,
    provider: &str,
    model: &str,
) -> Result<Vec<serde_json::Value>, VmError> {
    // Accept either a tool_registry dict or a list of tool dicts.
    let tools_list = match tools_val {
        VmValue::Dict(dict) => match dict.get("tools") {
            Some(VmValue::List(list)) => list.as_ref().clone(),
            _ => Vec::new(),
        },
        VmValue::List(list) => list.as_ref().clone(),
        _ => {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tools must be a tool_registry or a list of tool definition dicts",
            ))));
        }
    };

    let mut native_tools = Vec::new();
    for tool in &tools_list {
        match tool {
            VmValue::Dict(entry) => {
                let name = entry
                    .get("name")
                    .map(|value| value.display())
                    .unwrap_or_default();
                let description = entry
                    .get("description")
                    .map(|value| value.display())
                    .unwrap_or_default();
                let params = entry.get("parameters").and_then(|value| value.as_dict());
                let output_schema = entry
                    .get("outputSchema")
                    .map(super::super::vm_value_to_json);
                let defer_loading = matches!(entry.get("defer_loading"), Some(VmValue::Bool(true)));
                // Optional `namespace: "crm"` groups deferred tools for
                // OpenAI's `tool_search` meta-tool. Provider-agnostic at
                // this layer; Anthropic simply ignores the field.
                let namespace = entry.get("namespace").and_then(|value| match value {
                    VmValue::String(string) if !string.is_empty() => Some(string.to_string()),
                    _ => None,
                });

                let input_schema = vm_build_json_schema(params);

                if super::super::provider::provider_uses_anthropic_native_tools(provider, model) {
                    let mut tool_json = serde_json::json!({
                        "name": name,
                        "description": description,
                        "input_schema": input_schema,
                    });
                    if let Some(output_schema) = output_schema {
                        tool_json["x-harn-output-schema"] = output_schema;
                    }
                    if defer_loading {
                        // Anthropic's tool-search docs: per-tool
                        // `defer_loading: true` keeps the schema out of
                        // the model's context until a `tool_search_tool_*`
                        // call surfaces it. The server expands the
                        // `tool_reference` blocks for us on subsequent
                        // turns; we just pass the flag through.
                        tool_json["defer_loading"] = serde_json::Value::Bool(true);
                    }
                    if let Some(ns) = namespace {
                        // Anthropic ignores `namespace` today — harmless
                        // passthrough keeps replay fidelity and lets a
                        // future Anthropic release pick it up without
                        // another round of schema plumbing.
                        tool_json["namespace"] = serde_json::Value::String(ns);
                    }
                    native_tools.push(tool_json);
                } else {
                    let mut tool_json = serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": name,
                            "description": description,
                            "parameters": input_schema,
                        }
                    });
                    if let Some(output_schema) = output_schema {
                        tool_json["function"]["x-harn-output-schema"] = output_schema;
                    }
                    if defer_loading {
                        // Record the flag on the Harn-side wrapper so
                        // native and client-executed tool-search paths
                        // can read it without re-walking the VmValue
                        // tree. Non-OpenAI OpenAI-compat providers that don't
                        // understand `defer_loading` today will return an
                        // error when the user explicitly requests
                        // tool_search — the capability gate in options.rs
                        // catches that before the payload ever reaches
                        // the provider.
                        tool_json["defer_loading"] = serde_json::Value::Bool(true);
                    }
                    if let Some(ns) = namespace {
                        // OpenAI's `tool_search` meta-tool groups
                        // deferred tools by namespace. Placed on the
                        // wrapper (alongside `type: "function"`) so the
                        // Responses API sees it next to `defer_loading`.
                        tool_json["namespace"] = serde_json::Value::String(ns);
                    }
                    native_tools.push(tool_json);
                }
            }
            VmValue::String(_) => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tools must be declared as tool definition dicts or a tool_registry",
                ))));
            }
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tools must contain only tool definition dicts",
                ))));
            }
        }
    }
    Ok(native_tools)
}

/// Return the names of all tools in `native_tools` that have the
/// `defer_loading: true` flag set. Used for pre-flight validation
/// (Anthropic rejects all-deferred tool lists with HTTP 400).
pub(crate) fn extract_deferred_tool_names(native_tools: &[serde_json::Value]) -> Vec<String> {
    native_tools
        .iter()
        .filter_map(|tool| {
            if tool.get("defer_loading").and_then(|value| value.as_bool()) == Some(true) {
                // Anthropic shape: `name` at top level.
                if let Some(name) = tool.get("name").and_then(|value| value.as_str()) {
                    return Some(name.to_string());
                }
                // OpenAI shape: `function.name`.
                if let Some(name) = tool
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(|value| value.as_str())
                {
                    return Some(name.to_string());
                }
            }
            None
        })
        .collect()
}

/// When tool_search resolves to native mode, prepend the server-side
/// tool-search meta-tool to the provider's tools array. See the docs at
/// <https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool>.
///
/// Variant strings: `"bm25"` or `"regex"`. Provider must be an Anthropic
/// Claude 4.0+ model at the model-capability level — the caller is
/// responsible for gating on that.
///
/// No-ops if `native_tools` is `None` (no tools passed = no search to
/// do). The meta-tool itself never has `defer_loading` — that's a hard
/// requirement of Anthropic's API and we match it here.
/// Native tool-search injection with an explicit wire shape + OpenAI
/// execution mode. `mode` is `"hosted"` or `"client"`; it only affects
/// the OpenAI Responses-API shape (Anthropic's server always runs the
/// search). `"hosted"` is the OpenAI default.
pub(crate) fn apply_tool_search_native_injection_typed(
    native_tools: &mut Option<Vec<serde_json::Value>>,
    shape: super::super::provider::NativeToolSearchShape,
    variant: &str,
    mode: &str,
) {
    use super::super::provider::NativeToolSearchShape;

    match shape {
        NativeToolSearchShape::Anthropic => {
            // Anthropic's documented versioned types. If/when Anthropic
            // issues a newer dated variant, we'll bump these constants
            // in lockstep; the short names (`bm25`/`regex`) stay stable
            // for users.
            let (type_name, tool_name) = match variant {
                "regex" => ("tool_search_tool_regex_20251119", "tool_search_tool_regex"),
                // "bm25" and anything else → default to bm25.
                _ => ("tool_search_tool_bm25_20251119", "tool_search_tool_bm25"),
            };
            let meta = serde_json::json!({
                "type": type_name,
                "name": tool_name,
            });
            prepend_meta_tool(native_tools, meta);
        }
        NativeToolSearchShape::OpenAi => {
            // OpenAI Responses-API shape. The meta-tool goes at the
            // front of the tools array and carries a `mode`
            // field ("hosted"/"client"). Deferred tools get
            // `defer_loading: true` set at the wrapper level in
            // `vm_tools_to_native`; the model sees only stub schemas
            // until a `tool_search_call` surfaces them. See
            // <https://developers.openai.com/api/docs/guides/tools-tool-search>.
            let resolved_mode = if mode == "client" { "client" } else { "hosted" };
            let mut meta = serde_json::json!({
                "type": "tool_search",
                "mode": resolved_mode,
            });
            // Collect any `namespace` values declared on user tools so
            // OpenAI can group deferred tools into searchable buckets.
            // Omit the field when no tool declared a namespace — keeps
            // the payload minimal for the common case.
            if let Some(tools) = native_tools.as_ref() {
                let mut namespaces: Vec<String> = tools.iter().filter_map(tool_namespace).collect();
                namespaces.sort();
                namespaces.dedup();
                if !namespaces.is_empty() {
                    meta["namespaces"] = serde_json::json!(namespaces);
                }
            }
            prepend_meta_tool(native_tools, meta);
        }
    }
}

fn prepend_meta_tool(native_tools: &mut Option<Vec<serde_json::Value>>, meta: serde_json::Value) {
    match native_tools {
        Some(list) => list.insert(0, meta),
        None => *native_tools = Some(vec![meta]),
    }
}

/// Extract the user-declared namespace from a native tool JSON, if any.
/// Anthropic shape keeps the field at the top level; OpenAI shape nests
/// it inside `function`. Either location is honoured.
pub(crate) fn tool_namespace(tool: &serde_json::Value) -> Option<String> {
    tool.get("namespace")
        .and_then(|value| value.as_str())
        .or_else(|| {
            tool.get("function")
                .and_then(|function| function.get("namespace"))
                .and_then(|value| value.as_str())
        })
        .map(|value| value.to_string())
}
