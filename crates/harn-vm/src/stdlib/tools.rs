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

    // Unknown config keys (beyond parameters/handler/returns/annotations) are
    // preserved verbatim so integrators can attach policy/effect metadata.
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

        // Resolve and validate the declared executor (harn#743). Every
        // tool must have exactly one execution backend; the `executor`
        // field is the source of truth so downstream code (agent_loop,
        // ACP `tool_call_update.executor`, `harn check`) doesn't have
        // to guess from `handler` presence.
        //
        // Back-compat: when `executor` is absent, infer from `handler`
        // — present → `"harn"`, missing → reject with a clear error.
        // The reject path is the foot-gun the issue eliminates: a
        // handlerless registration that previously slipped through
        // `tool_define` only to fail at the first model call with
        // `[builtin_call] unhandled: <name>`.
        let executor_value = config.get("executor").cloned();
        let executor = match executor_value.as_ref() {
            Some(VmValue::String(s)) => Some(s.to_string()),
            Some(VmValue::Nil) | None => None,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_define: `executor` must be a string \
                     (\"harn\", \"host_bridge\", \"mcp_server\", or \"provider_native\")",
                ))));
            }
        };
        let resolved_executor = match executor.as_deref() {
            Some("harn") | Some("harn_builtin") => "harn",
            Some("host_bridge") => "host_bridge",
            Some("mcp_server") => "mcp_server",
            Some("provider_native") => "provider_native",
            Some(other) => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "tool_define: unknown executor {other:?} for tool {name:?}. \
                     Expected one of: \"harn\", \"host_bridge\", \"mcp_server\", \
                     \"provider_native\""
                )))));
            }
            None => {
                if has_handler {
                    "harn"
                } else {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "tool_define: tool {name:?} has no `handler` and no `executor`. \
                         Either attach a `handler` fn (executor: \"harn\") or \
                         declare an alternate backend, e.g. \
                         executor: \"host_bridge\" + host_capability: \"cap.op\", \
                         executor: \"mcp_server\" + mcp_server: \"server-name\", \
                         executor: \"provider_native\". \
                         Defining a handlerless tool would surface as \
                         `[builtin_call] unhandled: {name}` at the first model call."
                    )))));
                }
            }
        };

        // Per-executor invariants:
        // - `"harn"` requires a callable handler.
        // - `"host_bridge"` forbids `handler` and requires
        //   `host_capability` so `harn check` can validate the binding
        //   against the project's host capability manifest.
        // - `"mcp_server"` forbids `handler` and requires `mcp_server`
        //   (the configured server name; mirrors the `_mcp_server`
        //   annotation `mcp_list_tools` injects on every tool dict).
        // - `"provider_native"` forbids `handler` (the model returns
        //   the already-executed result inline).
        let host_capability = config.get("host_capability");
        let mcp_server = config.get("mcp_server");
        match resolved_executor {
            "harn" => {
                if !has_handler && !crate::llm::tools::is_vm_stdlib_short_circuit(name.as_str()) {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "tool_define: tool {name:?} declares executor: \"harn\" \
                         but has no `handler`. Attach the handler fn or change \
                         the executor."
                    )))));
                }
                if host_capability.is_some_and(|v| !matches!(v, VmValue::Nil)) {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "tool_define: tool {name:?} declares executor: \"harn\" \
                         but also sets `host_capability`. Drop one — the harn \
                         executor runs the handler in-VM and never reaches \
                         the host bridge."
                    )))));
                }
                if mcp_server.is_some_and(|v| !matches!(v, VmValue::Nil)) {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "tool_define: tool {name:?} declares executor: \"harn\" \
                         but also sets `mcp_server`. Drop one — MCP-served \
                         tools must declare executor: \"mcp_server\"."
                    )))));
                }
            }
            "host_bridge" => {
                if has_handler {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "tool_define: tool {name:?} declares executor: \"host_bridge\" \
                         but also has a `handler`. Drop the handler — host-bridge \
                         tools are dispatched by the host shell, not the VM."
                    )))));
                }
                match host_capability {
                    Some(VmValue::String(s)) if !s.is_empty() => {}
                    _ => {
                        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                            "tool_define: tool {name:?} declares executor: \"host_bridge\" \
                             but is missing `host_capability`. Set it to the \
                             canonical bridge identifier (e.g. \"interaction.ask\") \
                             so `harn check` can validate the binding against the \
                             host capability manifest."
                        )))));
                    }
                }
            }
            "mcp_server" => {
                if has_handler {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "tool_define: tool {name:?} declares executor: \"mcp_server\" \
                         but also has a `handler`. Drop the handler — MCP-served \
                         tools route through the MCP transport."
                    )))));
                }
                match mcp_server {
                    Some(VmValue::String(s)) if !s.is_empty() => {}
                    _ => {
                        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                            "tool_define: tool {name:?} declares executor: \"mcp_server\" \
                             but is missing `mcp_server` (the configured server name)."
                        )))));
                    }
                }
            }
            "provider_native" => {
                if has_handler {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "tool_define: tool {name:?} declares executor: \"provider_native\" \
                         but also has a `handler`. Provider-side tools are \
                         executed by the model server; the runtime never \
                         dispatches them locally."
                    )))));
                }
            }
            _ => unreachable!("resolved_executor is matched above"),
        }

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
        // Stash the resolved executor so callers can read the declared
        // backend without re-parsing config (and so back-compat
        // `executor: "harn_builtin"` aliases collapse to the canonical
        // `"harn"` form). Stored as a plain string — wire serialization
        // is handled by the ACP adapter.
        tool_entry.insert(
            "executor".to_string(),
            VmValue::String(Rc::from(resolved_executor)),
        );
        if !matches!(output_schema, VmValue::Nil) {
            tool_entry.insert("outputSchema".to_string(), output_schema);
        }

        if let Some(annotations) = config.get("annotations") {
            tool_entry.insert("annotations".to_string(), annotations.clone());
        }

        for (key, value) in config.iter() {
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
