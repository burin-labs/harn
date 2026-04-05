use std::collections::{BTreeMap, BTreeSet};
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

type ToolParamSchema = (String, String, String);

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct ToolSchema {
    name: String,
    description: String,
    params: Vec<ToolParamSchema>,
}

fn collect_vm_tool_schemas(tools_val: Option<&VmValue>) -> Vec<ToolSchema> {
    let entries: Vec<&VmValue> = match tools_val {
        Some(VmValue::List(list)) => list.iter().collect(),
        Some(VmValue::Dict(d)) => {
            if let Some(VmValue::List(tools)) = d.get("tools") {
                tools.iter().collect()
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    };

    entries
        .into_iter()
        .filter_map(|value| match value {
            VmValue::Dict(td) => {
                let name = td.get("name")?.display();
                let description = td
                    .get("description")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let params = extract_params_from_vm_dict(td);
                Some(ToolSchema {
                    name,
                    description,
                    params,
                })
            }
            _ => None,
        })
        .collect()
}

fn schema_type_from_json(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(ToString::to_string)
        .or_else(|| {
            value
                .get("type")
                .and_then(|inner| inner.as_str())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "string".to_string())
}

fn schema_description_from_json(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(ToString::to_string)
        .or_else(|| {
            value
                .get("description")
                .and_then(|inner| inner.as_str())
                .map(ToString::to_string)
        })
        .unwrap_or_default()
}

fn extract_params_from_native_schema(input_schema: &serde_json::Value) -> Vec<ToolParamSchema> {
    input_schema
        .get("properties")
        .and_then(|value| value.as_object())
        .map(|properties| {
            let mut params = properties
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        schema_type_from_json(value),
                        schema_description_from_json(value),
                    )
                })
                .collect::<Vec<_>>();
            params.sort_by(|a, b| a.0.cmp(&b.0));
            params
        })
        .unwrap_or_default()
}

