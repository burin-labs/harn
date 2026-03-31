use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};

// =============================================================================
// Built-in tool schemas
// =============================================================================

/// Built-in tool schemas for common agent tools. Maps short names
/// (as used in Harn pipelines) to full OpenAI-compatible tool definitions.
pub(crate) fn builtin_tool_schema(name: &str) -> Option<serde_json::Value> {
    match name {
        "read" | "read_file" => Some(serde_json::json!({
            "name": "read_file",
            "description": "Read the contents of a file. Use this to understand code before modifying it.",
            "parameters": {
                "path": {"type": "string", "description": "Relative file path to read"}
            }
        })),
        "search" => Some(serde_json::json!({
            "name": "search",
            "description": "Search for a text pattern across project files. Returns matching lines with file paths.",
            "parameters": {
                "pattern": {"type": "string", "description": "Search pattern (regex supported)"},
                "file_glob": {"type": "string", "description": "Optional glob to filter files (e.g. \"**/*.py\")"}
            }
        })),
        "edit" => Some(serde_json::json!({
            "name": "edit",
            "description": "Create a file or make targeted edits. Use action=\"create\" with content for new/replacement files (set overwrite=true for existing). Use action=\"patch\" with old_string/new_string for precise find-and-replace.",
            "parameters": {
                "action": {"type": "string", "description": "create (write full file) or patch (find/replace)"},
                "path": {"type": "string", "description": "File path"},
                "content": {"type": "string", "description": "Full file content (for create action)"},
                "old_string": {"type": "string", "description": "For patch: exact text to find"},
                "new_string": {"type": "string", "description": "For patch: replacement text"},
                "overwrite": {"type": "boolean", "description": "Set true to overwrite existing file (for create action)"}
            }
        })),
        "run" | "exec" => Some(serde_json::json!({
            "name": "run",
            "description": "Execute a shell command and return its output.",
            "parameters": {
                "command": {"type": "string", "description": "Shell command to execute"}
            }
        })),
        "outline" | "get_file_outline" => Some(serde_json::json!({
            "name": "outline",
            "description": "Get the structural outline of a file (function/class signatures).",
            "parameters": {
                "path": {"type": "string", "description": "File path to outline"}
            }
        })),
        "web_search" => Some(serde_json::json!({
            "name": "web_search",
            "description": "Search the web for information.",
            "parameters": {
                "query": {"type": "string", "description": "Search query"}
            }
        })),
        "web_fetch" => Some(serde_json::json!({
            "name": "web_fetch",
            "description": "Fetch content from a URL.",
            "parameters": {
                "url": {"type": "string", "description": "URL to fetch"}
            }
        })),
        "lsp_hover" => Some(serde_json::json!({
            "name": "lsp_hover",
            "description": "Get type info and documentation for a symbol at a position.",
            "parameters": {
                "file": {"type": "string", "description": "File path"},
                "line": {"type": "integer", "description": "Line number (1-based)"},
                "col": {"type": "integer", "description": "Column number (1-based)"}
            }
        })),
        "lsp_definition" => Some(serde_json::json!({
            "name": "lsp_definition",
            "description": "Jump to the definition of a symbol.",
            "parameters": {
                "file": {"type": "string", "description": "File path"},
                "line": {"type": "integer", "description": "Line number (1-based)"},
                "col": {"type": "integer", "description": "Column number (1-based)"}
            }
        })),
        "lsp_references" => Some(serde_json::json!({
            "name": "lsp_references",
            "description": "Find all references to a symbol.",
            "parameters": {
                "file": {"type": "string", "description": "File path"},
                "line": {"type": "integer", "description": "Line number (1-based)"},
                "col": {"type": "integer", "description": "Column number (1-based)"}
            }
        })),
        "list_directory" => Some(serde_json::json!({
            "name": "list_directory",
            "description": "List directory contents.",
            "parameters": {
                "path": {"type": "string", "description": "Directory path"}
            }
        })),
        _ => None,
    }
}

