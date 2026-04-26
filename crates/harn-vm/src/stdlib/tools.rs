use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use crate::schema::json_to_vm_value;

thread_local! {
    /// The tool registry bound to the current execution scope. Populated
    /// by `agent_loop` at the start of its run and cleared on exit, or by
    /// `tool_bind(registry)` in tests and prompt-building code. Consumed
    /// by `tool_ref` / `tool_def` to resolve tool-name references without
    /// threading the registry through every call site.
    static CURRENT_TOOL_REGISTRY: RefCell<Option<VmValue>> = const { RefCell::new(None) };
}

/// Install a registry as the current tool registry for this thread.
/// Returns the previous binding so callers can restore it (RAII-style).
pub fn install_current_tool_registry(registry: Option<VmValue>) -> Option<VmValue> {
    CURRENT_TOOL_REGISTRY.with(|slot| slot.replace(registry))
}

/// Read the currently-bound tool registry, if any.
pub fn current_tool_registry() -> Option<VmValue> {
    CURRENT_TOOL_REGISTRY.with(|slot| slot.borrow().clone())
}

/// Clear the thread-local tool registry. Used to reset state between
/// tests.
pub fn clear_current_tool_registry() {
    CURRENT_TOOL_REGISTRY.with(|slot| *slot.borrow_mut() = None);
}

fn vm_find_tool_entry<'a>(
    registry: &'a BTreeMap<String, VmValue>,
    name: &str,
) -> Option<&'a BTreeMap<String, VmValue>> {
    let tools = vm_get_tools(registry);
    for tool in tools {
        if let VmValue::Dict(entry) = tool {
            if let Some(VmValue::String(entry_name)) = entry.get("name") {
                if &**entry_name == name {
                    return Some(entry);
                }
            }
        }
    }
    None
}

fn vm_registered_names(registry: &BTreeMap<String, VmValue>) -> Vec<String> {
    let mut names: Vec<String> = vm_get_tools(registry)
        .iter()
        .filter_map(|tool| match tool {
            VmValue::Dict(entry) => entry.get("name").and_then(|v| match v {
                VmValue::String(s) => Some(s.to_string()),
                _ => None,
            }),
            _ => None,
        })
        .collect();
    names.sort();
    names
}

fn vm_current_registry_dict(builtin: &str) -> Result<VmValue, VmError> {
    match current_tool_registry() {
        Some(value) => match &value {
            VmValue::Dict(map) => {
                vm_validate_registry(builtin, map)?;
                Ok(value)
            }
            _ => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "{builtin}: bound tool registry is not a dict"
            ))))),
        },
        None => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{builtin}: no tool registry bound to this scope. \
             Call tool_bind(registry) first, or invoke inside an agent_loop."
        ))))),
    }
}

