use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::llm_config;
use crate::stdlib::json_to_vm_value;
use crate::value::{VmChannelHandle, VmError, VmValue};
use crate::vm::Vm;

// =============================================================================
// LLM trace log (thread-local for async-safe access)
// =============================================================================

/// A single LLM call trace entry.
#[derive(Debug, Clone)]
pub struct LlmTraceEntry {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub duration_ms: u64,
}

/// LLM replay mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LlmReplayMode {
    Off,
    Record,
    Replay,
}

thread_local! {
    static LLM_TRACE: RefCell<Vec<LlmTraceEntry>> = const { RefCell::new(Vec::new()) };
    static LLM_TRACING_ENABLED: RefCell<bool> = const { RefCell::new(false) };
    static LLM_REPLAY_MODE: RefCell<LlmReplayMode> = const { RefCell::new(LlmReplayMode::Off) };
    static LLM_FIXTURE_DIR: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Enable LLM tracing for the current thread.
pub fn enable_tracing() {
    LLM_TRACING_ENABLED.with(|v| *v.borrow_mut() = true);
}

/// Get and clear the trace log.
pub fn take_trace() -> Vec<LlmTraceEntry> {
    LLM_TRACE.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

/// Summarize trace usage without consuming entries.
pub fn peek_trace_summary() -> (i64, i64, i64, i64) {
    LLM_TRACE.with(|v| {
        let entries = v.borrow();
        let mut input = 0i64;
        let mut output = 0i64;
        let mut duration = 0i64;
        let count = entries.len() as i64;
        for e in entries.iter() {
            input += e.input_tokens;
            output += e.output_tokens;
            duration += e.duration_ms as i64;
        }
        (input, output, duration, count)
    })
}

/// Set LLM replay mode (record/replay) and fixture directory.
pub fn set_replay_mode(mode: LlmReplayMode, fixture_dir: &str) {
    LLM_REPLAY_MODE.with(|v| *v.borrow_mut() = mode);
    LLM_FIXTURE_DIR.with(|v| *v.borrow_mut() = fixture_dir.to_string());
}

fn get_replay_mode() -> LlmReplayMode {
    LLM_REPLAY_MODE.with(|v| *v.borrow())
}

fn get_fixture_dir() -> String {
    LLM_FIXTURE_DIR.with(|v| v.borrow().clone())
}

/// Hash a request for fixture file naming using canonical JSON serialization.
fn fixture_hash(model: &str, messages: &[serde_json::Value], system: Option<&str>) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    model.hash(&mut hasher);
    // Use canonical JSON string (not Debug format) for stable hashing
    serde_json::to_string(messages)
        .unwrap_or_default()
        .hash(&mut hasher);
    system.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn save_fixture(hash: &str, result: &LlmResult) {
    let dir = get_fixture_dir();
    if dir.is_empty() {
        return;
    }
    let _ = std::fs::create_dir_all(&dir);
    let path = format!("{dir}/{hash}.json");
    let json = serde_json::json!({
        "text": result.text,
        "tool_calls": result.tool_calls,
        "input_tokens": result.input_tokens,
        "output_tokens": result.output_tokens,
        "model": result.model,
    });
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&json).unwrap_or_default(),
    );
}

fn load_fixture(hash: &str) -> Option<LlmResult> {
    let dir = get_fixture_dir();
    if dir.is_empty() {
        return None;
    }
    let path = format!("{dir}/{hash}.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    Some(LlmResult {
        text: json["text"].as_str().unwrap_or("").to_string(),
        tool_calls: json["tool_calls"].as_array().cloned().unwrap_or_default(),
        input_tokens: json["input_tokens"].as_i64().unwrap_or(0),
        output_tokens: json["output_tokens"].as_i64().unwrap_or(0),
        model: json["model"].as_str().unwrap_or("").to_string(),
    })
}

fn trace_llm_call(entry: LlmTraceEntry) {
    LLM_TRACING_ENABLED.with(|enabled| {
        if *enabled.borrow() {
            LLM_TRACE.with(|v| v.borrow_mut().push(entry));
        }
    });
}

