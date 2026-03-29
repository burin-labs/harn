use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};

// =============================================================================
// Option extraction helpers
// =============================================================================

pub(crate) fn opt_str(options: &Option<BTreeMap<String, VmValue>>, key: &str) -> Option<String> {
    options.as_ref()?.get(key).map(|v| v.display())
}

pub(crate) fn opt_int(options: &Option<BTreeMap<String, VmValue>>, key: &str) -> Option<i64> {
    options.as_ref()?.get(key)?.as_int()
}

pub(crate) fn opt_float(options: &Option<BTreeMap<String, VmValue>>, key: &str) -> Option<f64> {
    options.as_ref()?.get(key).and_then(|v| match v {
        VmValue::Float(f) => Some(*f),
        VmValue::Int(i) => Some(*i as f64),
        _ => None,
    })
}

pub(crate) fn opt_bool(options: &Option<BTreeMap<String, VmValue>>, key: &str) -> bool {
    options
        .as_ref()
        .and_then(|o| o.get(key))
        .map(|v| v.is_truthy())
        .unwrap_or(false)
}

// =============================================================================
// Provider/model/key resolution
// =============================================================================

pub(crate) fn vm_resolve_provider(options: &Option<BTreeMap<String, VmValue>>) -> String {
    use crate::llm_config;
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

pub(crate) fn vm_resolve_model(
    options: &Option<BTreeMap<String, VmValue>>,
    provider: &str,
) -> String {
    use crate::llm_config;
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

pub(crate) fn vm_resolve_api_key(provider: &str) -> Result<String, VmError> {
    use crate::llm_config;
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

pub(crate) fn vm_messages_to_json(msg_list: &[VmValue]) -> Result<Vec<serde_json::Value>, VmError> {
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
// Helper: add a role message to a conversation list
// =============================================================================

pub(crate) fn vm_add_role_message(args: &[VmValue], role: &str) -> Result<VmValue, VmError> {
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
// Utility helpers
// =============================================================================

/// Extract JSON from a string that may contain markdown fences.
/// Looks for opening/closing fence pairs on their own lines to avoid matching
/// embedded backticks within JSON content.
pub(crate) fn extract_json(text: &str) -> &str {
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

    // No fences found -- try to find a JSON object/array directly
    trimmed
}

/// Convert a VmValue dict to serde_json::Value for API payloads.
pub(crate) fn vm_value_dict_to_json(dict: &BTreeMap<String, VmValue>) -> serde_json::Value {
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