pub(crate) fn register_tool_builtins(vm: &mut Vm) {
    vm.register_builtin("tool_registry", |_args, _out| {
        let mut registry = BTreeMap::new();
        registry.insert(
            "_type".to_string(),
            VmValue::String(Rc::from("tool_registry")),
        );
        registry.insert("tools".to_string(), VmValue::List(Rc::new(Vec::new())));
        Ok(VmValue::Dict(Rc::new(registry)))
    });

    vm.register_builtin("tool_list", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_list: requires a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_list", registry)?;

        let tools = vm_get_tools(registry);
        let mut result = Vec::new();
        for tool in tools {
            if let VmValue::Dict(entry) = tool {
                let mut desc = BTreeMap::new();
                for (key, value) in entry.iter() {
                    if key == "handler" {
                        continue;
                    }
                    desc.insert(key.clone(), value.clone());
                }
                result.push(VmValue::Dict(Rc::new(desc)));
            }
        }
        Ok(VmValue::List(Rc::new(result)))
    });

    vm.register_builtin("tool_find", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_find: requires registry and name",
            ))));
        }
        let registry = match &args[0] {
            VmValue::Dict(map) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_find: first argument must be a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_find", registry)?;

        let target_name = args[1].display();
        let tools = vm_get_tools(registry);

        for tool in tools {
            if let VmValue::Dict(entry) = tool {
                if let Some(VmValue::String(name)) = entry.get("name") {
                    if &**name == target_name.as_str() {
                        return Ok(tool.clone());
                    }
                }
            }
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("tool_select", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_select: requires registry and names list",
            ))));
        }
        let registry = match &args[0] {
            VmValue::Dict(map) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_select: first argument must be a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_select", registry)?;
        let names = match &args[1] {
            VmValue::List(list) => list
                .iter()
                .map(|value| value.display())
                .collect::<std::collections::BTreeSet<_>>(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_select: second argument must be a list of tool names",
                ))));
            }
        };

        let selected: Vec<VmValue> = vm_get_tools(registry)
            .iter()
            .filter(|tool| {
                tool.as_dict()
                    .and_then(|entry| entry.get("name"))
                    .map(|name| names.contains(&name.display()))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();

        let mut new_registry = (**registry).clone();
        new_registry.insert("tools".to_string(), VmValue::List(Rc::new(selected)));
        Ok(VmValue::Dict(Rc::new(new_registry)))
    });

    vm.register_builtin("tool_describe", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_describe: requires a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_describe", registry)?;

        let tools = vm_get_tools(registry);

        if tools.is_empty() {
            return Ok(VmValue::String(Rc::from("Available tools:\n(none)")));
        }

        let mut tool_infos: Vec<(String, String, String, String)> = Vec::new();
        for tool in tools {
            if let VmValue::Dict(entry) = tool {
                let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
                let description = entry
                    .get("description")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let params_str = vm_format_parameters(entry.get("parameters"));
                let returns_str = vm_format_schema(entry.get("outputSchema"));
                tool_infos.push((name, params_str, returns_str, description));
            }
        }

        tool_infos.sort_by(|a, b| a.0.cmp(&b.0));

        let mut lines = vec!["Available tools:".to_string()];
        for (name, params, returns, desc) in &tool_infos {
            lines.push(format!("- {name}({params}): {desc}"));
            if !returns.is_empty() {
                lines.push(format!("  returns {returns}"));
            }
        }

        Ok(VmValue::String(Rc::from(lines.join("\n"))))
    });

    vm.register_builtin("tool_remove", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_remove: requires registry and name",
            ))));
        }

        let registry = match &args[0] {
            VmValue::Dict(map) => (**map).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_remove: first argument must be a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_remove", &registry)?;

        let target_name = args[1].display();

        let tools = match registry.get("tools") {
            Some(VmValue::List(list)) => (**list).clone(),
            _ => Vec::new(),
        };

        let filtered: Vec<VmValue> = tools
            .into_iter()
            .filter(|tool| {
                if let VmValue::Dict(entry) = tool {
                    if let Some(VmValue::String(name)) = entry.get("name") {
                        return &**name != target_name.as_str();
                    }
                }
                true
            })
            .collect();

        let mut new_registry = registry;
        new_registry.insert("tools".to_string(), VmValue::List(Rc::new(filtered)));
        Ok(VmValue::Dict(Rc::new(new_registry)))
    });

    vm.register_builtin("tool_count", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_count: requires a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_count", registry)?;
        let count = vm_get_tools(registry).len();
        Ok(VmValue::Int(count as i64))
    });

    vm.register_builtin("tool_schema", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => {
                vm_validate_registry("tool_schema", map)?;
                map
            }
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_schema: requires a tool registry",
                ))));
            }
        };

        let components = args.get(1).and_then(|v| v.as_dict()).cloned();

        let tools = match registry.get("tools") {
            Some(VmValue::List(list)) => list,
            _ => return Ok(VmValue::Dict(Rc::new(vm_build_empty_schema()))),
        };

        let mut tool_schemas = Vec::new();
        for tool in tools.iter() {
            if let VmValue::Dict(entry) = tool {
                let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
                let description = entry
                    .get("description")
                    .map(|v| v.display())
                    .unwrap_or_default();

                let input_schema =
                    vm_build_input_schema(entry.get("parameters"), components.as_ref());
                let output_schema =
                    vm_build_output_schema(entry.get("outputSchema"), components.as_ref());

                let mut tool_def = BTreeMap::new();
                tool_def.insert("name".to_string(), VmValue::String(Rc::from(name)));
                tool_def.insert(
                    "description".to_string(),
                    VmValue::String(Rc::from(description)),
                );
                tool_def.insert("inputSchema".to_string(), input_schema);
                if let Some(output_schema) = output_schema {
                    tool_def.insert("outputSchema".to_string(), output_schema);
                }
                tool_schemas.push(VmValue::Dict(Rc::new(tool_def)));
            }
        }

        let mut schema = BTreeMap::new();
        schema.insert(
            "schema_version".to_string(),
            VmValue::String(Rc::from("harn-tools/1.0")),
        );

        if let Some(comps) = &components {
            let mut comp_wrapper = BTreeMap::new();
            comp_wrapper.insert("schemas".to_string(), VmValue::Dict(Rc::new(comps.clone())));
            schema.insert(
                "components".to_string(),
                VmValue::Dict(Rc::new(comp_wrapper)),
            );
        }

        schema.insert("tools".to_string(), VmValue::List(Rc::new(tool_schemas)));
        Ok(VmValue::Dict(Rc::new(schema)))
    });

    // Unknown config keys (beyond parameters/handler/returns/annotations/
    // executor/host_capability/mcp_server) are preserved verbatim so
    // integrators can attach policy/effect metadata.
    vm.register_builtin("tool_define", |args, _out| {
        if args.len() < 4 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_define: requires registry, name, description, and config dict",
            ))));
        }

        let registry = match &args[0] {
            VmValue::Dict(map) => (**map).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_define: first argument must be a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_define", &registry)?;

        let name = args[1].display();
        let description = args[2].display();

        let config = match &args[3] {
            VmValue::Dict(map) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_define: config must be a dict with parameters and handler",
                ))));
            }
        };

        let handler = config.get("handler").cloned().unwrap_or(VmValue::Nil);
        let has_handler = !matches!(handler, VmValue::Nil);

        if config.contains_key("params") && !config.contains_key("parameters") {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_define: use 'parameters', not 'params'",
            ))));
        }

        // Resolve executor (harn#743). Authors must pick exactly one
        // backend; the dispatcher and ACP transcript both honor the
        // declared value verbatim so clients can render "via host
        // bridge" / "via mcp:linear" badges without inferring backends
        // from missing fields.
        let executor = resolve_tool_executor(&name, config, has_handler)?;

        // `defer_loading` controls progressive-disclosure behavior on
        // capable providers (Anthropic Claude 4.0+ and OpenAI GPT 5.4+).
        // Gate the type here so typos don't silently fall back to the
        // "no defer" default.
        if let Some(v) = config.get("defer_loading") {
            if !matches!(v, VmValue::Bool(_)) {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_define: `defer_loading` must be a bool \
                     (true → hold schema back until a tool_search call \
                     surfaces it; false or absent → ship eagerly)",
                ))));
            }
        }

        // `namespace` groups deferred tools for OpenAI's `tool_search`
        // meta-tool (Anthropic ignores the field). Must be a non-empty
        // string so typos don't silently flow through to the payload.
        if let Some(v) = config.get("namespace") {
            match v {
                VmValue::String(s) if !s.is_empty() => {}
                VmValue::Nil => {}
                _ => {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(
                        "tool_define: `namespace` must be a non-empty string \
                         (groups deferred tools for OpenAI tool_search; \
                         Anthropic ignores it)",
                    ))));
                }
            }
        }

        let parameters = config
            .get("parameters")
            .cloned()
            .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
        let output_schema = config.get("returns").cloned().unwrap_or(VmValue::Nil);

        let mut tool_entry = BTreeMap::new();
        tool_entry.insert("name".to_string(), VmValue::String(Rc::from(name.as_str())));
        tool_entry.insert(
            "description".to_string(),
            VmValue::String(Rc::from(description)),
        );
        tool_entry.insert("handler".to_string(), handler);
        tool_entry.insert("parameters".to_string(), parameters);
        if !matches!(output_schema, VmValue::Nil) {
            tool_entry.insert("outputSchema".to_string(), output_schema);
        }
        tool_entry.insert(
            "executor".to_string(),
            VmValue::String(Rc::from(executor.as_str())),
        );

        if let Some(annotations) = config.get("annotations") {
            tool_entry.insert("annotations".to_string(), annotations.clone());
        }

        for (key, value) in config.iter() {
            // `executor` is normalized into the canonical entry above
            // (the resolver fills the default for legacy `harn` handlers
            // and rejects mismatches), so skip the raw author input
            // here. Same for the fields that have dedicated slots.
            if matches!(
                key.as_str(),
                "handler" | "parameters" | "returns" | "annotations" | "executor"
            ) {
                continue;
            }
            tool_entry.insert(key.clone(), value.clone());
        }

        let mut tools: Vec<VmValue> = match registry.get("tools") {
            Some(VmValue::List(list)) => list
                .iter()
                .filter(|t| {
                    if let VmValue::Dict(e) = t {
                        e.get("name").map(|v| v.display()).as_deref() != Some(name.as_str())
                    } else {
                        true
                    }
                })
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        tools.push(VmValue::Dict(Rc::new(tool_entry)));

        let mut new_registry = registry;
        new_registry.insert("tools".to_string(), VmValue::List(Rc::new(tools)));
        Ok(VmValue::Dict(Rc::new(new_registry)))
    });

    vm.register_builtin("tool_parse_call", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();

        let mut results = Vec::new();
        let mut search_from = 0;

        while let Some(start) = text[search_from..].find("<tool_call>") {
            let abs_start = search_from + start + "<tool_call>".len();
            if let Some(end) = text[abs_start..].find("</tool_call>") {
                let json_str = text[abs_start..abs_start + end].trim();
                if let Ok(jv) = serde_json::from_str::<serde_json::Value>(json_str) {
                    results.push(json_to_vm_value(&jv));
                }
                search_from = abs_start + end + "</tool_call>".len();
            } else {
                break;
            }
        }

        Ok(VmValue::List(Rc::new(results)))
    });

    vm.register_builtin("tool_format_result", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_format_result: requires name and result",
            ))));
        }
        let name = args[0].display();
        let result = args[1].display();

        let json_name = super::logging::vm_escape_json_str(&name);
        let json_result = super::logging::vm_escape_json_str(&result);
        Ok(VmValue::String(Rc::from(
            format!(
                "<tool_result>{{\"name\": \"{json_name}\", \"result\": \"{json_result}\"}}</tool_result>"
            )
            .as_str(),
        )))
    });

    vm.register_builtin("tool_prompt", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => {
                vm_validate_registry("tool_prompt", map)?;
                map
            }
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_prompt: requires a tool registry",
                ))));
            }
        };

        let tools = match registry.get("tools") {
            Some(VmValue::List(list)) => list,
            _ => {
                return Ok(VmValue::String(Rc::from("No tools are available.")));
            }
        };

        if tools.is_empty() {
            return Ok(VmValue::String(Rc::from("No tools are available.")));
        }

        let mut prompt = String::from("# Available Tools\n\n");
        prompt.push_str("You have access to the following tools. To use a tool, output a tool call in this exact format:\n\n");
        prompt.push_str("<tool_call>{\"name\": \"tool_name\", \"arguments\": {\"param\": \"value\"}}</tool_call>\n\n");
        prompt.push_str("You may make multiple tool calls in a single response. Wait for tool results before proceeding.\n\n");
        prompt.push_str("## Tools\n\n");

        let mut tool_infos: Vec<(&BTreeMap<String, VmValue>, String)> = Vec::new();
        for tool in tools.iter() {
            if let VmValue::Dict(entry) = tool {
                let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
                tool_infos.push((entry, name));
            }
        }
        tool_infos.sort_by(|a, b| a.1.cmp(&b.1));

        for (entry, name) in &tool_infos {
            let description = entry
                .get("description")
                .map(|v| v.display())
                .unwrap_or_default();
            let params_str = vm_format_parameters(entry.get("parameters"));
            let returns_str = vm_format_schema(entry.get("outputSchema"));

            prompt.push_str(&format!("### {name}\n"));
            prompt.push_str(&format!("{description}\n"));
            if !params_str.is_empty() {
                prompt.push_str(&format!("Parameters: {params_str}\n"));
            }
            if !returns_str.is_empty() {
                prompt.push_str(&format!("Returns: {returns_str}\n"));
            }
            prompt.push('\n');
        }

        Ok(VmValue::String(Rc::from(prompt.trim_end())))
    });

    vm.register_builtin("tool_bind", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => {
                vm_validate_registry("tool_bind", map)?;
                VmValue::Dict(map.clone())
            }
            Some(VmValue::Nil) | None => {
                install_current_tool_registry(None);
                return Ok(VmValue::Nil);
            }
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_bind: argument must be a tool registry or nil",
                ))));
            }
        };
        install_current_tool_registry(Some(registry.clone()));
        Ok(registry)
    });

    vm.register_builtin("tool_ref", |args, _out| {
        let name = match args.first() {
            Some(VmValue::String(s)) => s.to_string(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_ref: name must be a string literal",
                ))));
            }
        };

        let registry_value = vm_current_registry_dict("tool_ref")?;
        let VmValue::Dict(registry) = &registry_value else {
            unreachable!("vm_current_registry_dict returns Dict or errors");
        };

        if vm_find_tool_entry(registry, &name).is_some() {
            return Ok(VmValue::String(Rc::from(name.as_str())));
        }

        let registered = vm_registered_names(registry);
        let listed = if registered.is_empty() {
            "(none registered)".to_string()
        } else {
            registered.join(", ")
        };
        Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "tool_ref: unknown tool {name:?}. Registered tools: {listed}"
        )))))
    });

    vm.register_builtin("tool_def", |args, _out| {
        let name = match args.first() {
            Some(VmValue::String(s)) => s.to_string(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_def: name must be a string literal",
                ))));
            }
        };

        let registry_value = vm_current_registry_dict("tool_def")?;
        let VmValue::Dict(registry) = &registry_value else {
            unreachable!("vm_current_registry_dict returns Dict or errors");
        };

        if let Some(entry) = vm_find_tool_entry(registry, &name) {
            let mut desc = BTreeMap::new();
            for (key, value) in entry.iter() {
                if key == "handler" {
                    continue;
                }
                desc.insert(key.clone(), value.clone());
            }
            return Ok(VmValue::Dict(Rc::new(desc)));
        }

        let registered = vm_registered_names(registry);
        let listed = if registered.is_empty() {
            "(none registered)".to_string()
        } else {
            registered.join(", ")
        };
        Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "tool_def: unknown tool {name:?}. Registered tools: {listed}"
        )))))
    });
}