/// Register LLM builtins on a VM.
pub fn register_llm_builtins(vm: &mut Vm) {
    // =========================================================================
    // llm_call — core LLM request with structured output + tool use
    // =========================================================================
    vm.register_async_builtin("llm_call", |args| async move {
        let prompt = args.first().map(|a| a.display()).unwrap_or_default();
        let system = args.get(1).map(|a| a.display());
        let options = args.get(2).and_then(|a| a.as_dict()).cloned();

        let provider = vm_resolve_provider(&options);
        let model = vm_resolve_model(&options, &provider);
        let api_key = vm_resolve_api_key(&provider)?;
        let max_tokens = opt_int(&options, "max_tokens").unwrap_or(4096);
        let response_format = opt_str(&options, "response_format");
        let json_schema = options
            .as_ref()
            .and_then(|o| o.get("schema"))
            .and_then(|v| v.as_dict())
            .cloned();
        let temperature = opt_float(&options, "temperature");
        let tools_val = options.as_ref().and_then(|o| o.get("tools")).cloned();
        let messages_val = options.as_ref().and_then(|o| o.get("messages")).cloned();

        // Build messages — either from messages option or from prompt
        let messages = if let Some(VmValue::List(msg_list)) = &messages_val {
            vm_messages_to_json(msg_list)?
        } else {
            vec![serde_json::json!({"role": "user", "content": prompt})]
        };

        // Build native tool definitions from tool_registry or tool list
        let native_tools = if let Some(tools) = &tools_val {
            Some(vm_tools_to_native(tools, &provider)?)
        } else {
            None
        };

        let start = std::time::Instant::now();
        let result = vm_call_llm_full(
            &provider,
            &model,
            &api_key,
            &messages,
            system.as_deref(),
            max_tokens,
            response_format.as_deref(),
            json_schema.as_ref(),
            temperature,
            native_tools.as_deref(),
        )
        .await?;
        trace_llm_call(LlmTraceEntry {
            model: result.model.clone(),
            input_tokens: result.input_tokens,
            output_tokens: result.output_tokens,
            duration_ms: start.elapsed().as_millis() as u64,
        });

        // If response_format is "json", parse the response
        if response_format.as_deref() == Some("json") {
            // Find JSON in the response (may have markdown code fences)
            let json_str = extract_json(&result.text);
            let parsed = serde_json::from_str::<serde_json::Value>(json_str)
                .ok()
                .map(|jv| json_to_vm_value(&jv));
            return Ok(vm_build_llm_result(&result, parsed));
        }

        // If tool_use blocks are present, return structured result
        if !result.tool_calls.is_empty() {
            return Ok(vm_build_llm_result(&result, None));
        }

        // Backward compat: if no special options were used, return plain string
        // This matches the old llm_call(prompt, system) two-arg API
        let is_simple_call = tools_val.is_none() && messages_val.is_none();
        if is_simple_call {
            return Ok(VmValue::String(Rc::from(result.text.as_str())));
        }

        Ok(vm_build_llm_result(&result, None))
    });

    // =========================================================================
    // agent_loop — multi-turn persistent agent loop
    // =========================================================================
    vm.register_async_builtin("agent_loop", |args| async move {
        let prompt = args.first().map(|a| a.display()).unwrap_or_default();
        let system = args.get(1).map(|a| a.display());
        let options = args.get(2).and_then(|a| a.as_dict()).cloned();

        let provider = vm_resolve_provider(&options);
        let model = vm_resolve_model(&options, &provider);
        let api_key = vm_resolve_api_key(&provider)?;
        let max_iterations = opt_int(&options, "max_iterations").unwrap_or(50) as usize;
        let persistent = opt_bool(&options, "persistent");
        let max_nudges = opt_int(&options, "max_nudges").unwrap_or(3) as usize;
        let custom_nudge = opt_str(&options, "nudge");
        let max_tokens = opt_int(&options, "max_tokens").unwrap_or(4096);

        let mut system_prompt = system.unwrap_or_default();
        if persistent {
            system_prompt.push_str(
                "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                 Do NOT stop to explain or summarize — take action. \
                 Output ##DONE## only when the task is fully complete and verified.",
            );
        }

        let mut messages: Vec<serde_json::Value> = vec![serde_json::json!({
            "role": "user",
            "content": prompt,
        })];

        let mut total_text = String::new();
        let mut consecutive_text_only = 0usize;
        let mut total_iterations = 0usize;
        let mut final_status = "done";
        let loop_start = std::time::Instant::now();

        for iteration in 0..max_iterations {
            total_iterations = iteration + 1;
            let start = std::time::Instant::now();
            let result = vm_call_llm_full(
                &provider,
                &model,
                &api_key,
                &messages,
                if system_prompt.is_empty() {
                    None
                } else {
                    Some(&system_prompt)
                },
                max_tokens,
                None,
                None,
                None,
                None,
            )
            .await?;
            trace_llm_call(LlmTraceEntry {
                model: result.model.clone(),
                input_tokens: result.input_tokens,
                output_tokens: result.output_tokens,
                duration_ms: start.elapsed().as_millis() as u64,
            });

            let text = result.text.clone();
            total_text.push_str(&text);

            messages.push(serde_json::json!({
                "role": "assistant",
                "content": text,
            }));

            if persistent && text.contains("##DONE##") {
                break;
            }

            if !persistent {
                break;
            }

            consecutive_text_only += 1;
            if consecutive_text_only > max_nudges {
                final_status = "stuck";
                break;
            }

            let nudge = custom_nudge.clone().unwrap_or_else(|| {
                "You have not output ##DONE## yet — the task is not complete. \
                 Use your tools to continue working. \
                 Only output ##DONE## when the task is fully complete and verified."
                    .to_string()
            });

            messages.push(serde_json::json!({
                "role": "user",
                "content": nudge,
            }));
        }

        let mut result_dict = BTreeMap::new();
        result_dict.insert(
            "status".to_string(),
            VmValue::String(Rc::from(final_status)),
        );
        result_dict.insert(
            "text".to_string(),
            VmValue::String(Rc::from(total_text.as_str())),
        );
        result_dict.insert(
            "iterations".to_string(),
            VmValue::Int(total_iterations as i64),
        );
        result_dict.insert(
            "duration_ms".to_string(),
            VmValue::Int(loop_start.elapsed().as_millis() as i64),
        );
        result_dict.insert(
            "tools_used".to_string(),
            VmValue::List(Rc::from(Vec::<VmValue>::new())),
        );
        Ok(VmValue::Dict(Rc::from(result_dict)))
    });

    // Remaining builtins (llm_stream, conversation management)
    register_llm_builtins_continued(vm);
}

// =============================================================================
// Tool-aware agent_loop (used by ACP mode where host executes tools)
// =============================================================================

