use std::collections::BTreeMap;

use harn_runtime::{Interpreter, RuntimeError, Value};

use crate::json::json_parse;

/// Register tool registry builtins on an interpreter.
pub fn register_tool_builtins(interp: &mut Interpreter) {
    // tool_registry() -> Dict
    // Creates a new empty tool registry.
    interp.register_builtin("tool_registry", |_args, _out| {
        let mut registry = BTreeMap::new();
        registry.insert(
            "_type".to_string(),
            Value::String("tool_registry".to_string()),
        );
        registry.insert("tools".to_string(), Value::List(Vec::new()));
        Ok(Value::Dict(registry))
    });

    // tool_add(registry, name, description, handler, parameters?) -> Dict
    // Adds a tool to the registry and returns the updated registry.
    interp.register_builtin("tool_add", |args, _out| {
        if args.len() < 4 {
            return Err(RuntimeError::thrown(
                "tool_add: requires registry, name, description, and handler",
            ));
        }

        let registry = match &args[0] {
            Value::Dict(map) => map.clone(),
            _ => {
                return Err(RuntimeError::thrown(
                    "tool_add: first argument must be a tool registry",
                ))
            }
        };

        // Validate it's a tool registry
        match registry.get("_type") {
            Some(Value::String(t)) if t == "tool_registry" => {}
            _ => {
                return Err(RuntimeError::thrown(
                    "tool_add: first argument must be a tool registry",
                ))
            }
        }

        let name = args[1].as_string();
        let description = args[2].as_string();
        let handler = args[3].clone();
        let parameters = if args.len() > 4 {
            args[4].clone()
        } else {
            Value::Dict(BTreeMap::new())
        };

        // Build the tool entry
        let mut tool_entry = BTreeMap::new();
        tool_entry.insert("name".to_string(), Value::String(name));
        tool_entry.insert("description".to_string(), Value::String(description));
        tool_entry.insert("handler".to_string(), handler);
        tool_entry.insert("parameters".to_string(), parameters);

        // Get the existing tools list and add the new tool
        let mut tools = match registry.get("tools") {
            Some(Value::List(list)) => list.clone(),
            _ => Vec::new(),
        };
        tools.push(Value::Dict(tool_entry));

        // Build the updated registry
        let mut new_registry = registry;
        new_registry.insert("tools".to_string(), Value::List(tools));
        Ok(Value::Dict(new_registry))
    });

    // tool_list(registry) -> List
    // Returns a list of tool descriptions (without handlers).
    interp.register_builtin("tool_list", |args, _out| {
        let registry = match args.first() {
            Some(Value::Dict(map)) => map,
            _ => return Err(RuntimeError::thrown("tool_list: requires a tool registry")),
        };

        let tools = match registry.get("tools") {
            Some(Value::List(list)) => list,
            _ => return Ok(Value::List(Vec::new())),
        };

        let mut result = Vec::new();
        for tool in tools {
            if let Value::Dict(entry) = tool {
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
                result.push(Value::Dict(desc));
            }
        }
        Ok(Value::List(result))
    });

    // tool_find(registry, name) -> Dict | Nil
    // Finds a tool by name and returns it (including handler), or nil.
    interp.register_builtin("tool_find", |args, _out| {
        if args.len() < 2 {
            return Err(RuntimeError::thrown(
                "tool_find: requires registry and name",
            ));
        }

        let registry = match &args[0] {
            Value::Dict(map) => map,
            _ => {
                return Err(RuntimeError::thrown(
                    "tool_find: first argument must be a tool registry",
                ))
            }
        };

        let target_name = args[1].as_string();

        let tools = match registry.get("tools") {
            Some(Value::List(list)) => list,
            _ => return Ok(Value::Nil),
        };

        for tool in tools {
            if let Value::Dict(entry) = tool {
                if let Some(Value::String(name)) = entry.get("name") {
                    if name == &target_name {
                        return Ok(tool.clone());
                    }
                }
            }
        }
        Ok(Value::Nil)
    });

    // tool_describe(registry) -> String
    // Returns a formatted description of all tools for LLM system prompts.
    // Tools are sorted alphabetically by name.
    interp.register_builtin("tool_describe", |args, _out| {
        let registry = match args.first() {
            Some(Value::Dict(map)) => map,
            _ => {
                return Err(RuntimeError::thrown(
                    "tool_describe: requires a tool registry",
                ))
            }
        };

        let tools = match registry.get("tools") {
            Some(Value::List(list)) => list,
            _ => return Ok(Value::String("Available tools:\n(none)".to_string())),
        };

        if tools.is_empty() {
            return Ok(Value::String("Available tools:\n(none)".to_string()));
        }

        // Collect tool info for sorting
        let mut tool_infos: Vec<(String, String, String)> = Vec::new();
        for tool in tools {
            if let Value::Dict(entry) = tool {
                let name = entry.get("name").map(|v| v.as_string()).unwrap_or_default();
                let description = entry
                    .get("description")
                    .map(|v| v.as_string())
                    .unwrap_or_default();
                let params_str = format_parameters(entry.get("parameters"));
                tool_infos.push((name, params_str, description));
            }
        }

        // Sort alphabetically by name
        tool_infos.sort_by(|a, b| a.0.cmp(&b.0));

        let mut lines = vec!["Available tools:".to_string()];
        for (name, params, desc) in &tool_infos {
            lines.push(format!("- {name}({params}): {desc}"));
        }

        Ok(Value::String(lines.join("\n")))
    });

    // tool_remove(registry, name) -> Dict
    // Removes a tool by name and returns the updated registry.
    interp.register_builtin("tool_remove", |args, _out| {
        if args.len() < 2 {
            return Err(RuntimeError::thrown(
                "tool_remove: requires registry and name",
            ));
        }

        let registry = match &args[0] {
            Value::Dict(map) => map.clone(),
            _ => {
                return Err(RuntimeError::thrown(
                    "tool_remove: first argument must be a tool registry",
                ))
            }
        };

        let target_name = args[1].as_string();

        let tools = match registry.get("tools") {
            Some(Value::List(list)) => list.clone(),
            _ => Vec::new(),
        };

        let filtered: Vec<Value> = tools
            .into_iter()
            .filter(|tool| {
                if let Value::Dict(entry) = tool {
                    if let Some(Value::String(name)) = entry.get("name") {
                        return name != &target_name;
                    }
                }
                true
            })
            .collect();

        let mut new_registry = registry;
        new_registry.insert("tools".to_string(), Value::List(filtered));
        Ok(Value::Dict(new_registry))
    });

    // tool_count(registry) -> Int
    // Returns the number of tools in the registry.
    interp.register_builtin("tool_count", |args, _out| {
        let registry = match args.first() {
            Some(Value::Dict(map)) => map,
            _ => return Err(RuntimeError::thrown("tool_count: requires a tool registry")),
        };

        let count = match registry.get("tools") {
            Some(Value::List(list)) => list.len(),
            _ => 0,
        };

        Ok(Value::Int(count as i64))
    });

    // tool_schema(registry) -> Dict
    // Generates a JSON Schema / OpenAPI-compatible tool schema for all tools.
    // Output follows the universal inputSchema convention with $ref support.
    // Format:
    // {
    //   schema_version: "harn-tools/1.0",
    //   components: { schemas: { ... } },   // reusable type definitions
    //   tools: [
    //     {
    //       name: "...",
    //       description: "...",
    //       inputSchema: {
    //         type: "object",
    //         properties: { param: { type: "string", description: "..." } },
    //         required: [...]
    //       }
    //     }
    //   ]
    // }
    interp.register_builtin("tool_schema", |args, _out| {
        let registry = match args.first() {
            Some(Value::Dict(map)) => map,
            _ => {
                return Err(RuntimeError::thrown(
                    "tool_schema: requires a tool registry",
                ))
            }
        };

        // Optional second arg: components dict for $ref resolution
        let components = args.get(1).and_then(|v| v.as_dict()).cloned();

        let tools = match registry.get("tools") {
            Some(Value::List(list)) => list,
            _ => return Ok(Value::Dict(build_empty_schema())),
        };

        let mut tool_schemas = Vec::new();
        for tool in tools {
            if let Value::Dict(entry) = tool {
                let name = entry.get("name").map(|v| v.as_string()).unwrap_or_default();
                let description = entry
                    .get("description")
                    .map(|v| v.as_string())
                    .unwrap_or_default();

                // Build inputSchema from parameters
                let input_schema = build_input_schema(entry.get("parameters"), components.as_ref());

                let mut tool_def = BTreeMap::new();
                tool_def.insert("name".to_string(), Value::String(name));
                tool_def.insert("description".to_string(), Value::String(description));
                tool_def.insert("inputSchema".to_string(), input_schema);
                tool_schemas.push(Value::Dict(tool_def));
            }
        }

        let mut schema = BTreeMap::new();
        schema.insert(
            "schema_version".to_string(),
            Value::String("harn-tools/1.0".to_string()),
        );

        // Include components if provided (for $ref documentation)
        if let Some(comps) = &components {
            let mut comp_wrapper = BTreeMap::new();
            comp_wrapper.insert("schemas".to_string(), Value::Dict(comps.clone()));
            schema.insert("components".to_string(), Value::Dict(comp_wrapper));
        }

        schema.insert("tools".to_string(), Value::List(tool_schemas));
        Ok(Value::Dict(schema))
    });

    // tool_parse_call(text) -> Dict | Nil
    // Parses a tool call from LLM output text using the universal
    // <tool_call>{"name": "...", "arguments": {...}}</tool_call> format.
    // Returns {name: "...", arguments: {...}} or nil if no tool call found.
    interp.register_builtin("tool_parse_call", |args, _out| {
        let text = args.first().map(|a| a.as_string()).unwrap_or_default();

        let mut results = Vec::new();
        let mut search_from = 0;

        while let Some(start) = text[search_from..].find("<tool_call>") {
            let abs_start = search_from + start + "<tool_call>".len();
            if let Some(end) = text[abs_start..].find("</tool_call>") {
                let json_str = text[abs_start..abs_start + end].trim();
                if let Ok(Value::Dict(call)) = json_parse(json_str) {
                    results.push(Value::Dict(call));
                }
                search_from = abs_start + end + "</tool_call>".len();
            } else {
                break;
            }
        }

        if results.is_empty() {
            Ok(Value::Nil)
        } else if results.len() == 1 {
            Ok(results.into_iter().next().unwrap())
        } else {
            Ok(Value::List(results))
        }
    });

    // tool_format_result(name, result) -> String
    // Formats a tool result for feeding back to the LLM.
    // Output: <tool_result>{"name": "...", "result": "..."}</tool_result>
    interp.register_builtin("tool_format_result", |args, _out| {
        let name = args.first().map(|a| a.as_string()).unwrap_or_default();
        let result = args
            .get(1)
            .map(|a| a.as_string())
            .unwrap_or("nil".to_string());

        let json_name = escape_json_str(&name);
        let json_result = escape_json_str(&result);
        Ok(Value::String(format!(
            "<tool_result>{{\"name\": \"{json_name}\", \"result\": \"{json_result}\"}}</tool_result>"
        )))
    });

    // tool_prompt(registry, components?) -> String
    // Generates a complete system prompt section describing tools and the
    // calling convention. This is the universal function-calling prompt that
    // works with any LLM, regardless of native tool support.
    interp.register_builtin("tool_prompt", |args, _out| {
        let registry = match args.first() {
            Some(Value::Dict(map)) => map,
            _ => {
                return Err(RuntimeError::thrown(
                    "tool_prompt: requires a tool registry",
                ))
            }
        };

        let tools = match registry.get("tools") {
            Some(Value::List(list)) => list,
            _ => {
                return Ok(Value::String(
                    "No tools are available.".to_string(),
                ))
            }
        };

        if tools.is_empty() {
            return Ok(Value::String("No tools are available.".to_string()));
        }

        let mut prompt = String::from("# Available Tools\n\n");
        prompt.push_str("You have access to the following tools. To use a tool, output a tool call in this exact format:\n\n");
        prompt.push_str("<tool_call>{\"name\": \"tool_name\", \"arguments\": {\"param\": \"value\"}}</tool_call>\n\n");
        prompt.push_str("You may make multiple tool calls in a single response. Wait for tool results before proceeding.\n\n");
        prompt.push_str("## Tools\n\n");

        // Collect and sort tools
        let mut tool_infos: Vec<(&BTreeMap<String, Value>, String)> = Vec::new();
        for tool in tools {
            if let Value::Dict(entry) = tool {
                let name = entry.get("name").map(|v| v.as_string()).unwrap_or_default();
                tool_infos.push((entry, name));
            }
        }
        tool_infos.sort_by(|a, b| a.1.cmp(&b.1));

        for (entry, name) in &tool_infos {
            let description = entry
                .get("description")
                .map(|v| v.as_string())
                .unwrap_or_default();
            let params_str = format_parameters(entry.get("parameters"));

            prompt.push_str(&format!("### {name}\n"));
            prompt.push_str(&format!("{description}\n"));
            if !params_str.is_empty() {
                prompt.push_str(&format!("Parameters: {params_str}\n"));
            }
            prompt.push('\n');
        }

        Ok(Value::String(prompt.trim_end().to_string()))
    });
}

