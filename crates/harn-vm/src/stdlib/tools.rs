use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmClosure, VmEnv, VmError, VmValue};
use crate::vm::Vm;

use crate::schema::json_to_vm_value;

thread_local! {
    /// The tool registry bound to the current execution scope. Populated
    /// by `agent_loop` at the start of its run and cleared on exit, or by
    /// `tool_bind(registry)` in tests and prompt-building code. Consumed
    /// by `tool_ref` / `tool_def` to resolve tool-name references without
    /// threading the registry through every call site.
    static CURRENT_TOOL_REGISTRY: RefCell<Option<VmValue>> = const { RefCell::new(None) };
    static TOOL_SYNTHESIS_CACHE: RefCell<BTreeMap<String, SynthesizedToolSpec>> = const { RefCell::new(BTreeMap::new()) };
}

#[derive(Clone)]
enum SynthesizedToolExecutor {
    DryRun,
    HostBridge {
        host_tool: String,
    },
    McpServer {
        tool_name: String,
        client: Option<crate::mcp::VmMcpClientHandle>,
        server_name: Option<String>,
    },
}

#[derive(Clone)]
struct SynthesizedToolSpec {
    id: String,
    name: String,
    description: String,
    parameters: VmValue,
    return_type: VmValue,
    capabilities: Vec<String>,
    side_effect_level: String,
    executor: SynthesizedToolExecutor,
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

pub fn clear_tool_synthesis_cache() {
    TOOL_SYNTHESIS_CACHE.with(|slot| slot.borrow_mut().clear());
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
    vm.register_builtin("tool_synthesize", |args, _out| {
        let spec = synthesize_tool_spec(args.first())?;
        let closure = compile_synthesized_tool_closure(&spec.id)?;
        TOOL_SYNTHESIS_CACHE.with(|cache| {
            cache.borrow_mut().insert(spec.id.clone(), spec);
        });
        Ok(closure)
    });

    vm.register_async_builtin("tool_synth_invoke", |args| async move {
        let id = match args.first() {
            Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_synth_invoke: synthesis id is required",
                ))));
            }
        };
        let call_args = args.get(1).cloned().unwrap_or(VmValue::Nil);
        invoke_synthesized_tool(&id, call_args).await
    });

    vm.register_builtin("tool_synthesis_cache", |_args, _out| {
        let specs = TOOL_SYNTHESIS_CACHE.with(|cache| {
            cache
                .borrow()
                .values()
                .map(synthesized_tool_spec_value)
                .collect::<Vec<_>>()
        });
        Ok(VmValue::List(Rc::new(specs)))
    });

    vm.register_builtin("tool_synthesis_clear", |_args, _out| {
        clear_tool_synthesis_cache();
        Ok(VmValue::Nil)
    });

    vm.register_builtin("plan_artifact", |args, _out| {
        let input = args.first().cloned().unwrap_or(VmValue::Nil);
        let json = crate::llm::vm_value_to_json(&input);
        let plan =
            crate::llm::plan::normalize_plan_tool_call(crate::llm::plan::EMIT_PLAN_TOOL, &json);
        Ok(json_to_vm_value(&plan))
    });

    vm.register_builtin("plan_entries", |args, _out| {
        let input = args.first().cloned().unwrap_or(VmValue::Nil);
        let json = crate::llm::vm_value_to_json(&input);
        Ok(json_to_vm_value(&crate::llm::plan::plan_entries(&json)))
    });

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

    vm.register_builtin("tool_surface_validate", |args, _out| {
        let surface = args
            .first()
            .cloned()
            .unwrap_or_else(|| current_tool_registry().unwrap_or(VmValue::Nil));
        let input = crate::tool_surface::surface_input_from_vm(&surface, args.get(1));
        let report = crate::tool_surface::validate_tool_surface(&input);
        Ok(crate::stdlib::json_to_vm_value(
            &crate::tool_surface::surface_report_to_json(&report),
        ))
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

fn synthesize_tool_spec(input: Option<&VmValue>) -> Result<SynthesizedToolSpec, VmError> {
    let config = match input {
        Some(VmValue::Dict(map)) => map,
        _ => {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_synthesize: requires a config dict",
            ))));
        }
    };
    if config.contains_key("handler") || config.contains_key("body") {
        return Err(VmError::Thrown(VmValue::String(Rc::from(
            "tool_synthesize: executable handlers must be supplied with tool_define; \
             synthesized tools are dry-run, host_bridge, or mcp_server only",
        ))));
    }

    let description = required_string(config, "tool_synthesize", "description")?;
    let name = optional_string(config, "name")
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| synthesize_tool_name(&description));
    validate_tool_name(&name)?;

    let parameters = config
        .get("parameters")
        .or_else(|| config.get("params"))
        .cloned()
        .unwrap_or_else(|| VmValue::Dict(Rc::new(BTreeMap::new())));
    let return_type = config
        .get("return_type")
        .or_else(|| config.get("returns"))
        .cloned()
        .unwrap_or(VmValue::Nil);
    let capabilities = optional_string_list(config, "capabilities")?;
    let side_effect_level = optional_string(config, "side_effect_level")
        .unwrap_or_else(|| infer_side_effect_level(&capabilities));

    let executor_name =
        optional_string(config, "executor").unwrap_or_else(|| "dry_run".to_string());
    let executor = match executor_name.as_str() {
        "dry_run" => SynthesizedToolExecutor::DryRun,
        "host_bridge" => {
            let host_tool = optional_string(config, "host_tool").unwrap_or_else(|| name.clone());
            if host_tool.trim().is_empty() {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_synthesize: host_bridge executor requires a non-empty host_tool",
                ))));
            }
            SynthesizedToolExecutor::HostBridge { host_tool }
        }
        "mcp_server" => {
            let tool_name = optional_string(config, "mcp_tool").unwrap_or_else(|| name.clone());
            if tool_name.trim().is_empty() {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_synthesize: mcp_server executor requires a non-empty mcp_tool",
                ))));
            }
            let client = match config.get("mcp_client") {
                Some(VmValue::McpClient(client)) => Some(client.clone()),
                Some(VmValue::Nil) | None => None,
                _ => {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(
                        "tool_synthesize: mcp_client must be an MCP client handle",
                    ))));
                }
            };
            SynthesizedToolExecutor::McpServer {
                tool_name,
                client,
                server_name: optional_string(config, "mcp_server"),
            }
        }
        other => {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "tool_synthesize: unknown executor {other:?}; expected \
                 \"dry_run\", \"host_bridge\", or \"mcp_server\""
            )))));
        }
    };

    let mut spec = SynthesizedToolSpec {
        id: String::new(),
        name,
        description,
        parameters,
        return_type,
        capabilities,
        side_effect_level,
        executor,
    };
    spec.id = synthesized_tool_hash(&spec);
    Ok(spec)
}