/// Built-in tool schemas for common agent tools. Maps short names
/// (as used in Harn pipelines) to full OpenAI-compatible tool definitions.
fn builtin_tool_schema(name: &str) -> Option<serde_json::Value> {
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
fn tool_names_to_schemas(names: &[String], provider: &str) -> Vec<serde_json::Value> {
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
fn build_assistant_tool_message(
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
fn build_tool_result_message(
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
/// - String list: `["read", "search", "edit"]` → static schema lookup
/// - tool_registry: `{_type: "tool_registry", tools: [...]}` → extract schemas
/// - List of tool dicts: `[{name, description, parameters}, ...]` → use directly
fn resolve_tools_for_agent(
    val: &VmValue,
    provider: &str,
) -> Result<Option<Vec<serde_json::Value>>, VmError> {
    match val {
        VmValue::List(list) if list.is_empty() => Ok(None),
        VmValue::List(list) => {
            // Check if this is a list of strings or a list of dicts
            if matches!(list.first(), Some(VmValue::String(_))) {
                // String name list → static schema lookup
                let names: Vec<String> = list.iter().map(|v| v.display()).collect();
                let schemas = tool_names_to_schemas(&names, provider);
                Ok(if schemas.is_empty() {
                    None
                } else {
                    Some(schemas)
                })
            } else {
                // List of tool definition dicts → use vm_tools_to_native
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
            // tool_registry object → extract tool schemas via vm_tools_to_native
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
fn normalize_tool_args(name: &str, args: &serde_json::Value) -> serde_json::Value {
    let mut obj = match args.as_object() {
        Some(o) => o.clone(),
        None => return args.clone(),
    };

    if name == "edit" {
        // Normalize action aliases: mode, command → action
        if !obj.contains_key("action") {
            if let Some(v) = obj.remove("mode").or_else(|| obj.remove("command")) {
                obj.insert("action".to_string(), v);
            }
        }

        // For patch actions: normalize find→old_string, content→new_string
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

        // Normalize file→path alias
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
fn handle_tool_locally(name: &str, args: &serde_json::Value) -> Option<String> {
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

/// Register a tool-aware `agent_loop` that uses a bridge for tool execution.
/// This overrides the native text-only agent_loop with one that can:
/// 1. Pass tool definitions to the LLM
/// 2. Execute tool calls via the bridge (delegated to host)
/// 3. Feed tool results back into the conversation
pub fn register_agent_loop_with_bridge(vm: &mut Vm, bridge: Rc<crate::bridge::HostBridge>) {
    let b = bridge;
    vm.register_async_builtin("agent_loop", move |args| {
        let bridge = b.clone();
        async move {
            let prompt = args.first().map(|a| a.display()).unwrap_or_default();
            let system = args.get(1).map(|a| a.display());
            let options = args.get(2).and_then(|a| a.as_dict()).cloned();

            let provider = vm_resolve_provider(&options);
            let model = vm_resolve_model(&options, &provider);
            let api_key = vm_resolve_api_key(&provider)?;
            let max_iterations = opt_int(&options, "max_iterations").unwrap_or(25) as usize;
            let persistent = opt_bool(&options, "persistent");
            let max_nudges = opt_int(&options, "max_nudges").unwrap_or(5) as usize;
            let custom_nudge = opt_str(&options, "nudge");
            let max_tokens = opt_int(&options, "max_tokens").unwrap_or(8192);
            let tool_format = opt_str(&options, "tool_format")
                .unwrap_or_else(|| "text".to_string());

            // Resolve tool definitions from options.tools.
            // Accepts: string name list, tool_registry, or list of tool dicts.
            let tools_val = options.as_ref().and_then(|o| o.get("tools")).cloned();
            let tool_schemas = match &tools_val {
                Some(val) => resolve_tools_for_agent(val, &provider)?,
                None => None,
            };
            let has_tools = tool_schemas.as_ref().is_some_and(|t| !t.is_empty());

            eprintln!(
                "[agent_loop] model={}/{} prompt_chars={} sys_chars={} has_tools={} format={}",
                provider, model, prompt.len(),
                system.as_ref().map_or(0, |s| s.len()),
                has_tools, tool_format,
            );

            // Native format: pass tools via API. Text format: inject into prompt.
            let native_tools = if has_tools && tool_format == "native" {
                tool_schemas
            } else {
                None
            };

            let mut system_prompt = system.unwrap_or_default();

            // Always inject tool schema for text-based tool calling.
            // The schema lists tool names and parameters explicitly.
            // If the system prompt already has ```call examples, skip the format
            // instructions (the few-shot examples teach the format).
            if has_tools && tool_format == "text" {
                let has_examples = system_prompt.contains("```call");
                system_prompt.push_str(&build_text_tool_prompt(
                    tools_val.as_ref(),
                    !has_examples, // include format instructions only if no examples present
                ));
            }

            if persistent {
                system_prompt.push_str(
                    "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                     Do NOT stop to explain or summarize — take action with tools. \
                     When you are done, output ##DONE## on its own line.",
                );
            }

            let mut messages: Vec<serde_json::Value> = vec![serde_json::json!({
                "role": "user",
                "content": prompt,
            })];

            let mut total_text = String::new();
            let mut consecutive_text_only = 0usize;
            let mut all_tools_used: Vec<String> = Vec::new();
            let mut total_iterations = 0usize;
            let mut final_status = "done";
            let loop_start = std::time::Instant::now();

            for iteration in 0..max_iterations {
                total_iterations = iteration + 1;
                let start = std::time::Instant::now();
                let result = vm_call_llm_full(
                    &provider,
                    &model,
                    &api_key,
                    &messages,
                    if system_prompt.is_empty() {
                        None
                    } else {
                        Some(&system_prompt)
                    },
                    max_tokens,
                    None,
                    None,
                    None,
                    native_tools.as_deref(),
                )
                .await?;
                trace_llm_call(LlmTraceEntry {
                    model: result.model.clone(),
                    input_tokens: result.input_tokens,
                    output_tokens: result.output_tokens,
                    duration_ms: start.elapsed().as_millis() as u64,
                });

                let text = result.text.clone();
                total_text.push_str(&text);

                // Collect tool calls from native API or text parsing
                let tool_calls = if !result.tool_calls.is_empty() {
                    result.tool_calls.clone()
                } else if has_tools && tool_format == "text" {
                    parse_text_tool_calls(&text)
                } else {
                    Vec::new()
                };

                // Handle tool calls
                if !tool_calls.is_empty() {
                    consecutive_text_only = 0;

                    if tool_format == "native" {
                        // Native: add assistant message with tool_calls metadata
                        messages.push(build_assistant_tool_message(
                            &text, &tool_calls, &provider,
                        ));
                    } else {
                        // Text: add the raw assistant text (contains <tool_call> tags)
                        messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": text,
                        }));
                    }

                    // Execute each tool call
                    let mut observations = String::new();
                    let mut tools_used_this_iter: Vec<String> = Vec::new();
                    for tc in &tool_calls {
                        let tool_id = tc["id"].as_str().unwrap_or("");
                        let tool_name = tc["name"].as_str().unwrap_or("");
                        let tool_args = normalize_tool_args(tool_name, &tc["arguments"]);

                        tools_used_this_iter.push(tool_name.to_string());
                        eprintln!(
                            "[agent_loop] iter={iteration} tool={tool_name} args={}",
                            serde_json::to_string(&tool_args).unwrap_or_default()
                        );

                        // Try local handling first (read_file, list_directory),
                        // fall back to bridge for mutations and complex tools
                        let call_result = if let Some(local_result) =
                            handle_tool_locally(tool_name, &tool_args)
                        {
                            Ok(serde_json::Value::String(local_result))
                        } else {
                            bridge
                                .call(
                                    "builtin_call",
                                    serde_json::json!({
                                        "name": tool_name,
                                        "args": [tool_args],
                                    }),
                                )
                                .await
                        };

                        let result_text = match call_result {
                            Ok(val) => {
                                if let Some(s) = val.as_str() {
                                    s.to_string()
                                } else if val.is_null() {
                                    "(no output)".to_string()
                                } else {
                                    serde_json::to_string_pretty(&val).unwrap_or_default()
                                }
                            }
                            Err(e) => format!("Error: {e}"),
                        };

                        if tool_format == "native" {
                            messages.push(build_tool_result_message(
                                tool_id, &result_text, &provider,
                            ));
                        } else {
                            observations.push_str(&format!(
                                "<tool_result name=\"{tool_name}\">\n{result_text}\n</tool_result>\n\n"
                            ));
                        }
                    }

                    all_tools_used.extend(tools_used_this_iter);

                    // Text format: send all observations as one user message
                    if tool_format != "native" && !observations.is_empty() {
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": observations.trim_end(),
                        }));
                    }

                    continue; // Next iteration — LLM sees tool results
                }

                // Text-only response (no tool calls found)
                messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": text,
                }));

                if persistent && text.contains("##DONE##") {
                    break;
                }

                if !persistent {
                    break;
                }

                consecutive_text_only += 1;
                if consecutive_text_only > max_nudges {
                    eprintln!(
                        "[agent_loop] max nudges ({max_nudges}) reached after {iteration} iterations"
                    );
                    final_status = "stuck";
                    break;
                }

                // Escalating nudge: more directive each time
                let nudge = custom_nudge.clone().unwrap_or_else(|| {
                    if consecutive_text_only == 1 {
                        "You must use tools to complete this task. \
                         Use ```call blocks to invoke tools. Example:\n\
                         ```call\n\
                         read_file(path=\"relevant_file.py\")\n\
                         ```\n\
                         Start by reading a file relevant to the task."
                            .to_string()
                    } else if consecutive_text_only <= 3 {
                        "STOP explaining and USE TOOLS NOW. \
                         You have tools: read_file, search, edit, run. \
                         Call them with ```call blocks. \
                         If the task requires creating a file, use:\n\
                         ```call\n\
                         edit(action=\"create\", path=\"file.py\", content=\"\"\"your code\"\"\")\n\
                         ```\n\
                         Do NOT respond with text only — include a tool call."
                            .to_string()
                    } else {
                        "FINAL WARNING: You MUST use a ```call block NOW or the task will fail. \
                         Pick the single most important action and do it."
                            .to_string()
                    }
                });

                messages.push(serde_json::json!({
                    "role": "user",
                    "content": nudge,
                }));
            }

            // Return structured result for pipeline introspection.
            // Pipelines can access result.text, result.status, etc.
            let mut result_dict = BTreeMap::new();
            result_dict.insert("status".to_string(), VmValue::String(Rc::from(final_status)));
            result_dict.insert("text".to_string(), VmValue::String(Rc::from(total_text.as_str())));
            result_dict.insert("iterations".to_string(), VmValue::Int(total_iterations as i64));
            result_dict.insert("duration_ms".to_string(), VmValue::Int(loop_start.elapsed().as_millis() as i64));
            result_dict.insert(
                "tools_used".to_string(),
                VmValue::List(Rc::from(
                    all_tools_used.iter().map(|s| VmValue::String(Rc::from(s.as_str()))).collect::<Vec<_>>(),
                )),
            );
            Ok(VmValue::Dict(Rc::from(result_dict)))
        }
    });
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
fn build_text_tool_prompt(tools_val: Option<&VmValue>, include_format: bool) -> String {
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
            // tool_registry — extract from tools list
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
fn parse_text_tool_calls(text: &str) -> Vec<serde_json::Value> {
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
/// Also handles positional args: `read_file("foo.py")` → `{path: "foo.py"}`
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
                // Triple-quoted string: mostly raw, but process \" → " and \\ → \
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

// Continue register_llm_builtins (llm_stream and conversation builtins)
fn register_llm_builtins_continued(vm: &mut Vm) {
    // =========================================================================
    // llm_stream — SSE streaming
    // =========================================================================
    vm.register_async_builtin("llm_stream", |args| async move {
        let prompt = args.first().map(|a| a.display()).unwrap_or_default();
        let system = args.get(1).map(|a| a.display());
        let options = args.get(2).and_then(|a| a.as_dict()).cloned();

        let provider = vm_resolve_provider(&options);
        let model = vm_resolve_model(&options, &provider);
        let api_key = vm_resolve_api_key(&provider)?;
        let max_tokens = opt_int(&options, "max_tokens").unwrap_or(4096);

        let (tx, rx) = tokio::sync::mpsc::channel::<VmValue>(64);
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let closed_clone = closed.clone();
        #[allow(clippy::arc_with_non_send_sync)]
        let tx_arc = Arc::new(tx);
        let tx_for_task = tx_arc.clone();

        tokio::task::spawn_local(async move {
            // Mock provider: send deterministic chunks without API call
            if provider == "mock" {
                let words: Vec<&str> = prompt.split_whitespace().collect();
                for word in &words {
                    let _ = tx_for_task.send(VmValue::String(Rc::from(*word))).await;
                }
                closed_clone.store(true, std::sync::atomic::Ordering::Relaxed);
                return;
            }

            let result = vm_stream_llm(
                &provider,
                &model,
                &api_key,
                &prompt,
                system.as_deref(),
                max_tokens,
                &tx_for_task,
            )
            .await;
            closed_clone.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Err(e) = result {
                let _ = tx_for_task
                    .send(VmValue::String(Rc::from(format!("error: {e}"))))
                    .await;
            }
        });

        #[allow(clippy::arc_with_non_send_sync)]
        let handle = VmChannelHandle {
            name: "llm_stream".to_string(),
            sender: tx_arc,
            receiver: Arc::new(tokio::sync::Mutex::new(rx)),
            closed,
        };
        Ok(VmValue::Channel(handle))
    });

    // =========================================================================
    // Conversation management builtins
    // =========================================================================

    vm.register_builtin("conversation", |_args, _out| {
        // Returns a list (messages array) — can be passed to llm_call via options.messages
        Ok(VmValue::List(Rc::new(Vec::new())))
    });

    vm.register_builtin("add_message", |args, _out| {
        let messages = match args.first() {
            Some(VmValue::List(list)) => (**list).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "add_message: first argument must be a message list",
                ))));
            }
        };
        let role = args.get(1).map(|a| a.display()).unwrap_or_default();
        let content = args.get(2).map(|a| a.display()).unwrap_or_default();

        let mut msg = BTreeMap::new();
        msg.insert("role".to_string(), VmValue::String(Rc::from(role.as_str())));
        msg.insert(
            "content".to_string(),
            VmValue::String(Rc::from(content.as_str())),
        );

        let mut new_messages = messages;
        new_messages.push(VmValue::Dict(Rc::new(msg)));
        Ok(VmValue::List(Rc::new(new_messages)))
    });

    vm.register_builtin("add_user", |args, _out| vm_add_role_message(args, "user"));

    vm.register_builtin("add_assistant", |args, _out| {
        vm_add_role_message(args, "assistant")
    });

    vm.register_builtin("add_system", |args, _out| {
        vm_add_role_message(args, "system")
    });

    vm.register_builtin("add_tool_result", |args, _out| {
        let messages = match args.first() {
            Some(VmValue::List(list)) => (**list).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "add_tool_result: first argument must be a message list",
                ))));
            }
        };
        let tool_use_id = args.get(1).map(|a| a.display()).unwrap_or_default();
        let result_content = args.get(2).map(|a| a.display()).unwrap_or_default();

        let mut msg = BTreeMap::new();
        msg.insert("role".to_string(), VmValue::String(Rc::from("tool_result")));
        msg.insert(
            "tool_use_id".to_string(),
            VmValue::String(Rc::from(tool_use_id.as_str())),
        );
        msg.insert(
            "content".to_string(),
            VmValue::String(Rc::from(result_content.as_str())),
        );

        let mut new_messages = messages;
        new_messages.push(VmValue::Dict(Rc::new(msg)));
        Ok(VmValue::List(Rc::new(new_messages)))
    });

    // =========================================================================
    // Config-based builtins
    // =========================================================================

    vm.register_builtin("llm_infer_provider", |args, _out| {
        let model_id = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(
            llm_config::infer_provider(&model_id).as_str(),
        )))
    });

    vm.register_builtin("llm_model_tier", |args, _out| {
        let model_id = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(
            llm_config::model_tier(&model_id).as_str(),
        )))
    });

    vm.register_builtin("llm_resolve_model", |args, _out| {
        let alias = args.first().map(|a| a.display()).unwrap_or_default();
        let (id, provider) = llm_config::resolve_model(&alias);
        let mut dict = BTreeMap::new();
        dict.insert("id".to_string(), VmValue::String(Rc::from(id.as_str())));
        dict.insert(
            "provider".to_string(),
            provider
                .map(|p| VmValue::String(Rc::from(p.as_str())))
                .unwrap_or(VmValue::Nil),
        );
        Ok(VmValue::Dict(Rc::new(dict)))
    });

    vm.register_builtin("llm_providers", |_args, _out| {
        let names = llm_config::provider_names();
        let list: Vec<VmValue> = names
            .into_iter()
            .map(|n| VmValue::String(Rc::from(n.as_str())))
            .collect();
        Ok(VmValue::List(Rc::new(list)))
    });

    vm.register_builtin("llm_config", |args, _out| {
        let provider_name = args.first().map(|a| a.display());
        match provider_name {
            Some(name) => {
                if let Some(pdef) = llm_config::provider_config(&name) {
                    Ok(provider_def_to_vm_value(pdef))
                } else {
                    Ok(VmValue::Nil)
                }
            }
            None => {
                // Return all providers as a dict
                let mut dict = BTreeMap::new();
                for name in llm_config::provider_names() {
                    if let Some(pdef) = llm_config::provider_config(&name) {
                        dict.insert(name, provider_def_to_vm_value(pdef));
                    }
                }
                Ok(VmValue::Dict(Rc::new(dict)))
            }
        }
    });

    vm.register_async_builtin("llm_healthcheck", |args| async move {
        let provider_name = args
            .first()
            .map(|a| a.display())
            .unwrap_or_else(|| "anthropic".to_string());

        let api_key = vm_resolve_api_key(&provider_name).unwrap_or_default();

        let pdef = match llm_config::provider_config(&provider_name) {
            Some(p) => p,
            None => {
                return Ok(healthcheck_result(
                    false,
                    &format!("Unknown provider: {provider_name}"),
                ));
            }
        };

        let hc = match &pdef.healthcheck {
            Some(h) => h,
            None => {
                return Ok(healthcheck_result(
                    false,
                    &format!("No healthcheck configured for {provider_name}"),
                ));
            }
        };

        // Build URL
        let url = if let Some(absolute_url) = &hc.url {
            absolute_url.clone()
        } else {
            let base = llm_config::resolve_base_url(pdef);
            let path = hc.path.as_deref().unwrap_or("");
            format!("{base}{path}")
        };

        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let mut req = match hc.method.to_uppercase().as_str() {
            "POST" => {
                let mut r = client.post(&url).header("Content-Type", "application/json");
                if let Some(body) = &hc.body {
                    r = r.body(body.clone());
                }
                r
            }
            _ => client.get(&url),
        };

        // Apply auth
        req = apply_auth_headers(req, &api_key, Some(pdef));
        if let Some(p) = llm_config::provider_config(&provider_name) {
            for (k, v) in &p.extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
        }

        match req.send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                let valid = response.status().is_success();
                let body_text = response.text().await.unwrap_or_default();
                let message = if valid {
                    format!("{provider_name} is reachable (HTTP {status})")
                } else {
                    format!("{provider_name} returned HTTP {status}: {body_text}")
                };
                let mut dict = BTreeMap::new();
                dict.insert("valid".to_string(), VmValue::Bool(valid));
                dict.insert(
                    "message".to_string(),
                    VmValue::String(Rc::from(message.as_str())),
                );
                let mut meta = BTreeMap::new();
                meta.insert("status".to_string(), VmValue::Int(status as i64));
                meta.insert("url".to_string(), VmValue::String(Rc::from(url.as_str())));
                dict.insert("metadata".to_string(), VmValue::Dict(Rc::new(meta)));
                Ok(VmValue::Dict(Rc::new(dict)))
            }
            Err(e) => Ok(healthcheck_result(
                false,
                &format!("{provider_name} healthcheck failed: {e}"),
            )),
        }
    });
}