/// Format a parameters dict into a string like "query: string, limit: int".
/// Parameters are sorted alphabetically for deterministic output.
fn format_parameters(params: Option<&Value>) -> String {
    match params {
        Some(Value::Dict(map)) if !map.is_empty() => {
            let mut pairs: Vec<(String, String)> = map
                .iter()
                .map(|(k, v)| (k.clone(), v.as_string()))
                .collect();
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

/// Build an empty schema structure.
fn build_empty_schema() -> BTreeMap<String, Value> {
    let mut schema = BTreeMap::new();
    schema.insert(
        "schema_version".to_string(),
        Value::String("harn-tools/1.0".to_string()),
    );
    schema.insert("tools".to_string(), Value::List(Vec::new()));
    schema
}

/// Build a JSON-Schema-style inputSchema from a parameters dict.
/// Resolves $ref references against the components dict.
fn build_input_schema(
    params: Option<&Value>,
    components: Option<&BTreeMap<String, Value>>,
) -> Value {
    let mut schema = BTreeMap::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));

    let params_map = match params {
        Some(Value::Dict(map)) if !map.is_empty() => map,
        _ => {
            schema.insert("properties".to_string(), Value::Dict(BTreeMap::new()));
            return Value::Dict(schema);
        }
    };

    let mut properties = BTreeMap::new();
    let mut required = Vec::new();

    for (key, val) in params_map {
        let prop = resolve_param_type(val, components);
        properties.insert(key.clone(), prop);
        required.push(Value::String(key.clone()));
    }

    schema.insert("properties".to_string(), Value::Dict(properties));
    if !required.is_empty() {
        required.sort_by_key(|a| a.as_string());
        schema.insert("required".to_string(), Value::List(required));
    }

    Value::Dict(schema)
}

/// Resolve a parameter type value into a JSON Schema property.
/// Supports:
/// - Simple string types: "string", "int", "float", "bool"
/// - $ref references: {"$ref": "Address"} → resolves from components
/// - Full schema dicts: {"type": "string", "description": "..."}
fn resolve_param_type(val: &Value, components: Option<&BTreeMap<String, Value>>) -> Value {
    match val {
        // Simple type string: "string" -> {"type": "string"}
        Value::String(type_name) => {
            let json_type = harn_type_to_json_schema(type_name);
            let mut prop = BTreeMap::new();
            prop.insert("type".to_string(), Value::String(json_type.to_string()));
            Value::Dict(prop)
        }
        // Dict: could be a $ref or a full schema
        Value::Dict(map) => {
            // Check for $ref
            if let Some(Value::String(ref_name)) = map.get("$ref") {
                // Resolve from components
                if let Some(comps) = components {
                    if let Some(resolved) = comps.get(ref_name) {
                        return resolved.clone();
                    }
                }
                // Unresolved $ref: keep as-is for documentation
                let mut prop = BTreeMap::new();
                prop.insert(
                    "$ref".to_string(),
                    Value::String(format!("#/components/schemas/{ref_name}")),
                );
                Value::Dict(prop)
            } else {
                // Full schema dict passed through
                Value::Dict(map.clone())
            }
        }
        _ => {
            let mut prop = BTreeMap::new();
            prop.insert("type".to_string(), Value::String("string".to_string()));
            Value::Dict(prop)
        }
    }
}

/// Map Harn type names to JSON Schema types.
fn harn_type_to_json_schema(harn_type: &str) -> &str {
    match harn_type {
        "int" => "integer",
        "float" => "number",
        "bool" | "boolean" => "boolean",
        "list" | "array" => "array",
        "dict" | "object" => "object",
        _ => "string",
    }
}

/// Escape a string for safe embedding in JSON.
fn escape_json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}