fn collect_native_tool_schemas(native_tools: Option<&[serde_json::Value]>) -> Vec<ToolSchema> {
    native_tools
        .unwrap_or(&[])
        .iter()
        .filter_map(|tool| {
            let function = tool.get("function");
            let name = function
                .and_then(|value| value.get("name"))
                .or_else(|| tool.get("name"))
                .and_then(|value| value.as_str())?;
            let description = function
                .and_then(|value| value.get("description"))
                .or_else(|| tool.get("description"))
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let input_schema = function
                .and_then(|value| value.get("parameters"))
                .or_else(|| tool.get("input_schema"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type": "object"}));
            Some(ToolSchema {
                name: name.to_string(),
                description,
                params: extract_params_from_native_schema(&input_schema),
            })
        })
        .collect()
}

pub(crate) fn collect_tool_schemas(
    tools_val: Option<&VmValue>,
    native_tools: Option<&[serde_json::Value]>,
) -> Vec<ToolSchema> {
    let mut merged = collect_vm_tool_schemas(tools_val);
    let mut seen = merged
        .iter()
        .map(|schema| schema.name.clone())
        .collect::<BTreeSet<_>>();

    for schema in collect_native_tool_schemas(native_tools) {
        if seen.insert(schema.name.clone()) {
            merged.push(schema);
        }
    }

    merged.sort_by(|a, b| a.name.cmp(&b.name));
    merged
}

fn positional_param_name(tool_name: &str, position: usize, tools_val: Option<&VmValue>) -> String {
    let param_names = collect_tool_schemas(tools_val, None)
        .into_iter()
        .find(|schema| schema.name == tool_name)
        .map(|schema| {
            schema
                .params
                .into_iter()
                .map(|(name, _, _)| name)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if position == 0 && param_names.len() == 1 {
        return param_names[0].clone();
    }

    format!("arg{}", position + 1)
}

/// Build a runtime-owned tool-calling contract prompt.
/// The runtime injects this block so prompt templates do not need to carry
/// stale tool syntax examples that can drift from actual parser behavior.
pub(crate) fn build_tool_calling_contract_prompt(
    tools_val: Option<&VmValue>,
    native_tools: Option<&[serde_json::Value]>,
    mode: &str,
    include_format: bool,
) -> String {
    let mut prompt = String::from("\n\n## Tool Calling Contract\n");
    prompt.push_str(&format!(
        "Active mode: `{mode}`. Follow this runtime-owned contract even if older prompt text suggests another tool syntax.\n\n"
    ));
    prompt.push_str("## Available tools\n\n");

    let schemas = collect_tool_schemas(tools_val, native_tools);

    // Present tools as Python-like function signatures
    for schema in &schemas {
        let sig = schema
            .params
            .iter()
            .map(|(pname, ptype, _)| format!("{pname}: {ptype}"))
            .collect::<Vec<_>>()
            .join(", ");
        prompt.push_str(&format!(
            "### {}({sig})\n{}\n",
            schema.name, schema.description
        ));
        for (pname, _, pdesc) in &schema.params {
            if !pdesc.is_empty() {
                prompt.push_str(&format!("- `{pname}`: {pdesc}\n"));
            }
        }
        prompt.push('\n');
    }

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
             For multiline string values (like file content or code), use heredoc syntax:\n\
             ````\n\
             ```call\n\
             tool_name(param=\"value\", long_param=<<'EOF'\n\
             line 1\n\
             line 2\n\
             EOF\n\
             )\n\
             ```\n\
             ````\n\
             The heredoc tag (EOF, BODY, etc.) can be any word. Content between the opening and closing tag is passed raw with no escaping needed.\n\
             You can use multiple heredocs in one call:\n\
             ````\n\
             ```call\n\
             edit(action=\"patch\", path=\"foo.py\", old_string=<<'OLD'\n\
             old code\n\
             OLD\n\
             new_string=<<'NEW'\n\
             new code\n\
             NEW\n\
             )\n\
             ```\n\
             ````\n\
             You can make multiple tool calls in one response by emitting multiple ` ```call ` blocks.\n\
             After each call, you will see the result in a <tool_result> tag.\n\
             Only the `### name(...)` headings above are tools. Parameter names listed under those headings are arguments, not standalone tools.\n\
             Use named arguments unless the tool signature above has exactly one parameter; positional calls beyond that are ambiguous and may be rejected.\n\
             If a parameter expects a string command or path, pass that value as a normal string unless the tool schema explicitly requests a list or object.\n",
        );
    }

    prompt
}

/// Parse tool calls from LLM text response.
/// Uses ```call blocks with Python-like function syntax:
///   ```call
///   tool_name(param="value", param2="value2")
///   ```
#[cfg(test)]
fn parse_text_tool_calls(text: &str) -> Vec<serde_json::Value> {
    parse_text_tool_calls_with_tools(text, None).calls
}

/// Why find_call_block_end failed to locate a closing fence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BlockEndReason {
    /// Block reached EOF while still inside a heredoc body.
    UnterminatedHeredoc { tag: String },
    /// Block reached EOF with no closing ``` fence.
    NoClosingFence,
}

/// Find the end of a ```call block starting from `s`, skipping over heredoc bodies
/// so that triple backticks inside heredoc content don't close the block early.
/// Returns the byte offset within `s` where the closing ``` begins, or a structured
/// reason for why no closing fence was found.
fn find_call_block_end(s: &str) -> Result<usize, BlockEndReason> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_double_quote = false;
    while i < bytes.len() {
        // Track double-quoted strings so we don't misinterpret << inside them
        if bytes[i] == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
            in_double_quote = !in_double_quote;
            i += 1;
            continue;
        }
        if in_double_quote {
            i += 1;
            continue;
        }
        // Check for heredoc opener: <<'TAG' or <<TAG (only outside quoted strings)
        if i + 1 < bytes.len() && bytes[i] == b'<' && bytes[i + 1] == b'<' {
            let mut j = i + 2;
            let quoted = j < bytes.len() && bytes[j] == b'\'';
            if quoted {
                j += 1;
            }
            let tag_start = j;
            while j < bytes.len() && bytes[j] != b'\n' {
                if quoted && bytes[j] == b'\'' {
                    break;
                }
                j += 1;
            }
            let tag = s[tag_start..j].to_string();
            if quoted && j < bytes.len() && bytes[j] == b'\'' {
                j += 1;
            }
            // Skip to end of the <<'TAG' line
            while j < bytes.len() && bytes[j] != b'\n' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'\n' {
                j += 1;
            }
            // Skip body until closing tag alone on a line
            let closing = format!("{}\n", tag);
            let rest = &s[j..];
            if let Some(pos) = rest.find(&closing) {
                i = j + pos + closing.len();
            } else if rest.trim_end() == tag.as_str() || rest.ends_with(&format!("\n{}", tag)) {
                // Closing tag at very end of input with no trailing newline.
                i = bytes.len();
            } else {
                // Unterminated heredoc body — the response was likely truncated.
                return Err(BlockEndReason::UnterminatedHeredoc { tag });
            }
            continue;
        }
        // Check for closing ```
        if i + 2 < bytes.len() && bytes[i] == b'`' && bytes[i + 1] == b'`' && bytes[i + 2] == b'`' {
            return Ok(i);
        }
        i += 1;
    }
    Err(BlockEndReason::NoClosingFence)
}