fn vm_validate_registry(name: &str, dict: &BTreeMap<String, VmValue>) -> Result<(), VmError> {
    match dict.get("_type") {
        Some(VmValue::String(t)) if &**t == "tool_registry" => Ok(()),
        _ => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{name}: argument must be a tool registry (created with tool_registry())"
        ))))),
    }
}

/// Canonical executor identifiers accepted by `tool_define`. The
/// dispatcher (`crate::llm::agent_tools::dispatch_tool_execution`)
/// reads the chosen value verbatim and ACP forwards it on every
/// `tool_call_update`, so the names must stay in lockstep with
/// [`crate::agent_events::ToolExecutor`] (harn#743).
pub(crate) const EXECUTOR_HARN: &str = "harn";
pub(crate) const EXECUTOR_HOST_BRIDGE: &str = "host_bridge";
pub(crate) const EXECUTOR_MCP_SERVER: &str = "mcp_server";
pub(crate) const EXECUTOR_PROVIDER_NATIVE: &str = "provider_native";

/// Resolve the declared executor for an in-registry tool entry,
/// applying the same legacy fall-back as [`resolve_tool_executor`]
/// (handler present + executor absent → `EXECUTOR_HARN`, MCP-discovered
/// entries with `_mcp_server` → `EXECUTOR_MCP_SERVER`). Returns `None`
/// when neither signal is present *or* when an explicit `executor`
/// value is not one of the canonical names — both cases mean the
/// tool has no executable backend the dispatcher can route to
/// (harn#743).
pub(crate) fn declared_executor_for_entry(entry: &BTreeMap<String, VmValue>) -> Option<String> {
    if let Some(VmValue::String(s)) = entry.get("executor") {
        return if known_executor(s) {
            Some(s.to_string())
        } else {
            None
        };
    }
    if entry.contains_key("_mcp_server") {
        return Some(EXECUTOR_MCP_SERVER.to_string());
    }
    if matches!(entry.get("handler"), Some(v) if !matches!(v, VmValue::Nil)) {
        return Some(EXECUTOR_HARN.to_string());
    }
    None
}