fn compile_synthesized_tool_closure(id: &str) -> Result<VmValue, VmError> {
    let source =
        format!("fn __harn_synthesized_tool(args) {{ return tool_synth_invoke(\"{id}\", args) }}");
    let program = harn_parser::check_source_strict(&source).map_err(|error| {
        VmError::Runtime(format!("tool_synthesize: internal compile failed: {error}"))
    })?;
    let Some(fn_node) = program.iter().find_map(|node| match &node.node {
        harn_parser::Node::FnDecl { params, body, .. } => Some((params, body)),
        _ => None,
    }) else {
        return Err(VmError::Runtime(
            "tool_synthesize: internal closure source had no function".to_string(),
        ));
    };
    let mut compiler = crate::Compiler::new();
    let func = compiler
        .compile_fn_body(fn_node.0, fn_node.1, Some("<tool_synthesize>".to_string()))
        .map_err(|error| VmError::Runtime(format!("tool_synthesize: {error}")))?;
    Ok(VmValue::Closure(Rc::new(VmClosure {
        func: Rc::new(func),
        env: VmEnv::new(),
        source_dir: None,
        module_functions: None,
        module_state: None,
    })))
}

async fn invoke_synthesized_tool(id: &str, call_args: VmValue) -> Result<VmValue, VmError> {
    let spec = TOOL_SYNTHESIS_CACHE.with(|cache| cache.borrow().get(id).cloned());
    let Some(spec) = spec else {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "tool_synth_invoke: unknown synthesis id {id:?}; synthesize the tool in this run first"
        )))));
    };

    validate_synthesized_tool_args(&spec, &call_args)?;

    match &spec.executor {
        SynthesizedToolExecutor::DryRun => Ok(synthesized_tool_dry_run_result(&spec, call_args)),
        SynthesizedToolExecutor::HostBridge { host_tool } => {
            crate::orchestration::enforce_current_policy_for_tool(&spec.name)?;
            crate::orchestration::enforce_current_policy_for_builtin(
                "host_tool_call",
                &[
                    VmValue::String(Rc::from(host_tool.as_str())),
                    call_args.clone(),
                ],
            )?;
            crate::stdlib::host::dispatch_host_tool_call(host_tool, &call_args).await
        }
        SynthesizedToolExecutor::McpServer {
            tool_name,
            client,
            server_name: _,
        } => {
            crate::orchestration::enforce_current_policy_for_tool(&spec.name)?;
            crate::orchestration::enforce_current_policy_for_builtin(
                "mcp_call",
                &[VmValue::Nil, VmValue::String(Rc::from(tool_name.as_str()))],
            )?;
            let Some(client) = client else {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_synth_invoke: mcp_server synthesized tools require mcp_client in the synthesis config",
                ))));
            };
            let arguments = match &call_args {
                VmValue::Dict(map) => serde_json::Value::Object(
                    map.iter()
                        .map(|(key, value)| (key.clone(), crate::llm::vm_value_to_json(value)))
                        .collect(),
                ),
                VmValue::Nil => serde_json::json!({}),
                _ => {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(
                        "tool_synth_invoke: mcp_server tool arguments must be a dict",
                    ))));
                }
            };
            Ok(json_to_vm_value(
                &crate::mcp::call_mcp_tool(client, tool_name, arguments).await?,
            ))
        }
    }
}