/// Detect the common mistake of using `:` instead of `=` before a heredoc:
/// `new_body: <<'EOF'` rather than `new_body=<<'EOF'`. Returns the offending
/// key name if found, else None.
///
/// Heuristic: the pattern `<arg-boundary> <ident> \s* : \s* <<` where
/// arg-boundary is `(`, `,`, or a newline. Colons inside quoted strings, and
/// colons that are not preceded by an identifier at an argument boundary, are
/// ignored. This avoids false positives on JSON/dict content inside heredoc
/// bodies.
fn detect_colon_before_heredoc(call_text: &str) -> Option<String> {
    let bytes = call_text.as_bytes();
    let mut i = 0;
    let mut in_quote = false;
    let mut quote_ch: u8 = b'"';
    // Track whether we have just crossed an argument boundary and are now
    // looking for an identifier. Start true because the very beginning of
    // args_str is also an argument boundary (right after the opening paren).
    let mut at_arg_boundary = true;
    while i + 1 < bytes.len() {
        let b = bytes[i];
        if !in_quote && (b == b'"' || b == b'\'') {
            in_quote = true;
            quote_ch = b;
            at_arg_boundary = false;
            i += 1;
            continue;
        }
        if in_quote {
            if b == quote_ch && (i == 0 || bytes[i - 1] != b'\\') {
                in_quote = false;
            }
            i += 1;
            continue;
        }
        if b == b'(' || b == b',' || b == b'\n' {
            at_arg_boundary = true;
            i += 1;
            continue;
        }
        if b == b' ' || b == b'\t' || b == b'\r' {
            i += 1;
            continue;
        }
        if at_arg_boundary && (b.is_ascii_alphabetic() || b == b'_') {
            // Walk an identifier
            let id_start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let id_end = i;
            // Skip whitespace
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                i += 1;
            }
            // Check for `:` followed by whitespace and `<<`
            if i < bytes.len() && bytes[i] == b':' {
                let mut j = i + 1;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                if j + 1 < bytes.len() && bytes[j] == b'<' && bytes[j + 1] == b'<' {
                    let key = std::str::from_utf8(&bytes[id_start..id_end]).ok()?;
                    return Some(key.to_string());
                }
            }
            at_arg_boundary = false;
            continue;
        }
        at_arg_boundary = false;
        i += 1;
    }
    None
}

/// Result of parsing text tool calls: successfully parsed calls + diagnostics for failures.
pub(crate) struct TextToolParseResult {
    pub calls: Vec<serde_json::Value>,
    pub errors: Vec<String>,
}