/// Walk a tool registry value (dict with `tools: List[Dict]`) and
/// return a list of tool names that have no executable backend. Used
/// by `agent_loop` to refuse to start when the registry contains a
/// tool that the dispatcher would later fail to handle (harn#743).
/// Non-registry shapes return an empty list — the existing argument
/// validators in `agent_loop` / `extract_llm_options` already reject
/// the bad shape with their own diagnostics.
pub(crate) fn tools_missing_executor(tools_val: &VmValue) -> Vec<String> {
    let dict = match tools_val {
        VmValue::Dict(d) => d,
        _ => return Vec::new(),
    };
    let tools = match dict.get("tools") {
        Some(VmValue::List(list)) => list,
        _ => return Vec::new(),
    };
    let mut missing = Vec::new();
    for tool in tools.iter() {
        let entry = match tool {
            VmValue::Dict(d) => d,
            _ => continue,
        };
        if declared_executor_for_entry(entry).is_some() {
            continue;
        }
        let name = entry
            .get("name")
            .map(|v| v.display())
            .unwrap_or_else(|| "<unnamed>".to_string());
        missing.push(name);
    }
    missing
}

/// Run the [`tools_missing_executor`] guard and convert any missing
/// backends into the canonical agent-loop error. Both `agent_loop`
/// registrations (the bare and the bridge-attached variants) call
/// this so the diagnostic stays in lockstep regardless of which
/// builtin is bound (harn#743).
pub(crate) fn ensure_tools_have_executors(tools_val: Option<&VmValue>) -> Result<(), VmError> {
    let Some(tools_val) = tools_val else {
        return Ok(());
    };
    let missing = tools_missing_executor(tools_val);
    if missing.is_empty() {
        return Ok(());
    }
    Err(VmError::Thrown(VmValue::String(Rc::from(format!(
        "agent_loop: registry contains {} tool(s) with no executable backend: {}. \
         Each tool must declare an `executor` (\"harn\", \"host_bridge\", \"mcp_server\", \
         or \"provider_native\") with the matching backing field, or supply a `handler` \
         closure for the legacy in-Harn case.",
        missing.len(),
        missing.join(", "),
    )))))
}

