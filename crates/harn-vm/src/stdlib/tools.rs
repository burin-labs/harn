use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::json::json_to_vm_value;

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

    vm.register_builtin("tool_add", |args, _out| {
        if args.len() < 4 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_add: requires registry, name, description, and handler",
            ))));
        }

        let registry = match &args[0] {
            VmValue::Dict(map) => (**map).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_add: first argument must be a tool registry",
                ))));
            }
        };

        match registry.get("_type") {
            Some(VmValue::String(t)) if &**t == "tool_registry" => {}
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_add: first argument must be a tool registry",
                ))));
            }
        }

        let name = args[1].display();
        let description = args[2].display();
        let handler = args[3].clone();
        let parameters = if args.len() > 4 {
            args[4].clone()
        } else {
            VmValue::Dict(Rc::new(BTreeMap::new()))
        };

        let mut tool_entry = BTreeMap::new();
        tool_entry.insert("name".to_string(), VmValue::String(Rc::from(name.as_str())));
        tool_entry.insert(
            "description".to_string(),
            VmValue::String(Rc::from(description)),
        );
        tool_entry.insert("handler".to_string(), handler);
        tool_entry.insert("parameters".to_string(), parameters);

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
                if let Some(name) = entry.get("name") {
                    desc.insert("name".to_string(), name.clone());
                }
                if let Some(description) = entry.get("description") {
                    desc.insert("description".to_string(), description.clone());
                }
                if let Some(parameters) = entry.get("parameters") {
                    desc.insert("parameters".to_string(), parameters.clone());
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

        let mut tool_infos: Vec<(String, String, String)> = Vec::new();
        for tool in tools {
            if let VmValue::Dict(entry) = tool {
                let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
                let description = entry
                    .get("description")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let params_str = vm_format_parameters(entry.get("parameters"));
                tool_infos.push((name, params_str, description));
            }
        }

        tool_infos.sort_by(|a, b| a.0.cmp(&b.0));

        let mut lines = vec!["Available tools:".to_string()];
        for (name, params, desc) in &tool_infos {
            lines.push(format!("- {name}({params}): {desc}"));
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

                let mut tool_def = BTreeMap::new();
                tool_def.insert("name".to_string(), VmValue::String(Rc::from(name)));
                tool_def.insert(
                    "description".to_string(),
                    VmValue::String(Rc::from(description)),
                );
                tool_def.insert("inputSchema".to_string(), input_schema);
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

    // tool_define(registry, name, description, config) -> registry
    // config is {params: {name: {type, description, required?, default?}}, handler: fn}
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
                    "tool_define: config must be a dict with params and handler",
                ))));
            }
        };

        let handler = config.get("handler").cloned().unwrap_or(VmValue::Nil);

        let parameters = config
            .get("params")
            .cloned()
            .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));

        let mut tool_entry = BTreeMap::new();
        tool_entry.insert("name".to_string(), VmValue::String(Rc::from(name.as_str())));
        tool_entry.insert(
            "description".to_string(),
            VmValue::String(Rc::from(description)),
        );
        tool_entry.insert("handler".to_string(), handler);
        tool_entry.insert("parameters".to_string(), parameters);

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

            prompt.push_str(&format!("### {name}\n"));
            prompt.push_str(&format!("{description}\n"));
            if !params_str.is_empty() {
                prompt.push_str(&format!("Parameters: {params_str}\n"));
            }
            prompt.push('\n');
        }

        Ok(VmValue::String(Rc::from(prompt.trim_end())))
    });
}

// =============================================================================
// Tool registry helpers
// =============================================================================

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
