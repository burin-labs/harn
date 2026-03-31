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
// Resolved provider config (shared between api.rs and stream.rs)
// =============================================================================

pub(crate) struct ResolvedProvider<'a> {
    pub pdef: Option<&'a crate::llm_config::ProviderDef>,
    pub is_anthropic_style: bool,
    pub base_url: String,
    pub endpoint: &'a str,
}

impl<'a> ResolvedProvider<'a> {
    pub fn resolve(provider: &str) -> ResolvedProvider<'static> {
        let pdef = crate::llm_config::provider_config(provider);
        let is_anthropic_style = pdef
            .map(|p| p.chat_endpoint.contains("/messages"))
            .unwrap_or(provider == "anthropic");
        let (default_base, default_endpoint) = if is_anthropic_style {
            ("https://api.anthropic.com/v1", "/messages")
        } else {
            ("https://api.openai.com/v1", "/chat/completions")
        };
        let base_url = pdef
            .map(crate::llm_config::resolve_base_url)
            .unwrap_or_else(|| default_base.to_string());
        let endpoint = pdef
            .map(|p| p.chat_endpoint.as_str())
            .unwrap_or(default_endpoint);
        ResolvedProvider {
            pdef,
            is_anthropic_style,
            base_url,
            endpoint,
        }
    }

    pub fn url(&self) -> String {
        format!("{}{}", self.base_url, self.endpoint)
    }

    pub fn apply_headers(
        &self,
        mut req: reqwest::RequestBuilder,
        api_key: &str,
    ) -> reqwest::RequestBuilder {
        req = super::api::apply_auth_headers(req, api_key, self.pdef);
        if let Some(p) = self.pdef {
            for (k, v) in &p.extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
        }
        req
    }
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
    msg.insert("role".to_string(), VmValue::String(Rc::from(role)));
    msg.insert("content".to_string(), VmValue::String(Rc::from(content)));

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

// =============================================================================
// Unified option extraction
// =============================================================================

/// Extract all LLM call options from the standard (prompt, system, options) args.
pub(crate) fn extract_llm_options(
    args: &[VmValue],
) -> Result<super::api::LlmCallOptions, VmError> {
    use super::api::{LlmCallOptions, ThinkingConfig};
    use super::tools::vm_tools_to_native;

    let prompt = args.first().map(|a| a.display()).unwrap_or_default();
    let system = args.get(1).and_then(|a| {
        if matches!(a, VmValue::Nil) {
            None
        } else {
            Some(a.display())
        }
    });
    let options = args.get(2).and_then(|a| a.as_dict()).cloned();

    let provider = vm_resolve_provider(&options);
    let model = vm_resolve_model(&options, &provider);
    let api_key = vm_resolve_api_key(&provider)?;

    let max_tokens = opt_int(&options, "max_tokens").unwrap_or(4096);
    let temperature = opt_float(&options, "temperature");
    let top_p = opt_float(&options, "top_p");
    let top_k = opt_int(&options, "top_k");
    let stop = opt_str_list(&options, "stop");
    let seed = opt_int(&options, "seed");
    let frequency_penalty = opt_float(&options, "frequency_penalty");
    let presence_penalty = opt_float(&options, "presence_penalty");
    let response_format = opt_str(&options, "response_format");
    let timeout = opt_int(&options, "timeout").map(|t| t as u64);
    let cache = opt_bool(&options, "cache");

    // Thinking: bool or {budget_tokens: N}
    let thinking = options
        .as_ref()
        .and_then(|o| o.get("thinking"))
        .and_then(|v| match v {
            VmValue::Bool(true) => Some(ThinkingConfig::Enabled),
            VmValue::Dict(d) => {
                let budget = d.get("budget_tokens").and_then(|b| b.as_int()).unwrap_or(10000);
                Some(ThinkingConfig::WithBudget(budget))
            }
            _ if v.is_truthy() => Some(ThinkingConfig::Enabled),
            _ => None,
        });

    // JSON schema: convert VmValue dict to serde_json::Value at extraction time
    let json_schema = options
        .as_ref()
        .and_then(|o| o.get("schema"))
        .and_then(|v| v.as_dict())
        .map(vm_value_dict_to_json);

    // Messages: either from options.messages or from prompt
    let messages_val = options.as_ref().and_then(|o| o.get("messages")).cloned();
    let messages = if let Some(VmValue::List(msg_list)) = &messages_val {
        vm_messages_to_json(msg_list)?
    } else {
        vec![serde_json::json!({"role": "user", "content": prompt})]
    };

    // Tools
    let tools_val = options.as_ref().and_then(|o| o.get("tools")).cloned();
    let native_tools = if let Some(tools) = &tools_val {
        Some(vm_tools_to_native(tools, &provider)?)
    } else {
        None
    };

    // Tool choice
    let tool_choice = options
        .as_ref()
        .and_then(|o| o.get("tool_choice"))
        .map(vm_value_to_json);

    // Provider-specific overrides
    let provider_overrides = options
        .as_ref()
        .and_then(|o| o.get(&provider))
        .and_then(|v| v.as_dict())
        .map(vm_value_dict_to_json);

    // Validate options against provider capabilities
    let opts = LlmCallOptions {
        provider,
        model,
        api_key,
        messages,
        system,
        max_tokens,
        temperature,
        top_p,
        top_k,
        stop,
        seed,
        frequency_penalty,
        presence_penalty,
        response_format,
        json_schema,
        thinking,
        native_tools,
        tool_choice,
        cache,
        timeout,
        provider_overrides,
    };

    validate_options(&opts);
    Ok(opts)
}

fn opt_str_list(options: &Option<BTreeMap<String, VmValue>>, key: &str) -> Option<Vec<String>> {
    let val = options.as_ref()?.get(key)?;
    match val {
        VmValue::List(list) => {
            let strs: Vec<String> = list.iter().map(|v| v.display()).collect();
            if strs.is_empty() {
                None
            } else {
                Some(strs)
            }
        }
        _ => None,
    }
}

/// Emit warnings for options not supported by the target provider.
fn validate_options(opts: &super::api::LlmCallOptions) {
    let p = opts.provider.as_str();
    let warn = |param: &str| {
        eprintln!("[harn] warning: \"{param}\" is not supported by provider \"{p}\", ignoring");
    };

    match p {
        "anthropic" => {
            if opts.seed.is_some() { warn("seed"); }
            if opts.frequency_penalty.is_some() { warn("frequency_penalty"); }
            if opts.presence_penalty.is_some() { warn("presence_penalty"); }
        }
        "openai" | "openrouter" | "huggingface" => {
            if opts.top_k.is_some() { warn("top_k"); }
            if opts.thinking.is_some() { warn("thinking"); }
            if opts.cache { warn("cache"); }
        }
        "ollama" => {
            if opts.frequency_penalty.is_some() { warn("frequency_penalty"); }
            if opts.presence_penalty.is_some() { warn("presence_penalty"); }
            if opts.cache { warn("cache"); }
        }
        _ => {} // Unknown provider: skip validation
    }
}

// =============================================================================
// Convert VmValue dict to serde_json::Value for API payloads
// =============================================================================

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