fn known_executor(value: &str) -> bool {
    matches!(
        value,
        EXECUTOR_HARN | EXECUTOR_HOST_BRIDGE | EXECUTOR_MCP_SERVER | EXECUTOR_PROVIDER_NATIVE
    )
}

/// Validate the `executor` declaration on a `tool_define` config and
/// return the canonical executor name. Authors must pick exactly one
/// backend: missing handlers can no longer silently fall through to
/// the host bridge (harn#743). When the author omits `executor` but
/// provides a `handler`, this defaults to `EXECUTOR_HARN` so existing
/// scripts keep compiling — the leaky case the issue calls out
/// (handler-less tools relying on bridge fall-through) is the only
/// one that has to declare an executor explicitly.
fn resolve_tool_executor(
    name: &str,
    config: &BTreeMap<String, VmValue>,
    has_handler: bool,
) -> Result<String, VmError> {
    let executor = match config.get("executor") {
        Some(VmValue::String(s)) => Some(s.to_string()),
        Some(VmValue::Nil) | None => None,
        Some(other) => {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "tool_define({name:?}): `executor` must be a string \
                 (one of \"harn\", \"host_bridge\", \"mcp_server\", \"provider_native\"), \
                 got {}",
                other.display()
            )))));
        }
    };

    let host_capability = config.get("host_capability");
    let mcp_server = config.get("mcp_server");

    let executor = match executor {
        Some(value) => {
            if !known_executor(&value) {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "tool_define({name:?}): unknown executor {value:?}. \
                     Expected one of \"harn\", \"host_bridge\", \"mcp_server\", \"provider_native\"."
                )))));
            }
            value
        }
        None => {
            if has_handler {
                EXECUTOR_HARN.to_string()
            } else {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "tool_define({name:?}): tool has no `handler` and no explicit `executor`. \
                     Add `handler: ...` for an in-Harn tool, or declare \
                     `executor: \"host_bridge\", host_capability: \"capability.operation\"` \
                     for a tool the host backs (or `executor: \"mcp_server\", mcp_server: \"name\"` / \
                     `executor: \"provider_native\"`)."
                )))));
            }
        }
    };

    match executor.as_str() {
        EXECUTOR_HARN => {
            if !has_handler {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "tool_define({name:?}): executor: \"harn\" requires a `handler` closure. \
                     Either supply `handler: {{ args -> ... }}` or pick a different executor."
                )))));
            }
            if host_capability.is_some_and(|v| !matches!(v, VmValue::Nil)) {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "tool_define({name:?}): executor: \"harn\" must not declare \
                     `host_capability`; that field is only valid for executor: \"host_bridge\"."
                )))));
            }
            if mcp_server.is_some_and(|v| !matches!(v, VmValue::Nil)) {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "tool_define({name:?}): executor: \"harn\" must not declare \
                     `mcp_server`; that field is only valid for executor: \"mcp_server\"."
                )))));
            }
        }
        EXECUTOR_HOST_BRIDGE => {
            if has_handler {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "tool_define({name:?}): executor: \"host_bridge\" must not declare a \
                     `handler` — the host owns execution. Drop the handler closure or pick \
                     executor: \"harn\"."
                )))));
            }
            match host_capability {
                Some(VmValue::String(s)) if !s.is_empty() => {
                    if s.split_once('.')
                        .is_none_or(|(c, op)| c.is_empty() || op.is_empty())
                    {
                        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                            "tool_define({name:?}): `host_capability` must look like \
                             \"capability.operation\" (e.g. \"interaction.ask\"); got {s:?}."
                        )))));
                    }
                }
                _ => {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "tool_define({name:?}): executor: \"host_bridge\" requires a \
                         `host_capability: \"capability.operation\"` field so `harn check` can \
                         validate the host backs the tool (e.g. \"interaction.ask\")."
                    )))));
                }
            }
            if mcp_server.is_some_and(|v| !matches!(v, VmValue::Nil)) {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "tool_define({name:?}): executor: \"host_bridge\" must not declare \
                     `mcp_server`; that field is only valid for executor: \"mcp_server\"."
                )))));
            }
        }
        EXECUTOR_MCP_SERVER => {
            if has_handler {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "tool_define({name:?}): executor: \"mcp_server\" must not declare a \
                     `handler` — the MCP server owns execution. Drop the handler closure or \
                     pick executor: \"harn\"."
                )))));
            }
            match mcp_server {
                Some(VmValue::String(s)) if !s.is_empty() => {}
                _ => {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "tool_define({name:?}): executor: \"mcp_server\" requires an \
                         `mcp_server: \"<server name>\"` field naming the configured MCP server."
                    )))));
                }
            }
            if host_capability.is_some_and(|v| !matches!(v, VmValue::Nil)) {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "tool_define({name:?}): executor: \"mcp_server\" must not declare \
                     `host_capability`; that field is only valid for executor: \"host_bridge\"."
                )))));
            }
        }
        EXECUTOR_PROVIDER_NATIVE => {
            if has_handler {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "tool_define({name:?}): executor: \"provider_native\" must not declare a \
                     `handler` — the provider already returns the executed result inline."
                )))));
            }
            if host_capability.is_some_and(|v| !matches!(v, VmValue::Nil)) {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "tool_define({name:?}): executor: \"provider_native\" must not declare \
                     `host_capability`."
                )))));
            }
            if mcp_server.is_some_and(|v| !matches!(v, VmValue::Nil)) {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "tool_define({name:?}): executor: \"provider_native\" must not declare \
                     `mcp_server`."
                )))));
            }
        }
        _ => unreachable!("known_executor guard above"),
    }

    Ok(executor)
}