fn validate_synthesized_tool_args(
    spec: &SynthesizedToolSpec,
    call_args: &VmValue,
) -> Result<(), VmError> {
    let VmValue::Dict(params) = &spec.parameters else {
        return Ok(());
    };
    if params.is_empty() {
        return Ok(());
    }
    let args = match call_args {
        VmValue::Dict(map) => map,
        _ => {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "{}: arguments must be a dict",
                spec.name
            )))));
        }
    };
    for (param, schema) in params.iter() {
        if is_optional_param(schema) {
            continue;
        }
        if !args.contains_key(param) {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "{}: missing required argument {param:?}",
                spec.name
            )))));
        }
    }
    Ok(())
}

fn is_optional_param(schema: &VmValue) -> bool {
    schema
        .as_dict()
        .and_then(|map| map.get("required"))
        .is_some_and(|value| matches!(value, VmValue::Bool(false)))
}

fn synthesized_tool_dry_run_result(spec: &SynthesizedToolSpec, call_args: VmValue) -> VmValue {
    let mut result = BTreeMap::new();
    result.insert(
        "_type".to_string(),
        VmValue::String(Rc::from("synthesized_tool_result")),
    );
    result.insert("status".to_string(), VmValue::String(Rc::from("dry_run")));
    result.insert(
        "tool_id".to_string(),
        VmValue::String(Rc::from(spec.id.as_str())),
    );
    result.insert(
        "name".to_string(),
        VmValue::String(Rc::from(spec.name.as_str())),
    );
    result.insert(
        "description".to_string(),
        VmValue::String(Rc::from(spec.description.as_str())),
    );
    result.insert("args".to_string(), call_args);
    result.insert(
        "capabilities".to_string(),
        VmValue::List(Rc::new(
            spec.capabilities
                .iter()
                .map(|capability| VmValue::String(Rc::from(capability.as_str())))
                .collect(),
        )),
    );
    result.insert(
        "side_effect_level".to_string(),
        VmValue::String(Rc::from(spec.side_effect_level.as_str())),
    );
    result.insert(
        "message".to_string(),
        VmValue::String(Rc::from(
            "synthesized tool is pinned and validated, but executor is dry_run; \
             set executor: \"host_bridge\" or \"mcp_server\" to dispatch",
        )),
    );
    VmValue::Dict(Rc::new(result))
}

fn synthesized_tool_spec_value(spec: &SynthesizedToolSpec) -> VmValue {
    let mut value = BTreeMap::new();
    value.insert(
        "id".to_string(),
        VmValue::String(Rc::from(spec.id.as_str())),
    );
    value.insert(
        "name".to_string(),
        VmValue::String(Rc::from(spec.name.as_str())),
    );
    value.insert(
        "description".to_string(),
        VmValue::String(Rc::from(spec.description.as_str())),
    );
    value.insert("parameters".to_string(), spec.parameters.clone());
    value.insert("return_type".to_string(), spec.return_type.clone());
    value.insert(
        "capabilities".to_string(),
        VmValue::List(Rc::new(
            spec.capabilities
                .iter()
                .map(|capability| VmValue::String(Rc::from(capability.as_str())))
                .collect(),
        )),
    );
    value.insert(
        "side_effect_level".to_string(),
        VmValue::String(Rc::from(spec.side_effect_level.as_str())),
    );
    match &spec.executor {
        SynthesizedToolExecutor::DryRun => {
            value.insert("executor".to_string(), VmValue::String(Rc::from("dry_run")));
        }
        SynthesizedToolExecutor::HostBridge { host_tool } => {
            value.insert(
                "executor".to_string(),
                VmValue::String(Rc::from("host_bridge")),
            );
            value.insert(
                "host_tool".to_string(),
                VmValue::String(Rc::from(host_tool.as_str())),
            );
        }
        SynthesizedToolExecutor::McpServer {
            tool_name,
            server_name,
            ..
        } => {
            value.insert(
                "executor".to_string(),
                VmValue::String(Rc::from("mcp_server")),
            );
            value.insert(
                "mcp_tool".to_string(),
                VmValue::String(Rc::from(tool_name.as_str())),
            );
            if let Some(server_name) = server_name {
                value.insert(
                    "mcp_server".to_string(),
                    VmValue::String(Rc::from(server_name.as_str())),
                );
            }
        }
    }
    VmValue::Dict(Rc::new(value))
}

