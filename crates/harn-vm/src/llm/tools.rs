use std::collections::BTreeMap;
use std::rc::Rc;

use super::vm_value_to_json;
use crate::value::{VmError, VmValue};

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

/// Build a durable assistant message for transcript/run-record storage.
/// Prefer canonical structured blocks when available so hosts can restore
/// richer assistant state without reparsing visible text.
pub(crate) fn build_assistant_response_message(
    text: &str,
    blocks: &[serde_json::Value],
    tool_calls: &[serde_json::Value],
    provider: &str,
) -> serde_json::Value {
    if !tool_calls.is_empty() {
        return build_assistant_tool_message(text, tool_calls, provider);
    }
    if !blocks.is_empty() {
        return serde_json::json!({
            "role": "assistant",
            "content": blocks,
        });
    }
    serde_json::json!({
        "role": "assistant",
        "content": text,
    })
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

    if name == "run" || name == "exec" {
        if !obj.contains_key("command") {
            if let Some(v) = obj.remove("args").or_else(|| obj.remove("argv")) {
                obj.insert("command".to_string(), v);
            }
        }

        let command_value = obj.get("command").cloned();
        let args_value = obj
            .get("args")
            .cloned()
            .or_else(|| obj.get("argv").cloned());
        if let Some(command) = normalize_run_command(command_value.as_ref(), args_value.as_ref()) {
            obj.insert("command".to_string(), serde_json::json!(command));
        }
        obj.remove("args");
        obj.remove("argv");
    }

    serde_json::Value::Object(obj)
}

fn normalize_run_command(
    command_value: Option<&serde_json::Value>,
    fallback_value: Option<&serde_json::Value>,
) -> Option<String> {
    let command_parts = command_value
        .and_then(run_command_tokens)
        .unwrap_or_default();
    let fallback_parts = fallback_value
        .and_then(run_command_tokens)
        .unwrap_or_default();
    let parts = if command_parts.is_empty() {
        fallback_parts
    } else if fallback_parts.is_empty() {
        command_parts
    } else {
        let mut combined = fallback_parts;
        combined.extend(command_parts);
        combined
    };

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn run_command_tokens(value: &serde_json::Value) -> Option<Vec<String>> {
    match value {
        serde_json::Value::Array(parts) => {
            let tokens = parts
                .iter()
                .filter_map(|part| part.as_str())
                .map(|part| part.trim().to_string())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            (!tokens.is_empty()).then_some(tokens)
        }
        serde_json::Value::String(text) => run_command_tokens_from_str(text),
        _ => None,
    }
}

fn run_command_tokens_from_str(text: &str) -> Option<Vec<String>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        if let Ok(parts) = serde_json::from_str::<Vec<String>>(trimmed) {
            let tokens = parts
                .into_iter()
                .map(|part| part.trim().to_string())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            if !tokens.is_empty() {
                return Some(tokens);
            }
        }
    }

    if (trimmed.contains('[') || trimmed.contains(']') || trimmed.contains("\\\""))
        && trimmed.contains('"')
    {
        let tokens = extract_quoted_tokens(trimmed);
        if !tokens.is_empty() {
            return Some(tokens);
        }
    }

    Some(vec![trimmed.to_string()])
}

fn extract_quoted_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut escape = false;

    for ch in text.chars() {
        if !in_quote {
            if ch == '"' {
                in_quote = true;
                current.clear();
            }
            continue;
        }

        if escape {
            current.push(ch);
            escape = false;
            continue;
        }

        match ch {
            '\\' => escape = true,
            '"' => {
                if !current.trim().is_empty() {
                    tokens.push(current.trim().to_string());
                }
                current.clear();
                in_quote = false;
            }
            _ => current.push(ch),
        }
    }

    tokens
}