pub(crate) fn parse_text_tool_calls_with_tools(
    text: &str,
    tools_val: Option<&VmValue>,
) -> TextToolParseResult {
    let mut calls = Vec::new();
    let mut errors = Vec::new();
    let mut search_from = 0;

    while let Some(start_offset) = text[search_from..].find("```call") {
        let after_marker = search_from + start_offset + "```call".len();
        // Skip newline after ```call
        let content_start = if text.as_bytes().get(after_marker) == Some(&b'\n') {
            after_marker + 1
        } else {
            after_marker
        };
        match find_call_block_end(&text[content_start..]) {
            Ok(end_offset) => {
                let content_end = content_start + end_offset;
                let call_text = text[content_start..content_end].trim();
                if let Some((name, arguments)) = parse_function_call_syntax(call_text, tools_val) {
                    calls.push(serde_json::json!({
                        "id": format!("tc_{}", calls.len()),
                        "name": name,
                        "arguments": arguments,
                    }));
                } else if let Some(key) = detect_colon_before_heredoc(call_text) {
                    errors.push(format!(
                        "TOOL CALL PARSE ERROR: argument `{key}` used `:` as the separator \
                         before a heredoc. Tool call arguments use `=`, not `:`. \
                         Write `{key}=<<'EOF'` (no space, `=` not `:`) on its own line, \
                         then the body, then `EOF` on its own line."
                    ));
                } else {
                    let preview: String = call_text.chars().take(120).collect();
                    errors.push(format!(
                        "TOOL CALL PARSE ERROR: Could not parse tool call from: `{preview}...`\n\
                         Check: missing closing `)`, unmatched quotes, or malformed arguments.\n\
                         For multiline string values, use heredoc syntax: param=<<'EOF'\\n...\\nEOF"
                    ));
                }
                search_from = content_end + "```".len();
            }
            Err(BlockEndReason::UnterminatedHeredoc { tag }) => {
                // Prefer the colon-misuse diagnostic if we can spot it in the
                // partial block — it is more actionable than "truncated".
                let partial = &text[content_start..];
                if let Some(key) = detect_colon_before_heredoc(partial) {
                    errors.push(format!(
                        "TOOL CALL PARSE ERROR: argument `{key}` used `:` as the separator \
                         before a heredoc. Tool call arguments use `=`, not `:`. \
                         Write `{key}=<<'{tag}'` (no space, `=` not `:`) on its own line, \
                         then the body, then `{tag}` on its own line."
                    ));
                } else {
                    errors.push(format!(
                        "TOOL CALL PARSE ERROR: heredoc body for tag `{tag}` was never closed \
                         — the response was likely truncated mid-call. \
                         Re-emit the call with a shorter body, or split the edit into \
                         smaller pieces so the whole call fits in one response."
                    ));
                }
                // Skip past this marker so we still scan for subsequent valid blocks.
                search_from = after_marker;
            }
            Err(BlockEndReason::NoClosingFence) => {
                let preview: String = text[content_start..].chars().take(80).collect();
                errors.push(format!(
                    "TOOL CALL PARSE ERROR: Found ```call block but no closing ```. Content starts with: `{preview}...`\n\
                     Make sure every ```call block has a matching closing ```."
                ));
                search_from = after_marker;
            }
        }
    }

    TextToolParseResult { calls, errors }
}

