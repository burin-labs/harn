use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

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

        for _iteration in 0..max_iterations {
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

        Ok(VmValue::String(Rc::from(total_text.as_str())))
    });

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
    options
        .as_ref()
        .and_then(|o| o.get("provider"))
        .map(|v| v.display())
        .unwrap_or_else(|| "anthropic".to_string())
}

fn vm_resolve_model(options: &Option<BTreeMap<String, VmValue>>, provider: &str) -> String {
    options
        .as_ref()
        .and_then(|o| o.get("model"))
        .map(|v| v.display())
        .unwrap_or_else(|| match provider {
            "openai" => "gpt-4o".to_string(),
            "ollama" => "llama3.2".to_string(),
            "openrouter" => "anthropic/claude-sonnet-4-20250514".to_string(),
            _ => "claude-sonnet-4-20250514".to_string(),
        })
}

fn vm_resolve_api_key(provider: &str) -> Result<String, VmError> {
    match provider {
        "mock" | "ollama" => Ok(String::new()),
        "openai" => std::env::var("OPENAI_API_KEY").map_err(|_| {
            VmError::Thrown(VmValue::String(Rc::from(
                "Missing API key: set OPENAI_API_KEY environment variable",
            )))
        }),
        "openrouter" => std::env::var("OPENROUTER_API_KEY").map_err(|_| {
            VmError::Thrown(VmValue::String(Rc::from(
                "Missing API key: set OPENROUTER_API_KEY environment variable",
            )))
        }),
        _ => std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
            VmError::Thrown(VmValue::String(Rc::from(
                "Missing API key: set ANTHROPIC_API_KEY environment variable",
            )))
        }),
    }
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
    let client = reqwest::Client::new();

    match provider {
        "openai" | "ollama" | "openrouter" => {
            let base_url = match provider {
                "ollama" => std::env::var("OLLAMA_HOST")
                    .unwrap_or_else(|_| "http://localhost:11434".to_string()),
                "openrouter" => "https://openrouter.ai/api".to_string(),
                _ => "https://api.openai.com".to_string(),
            };

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
                    // OpenAI structured output with JSON schema
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

            // Native tool use for OpenAI
            if let Some(tools) = native_tools {
                if !tools.is_empty() {
                    body["tools"] = serde_json::json!(tools);
                }
            }

            let mut req = client
                .post(format!("{base_url}/v1/chat/completions"))
                .header("Content-Type", "application/json")
                .json(&body);

            if !api_key.is_empty() {
                req = req.header("Authorization", format!("Bearer {api_key}"));
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
        _ => {
            // Anthropic
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

            let response = client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "Anthropic API error: {e}"
                    ))))
                })?;

            let json: serde_json::Value = response.json().await.map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Anthropic response parse error: {e}"
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
                        "Anthropic API error: {err}"
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
        }
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

    let client = reqwest::Client::new();

    let request = match provider {
        "openai" | "ollama" | "openrouter" => {
            let base_url = match provider {
                "ollama" => std::env::var("OLLAMA_HOST")
                    .unwrap_or_else(|_| "http://localhost:11434".to_string()),
                "openrouter" => "https://openrouter.ai/api".to_string(),
                _ => "https://api.openai.com".to_string(),
            };

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
                .post(format!("{base_url}/v1/chat/completions"))
                .header("Content-Type", "application/json")
                .json(&body);
            if !api_key.is_empty() {
                req = req.header("Authorization", format!("Bearer {api_key}"));
            }
            req
        }
        _ => {
            let mut body = serde_json::json!({
                "model": model,
                "messages": [{"role": "user", "content": prompt}],
                "max_tokens": max_tokens,
                "stream": true,
            });
            if let Some(sys) = system {
                body["system"] = serde_json::json!(sys);
            }

            client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .json(&body)
        }
    };

    let is_anthropic = !matches!(provider, "openai" | "ollama" | "openrouter");

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