/// Convert a ProviderDef to a VmValue dict for the llm_config builtin.
fn provider_def_to_vm_value(pdef: &llm_config::ProviderDef) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "base_url".to_string(),
        VmValue::String(Rc::from(pdef.base_url.as_str())),
    );
    dict.insert(
        "auth_style".to_string(),
        VmValue::String(Rc::from(pdef.auth_style.as_str())),
    );
    dict.insert(
        "chat_endpoint".to_string(),
        VmValue::String(Rc::from(pdef.chat_endpoint.as_str())),
    );
    if let Some(header) = &pdef.auth_header {
        dict.insert(
            "auth_header".to_string(),
            VmValue::String(Rc::from(header.as_str())),
        );
    }
    if !pdef.extra_headers.is_empty() {
        let mut headers = BTreeMap::new();
        for (k, v) in &pdef.extra_headers {
            headers.insert(k.clone(), VmValue::String(Rc::from(v.as_str())));
        }
        dict.insert("extra_headers".to_string(), VmValue::Dict(Rc::new(headers)));
    }
    if !pdef.features.is_empty() {
        let features: Vec<VmValue> = pdef
            .features
            .iter()
            .map(|f| VmValue::String(Rc::from(f.as_str())))
            .collect();
        dict.insert("features".to_string(), VmValue::List(Rc::new(features)));
    }
    VmValue::Dict(Rc::new(dict))
}