fn synthesized_tool_hash(spec: &SynthesizedToolSpec) -> String {
    let json = serde_json::json!({
        "name": spec.name,
        "description": spec.description,
        "parameters": crate::llm::vm_value_to_json(&spec.parameters),
        "return_type": crate::llm::vm_value_to_json(&spec.return_type),
        "capabilities": spec.capabilities,
        "side_effect_level": spec.side_effect_level,
        "executor": synthesized_executor_hash_value(&spec.executor),
    });
    let hash = blake3::hash(json.to_string().as_bytes());
    format!("tool_synth_{}", &hash.to_hex()[..16])
}

fn synthesized_executor_hash_value(executor: &SynthesizedToolExecutor) -> serde_json::Value {
    match executor {
        SynthesizedToolExecutor::DryRun => serde_json::json!({"kind": "dry_run"}),
        SynthesizedToolExecutor::HostBridge { host_tool } => {
            serde_json::json!({"kind": "host_bridge", "host_tool": host_tool})
        }
        SynthesizedToolExecutor::McpServer {
            tool_name,
            server_name,
            ..
        } => {
            serde_json::json!({
                "kind": "mcp_server",
                "tool_name": tool_name,
                "server_name": server_name,
            })
        }
    }
}

fn required_string(
    config: &BTreeMap<String, VmValue>,
    builtin: &str,
    key: &str,
) -> Result<String, VmError> {
    match config.get(key) {
        Some(VmValue::String(value)) if !value.trim().is_empty() => Ok(value.to_string()),
        _ => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{builtin}: {key} must be a non-empty string"
        ))))),
    }
}

fn optional_string(config: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    match config.get(key) {
        Some(VmValue::String(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn optional_string_list(
    config: &BTreeMap<String, VmValue>,
    key: &str,
) -> Result<Vec<String>, VmError> {
    let Some(value) = config.get(key) else {
        return Ok(Vec::new());
    };
    let VmValue::List(items) = value else {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "tool_synthesize: {key} must be a list of strings"
        )))));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items.iter() {
        match item {
            VmValue::String(value) if !value.trim().is_empty() => out.push(value.to_string()),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "tool_synthesize: {key} must be a list of non-empty strings"
                )))));
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn synthesize_tool_name(description: &str) -> String {
    let mut name = String::from("synth_");
    let mut prev_underscore = false;
    for ch in description.chars() {
        if ch.is_ascii_alphanumeric() {
            if prev_underscore && !name.ends_with('_') {
                name.push('_');
            }
            name.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else {
            prev_underscore = true;
        }
        if name.len() >= 64 {
            break;
        }
    }
    while name.ends_with('_') {
        name.pop();
    }
    if name == "synth" {
        name.push_str("_tool");
    }
    name
}

fn validate_tool_name(name: &str) -> Result<(), VmError> {
    let mut chars = name.chars();
    let valid_start = chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_');
    let valid_rest = chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    if valid_start && valid_rest {
        return Ok(());
    }
    Err(VmError::Thrown(VmValue::String(Rc::from(format!(
        "tool_synthesize: invalid tool name {name:?}; use ASCII letters, digits, and underscores"
    )))))
}

fn infer_side_effect_level(capabilities: &[String]) -> String {
    if capabilities.iter().any(|capability| {
        capability.starts_with("http")
            || capability.starts_with("oauth")
            || capability.starts_with("network")
            || capability.starts_with("connector")
            || capability.starts_with("mcp")
    }) {
        "network".to_string()
    } else if capabilities.iter().any(|capability| {
        capability.starts_with("process")
            || capability.starts_with("exec")
            || capability.starts_with("shell")
    }) {
        "process_exec".to_string()
    } else if capabilities.iter().any(|capability| {
        capability.starts_with("workspace.write")
            || capability.starts_with("workspace.apply")
            || capability.starts_with("workspace.delete")
    }) {
        "workspace_write".to_string()
    } else if capabilities.is_empty() {
        "none".to_string()
    } else {
        "read_only".to_string()
    }
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