/// Convert a list of tool name strings (e.g. ["read", "search", "edit"]) into
/// full tool definitions suitable for the LLM API.
pub(crate) fn tool_names_to_schemas(names: &[String], provider: &str) -> Vec<serde_json::Value> {
    let mut tools = Vec::new();
    for name in names {
        if let Some(schema) = builtin_tool_schema(name) {
            let tool_name = schema["name"].as_str().unwrap_or(name);
            let description = schema["description"].as_str().unwrap_or("");
            let params = &schema["parameters"];
            let input_schema = vm_build_json_schema_from_json(params);

            match provider {
                "openai" | "openrouter" => {
                    tools.push(serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": tool_name,
                            "description": description,
                            "parameters": input_schema,
                        }
                    }));
                }
                _ => {
                    // Anthropic format
                    tools.push(serde_json::json!({
                        "name": tool_name,
                        "description": description,
                        "input_schema": input_schema,
                    }));
                }
            }
        }
    }
    tools
}

/// Build a JSON Schema object from a parameters JSON value.
fn vm_build_json_schema_from_json(params: &serde_json::Value) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    if let Some(obj) = params.as_object() {
        for (name, type_val) in obj {
            let type_str = type_val
                .as_object()
                .and_then(|o| o.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("string");
            let desc = type_val
                .as_object()
                .and_then(|o| o.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let mut prop = serde_json::json!({"type": type_str});
            if !desc.is_empty() {
                prop["description"] = serde_json::json!(desc);
            }
            properties.insert(name.clone(), prop);

            // First parameter is always required
            if required.is_empty() {
                required.push(serde_json::json!(name));
            }
        }
    }

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

/// Build an assistant message with tool_calls for the conversation history.
/// Format varies by provider (OpenAI vs Anthropic).
pub(crate) fn build_assistant_tool_message(
    text: &str,
    tool_calls: &[serde_json::Value],
    provider: &str,
) -> serde_json::Value {
    match provider {
        "openai" | "openrouter" => {
            // OpenAI format: assistant message with tool_calls array
            let calls: Vec<serde_json::Value> = tool_calls
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc["id"],
                        "type": "function",
                        "function": {
                            "name": tc["name"],
                            "arguments": serde_json::to_string(&tc["arguments"]).unwrap_or_default(),
                        }
                    })
                })
                .collect();
            let mut msg = serde_json::json!({
                "role": "assistant",
                "tool_calls": calls,
            });
            if !text.is_empty() {
                msg["content"] = serde_json::json!(text);
            }
            msg
        }
        _ => {
            // Anthropic format: content blocks with text and tool_use
            let mut content = Vec::new();
            if !text.is_empty() {
                content.push(serde_json::json!({"type": "text", "text": text}));
            }
            for tc in tool_calls {
                content.push(serde_json::json!({
                    "type": "tool_use",
                    "id": tc["id"],
                    "name": tc["name"],
                    "input": tc["arguments"],
                }));
            }
            serde_json::json!({"role": "assistant", "content": content})
        }
    }
}

/// Build a tool result message for the conversation history.
pub(crate) fn build_tool_result_message(
    tool_call_id: &str,
    result: &str,
    provider: &str,
) -> serde_json::Value {
    match provider {
        "openai" | "openrouter" => {
            serde_json::json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "content": result,
            })
        }
        _ => {
            // Anthropic: tool_result inside a user message
            serde_json::json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_call_id,
                    "content": result,
                }]
            })
        }
    }
}

/// Resolve tools from any supported format:
/// - String list: `["read", "search", "edit"]` -> static schema lookup
/// - tool_registry: `{_type: "tool_registry", tools: [...]}` -> extract schemas
/// - List of tool dicts: `[{name, description, parameters}, ...]` -> use directly
pub(crate) fn resolve_tools_for_agent(
    val: &VmValue,
    provider: &str,
) -> Result<Option<Vec<serde_json::Value>>, VmError> {
    match val {
        VmValue::List(list) if list.is_empty() => Ok(None),
        VmValue::List(list) => {
            // Check if this is a list of strings or a list of dicts
            if matches!(list.first(), Some(VmValue::String(_))) {
                // String name list -> static schema lookup
                let names: Vec<String> = list.iter().map(|v| v.display()).collect();
                let schemas = tool_names_to_schemas(&names, provider);
                Ok(if schemas.is_empty() {
                    None
                } else {
                    Some(schemas)
                })
            } else {
                // List of tool definition dicts -> use vm_tools_to_native
                let schemas = vm_tools_to_native(val, provider)?;
                Ok(if schemas.is_empty() {
                    None
                } else {
                    Some(schemas)
                })
            }
        }
        VmValue::Dict(d)
            if d.get("_type").map(|v| v.display()).as_deref() == Some("tool_registry") =>
        {
            // tool_registry object -> extract tool schemas via vm_tools_to_native
            let schemas = vm_tools_to_native(val, provider)?;
            Ok(if schemas.is_empty() {
                None
            } else {
                Some(schemas)
            })
        }
        _ => Ok(None),
    }
}