/// Build a healthcheck result dict.
fn healthcheck_result(valid: bool, message: &str) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("valid".to_string(), VmValue::Bool(valid));
    dict.insert("message".to_string(), VmValue::String(Rc::from(message)));
    dict.insert(
        "metadata".to_string(),
        VmValue::Dict(Rc::new(BTreeMap::new())),
    );
    VmValue::Dict(Rc::new(dict))
}

// =============================================================================
// LLM response type
// =============================================================================

struct LlmResult {
    text: String,
    tool_calls: Vec<serde_json::Value>,
    input_tokens: i64,
    output_tokens: i64,
    model: String,
}

fn vm_build_llm_result(result: &LlmResult, parsed_json: Option<VmValue>) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "text".to_string(),
        VmValue::String(Rc::from(result.text.as_str())),
    );
    dict.insert(
        "model".to_string(),
        VmValue::String(Rc::from(result.model.as_str())),
    );
    dict.insert(
        "input_tokens".to_string(),
        VmValue::Int(result.input_tokens),
    );
    dict.insert(
        "output_tokens".to_string(),
        VmValue::Int(result.output_tokens),
    );

    if let Some(json_val) = parsed_json {
        dict.insert("data".to_string(), json_val);
    }

    if !result.tool_calls.is_empty() {
        let calls: Vec<VmValue> = result.tool_calls.iter().map(json_to_vm_value).collect();
        dict.insert("tool_calls".to_string(), VmValue::List(Rc::new(calls)));
    }

    VmValue::Dict(Rc::new(dict))
}

// =============================================================================
// Helper: add a role message to a conversation list
// =============================================================================