/// Parse function-call syntax: `name(key="value", key2="value2")`
/// Also handles positional args for single-parameter tools declared in the
/// active tool schema.
fn parse_function_call_syntax(
    text: &str,
    tools_val: Option<&VmValue>,
) -> Option<(String, serde_json::Value)> {
    // Strip whitespace, then trailing literal "\n" that models sometimes emit before closing ```
    let text = text.trim();
    let text = text.strip_suffix("\\n").unwrap_or(text);
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
        // If a part contains a heredoc sentinel but no `=`, the model used
        // the wrong separator (e.g. `new_body: <<'EOF'`). Bail out so the
        // caller can produce a precise `key: <<` diagnostic.
        if part.contains("\x00H") && !part.contains('=') {
            return None;
        }
        if let Some(eq_pos) = part.find('=') {
            let key = part[..eq_pos].trim().to_string();
            let val_str = part[eq_pos + 1..].trim();
            let val =
                if val_str.starts_with("\x00H") && val_str.ends_with("\x00H") && val_str.len() >= 4
                {
                    // Heredoc content: completely raw, no unescaping
                    serde_json::json!(&val_str[2..val_str.len() - 2])
                } else if val_str.starts_with('[') && val_str.ends_with(']') {
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
                } else if val_str == "null" {
                    serde_json::Value::Null
                } else if let Ok(n) = val_str.parse::<i64>() {
                    serde_json::json!(n)
                } else if let Ok(n) = val_str.parse::<f64>() {
                    serde_json::json!(n)
                } else {
                    serde_json::json!(val_str)
                };
            args.insert(key, val);
        } else if !part.is_empty() {
            // Positional arguments are only schema-driven for single-parameter tools.
            let key = positional_param_name(&name, positional_index, tools_val);
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
        // Heredoc detection: <<'TAG' or <<TAG outside any quote/triple mode
        if !in_quote && !in_triple && i + 1 < chars.len() && chars[i] == '<' && chars[i + 1] == '<'
        {
            // Parse the tag — skip past <<
            let mut j = i + 2;
            // Strip optional surrounding quotes on the tag
            let quoted_tag = j < chars.len() && chars[j] == '\'';
            if quoted_tag {
                j += 1;
            }
            let tag_start = j;
            while j < chars.len() && chars[j] != '\n' {
                if quoted_tag && chars[j] == '\'' {
                    break;
                }
                j += 1;
            }
            let tag: String = chars[tag_start..j].iter().collect();
            if quoted_tag && j < chars.len() && chars[j] == '\'' {
                j += 1; // skip closing quote
            }
            // Skip to end of the <<'TAG' line (past the newline)
            while j < chars.len() && chars[j] != '\n' {
                j += 1;
            }
            if j < chars.len() && chars[j] == '\n' {
                j += 1;
            }
            // Now collect body lines until the closing tag appears alone on a line
            let mut body = String::new();
            let rest: String = chars[j..].iter().collect();
            let closing = format!("{}\n", tag);
            let closing_eoi = tag.as_str(); // closing at end with no trailing newline
            let end_offset = if let Some(pos) = rest.find(&closing) {
                // found tag followed by newline
                let body_part = &rest[..pos];
                body.push_str(body_part);
                // strip trailing newline from body (heredoc convention)
                if body.ends_with('\n') {
                    body.pop();
                }
                j + rest[..pos + closing.len()].chars().count()
            } else if rest.trim_end() == closing_eoi
                || rest.ends_with(&format!("\n{}", closing_eoi))
            {
                // closing tag at very end of input with no trailing newline
                let tag_at_end = format!("\n{}", closing_eoi);
                if let Some(pos) = rest.rfind(&tag_at_end) {
                    let body_part = &rest[..pos];
                    body.push_str(body_part);
                    j + rest[..pos + tag_at_end.len()].chars().count()
                } else {
                    body.push_str(&rest);
                    j + rest.chars().count()
                }
            } else {
                // no closing tag found; treat rest as body
                body.push_str(&rest);
                j + rest.chars().count()
            };
            // Encode as heredoc marker
            current.push('\x00');
            current.push('H');
            current.push_str(&body);
            current.push('\x00');
            current.push('H');
            // A heredoc is always a complete value — push the part now so the
            // next key=value (with no preceding comma) starts fresh.
            if !current.trim().is_empty() {
                parts.push(current.trim().to_string());
                current = String::new();
            }
            // Skip an optional comma + whitespace after the closing tag line
            let rest_chars: Vec<char> = chars[end_offset..].to_vec();
            let mut skip = 0;
            while skip < rest_chars.len()
                && (rest_chars[skip] == ' '
                    || rest_chars[skip] == '\t'
                    || rest_chars[skip] == '\n'
                    || rest_chars[skip] == '\r')
            {
                skip += 1;
            }
            if skip < rest_chars.len() && rest_chars[skip] == ',' {
                skip += 1;
                while skip < rest_chars.len()
                    && (rest_chars[skip] == ' '
                        || rest_chars[skip] == '\t'
                        || rest_chars[skip] == '\n'
                        || rest_chars[skip] == '\r')
                {
                    skip += 1;
                }
            }
            i = end_offset + skip;
            continue;
        }
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

#[cfg(test)]
mod tests {
    use super::{
        build_tool_calling_contract_prompt, detect_colon_before_heredoc, find_call_block_end,
        normalize_tool_args, parse_text_tool_calls, parse_text_tool_calls_with_tools,
        split_call_args, BlockEndReason,
    };
    use crate::value::VmValue;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::rc::Rc;

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
            "```call\nexecute(command=[\"ls\",\"internal/manifest/\"], timeout=30)\n```",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["name"], json!("execute"));
        assert_eq!(
            calls[0]["arguments"]["command"],
            json!(["ls", "internal/manifest/"])
        );
        assert_eq!(calls[0]["arguments"]["timeout"], json!(30));
    }

    #[test]
    fn parse_text_tool_calls_preserves_scalar_json_types() {
        let calls = parse_text_tool_calls(
            "```call\nlookup(score=0.5, limit=3, exact=false, note=null)\n```",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["arguments"]["score"], json!(0.5));
        assert_eq!(calls[0]["arguments"]["limit"], json!(3));
        assert_eq!(calls[0]["arguments"]["exact"], json!(false));
        assert_eq!(calls[0]["arguments"]["note"], serde_json::Value::Null);
    }

    #[test]
    fn parse_text_tool_calls_uses_schema_for_single_positional_argument() {
        let tools = VmValue::List(Rc::new(vec![VmValue::Dict(
            BTreeMap::from([
                ("name".into(), VmValue::String(Rc::from("lookup"))),
                (
                    "parameters".into(),
                    VmValue::Dict(Rc::new(BTreeMap::from([(
                        "target".into(),
                        VmValue::Dict(Rc::new(BTreeMap::from([(
                            "type".into(),
                            VmValue::String(Rc::from("str")),
                        )]))),
                    )]))),
                ),
            ])
            .into(),
        )]));

        let result =
            parse_text_tool_calls_with_tools("```call\nlookup(\"README.md\")\n```", Some(&tools));
        assert_eq!(result.calls.len(), 1);
        assert_eq!(result.calls[0]["arguments"]["target"], json!("README.md"));
    }

    #[test]
    fn parse_text_tool_calls_handles_trailing_literal_backslash_n() {
        // Models sometimes emit a literal \n before closing ```, which caused tool calls
        // to be silently dropped because strip_suffix(')') failed.
        let calls = parse_text_tool_calls(
            "```call\nedit(action=\"patch\", path=\"foo.swift\", old_string=\"a\\nb\", new_string=\"c\\nd\")\\n```",
        );
        assert_eq!(
            calls.len(),
            1,
            "tool call should be parsed despite trailing \\n"
        );
        assert_eq!(calls[0]["name"], json!("edit"));
        assert_eq!(calls[0]["arguments"]["action"], json!("patch"));
        assert_eq!(calls[0]["arguments"]["path"], json!("foo.swift"));
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
        let prompt = build_tool_calling_contract_prompt(None, None, "text", true);
        assert!(prompt.contains("Active mode: `text`"));
        assert!(prompt.contains("```call"));
    }

    #[test]
    fn tool_calling_contract_lists_tools_and_text_mode_guardrails() {
        let tools = VmValue::List(Rc::new(vec![VmValue::Dict(
            BTreeMap::from([
                ("name".into(), VmValue::String(Rc::from("lookup"))),
                (
                    "description".into(),
                    VmValue::String(Rc::from("Fetch one resource")),
                ),
                (
                    "parameters".into(),
                    VmValue::Dict(Rc::new(BTreeMap::from([(
                        "target".into(),
                        VmValue::Dict(Rc::new(BTreeMap::from([
                            ("type".into(), VmValue::String(Rc::from("str"))),
                            (
                                "description".into(),
                                VmValue::String(Rc::from("Resource identifier")),
                            ),
                        ]))),
                    )]))),
                ),
            ])
            .into(),
        )]));

        let prompt = build_tool_calling_contract_prompt(Some(&tools), None, "text", true);
        assert!(prompt.contains("Only the `### name(...)` headings above are tools."));
        assert!(prompt.contains(
            "Use named arguments unless the tool signature above has exactly one parameter"
        ));
    }

    #[test]
    fn tool_calling_contract_renders_native_tools_when_vm_registry_is_missing() {
        let native_tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "lookup",
                "description": "Look up a symbol",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Symbol name"},
                        "folder": {"type": "string", "description": "Optional folder scope"}
                    },
                    "required": []
                }
            }
        })];

        let prompt = build_tool_calling_contract_prompt(None, Some(&native_tools), "text", true);
        assert!(prompt.contains("### lookup("));
        assert!(prompt.contains("query: string"));
        assert!(prompt.contains("folder: string"));
    }

    #[test]
    fn parse_text_tool_calls_handles_heredoc_syntax() {
        let calls = parse_text_tool_calls(
            "```call\nedit(action=\"create\", path=\"test.py\", content=<<'EOF'\n\"\"\"Tests.\"\"\"\n\nimport pytest\nEOF\n)\n```",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["name"], json!("edit"));
        assert_eq!(calls[0]["arguments"]["action"], json!("create"));
        assert_eq!(calls[0]["arguments"]["path"], json!("test.py"));
        let content = calls[0]["arguments"]["content"].as_str().unwrap();
        assert!(
            content.contains("\"\"\"Tests.\"\"\""),
            "should preserve Python docstrings raw"
        );
        assert!(content.contains("import pytest"));
    }

    #[test]
    fn parse_text_tool_calls_handles_multiple_heredocs() {
        let calls = parse_text_tool_calls(
            "```call\nedit(action=\"patch\", path=\"foo.py\", old_string=<<'OLD'\ndef hello():\n    pass\nOLD\nnew_string=<<'NEW'\ndef hello():\n    print(\"hi\")\nNEW\n)\n```",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0]["arguments"]["old_string"],
            json!("def hello():\n    pass")
        );
        assert_eq!(
            calls[0]["arguments"]["new_string"],
            json!("def hello():\n    print(\"hi\")")
        );
    }

    #[test]
    fn parse_text_tool_calls_heredoc_preserves_triple_backticks() {
        let calls = parse_text_tool_calls(
            "```call\nedit(action=\"create\", path=\"README.md\", content=<<'EOF'\n# Title\n```python\nprint(\"hello\")\n```\nEOF\n)\n```",
        );
        assert_eq!(calls.len(), 1);
        let content = calls[0]["arguments"]["content"].as_str().unwrap();
        assert!(
            content.contains("```python"),
            "should preserve triple backticks in content"
        );
    }

    #[test]
    fn parse_text_tool_calls_handles_unicode_inside_heredoc_bodies() {
        let calls = parse_text_tool_calls(
            "```call\nedit(action=\"patch\", path=\"notes.txt\", old_string=<<'OLD'\nalpha — beta\nOLD\n, new_string=<<'NEW'\ngamma — delta\nNEW\n)\n```",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["name"], json!("edit"));
        assert_eq!(calls[0]["arguments"]["old_string"], json!("alpha — beta"));
        assert_eq!(calls[0]["arguments"]["new_string"], json!("gamma — delta"));
    }

    /// Regression: find_call_block_end must not treat << inside double-quoted
    /// string arguments as a heredoc opener.
    #[test]
    fn find_call_block_end_ignores_heredoc_in_quotes() {
        let block = r#"edit(action="insert", body="<<'EOF'\nsome code\nEOF")
```"#;
        let end = find_call_block_end(block);
        assert!(
            end.is_ok(),
            "find_call_block_end should find closing ``` even when << appears inside quotes"
        );
    }

    /// Truncated heredoc body should yield a specific UnterminatedHeredoc reason,
    /// not a generic "no closing fence" error.
    #[test]
    fn find_call_block_end_reports_unterminated_heredoc() {
        let block = "edit(body=<<'EOF'\nline 1\nline 2\n";
        let result = find_call_block_end(block);
        match result {
            Err(BlockEndReason::UnterminatedHeredoc { tag }) => {
                assert_eq!(tag, "EOF");
            }
            other => panic!("expected UnterminatedHeredoc, got {:?}", other),
        }
    }

    #[test]
    fn parse_text_tool_calls_reports_truncated_heredoc() {
        // Mimics the local-gemma4 failure: heredoc body never closes.
        let text = "```call\nedit(body=<<'EOF'\nsome code\n";
        let result = parse_text_tool_calls_with_tools(text, None);
        assert_eq!(result.calls.len(), 0);
        assert_eq!(result.errors.len(), 1);
        let err = &result.errors[0];
        assert!(
            err.contains("heredoc body for tag `EOF` was never closed"),
            "expected truncated-heredoc diagnostic, got: {err}"
        );
        assert!(err.contains("truncated"));
    }

    /// The `key: <<'TAG'` misuse (colon instead of `=`) should produce a
    /// specific diagnostic that names the offending key.
    #[test]
    fn parse_text_tool_calls_reports_colon_before_heredoc() {
        let text = "```call\nreplace_body(\n  path=\"a.go\",\n  function_name=\"Foo\",\n  new_body: <<'EOF'\nbody\nEOF\n)\n```";
        let result = parse_text_tool_calls_with_tools(text, None);
        assert_eq!(
            result.calls.len(),
            0,
            "malformed call should not be accepted: {:?}",
            result.calls
        );
        assert_eq!(result.errors.len(), 1);
        let err = &result.errors[0];
        assert!(
            err.contains("`new_body`"),
            "diagnostic should name the offending key, got: {err}"
        );
        assert!(
            err.contains("`:`") && err.contains("`=`"),
            "diagnostic should mention : vs =, got: {err}"
        );
    }

    /// When BOTH the colon-misuse and a truncation are present (exactly the
    /// failing local-gemma4 scenario), prefer the colon diagnostic since it is
    /// more actionable.
    #[test]
    fn parse_text_tool_calls_prefers_colon_diagnostic_over_truncation() {
        let text = "```call\nreplace_body(\n  path=\"a.go\",\n  new_body: <<'EOF'\nbody that never terminates";
        let result = parse_text_tool_calls_with_tools(text, None);
        assert_eq!(result.errors.len(), 1);
        let err = &result.errors[0];
        assert!(
            err.contains("`new_body`") && err.contains("`:`"),
            "expected colon diagnostic to win, got: {err}"
        );
    }

    #[test]
    fn detect_colon_before_heredoc_ignores_colons_inside_strings() {
        // Colon in a quoted dict value must not trigger the heuristic.
        let text = "replace_body(path=\"a.go\", new_body=<<'EOF'\n{\"k\": <<inline>>}\nEOF\n)";
        assert!(detect_colon_before_heredoc(text).is_none());
    }

    /// Regression: real eval transcript where model emitted 14 ```call blocks
    /// with heredoc-like syntax inside quoted string arguments.
    #[test]
    fn parse_text_tool_calls_real_transcript_14_blocks() {
        let text = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test_fixtures/execute_response_raw.txt"
        ));
        let result = parse_text_tool_calls_with_tools(text, None);
        assert_eq!(
            result.calls.len(),
            14,
            "expected 14 parsed calls, got {} calls and {} errors: {:?}",
            result.calls.len(),
            result.errors.len(),
            result.errors
        );
        assert!(
            result.errors.is_empty(),
            "expected no parse errors, got: {:?}",
            result.errors
        );
        // First call should be scaffold
        assert_eq!(result.calls[0]["name"], json!("scaffold"));
        // Last call should be edit (add_import "time")
        assert_eq!(result.calls[13]["name"], json!("edit"));
        assert_eq!(
            result.calls[13]["arguments"]["import_statement"],
            json!("import \"time\"")
        );
    }

    /// Regression: an unclosed ```call block must not kill parsing of subsequent valid blocks.
    #[test]
    fn parse_text_tool_calls_continues_past_unclosed_block() {
        let text = concat!(
            "```call\n",
            "broken(arg=\"no closing\n",
            "Some random text\n",
            "```call\n",
            "valid_tool(x=\"hello\")\n",
            "```\n",
        );
        let result = parse_text_tool_calls_with_tools(text, None);
        assert_eq!(
            result.calls.len(),
            1,
            "should parse the valid block after the unclosed one"
        );
        assert_eq!(result.calls[0]["name"], json!("valid_tool"));
        assert_eq!(
            result.errors.len(),
            1,
            "should report exactly one error for the unclosed block"
        );
        assert!(result.errors[0].contains("no closing"));
    }
}