/// Normalize tool call arguments before dispatch.
/// Handles alias mapping so tool schemas and host implementations stay consistent
/// regardless of which parameter names the model chooses.
pub(crate) fn normalize_tool_args(name: &str, args: &serde_json::Value) -> serde_json::Value {
    let mut obj = match args.as_object() {
        Some(o) => o.clone(),
        None => return args.clone(),
    };

    if name == "edit" {
        // Normalize action aliases: mode, command -> action
        if !obj.contains_key("action") {
            if let Some(v) = obj.remove("mode").or_else(|| obj.remove("command")) {
                obj.insert("action".to_string(), v);
            }
        }

        // For patch actions: normalize find->old_string, content->new_string
        let action = obj
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if action == "patch" || action == "replace" {
            if !obj.contains_key("old_string") {
                if let Some(v) = obj.remove("find") {
                    obj.insert("old_string".to_string(), v);
                }
            }
            if !obj.contains_key("new_string") {
                if let Some(v) = obj.remove("content") {
                    obj.insert("new_string".to_string(), v);
                }
            }
        }

        // Normalize file->path alias
        if !obj.contains_key("path") {
            if let Some(v) = obj.remove("file") {
                obj.insert("path".to_string(), v);
            }
        }
    }

    serde_json::Value::Object(obj)
}

/// Handle read-only tools locally in the VM without bridging to the host.
/// This reduces latency and split-brain for passive operations.
pub(crate) fn handle_tool_locally(name: &str, args: &serde_json::Value) -> Option<String> {
    match name {
        "read_file" | "read" => {
            let path = args
                .get("path")
                .or_else(|| args.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if path.is_empty() {
                return Some("Error: missing path parameter".to_string());
            }
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    // Add line numbers like the Swift read_file does
                    let numbered: String = content
                        .lines()
                        .enumerate()
                        .map(|(i, line)| format!("{}\t{}", i + 1, line))
                        .collect::<Vec<_>>()
                        .join("\n");
                    Some(numbered)
                }
                Err(e) => Some(format!("Error: cannot read file '{}': {}", path, e)),
            }
        }
        "list_directory" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            match std::fs::read_dir(path) {
                Ok(entries) => {
                    let mut names: Vec<String> = entries
                        .filter_map(|e| e.ok())
                        .map(|e| {
                            let name = e.file_name().to_string_lossy().to_string();
                            if e.path().is_dir() {
                                format!("{}/", name)
                            } else {
                                name
                            }
                        })
                        .collect();
                    names.sort();
                    Some(names.join("\n"))
                }
                Err(e) => Some(format!("Error: cannot list directory '{}': {}", path, e)),
            }
        }
        _ => None,
    }
}

/// Extract (name, description, [(param_name, type, description)]) from a JSON tool schema.
fn extract_tool_info(
    schema: &serde_json::Value,
) -> (String, String, Vec<(String, String, String)>) {
    let name = schema["name"].as_str().unwrap_or("").to_string();
    let desc = schema["description"].as_str().unwrap_or("").to_string();
    let mut params = Vec::new();
    if let Some(obj) = schema["parameters"].as_object() {
        for (pname, pval) in obj {
            let ptype = pval
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("string");
            let type_str = match ptype {
                "string" => "str",
                "integer" => "int",
                "boolean" => "bool",
                other => other,
            };
            let pdesc = pval
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            params.push((pname.clone(), type_str.to_string(), pdesc.to_string()));
        }
    }
    (name, desc, params)
}