fn vm_add_role_message(args: &[VmValue], role: &str) -> Result<VmValue, VmError> {
    let messages = match args.first() {
        Some(VmValue::List(list)) => (**list).clone(),
        _ => {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "add_{role}: first argument must be a message list"
            )))));
        }
    };
    let content = args.get(1).map(|a| a.display()).unwrap_or_default();

    let mut msg = BTreeMap::new();
    msg.insert(
        "role".to_string(),
        VmValue::String(Rc::from(role.to_string().as_str())),
    );
    msg.insert(
        "content".to_string(),
        VmValue::String(Rc::from(content.as_str())),
    );

    let mut new_messages = messages;
    new_messages.push(VmValue::Dict(Rc::new(msg)));
    Ok(VmValue::List(Rc::new(new_messages)))
}

// =============================================================================
// Option extraction helpers
// =============================================================================

fn opt_str(options: &Option<BTreeMap<String, VmValue>>, key: &str) -> Option<String> {
    options.as_ref()?.get(key).map(|v| v.display())
}

fn opt_int(options: &Option<BTreeMap<String, VmValue>>, key: &str) -> Option<i64> {
    options.as_ref()?.get(key)?.as_int()
}

fn opt_float(options: &Option<BTreeMap<String, VmValue>>, key: &str) -> Option<f64> {
    options.as_ref()?.get(key).and_then(|v| match v {
        VmValue::Float(f) => Some(*f),
        VmValue::Int(i) => Some(*i as f64),
        _ => None,
    })
}

fn opt_bool(options: &Option<BTreeMap<String, VmValue>>, key: &str) -> bool {
    options
        .as_ref()
        .and_then(|o| o.get(key))
        .map(|v| v.is_truthy())
        .unwrap_or(false)
}

// =============================================================================
// Provider/model/key resolution
// =============================================================================

fn vm_resolve_provider(options: &Option<BTreeMap<String, VmValue>>) -> String {
    // Explicit option wins
    if let Some(p) = options
        .as_ref()
        .and_then(|o| o.get("provider"))
        .map(|v| v.display())
    {
        return p;
    }
    // Env var next
    if let Ok(p) = std::env::var("HARN_LLM_PROVIDER") {
        return p;
    }
    // Try to infer from model
    if let Some(m) = options
        .as_ref()
        .and_then(|o| o.get("model"))
        .map(|v| v.display())
    {
        return llm_config::infer_provider(&m);
    }
    if let Ok(m) = std::env::var("HARN_LLM_MODEL") {
        return llm_config::infer_provider(&m);
    }
    "anthropic".to_string()
}

fn vm_resolve_model(options: &Option<BTreeMap<String, VmValue>>, provider: &str) -> String {
    let raw = options
        .as_ref()
        .and_then(|o| o.get("model"))
        .map(|v| v.display())
        .or_else(|| std::env::var("HARN_LLM_MODEL").ok());

    if let Some(raw) = raw {
        let (resolved, _) = llm_config::resolve_model(&raw);
        return resolved;
    }
    // Default model per provider
    match provider {
        "openai" => "gpt-4o".to_string(),
        "ollama" => "llama3.2".to_string(),
        "openrouter" => "anthropic/claude-sonnet-4-20250514".to_string(),
        _ => "claude-sonnet-4-20250514".to_string(),
    }
}

fn vm_resolve_api_key(provider: &str) -> Result<String, VmError> {
    if provider == "mock" {
        return Ok(String::new());
    }

    if let Some(pdef) = llm_config::provider_config(provider) {
        if pdef.auth_style == "none" {
            return Ok(String::new());
        }
        match &pdef.auth_env {
            llm_config::AuthEnv::Single(env) => {
                return std::env::var(env).map_err(|_| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "Missing API key: set {env} environment variable"
                    ))))
                });
            }
            llm_config::AuthEnv::Multiple(envs) => {
                for env in envs {
                    if let Ok(val) = std::env::var(env) {
                        if !val.is_empty() {
                            return Ok(val);
                        }
                    }
                }
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Missing API key: set one of {} environment variables",
                    envs.join(", ")
                )))));
            }
            llm_config::AuthEnv::None => return Ok(String::new()),
        }
    }
    // Fallback for unknown providers
    std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        VmError::Thrown(VmValue::String(Rc::from(
            "Missing API key: set ANTHROPIC_API_KEY environment variable",
        )))
    })
}

// =============================================================================
// Convert VmValue messages to JSON for API calls
// =============================================================================

fn vm_messages_to_json(msg_list: &[VmValue]) -> Result<Vec<serde_json::Value>, VmError> {
    let mut messages = Vec::new();
    for msg in msg_list {
        if let VmValue::Dict(d) = msg {
            let role = d
                .get("role")
                .map(|v| v.display())
                .unwrap_or_else(|| "user".to_string());
            let content = d.get("content").map(|v| v.display()).unwrap_or_default();

            if role == "tool_result" {
                // Anthropic tool result format
                let tool_use_id = d
                    .get("tool_use_id")
                    .map(|v| v.display())
                    .unwrap_or_default();
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content,
                    }],
                }));
            } else {
                messages.push(serde_json::json!({
                    "role": role,
                    "content": content,
                }));
            }
        }
    }
    Ok(messages)
}

// =============================================================================
// Convert tool_registry to native tool definitions
// =============================================================================

fn vm_tools_to_native(
    tools_val: &VmValue,
    provider: &str,
) -> Result<Vec<serde_json::Value>, VmError> {
    // Accept either a tool_registry dict or a list of tool dicts
    let tools_list = match tools_val {
        VmValue::Dict(d) => {
            // tool_registry — extract tools list
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
        if let VmValue::Dict(entry) = tool {
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

// =============================================================================
// Mock LLM provider — deterministic responses for testing without API keys
// =============================================================================

fn mock_llm_response(
    messages: &[serde_json::Value],
    system: Option<&str>,
    native_tools: Option<&[serde_json::Value]>,
) -> LlmResult {
    // Extract the last user message for generating a deterministic response.
    let last_msg = messages
        .last()
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    // If tools are provided, generate a mock tool call for the first tool.
    if let Some(tools) = native_tools {
        if let Some(first_tool) = tools.first() {
            let tool_name = first_tool
                .get("name")
                .or_else(|| first_tool.get("function").and_then(|f| f.get("name")))
                .and_then(|n| n.as_str())
                .unwrap_or("unknown");
            return LlmResult {
                text: String::new(),
                tool_calls: vec![serde_json::json!({
                    "id": "mock_call_1",
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": "{}"
                    }
                })],
                input_tokens: last_msg.len() as i64,
                output_tokens: 20,
                model: "mock".to_string(),
            };
        }
    }

    // Generate response based on the prompt content.
    // Include ##DONE## if the system prompt mentions it (agent_loop compatibility).
    let done_sentinel = if system.is_some_and(|s| s.contains("##DONE##")) {
        " ##DONE##"
    } else {
        ""
    };

    let response = if last_msg.is_empty() {
        format!("Mock LLM response{done_sentinel}")
    } else {
        let word_count = last_msg.split_whitespace().count();
        format!(
            "Mock response to {word_count}-word prompt: {}{done_sentinel}",
            last_msg.chars().take(100).collect::<String>()
        )
    };

    LlmResult {
        text: response,
        tool_calls: vec![],
        input_tokens: last_msg.len() as i64,
        output_tokens: 30,
        model: "mock".to_string(),
    }
}

// =============================================================================
// Core LLM call with all options
// =============================================================================

#[allow(clippy::too_many_arguments)]
async fn vm_call_llm_full(
    provider: &str,
    model: &str,
    api_key: &str,
    messages: &[serde_json::Value],
    system: Option<&str>,
    max_tokens: i64,
    response_format: Option<&str>,
    json_schema: Option<&BTreeMap<String, VmValue>>,
    temperature: Option<f64>,
    native_tools: Option<&[serde_json::Value]>,
) -> Result<LlmResult, VmError> {
    // Mock provider: return deterministic response without API call.
    if provider == "mock" {
        return Ok(mock_llm_response(messages, system, native_tools));
    }

    let replay_mode = get_replay_mode();
    let hash = fixture_hash(model, messages, system);

    // In replay mode, return cached fixture
    if replay_mode == LlmReplayMode::Replay {
        if let Some(result) = load_fixture(&hash) {
            return Ok(result);
        }
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "No fixture found for LLM call (hash: {hash}). Run with --record first."
        )))));
    }

    let result = vm_call_llm_api(
        provider,
        model,
        api_key,
        messages,
        system,
        max_tokens,
        response_format,
        json_schema,
        temperature,
        native_tools,
    )
    .await?;

    // In record mode, save the fixture
    if replay_mode == LlmReplayMode::Record {
        save_fixture(&hash, &result);
    }

    Ok(result)
}