fn vm_get_tools(dict: &BTreeMap<String, VmValue>) -> &[VmValue] {
    match dict.get("tools") {
        Some(VmValue::List(list)) => list,
        _ => &[],
    }
}

fn vm_format_parameters(params: Option<&VmValue>) -> String {
    match params {
        Some(VmValue::Dict(map)) if !map.is_empty() => {
            let mut pairs: Vec<(String, String)> =
                map.iter().map(|(k, v)| (k.clone(), v.display())).collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            pairs
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
        _ => String::new(),
    }
}

fn vm_format_schema(schema: Option<&VmValue>) -> String {
    match schema {
        Some(VmValue::String(type_name)) => type_name.to_string(),
        Some(VmValue::Dict(map)) if !map.is_empty() => {
            if let Some(VmValue::String(reference)) = map.get("$ref") {
                return format!("$ref({reference})");
            }
            let mut pairs: Vec<(String, String)> =
                map.iter().map(|(k, v)| (k.clone(), v.display())).collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            pairs
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
        _ => String::new(),
    }
}

fn vm_build_empty_schema() -> BTreeMap<String, VmValue> {
    let mut schema = BTreeMap::new();
    schema.insert(
        "schema_version".to_string(),
        VmValue::String(Rc::from("harn-tools/1.0")),
    );
    schema.insert("tools".to_string(), VmValue::List(Rc::new(Vec::new())));
    schema
}

fn vm_build_input_schema(
    params: Option<&VmValue>,
    components: Option<&BTreeMap<String, VmValue>>,
) -> VmValue {
    let mut schema = BTreeMap::new();
    schema.insert("type".to_string(), VmValue::String(Rc::from("object")));

    let params_map = match params {
        Some(VmValue::Dict(map)) if !map.is_empty() => map,
        _ => {
            schema.insert(
                "properties".to_string(),
                VmValue::Dict(Rc::new(BTreeMap::new())),
            );
            return VmValue::Dict(Rc::new(schema));
        }
    };

    let mut properties = BTreeMap::new();
    let mut required = Vec::new();

    for (key, val) in params_map.iter() {
        let prop = vm_resolve_param_type(val, components);
        properties.insert(key.clone(), prop);
        required.push(VmValue::String(Rc::from(key.as_str())));
    }

    schema.insert("properties".to_string(), VmValue::Dict(Rc::new(properties)));
    if !required.is_empty() {
        required.sort_by_key(|a| a.display());
        schema.insert("required".to_string(), VmValue::List(Rc::new(required)));
    }

    VmValue::Dict(Rc::new(schema))
}

fn vm_build_output_schema(
    schema: Option<&VmValue>,
    components: Option<&BTreeMap<String, VmValue>>,
) -> Option<VmValue> {
    schema.map(|value| vm_resolve_param_type(value, components))
}

fn vm_resolve_param_type(val: &VmValue, components: Option<&BTreeMap<String, VmValue>>) -> VmValue {
    match val {
        VmValue::String(type_name) => {
            let json_type = vm_harn_type_to_json_schema(type_name);
            let mut prop = BTreeMap::new();
            prop.insert("type".to_string(), VmValue::String(Rc::from(json_type)));
            VmValue::Dict(Rc::new(prop))
        }
        VmValue::Dict(map) => {
            if let Some(VmValue::String(ref_name)) = map.get("$ref") {
                if let Some(comps) = components {
                    if let Some(resolved) = comps.get(&**ref_name) {
                        return resolved.clone();
                    }
                }
                let mut prop = BTreeMap::new();
                prop.insert(
                    "$ref".to_string(),
                    VmValue::String(Rc::from(
                        format!("#/components/schemas/{ref_name}").as_str(),
                    )),
                );
                VmValue::Dict(Rc::new(prop))
            } else {
                VmValue::Dict(Rc::new((**map).clone()))
            }
        }
        _ => {
            let mut prop = BTreeMap::new();
            prop.insert("type".to_string(), VmValue::String(Rc::from("string")));
            VmValue::Dict(Rc::new(prop))
        }
    }
}

fn vm_harn_type_to_json_schema(harn_type: &str) -> &str {
    match harn_type {
        "int" => "integer",
        "float" => "number",
        "bool" | "boolean" => "boolean",
        "list" | "array" => "array",
        "dict" | "object" => "object",
        _ => "string",
    }
}