/// Extract parameter info from a Harn VmValue dict (tool_registry entry).
fn extract_params_from_vm_dict(td: &BTreeMap<String, VmValue>) -> Vec<(String, String, String)> {
    let mut params = Vec::new();
    if let Some(VmValue::Dict(pd)) = td.get("parameters") {
        for (pname, pval) in pd.iter() {
            if let VmValue::Dict(pdef) = pval {
                let ptype = pdef
                    .get("type")
                    .map(|v| v.display())
                    .unwrap_or_else(|| "str".to_string());
                let pdesc = pdef
                    .get("description")
                    .map(|v| v.display())
                    .unwrap_or_default();
                params.push((pname.clone(), ptype, pdesc));
            } else {
                // Simple string description
                params.push((pname.clone(), "str".to_string(), pval.display()));
            }
        }
    }
    params
}

/// Build a text-based tool prompt to inject into the system prompt.
/// Always includes tool schema (names + parameter definitions).
/// Includes format instructions only if `include_format` is true
/// (skipped when few-shot examples already demonstrate the format).
pub(crate) fn build_text_tool_prompt(tools_val: Option<&VmValue>, include_format: bool) -> String {
    let mut prompt = String::from("\n\n## Available tools\n\n");

    // Collect tool schemas from any input format:
    // - String list: look up builtin schema by name
    // - tool_registry dict: extract inline schemas
    // - List of tool dicts: use directly
    type ToolSchema = (String, String, Vec<(String, String, String)>);
    let schemas: Vec<ToolSchema> = match tools_val {
        Some(VmValue::List(list)) => list
            .iter()
            .filter_map(|v| match v {
                VmValue::String(name) => {
                    builtin_tool_schema(name).map(|schema| extract_tool_info(&schema))
                }
                VmValue::Dict(td) => {
                    let name = td.get("name")?.display();
                    let desc = td
                        .get("description")
                        .map(|v| v.display())
                        .unwrap_or_default();
                    let params = extract_params_from_vm_dict(td);
                    Some((name, desc, params))
                }
                _ => None,
            })
            .collect(),
        Some(VmValue::Dict(d)) => {
            // tool_registry -- extract from tools list
            if let Some(VmValue::List(tools)) = d.get("tools") {
                tools
                    .iter()
                    .filter_map(|v| {
                        if let VmValue::Dict(td) = v {
                            let name = td.get("name")?.display();
                            let desc = td
                                .get("description")
                                .map(|v| v.display())
                                .unwrap_or_default();
                            let params = extract_params_from_vm_dict(td);
                            Some((name, desc, params))
                        } else {
                            None
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    };

    // Present tools as Python-like function signatures
    for (tool_name, desc, params) in &schemas {
        let sig = params
            .iter()
            .map(|(pname, ptype, _)| format!("{pname}: {ptype}"))
            .collect::<Vec<_>>()
            .join(", ");
        prompt.push_str(&format!("### {tool_name}({sig})\n{desc}\n"));
        for (pname, _, pdesc) in params {
            if !pdesc.is_empty() {
                prompt.push_str(&format!("- `{pname}`: {pdesc}\n"));
            }
        }
        prompt.push('\n');
    }

    if include_format {
        prompt.push_str(
            "\n## How to use tools\n\
             To call a tool, wrap it in a fenced code block with the `call` language tag:\n\
             ````\n\
             ```call\n\
             tool_name(param=\"value\", param2=\"value2\")\n\
             ```\n\
             ````\n\
             For multiline string values (like file content), use triple quotes:\n\
             ````\n\
             ```call\n\
             edit(action=\"create\", path=\"file.py\", content=\"\"\"\n\
             line 1\n\
             line 2\n\
             \"\"\")\n\
             ```\n\
             ````\n\
             You can make multiple tool calls in one response (each in its own block).\n\
             After each call, you will see the result in a <tool_result> tag.\n\
             ALWAYS read files before modifying them.\n",
        );
    }

    prompt
}

/// Parse tool calls from LLM text response.
/// Uses ```call blocks with Python-like function syntax:
///   ```call
///   tool_name(param="value", param2="value2")
///   ```
pub(crate) fn parse_text_tool_calls(text: &str) -> Vec<serde_json::Value> {
    let mut calls = Vec::new();
    let mut search_from = 0;

    while let Some(start_offset) = text[search_from..].find("```call") {
        let after_marker = search_from + start_offset + "```call".len();
        // Skip newline after ```call
        let content_start = if text.as_bytes().get(after_marker) == Some(&b'\n') {
            after_marker + 1
        } else {
            after_marker
        };
        if let Some(end_offset) = text[content_start..].find("```") {
            let content_end = content_start + end_offset;
            let call_text = text[content_start..content_end].trim();
            if let Some((name, arguments)) = parse_function_call_syntax(call_text) {
                calls.push(serde_json::json!({
                    "id": format!("tc_{}", calls.len()),
                    "name": name,
                    "arguments": arguments,
                }));
            }
            search_from = content_end + "```".len();
        } else {
            break;
        }
    }

    calls
}

/// Infer the default parameter name for a positional argument.
/// When the model writes `read_file("foo.py")` instead of `read_file(path="foo.py")`,
/// this maps the positional value to the correct named parameter.
fn default_param_name(tool_name: &str, position: usize) -> &'static str {
    match (tool_name, position) {
        ("read_file" | "read", 0) => "path",
        ("search", 0) => "pattern",
        ("search", 1) => "file_glob",
        ("edit", 0) => "action",
        ("edit", 1) => "path",
        ("edit", 2) => "content",
        ("run" | "exec", 0) => "command",
        ("outline" | "get_file_outline", 0) => "path",
        ("list_directory", 0) => "path",
        ("web_search", 0) => "query",
        ("web_fetch", 0) => "url",
        ("lsp_hover" | "lsp_definition" | "lsp_references", 0) => "file",
        ("lsp_hover" | "lsp_definition" | "lsp_references", 1) => "line",
        ("lsp_hover" | "lsp_definition" | "lsp_references", 2) => "col",
        _ => "arg",
    }
}

/// Parse function-call syntax: `name(key="value", key2="value2")`
/// Also handles positional args: `read_file("foo.py")` -> `{path: "foo.py"}`
fn parse_function_call_syntax(text: &str) -> Option<(String, serde_json::Value)> {
    let text = text.trim();
    let paren_start = text.find('(')?;
    let name = text[..paren_start].trim().to_string();
    if name.is_empty() {
        return None;
    }

    let args_str = text[paren_start + 1..].strip_suffix(')');
    let args_str = args_str?.trim();
    if args_str.is_empty() {
        return Some((name, serde_json::json!({})));
    }

    let mut args = serde_json::Map::new();
    let mut positional_index = 0usize;
    for part in split_call_args(args_str) {
        let part = part.trim();
        if let Some(eq_pos) = part.find('=') {
            let key = part[..eq_pos].trim().to_string();
            let val_str = part[eq_pos + 1..].trim();
            let val = if val_str.starts_with("\"\"\"")
                && val_str.ends_with("\"\"\"")
                && val_str.len() >= 6
            {
                // Triple-quoted string: mostly raw, but process \" -> " and \\ -> \
                // so models can include literal """ inside the block by writing \"\"\".
                // Must be checked BEFORE single-quote to avoid stripping only 1 char.
                let raw = &val_str[3..val_str.len() - 3];
                let unescaped = raw.replace("\\\"", "\"").replace("\\\\", "\\");
                serde_json::json!(unescaped)
            } else if (val_str.starts_with('"') && val_str.ends_with('"'))
                || (val_str.starts_with('\'') && val_str.ends_with('\''))
            {
                let inner = &val_str[1..val_str.len() - 1];
                let unescaped = inner
                    .replace("\\n", "\n")
                    .replace("\\t", "\t")
                    .replace("\\\"", "\"")
                    .replace("\\'", "'")
                    .replace("\\\\", "\\");
                serde_json::json!(unescaped)
            } else if val_str == "true" {
                serde_json::json!(true)
            } else if val_str == "false" {
                serde_json::json!(false)
            } else if let Ok(n) = val_str.parse::<i64>() {
                serde_json::json!(n)
            } else {
                serde_json::json!(val_str)
            };
            args.insert(key, val);
        } else if !part.is_empty() {
            // Positional argument: infer parameter name from tool + position
            let key = default_param_name(&name, positional_index).to_string();
            let val = if part.starts_with("\"\"\"") && part.ends_with("\"\"\"") && part.len() >= 6 {
                let raw = &part[3..part.len() - 3];
                serde_json::json!(raw.replace("\\\"", "\"").replace("\\\\", "\\"))
            } else if (part.starts_with('"') && part.ends_with('"'))
                || (part.starts_with('\'') && part.ends_with('\''))
            {
                let inner = &part[1..part.len() - 1];
                serde_json::json!(inner
                    .replace("\\n", "\n")
                    .replace("\\t", "\t")
                    .replace("\\\"", "\"")
                    .replace("\\'", "'")
                    .replace("\\\\", "\\"))
            } else {
                serde_json::json!(part)
            };
            args.insert(key, val);
            positional_index += 1;
        }
    }

    Some((name, serde_json::Value::Object(args)))
}

/// Split comma-separated arguments, respecting quoted strings.
fn split_call_args(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut quote_char = '"';
    let mut in_triple = false;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];
        if !in_quote
            && i + 2 < chars.len()
            && chars[i] == '"'
            && chars[i + 1] == '"'
            && chars[i + 2] == '"'
        {
            if in_triple {
                current.push_str("\"\"\"");
                i += 3;
                in_triple = false;
                continue;
            }
            current.push_str("\"\"\"");
            i += 3;
            in_triple = true;
            continue;
        }
        if in_triple {
            current.push(ch);
            i += 1;
            continue;
        }
        if !in_quote && (ch == '"' || ch == '\'') {
            in_quote = true;
            quote_char = ch;
            current.push(ch);
        } else if in_quote && ch == quote_char && (i == 0 || chars[i - 1] != '\\') {
            in_quote = false;
            current.push(ch);
        } else if !in_quote && ch == ',' {
            parts.push(current.trim().to_string());
            current = String::new();
        } else {
            current.push(ch);
        }
        i += 1;
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

// =============================================================================
// Convert tool_registry to native tool definitions
// =============================================================================

pub(crate) fn vm_tools_to_native(
    tools_val: &VmValue,
    provider: &str,
) -> Result<Vec<serde_json::Value>, VmError> {
    // Accept either a tool_registry dict or a list of tool dicts
    let tools_list = match tools_val {
        VmValue::Dict(d) => {
            // tool_registry -- extract tools list
            match d.get("tools") {
                Some(VmValue::List(list)) => list.as_ref().clone(),
                _ => Vec::new(),
            }
        }
        VmValue::List(list) => list.as_ref().clone(),
        _ => {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tools must be a tool_registry or a list of tool definitions",
            ))));
        }
    };

    let mut native_tools = Vec::new();
    for tool in &tools_list {
        match tool {
            VmValue::String(name) => {
                if let Some(schema) = builtin_tool_schema(name) {
                    let tool_name = schema["name"].as_str().unwrap_or(name);
                    let description = schema["description"].as_str().unwrap_or("");
                    let input_schema = vm_build_json_schema_from_json(&schema["parameters"]);
                    match provider {
                        "openai" | "openrouter" => {
                            native_tools.push(serde_json::json!({
                                "type": "function",
                                "function": {
                                    "name": tool_name,
                                    "description": description,
                                    "parameters": input_schema,
                                }
                            }));
                        }
                        _ => {
                            native_tools.push(serde_json::json!({
                                "name": tool_name,
                                "description": description,
                                "input_schema": input_schema,
                            }));
                        }
                    }
                }
            }
            VmValue::Dict(entry) => {
                let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
                let description = entry
                    .get("description")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let params = entry.get("parameters").and_then(|v| v.as_dict());

                let input_schema = vm_build_json_schema(params);

                match provider {
                    "openai" | "openrouter" => {
                        native_tools.push(serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": name,
                                "description": description,
                                "parameters": input_schema,
                            }
                        }));
                    }
                    _ => {
                        // Anthropic format
                        native_tools.push(serde_json::json!({
                            "name": name,
                            "description": description,
                            "input_schema": input_schema,
                        }));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(native_tools)
}

fn vm_build_json_schema(params: Option<&BTreeMap<String, VmValue>>) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    if let Some(params) = params {
        for (name, type_val) in params {
            let type_str = type_val.display();
            let json_type = match type_str.as_str() {
                "int" | "integer" => "integer",
                "float" | "number" => "number",
                "bool" | "boolean" => "boolean",
                "list" | "array" => "array",
                "dict" | "object" => "object",
                _ => "string",
            };
            properties.insert(name.clone(), serde_json::json!({"type": json_type}));
            required.push(serde_json::Value::String(name.clone()));
        }
    }

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}