#[allow(clippy::too_many_arguments)]
async fn vm_call_llm_api(
    provider: &str,
    model: &str,
    api_key: &str,
    messages: &[serde_json::Value],
    system: Option<&str>,
    max_tokens: i64,
    response_format: Option<&str>,
    json_schema: Option<&BTreeMap<String, VmValue>>,
    temperature: Option<f64>,
    native_tools: Option<&[serde_json::Value]>,
) -> Result<LlmResult, VmError> {
    let llm_timeout = std::env::var("HARN_LLM_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(120);
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(llm_timeout))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    // Resolve provider config for base URL and auth
    let pdef = llm_config::provider_config(provider);
    let is_anthropic_style = pdef
        .map(|p| p.chat_endpoint.contains("/messages"))
        .unwrap_or(provider == "anthropic");

    if is_anthropic_style {
        // Anthropic-style API (system as top-level field, content blocks response)
        let base_url = pdef
            .map(llm_config::resolve_base_url)
            .unwrap_or_else(|| "https://api.anthropic.com/v1".to_string());
        let endpoint = pdef
            .map(|p| p.chat_endpoint.as_str())
            .unwrap_or("/messages");

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "max_tokens": max_tokens,
        });
        if let Some(sys) = system {
            body["system"] = serde_json::json!(sys);
        }
        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        // Native tool use for Anthropic
        if let Some(tools) = native_tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(tools);
            }
        }

        let mut req = client
            .post(format!("{base_url}{endpoint}"))
            .header("Content-Type", "application/json")
            .json(&body);

        // Apply auth from config
        req = apply_auth_headers(req, api_key, pdef);

        // Apply extra headers from config
        if let Some(p) = pdef {
            for (k, v) in &p.extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
        }

        let response = req.send().await.map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "{provider} API error: {e}"
            ))))
        })?;

        let json: serde_json::Value = response.json().await.map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "{provider} response parse error: {e}"
            ))))
        })?;

        // Extract text and tool_use blocks from Anthropic response
        let mut text = String::new();
        let mut tool_calls = Vec::new();

        if let Some(content) = json["content"].as_array() {
            for block in content {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = block["text"].as_str() {
                            text.push_str(t);
                        }
                    }
                    Some("tool_use") => {
                        let name = block["name"].as_str().unwrap_or("").to_string();
                        let id = block["id"].as_str().unwrap_or("").to_string();
                        let input = block["input"].clone();
                        tool_calls.push(serde_json::json!({
                            "id": id,
                            "name": name,
                            "arguments": input,
                        }));
                    }
                    _ => {}
                }
            }
        }

        if text.is_empty() && tool_calls.is_empty() {
            if let Some(err) = json["error"]["message"].as_str() {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "{provider} API error: {err}"
                )))));
            }
        }

        let input_tokens = json["usage"]["input_tokens"].as_i64().unwrap_or(0);
        let output_tokens = json["usage"]["output_tokens"].as_i64().unwrap_or(0);

        Ok(LlmResult {
            text,
            tool_calls,
            input_tokens,
            output_tokens,
            model: model.to_string(),
        })
    } else {
        // OpenAI-compatible API (system as message, choices response)
        let base_url = pdef
            .map(llm_config::resolve_base_url)
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let endpoint = pdef
            .map(|p| p.chat_endpoint.as_str())
            .unwrap_or("/chat/completions");

        let mut msgs = Vec::new();
        if let Some(sys) = system {
            msgs.push(serde_json::json!({"role": "system", "content": sys}));
        }
        msgs.extend(messages.iter().cloned());

        let mut body = serde_json::json!({
            "model": model,
            "messages": msgs,
            "max_tokens": max_tokens,
        });

        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        // Structured output for OpenAI
        if response_format == Some("json") {
            if let Some(schema) = json_schema {
                let schema_json = vm_value_dict_to_json(schema);
                body["response_format"] = serde_json::json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": "response",
                        "schema": schema_json,
                        "strict": true,
                    }
                });
            } else {
                body["response_format"] = serde_json::json!({"type": "json_object"});
            }
        }

        // Native tool use
        if let Some(tools) = native_tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(tools);
            }
        }

        let mut req = client
            .post(format!("{base_url}{endpoint}"))
            .header("Content-Type", "application/json")
            .json(&body);

        // Apply auth from config
        req = apply_auth_headers(req, api_key, pdef);

        // Apply extra headers from config
        if let Some(p) = pdef {
            for (k, v) in &p.extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
        }

        let response = req.send().await.map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "{provider} API error: {e}"
            ))))
        })?;

        let json: serde_json::Value = response.json().await.map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "{provider} response parse error: {e}"
            ))))
        })?;

        if let Some(err) = json["error"]["message"].as_str() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "{provider} API error: {err}"
            )))));
        }

        let text = json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();

        // Extract tool calls from OpenAI format
        let mut tool_calls = Vec::new();
        if let Some(calls) = json["choices"][0]["message"]["tool_calls"].as_array() {
            for call in calls {
                let name = call["function"]["name"].as_str().unwrap_or("").to_string();
                let args_str = call["function"]["arguments"].as_str().unwrap_or("{}");
                let arguments: serde_json::Value =
                    serde_json::from_str(args_str).unwrap_or(serde_json::json!({}));
                let id = call["id"].as_str().unwrap_or("").to_string();
                tool_calls.push(serde_json::json!({
                    "id": id,
                    "name": name,
                    "arguments": arguments,
                }));
            }
        }

        let input_tokens = json["usage"]["prompt_tokens"].as_i64().unwrap_or(0);
        let output_tokens = json["usage"]["completion_tokens"].as_i64().unwrap_or(0);

        Ok(LlmResult {
            text,
            tool_calls,
            input_tokens,
            output_tokens,
            model: model.to_string(),
        })
    }
}