fn resolve_local_tool_path(path: &str) -> std::path::PathBuf {
    let candidate = std::path::PathBuf::from(path);
    if candidate.is_absolute() {
        return candidate;
    }
    if let Some(cwd) =
        crate::stdlib::process::current_execution_context().and_then(|context| context.cwd)
    {
        return std::path::PathBuf::from(cwd).join(candidate);
    }
    crate::stdlib::process::resolve_source_relative_path(path)
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
            let resolved = resolve_local_tool_path(path);
            if resolved.is_dir() {
                return match std::fs::read_dir(&resolved) {
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
                };
            }
            match std::fs::read_to_string(&resolved) {
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
            let resolved = resolve_local_tool_path(path);
            match std::fs::read_dir(&resolved) {
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

/// Build a runtime-owned tool-calling contract prompt.
/// The runtime injects this block so prompt templates do not need to carry
/// stale tool syntax examples that can drift from actual parser behavior.
pub(crate) fn build_tool_calling_contract_prompt(
    tools_val: Option<&VmValue>,
    mode: &str,
    include_format: bool,
) -> String {
    let mut prompt = String::from("\n\n## Tool Calling Contract\n");
    prompt.push_str(&format!(
        "Active mode: `{mode}`. Follow this runtime-owned contract even if older prompt text suggests another tool syntax.\n\n"
    ));
    prompt.push_str("## Available tools\n\n");

    // Collect tool schemas from a tool registry or a list of tool definition dicts.
    type ToolSchema = (String, String, Vec<(String, String, String)>);
    let schemas: Vec<ToolSchema> = match tools_val {
        Some(VmValue::List(list)) => list
            .iter()
            .filter_map(|v| match v {
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

    prompt.push_str(
        "Only the `### name(...)` headings above are tools. Parameter names like `path`, `pattern`, or `file_glob` are arguments, not standalone tools.\n\
         Example: use `search(pattern=\"parser\", file_glob=\"**/*.go\")`, never `file_glob(...)`.\n\
         For `run`, pass one shell command string such as `run(command=\"<verification or build command here>\")`; do not pass JSON arrays unless the tool schema explicitly asks for one.\n\n",
    );

    if mode == "native" {
        prompt.push_str(
            "Use the provider's native tool-calling channel for tool invocations. Do not emit ```call blocks in this mode.\n",
        );
    } else if include_format {
        prompt.push_str(
            "\n## How to call tools in text mode\n\
             Emit each tool call in its own fenced code block with the `call` language tag:\n\
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
             You can make multiple tool calls in one response by emitting multiple ` ```call ` blocks.\n\
             After each call, you will see the result in a <tool_result> tag.\n\
             Use prompt context efficiently: if the prompt already includes the relevant file text, API signatures, directory inventory, or pattern examples you need, do not spend extra tool calls rediscovering the same information.\n\
             If the prompt already names the target files or target directories, do not inspect `.` or unrelated parent directories just to find them again.\n\
             `search(...)` is only for exact identifiers or literal code text. Do not use it to rediscover filenames, path strings, package declarations, directory inventory, or broad test names that are already visible in the prompt.\n\
             Read before modifying existing code when you still need exact local details. For new files, if the prompt already provides enough grounded context, use the creation tool the prompt specifies directly rather than spending turns rediscovering context, then verify.\n",
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
            let val = if val_str.starts_with('[') && val_str.ends_with(']') {
                serde_json::from_str(val_str).unwrap_or_else(|_| serde_json::json!(val_str))
            } else if val_str.starts_with('{') && val_str.ends_with('}') {
                serde_json::from_str(val_str).unwrap_or_else(|_| serde_json::json!(val_str))
            } else if val_str.starts_with("\"\"\"")
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
            let val = if part.starts_with('[') && part.ends_with(']') {
                serde_json::from_str(part).unwrap_or_else(|_| serde_json::json!(part))
            } else if part.starts_with('{') && part.ends_with('}') {
                serde_json::from_str(part).unwrap_or_else(|_| serde_json::json!(part))
            } else if part.starts_with("\"\"\"") && part.ends_with("\"\"\"") && part.len() >= 6 {
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
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
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
        } else if !in_quote && ch == '[' {
            bracket_depth += 1;
            current.push(ch);
        } else if !in_quote && ch == ']' {
            bracket_depth = bracket_depth.saturating_sub(1);
            current.push(ch);
        } else if !in_quote && ch == '{' {
            brace_depth += 1;
            current.push(ch);
        } else if !in_quote && ch == '}' {
            brace_depth = brace_depth.saturating_sub(1);
            current.push(ch);
        } else if !in_quote && bracket_depth == 0 && brace_depth == 0 && ch == ',' {
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

#[cfg(test)]
mod tests {
    use super::{
        build_tool_calling_contract_prompt, normalize_tool_args, parse_text_tool_calls,
        split_call_args,
    };
    use serde_json::json;

    #[test]
    fn split_call_args_keeps_array_values_intact() {
        let parts = split_call_args(r#"command=["ls","internal/manifest/"], timeout=30"#);
        assert_eq!(
            parts,
            vec![r#"command=["ls","internal/manifest/"]"#, "timeout=30"]
        );
    }

    #[test]
    fn parse_text_tool_calls_supports_json_array_arguments() {
        let calls = parse_text_tool_calls(
            "```call\nrun(command=[\"ls\",\"internal/manifest/\"], timeout=30)\n```",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["name"], json!("run"));
        assert_eq!(
            calls[0]["arguments"]["command"],
            json!(["ls", "internal/manifest/"])
        );
        assert_eq!(calls[0]["arguments"]["timeout"], json!(30));
    }

    #[test]
    fn normalize_tool_args_joins_run_command_arrays() {
        let normalized =
            normalize_tool_args("run", &json!({"command": ["ls", "internal/manifest/"]}));
        assert_eq!(normalized["command"], json!("ls internal/manifest/"));
    }

    #[test]
    fn normalize_tool_args_accepts_run_args_alias() {
        let normalized = normalize_tool_args(
            "run",
            &json!({"args": ["go", "test", "./internal/manifest/"]}),
        );
        assert_eq!(normalized["command"], json!("go test ./internal/manifest/"));
        assert!(normalized.get("args").is_none());
    }

    #[test]
    fn normalize_tool_args_recovers_stringified_run_array() {
        let normalized = normalize_tool_args(
            "run",
            &json!({"command": "[\"go\",\"test\",\"./internal/manifest/\"]"}),
        );
        assert_eq!(normalized["command"], json!("go test ./internal/manifest/"));
    }

    #[test]
    fn normalize_tool_args_recovers_fragmented_run_array() {
        let normalized = normalize_tool_args(
            "run",
            &json!({"command": "\"internal/manifest/\"]", "args": "[\"ls\""}),
        );
        assert_eq!(normalized["command"], json!("ls internal/manifest/"));
    }

    #[test]
    fn tool_calling_contract_marks_active_text_mode() {
        let prompt = build_tool_calling_contract_prompt(None, "text", true);
        assert!(prompt.contains("Active mode: `text`"));
        assert!(prompt.contains("```call"));
    }
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
                "tools must be a tool_registry or a list of tool definition dicts",
            ))));
        }
    };

    let mut native_tools = Vec::new();
    for tool in &tools_list {
        match tool {
            VmValue::Dict(entry) => {
                let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
                let description = entry
                    .get("description")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let params = entry.get("parameters").and_then(|v| v.as_dict());
                let output_schema = entry.get("outputSchema").map(vm_value_to_json);

                let input_schema = vm_build_json_schema(params);

                match provider {
                    "openai" | "openrouter" => {
                        let mut tool = serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": name,
                                "description": description,
                                "parameters": input_schema,
                            }
                        });
                        if let Some(output_schema) = output_schema.clone() {
                            tool["function"]["x-harn-output-schema"] = output_schema;
                        }
                        native_tools.push(tool);
                    }
                    _ => {
                        // Anthropic format
                        let mut tool = serde_json::json!({
                            "name": name,
                            "description": description,
                            "input_schema": input_schema,
                        });
                        if let Some(output_schema) = output_schema {
                            tool["x-harn-output-schema"] = output_schema;
                        }
                        native_tools.push(tool);
                    }
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