/// Apply auth headers to a request based on provider config.
fn apply_auth_headers(
    req: reqwest::RequestBuilder,
    api_key: &str,
    pdef: Option<&llm_config::ProviderDef>,
) -> reqwest::RequestBuilder {
    if api_key.is_empty() {
        return req;
    }
    if let Some(p) = pdef {
        match p.auth_style.as_str() {
            "header" => {
                let header_name = p.auth_header.as_deref().unwrap_or("x-api-key");
                req.header(header_name, api_key)
            }
            "bearer" => req.header("Authorization", format!("Bearer {api_key}")),
            "none" => req,
            _ => req.header("Authorization", format!("Bearer {api_key}")),
        }
    } else {
        // Unknown provider: default to bearer
        req.header("Authorization", format!("Bearer {api_key}"))
    }
}

// =============================================================================
// Streaming
// =============================================================================

async fn vm_stream_llm(
    provider: &str,
    model: &str,
    api_key: &str,
    prompt: &str,
    system: Option<&str>,
    max_tokens: i64,
    tx: &tokio::sync::mpsc::Sender<VmValue>,
) -> Result<(), VmError> {
    use reqwest_eventsource::{Event, EventSource};
    use tokio_stream::StreamExt;

    // Streaming: only connect_timeout (no overall timeout — streams can run long)
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let pdef = llm_config::provider_config(provider);
    let is_anthropic = pdef
        .map(|p| p.chat_endpoint.contains("/messages"))
        .unwrap_or(provider == "anthropic");

    let request = if is_anthropic {
        let base_url = pdef
            .map(llm_config::resolve_base_url)
            .unwrap_or_else(|| "https://api.anthropic.com/v1".to_string());
        let endpoint = pdef
            .map(|p| p.chat_endpoint.as_str())
            .unwrap_or("/messages");

        let mut body = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": max_tokens,
            "stream": true,
        });
        if let Some(sys) = system {
            body["system"] = serde_json::json!(sys);
        }

        let mut req = client
            .post(format!("{base_url}{endpoint}"))
            .header("Content-Type", "application/json")
            .json(&body);
        req = apply_auth_headers(req, api_key, pdef);
        if let Some(p) = pdef {
            for (k, v) in &p.extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
        }
        req
    } else {
        let base_url = pdef
            .map(llm_config::resolve_base_url)
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let endpoint = pdef
            .map(|p| p.chat_endpoint.as_str())
            .unwrap_or("/chat/completions");

        let mut msgs = Vec::new();
        if let Some(sys) = system {
            msgs.push(serde_json::json!({"role": "system", "content": sys}));
        }
        msgs.push(serde_json::json!({"role": "user", "content": prompt}));

        let body = serde_json::json!({
            "model": model,
            "messages": msgs,
            "max_tokens": max_tokens,
            "stream": true,
        });

        let mut req = client
            .post(format!("{base_url}{endpoint}"))
            .header("Content-Type", "application/json")
            .json(&body);
        req = apply_auth_headers(req, api_key, pdef);
        if let Some(p) = pdef {
            for (k, v) in &p.extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
        }
        req
    };

    let mut es = EventSource::new(request).map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "LLM stream setup error: {e}"
        ))))
    })?;

    while let Some(event) = es.next().await {
        match event {
            Ok(Event::Message(msg)) => {
                if msg.data == "[DONE]" {
                    break;
                }
                let chunk_text = if is_anthropic {
                    parse_anthropic_sse_chunk(&msg.data)
                } else {
                    parse_openai_sse_chunk(&msg.data)
                };
                if let Some(text) = chunk_text {
                    if !text.is_empty()
                        && tx
                            .send(VmValue::String(Rc::from(text.as_str())))
                            .await
                            .is_err()
                    {
                        break;
                    }
                }
            }
            Ok(Event::Open) => {}
            Err(_) => break,
        }
    }

    es.close();
    Ok(())
}

fn parse_openai_sse_chunk(data: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(data).ok()?;
    json["choices"][0]["delta"]["content"]
        .as_str()
        .map(|s| s.to_string())
}

fn parse_anthropic_sse_chunk(data: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(data).ok()?;
    if json["type"].as_str() == Some("content_block_delta") {
        return json["delta"]["text"].as_str().map(|s| s.to_string());
    }
    None
}

// =============================================================================
// Utility helpers
// =============================================================================

/// Extract JSON from a string that may contain markdown fences.
/// Looks for opening/closing fence pairs on their own lines to avoid matching
/// embedded backticks within JSON content.
fn extract_json(text: &str) -> &str {
    let trimmed = text.trim();

    // Find ```json\n or ```\n at the start of a line, then the closing ``` on its own line
    for fence_start in ["```json", "```"] {
        if let Some(start) = trimmed.find(fence_start) {
            let after_fence = &trimmed[start + fence_start.len()..];
            // Skip to the next newline (end of opening fence line)
            let content_start = after_fence.find('\n').map(|i| i + 1).unwrap_or(0);
            let content = &after_fence[content_start..];
            // Find closing ``` that appears at the start of a line
            for (i, line) in content.lines().enumerate() {
                if line.trim_start().starts_with("```") {
                    // Return everything before this line
                    let byte_offset: usize = content
                        .lines()
                        .take(i)
                        .map(|l| l.len() + 1) // +1 for \n
                        .sum();
                    return content[..byte_offset].trim();
                }
            }
        }
    }

    // No fences found — try to find a JSON object/array directly
    trimmed
}

/// Convert a VmValue dict to serde_json::Value for API payloads.
fn vm_value_dict_to_json(dict: &BTreeMap<String, VmValue>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in dict {
        map.insert(k.clone(), vm_value_to_json(v));
    }
    serde_json::Value::Object(map)
}

pub fn vm_value_to_json(val: &VmValue) -> serde_json::Value {
    match val {
        VmValue::Int(i) => serde_json::json!(i),
        VmValue::Float(f) => serde_json::json!(f),
        VmValue::String(s) => serde_json::json!(s.as_ref()),
        VmValue::Bool(b) => serde_json::json!(b),
        VmValue::Nil => serde_json::Value::Null,
        VmValue::List(list) => {
            serde_json::Value::Array(list.iter().map(vm_value_to_json).collect())
        }
        VmValue::Dict(d) => vm_value_dict_to_json(d),
        _ => serde_json::json!(val.display()),
    }
}
