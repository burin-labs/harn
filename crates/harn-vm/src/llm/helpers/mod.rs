mod options;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use crate::events::{emit_log, EventLevel};
use crate::value::{VmError, VmValue};

pub(crate) use options::{
    expects_structured_output, extract_json, extract_llm_options, opt_str_list,
};

/// Cache of provider name → whether a usable API key is available.
/// Cached for process lifetime since env vars don't change mid-run.
static PROVIDER_KEY_CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
static MODEL_TIER_WARNING_CACHE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Check whether `provider` has a usable API key (or needs none). Cached.
pub(crate) fn provider_key_available(provider: &str) -> bool {
    let cache = PROVIDER_KEY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache.lock().unwrap();
    if let Some(&available) = map.get(provider) {
        return available;
    }
    let available = resolve_api_key(provider).is_ok();
    map.insert(provider.to_string(), available);
    available
}

/// Clear the provider key cache (for tests that manipulate env vars).
#[cfg(test)]
pub(crate) fn reset_provider_key_cache() {
    if let Some(cache) = PROVIDER_KEY_CACHE.get() {
        cache.lock().unwrap().clear();
    }
}

pub(super) const TRANSCRIPT_TYPE: &str = "transcript";
const TRANSCRIPT_ASSET_TYPE: &str = "transcript_asset";
const TRANSCRIPT_VERSION: i64 = 2;

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

fn push_unique(items: &mut Vec<String>, value: impl Into<String>) {
    let value = value.into();
    if !value.is_empty() && !items.iter().any(|existing| existing == &value) {
        items.push(value);
    }
}

fn warn_model_tier_fallback(target: &str, requested_provider: Option<&str>, chosen: (&str, &str)) {
    let key = format!(
        "{target}|{}|{}|{}",
        requested_provider.unwrap_or(""),
        chosen.0,
        chosen.1
    );
    let cache = MODEL_TIER_WARNING_CACHE.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = cache.lock().unwrap();
    if !guard.insert(key) {
        return;
    }
    drop(guard);

    emit_log(
        EventLevel::Warn,
        "llm",
        &format!(
            "model_tier '{target}' could not use provider '{}' in the current environment; \
             falling back to reachable provider '{}' with model '{}'",
            requested_provider.unwrap_or("the default tier mapping"),
            chosen.1,
            chosen.0
        ),
        BTreeMap::new(),
    );
}

fn env_selected_model_for_tier() -> Option<(String, String)> {
    use crate::llm_config;

    let selected_model = std::env::var("HARN_LLM_MODEL")
        .ok()
        .or_else(|| std::env::var("LOCAL_LLM_MODEL").ok())?;

    let selected_provider = std::env::var("HARN_LLM_PROVIDER")
        .ok()
        .filter(|provider| !provider.is_empty())
        .or_else(|| {
            if std::env::var("LOCAL_LLM_BASE_URL").is_ok() {
                Some("local".to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| llm_config::infer_provider(&selected_model));

    if provider_key_available(&selected_provider) {
        Some((selected_model, selected_provider))
    } else {
        None
    }
}

fn preferred_provider_order(preferred_provider: Option<&str>) -> Vec<String> {
    use crate::llm_config;

    let mut providers = Vec::new();
    if let Some(provider) = preferred_provider {
        push_unique(&mut providers, provider.to_string());
    }
    if let Ok(provider) = std::env::var("HARN_LLM_PROVIDER") {
        push_unique(&mut providers, provider);
    }
    if std::env::var("LOCAL_LLM_BASE_URL").is_ok() {
        push_unique(&mut providers, "local");
    }
    if let Ok(model) = std::env::var("HARN_LLM_MODEL") {
        push_unique(&mut providers, llm_config::infer_provider(&model));
    }
    if let Ok(model) = std::env::var("LOCAL_LLM_MODEL") {
        push_unique(&mut providers, llm_config::infer_provider(&model));
    }
    for provider in [
        "local",
        "ollama",
        "openrouter",
        "together",
        "huggingface",
        "openai",
        "anthropic",
    ] {
        push_unique(&mut providers, provider);
    }
    providers
}

fn resolve_available_tier_model(
    target: &str,
    preferred_provider: Option<&str>,
) -> Option<(String, String)> {
    use crate::llm_config;

    let requested = llm_config::resolve_tier_model(target, preferred_provider);
    if let Some((model, provider)) = requested.as_ref() {
        if preferred_provider == Some(provider.as_str()) && provider_key_available(provider) {
            return Some((model.clone(), provider.clone()));
        }
    }

    if let Some((model, provider)) = env_selected_model_for_tier() {
        if requested
            .as_ref()
            .map(|(_, requested_provider)| requested_provider != &provider)
            .unwrap_or(true)
        {
            warn_model_tier_fallback(
                target,
                requested.as_ref().map(|(_, provider)| provider.as_str()),
                (&model, &provider),
            );
        }
        return Some((model, provider));
    }

    let candidates = llm_config::tier_candidates(target);
    for provider in preferred_provider_order(preferred_provider) {
        if !provider_key_available(&provider) {
            continue;
        }
        if let Some((model, candidate_provider)) = candidates
            .iter()
            .find(|(_, candidate_provider)| candidate_provider == &provider)
        {
            if requested
                .as_ref()
                .map(|(_, requested_provider)| requested_provider != candidate_provider)
                .unwrap_or(true)
            {
                warn_model_tier_fallback(
                    target,
                    requested.as_ref().map(|(_, provider)| provider.as_str()),
                    (model, candidate_provider),
                );
            }
            return Some((model.clone(), candidate_provider.clone()));
        }
    }

    if let Some((model, provider)) = requested.as_ref() {
        if provider_key_available(provider) {
            return Some((model.clone(), provider.clone()));
        }
    }

    requested
}

pub(crate) fn vm_resolve_provider(options: &Option<BTreeMap<String, VmValue>>) -> String {
    use crate::llm_config;
    // Explicit option wins, except "auto" which means "run the normal
    // inference chain". Treating "auto" as a literal provider name would
    // make resolve_api_key default to anthropic and fail whenever
    // ANTHROPIC_API_KEY is absent, breaking any sub-call that couldn't
    // inspect the env itself.
    if let Some(p) = options
        .as_ref()
        .and_then(|o| o.get("provider"))
        .map(|v| v.display())
    {
        if !p.eq_ignore_ascii_case("auto") {
            return p;
        }
    }
    if let Ok(p) = std::env::var("HARN_LLM_PROVIDER") {
        return p;
    }
    // First-class local OpenAI-compatible server support.
    if std::env::var("LOCAL_LLM_BASE_URL").is_ok()
        && (options.as_ref().and_then(|o| o.get("model")).is_some()
            || std::env::var("HARN_LLM_MODEL").is_ok()
            || std::env::var("LOCAL_LLM_MODEL").is_ok())
    {
        return "local".to_string();
    }
    if let Some(m) = options
        .as_ref()
        .and_then(|o| o.get("model"))
        .map(|v| v.display())
    {
        return llm_config::infer_provider(&m);
    }
    if let Some(tier) = options
        .as_ref()
        .and_then(|o| o.get("model_tier"))
        .map(|v| v.display())
    {
        if let Some((_, provider)) = resolve_available_tier_model(&tier, None) {
            return provider;
        }
    }
    if let Ok(m) = std::env::var("HARN_LLM_MODEL") {
        return llm_config::infer_provider(&m);
    }
    // Default to anthropic, but fall back to keyless providers when its
    // key is missing — avoids noisy errors when a sub-pipeline (e.g.
    // enrichment) didn't inherit the provider env.
    let default = "anthropic";
    if provider_key_available(default) {
        return default.to_string();
    }
    for fallback in ["ollama", "local"] {
        if provider_key_available(fallback) {
            return fallback.to_string();
        }
    }
    // Let resolve_api_key surface its descriptive error.
    default.to_string()
}

pub(crate) fn vm_resolve_model(
    options: &Option<BTreeMap<String, VmValue>>,
    provider: &str,
) -> String {
    use crate::llm_config;
    if let Some(raw) = options
        .as_ref()
        .and_then(|o| o.get("model"))
        .map(|v| v.display())
    {
        let (resolved, _) = llm_config::resolve_model(&raw);
        return resolved;
    }
    if let Some(tier) = options
        .as_ref()
        .and_then(|o| o.get("model_tier"))
        .map(|v| v.display())
    {
        if let Some((resolved, _)) = resolve_available_tier_model(&tier, Some(provider)) {
            return resolved;
        }
    }
    if let Ok(raw) = std::env::var("HARN_LLM_MODEL") {
        let (resolved, resolved_provider) = llm_config::resolve_model(&raw);
        let env_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        if resolved_provider.as_deref() == Some(provider)
            || (resolved_provider.is_none() && env_provider.as_deref() == Some(provider))
        {
            return resolved;
        }
    }
    if provider == "local" {
        if let Ok(raw) = std::env::var("LOCAL_LLM_MODEL") {
            let (resolved, _) = llm_config::resolve_model(&raw);
            return resolved;
        }
    }
    match provider {
        "local" => std::env::var("LOCAL_LLM_MODEL")
            .or_else(|_| std::env::var("HARN_LLM_MODEL"))
            .unwrap_or_else(|_| "gpt-4o".to_string()),
        "openai" => "gpt-4o".to_string(),
        "ollama" => "llama3.2".to_string(),
        "openrouter" => "anthropic/claude-sonnet-4-20250514".to_string(),
        _ => "claude-sonnet-4-20250514".to_string(),
    }
}

pub fn resolve_api_key(provider: &str) -> Result<String, VmError> {
    use crate::llm_config;
    if provider == "mock" || crate::llm::mock::cli_llm_mock_replay_active() {
        return Ok(String::new());
    }

    // Explain provenance (env vs llm.toml vs default) and how to opt into mock.
    let selection_hint = {
        let config_path = llm_config::loaded_config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<built-in defaults>".to_string());
        format!(
            " (provider '{provider}' selected via LLM_PROVIDER / llm.toml @ {config_path}; \
             set HARN_LLM_PROVIDER=mock or LLM_PROVIDER=mock for offline use)"
        )
    };

    if let Some(pdef) = llm_config::provider_config(provider) {
        if pdef.auth_style == "none" {
            return Ok(String::new());
        }
        match &pdef.auth_env {
            llm_config::AuthEnv::Single(env) => {
                return std::env::var(env).map_err(|_| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "Missing API key: set {env} environment variable{selection_hint}"
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
                    "Missing API key: set one of {} environment variables{selection_hint}",
                    envs.join(", ")
                )))));
            }
            llm_config::AuthEnv::None => return Ok(String::new()),
        }
    }
    std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "Missing API key: set ANTHROPIC_API_KEY environment variable{selection_hint}"
        ))))
    })
}

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

pub(crate) fn vm_messages_to_json(msg_list: &[VmValue]) -> Result<Vec<serde_json::Value>, VmError> {
    let mut messages = Vec::new();
    for msg in msg_list {
        if let VmValue::Dict(d) = msg {
            let role = d
                .get("role")
                .map(|v| v.display())
                .unwrap_or_else(|| "user".to_string());
            let content = d
                .get("content")
                .cloned()
                .unwrap_or_else(|| VmValue::String(Rc::from("")));
            let content_json = match &content {
                VmValue::String(text) => serde_json::Value::String(text.to_string()),
                other => vm_value_to_json(other),
            };

            if role == "tool_result" {
                // Anthropic tool result format
                let tool_use_id = d
                    .get("tool_use_id")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let rendered = match &content_json {
                    serde_json::Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": rendered,
                    }],
                }));
            } else {
                // Preserve full message dict so OpenAI-style fields
                // (tool_call_id, tool_calls, reasoning, provider metadata)
                // survive transcript/message round-trips.
                let mut message = vm_value_dict_to_json(d);
                if !message
                    .get("content")
                    .map(|value| !value.is_null())
                    .unwrap_or(false)
                {
                    message["content"] = content_json;
                }
                if message
                    .get("role")
                    .and_then(|value| value.as_str())
                    .is_none()
                {
                    message["role"] = serde_json::json!(role);
                }
                messages.push(message);
            }
        }
    }
    Ok(messages)
}

pub(crate) fn transcript_message_list(
    transcript: &BTreeMap<String, VmValue>,
) -> Result<Vec<VmValue>, VmError> {
    match transcript.get("messages") {
        Some(VmValue::List(list)) => Ok((**list).clone()),
        Some(_) => Err(VmError::Thrown(VmValue::String(Rc::from(
            "transcript.messages must be a list",
        )))),
        None => Ok(Vec::new()),
    }
}

pub(crate) fn transcript_asset_list(
    transcript: &BTreeMap<String, VmValue>,
) -> Result<Vec<VmValue>, VmError> {
    match transcript.get("assets") {
        Some(VmValue::List(list)) => Ok((**list).clone()),
        Some(_) => Err(VmError::Thrown(VmValue::String(Rc::from(
            "transcript.assets must be a list",
        )))),
        None => Ok(Vec::new()),
    }
}

fn transcript_string_field(transcript: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    transcript.get(key).and_then(|v| match v {
        VmValue::String(s) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    })
}

pub(crate) fn transcript_summary_text(transcript: &BTreeMap<String, VmValue>) -> Option<String> {
    transcript_string_field(transcript, "summary")
}

pub(crate) fn transcript_id(transcript: &BTreeMap<String, VmValue>) -> Option<String> {
    transcript_string_field(transcript, "id")
}

pub(crate) fn vm_message(role: &str, content: &str) -> VmValue {
    vm_message_value(role, VmValue::String(Rc::from(content)))
}

pub(crate) fn vm_message_value(role: &str, content: VmValue) -> VmValue {
    let mut msg = BTreeMap::new();
    msg.insert("role".to_string(), VmValue::String(Rc::from(role)));
    msg.insert("content".to_string(), content);
    VmValue::Dict(Rc::new(msg))
}

pub(crate) fn json_messages_to_vm(msg_list: &[serde_json::Value]) -> Vec<VmValue> {
    msg_list
        .iter()
        .filter_map(|msg| {
            let role = msg.get("role").and_then(|v| v.as_str())?;

            // Preserve all fields for tool messages; strict providers
            // (Together AI and others) reject messages missing tool_calls
            // / tool_call_id.
            if role == "tool" || msg.get("tool_calls").is_some() {
                return Some(crate::stdlib::json_to_vm_value(msg));
            }

            if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                return Some(vm_message(role, content));
            }

            if let Some(blocks) = msg.get("content").and_then(|v| v.as_array()) {
                if role == "user" {
                    for block in blocks {
                        if block.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                            let content = block
                                .get("content")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default();
                            let tool_use_id = block
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default();
                            let mut result = BTreeMap::new();
                            result.insert(
                                "role".to_string(),
                                VmValue::String(Rc::from("tool_result")),
                            );
                            result.insert(
                                "tool_use_id".to_string(),
                                VmValue::String(Rc::from(tool_use_id)),
                            );
                            result
                                .insert("content".to_string(), VmValue::String(Rc::from(content)));
                            return Some(VmValue::Dict(Rc::new(result)));
                        }
                    }
                }
                return Some(vm_message_value(
                    role,
                    crate::stdlib::json_to_vm_value(&serde_json::Value::Array(blocks.clone())),
                ));
            }

            msg.get("content")
                .map(|content| vm_message_value(role, crate::stdlib::json_to_vm_value(content)))
        })
        .collect()
}

pub(crate) fn new_transcript_with(
    id: Option<String>,
    messages: Vec<VmValue>,
    summary: Option<String>,
    metadata: Option<VmValue>,
) -> VmValue {
    new_transcript_with_events(
        id,
        messages,
        summary,
        metadata,
        Vec::new(),
        Vec::new(),
        None,
    )
}

pub(crate) fn new_transcript_with_events(
    id: Option<String>,
    messages: Vec<VmValue>,
    summary: Option<String>,
    metadata: Option<VmValue>,
    extra_events: Vec<VmValue>,
    assets: Vec<VmValue>,
    state: Option<&str>,
) -> VmValue {
    let mut transcript = BTreeMap::new();
    let mut events = transcript_events_from_messages(&messages);
    events.extend(extra_events);
    transcript.insert(
        "_type".to_string(),
        VmValue::String(Rc::from(TRANSCRIPT_TYPE)),
    );
    transcript.insert("version".to_string(), VmValue::Int(TRANSCRIPT_VERSION));
    transcript.insert(
        "id".to_string(),
        VmValue::String(Rc::from(
            id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
        )),
    );
    transcript.insert("messages".to_string(), VmValue::List(Rc::new(messages)));
    transcript.insert("events".to_string(), VmValue::List(Rc::new(events)));
    transcript.insert("assets".to_string(), VmValue::List(Rc::new(assets)));
    if let Some(summary) = summary {
        transcript.insert("summary".to_string(), VmValue::String(Rc::from(summary)));
    }
    if let Some(metadata) = metadata {
        transcript.insert("metadata".to_string(), metadata);
    }
    if let Some(state) = state {
        transcript.insert("state".to_string(), VmValue::String(Rc::from(state)));
    }
    VmValue::Dict(Rc::new(transcript))
}

fn transcript_event_from_message(message: &VmValue) -> VmValue {
    let dict = message.as_dict().cloned().unwrap_or_default();
    let role = dict
        .get("role")
        .map(|v| v.display())
        .unwrap_or_else(|| "user".to_string());
    let blocks = normalize_message_blocks(dict.get("content"), &role);
    let text = render_blocks_text(&blocks);
    let visibility = overall_visibility(&blocks, default_visibility_for_role(&role));
    let kind = if role == "tool_result" {
        "tool_result"
    } else {
        "message"
    };
    let mut event = BTreeMap::new();
    event.insert(
        "id".to_string(),
        VmValue::String(Rc::from(uuid::Uuid::now_v7().to_string())),
    );
    event.insert("kind".to_string(), VmValue::String(Rc::from(kind)));
    event.insert("role".to_string(), VmValue::String(Rc::from(role.as_str())));
    event.insert(
        "visibility".to_string(),
        VmValue::String(Rc::from(visibility)),
    );
    event.insert("text".to_string(), VmValue::String(Rc::from(text)));
    event.insert("blocks".to_string(), VmValue::List(Rc::new(blocks)));
    VmValue::Dict(Rc::new(event))
}

pub(crate) fn transcript_events_from_messages(messages: &[VmValue]) -> Vec<VmValue> {
    messages.iter().map(transcript_event_from_message).collect()
}

pub(crate) fn transcript_to_vm_with_events(
    id: Option<String>,
    summary: Option<String>,
    metadata: Option<serde_json::Value>,
    messages: &[serde_json::Value],
    extra_events: Vec<VmValue>,
    assets: Vec<VmValue>,
    state: Option<&str>,
) -> VmValue {
    let metadata_vm = metadata.as_ref().map(crate::stdlib::json_to_vm_value);
    new_transcript_with_events(
        id,
        json_messages_to_vm(messages),
        summary,
        metadata_vm,
        extra_events,
        assets,
        state,
    )
}

pub(crate) fn transcript_event(
    kind: &str,
    role: &str,
    visibility: &str,
    text: &str,
    metadata: Option<serde_json::Value>,
) -> VmValue {
    let mut event = BTreeMap::new();
    event.insert(
        "id".to_string(),
        VmValue::String(Rc::from(uuid::Uuid::now_v7().to_string())),
    );
    event.insert("kind".to_string(), VmValue::String(Rc::from(kind)));
    event.insert("role".to_string(), VmValue::String(Rc::from(role)));
    event.insert(
        "visibility".to_string(),
        VmValue::String(Rc::from(visibility)),
    );
    event.insert("text".to_string(), VmValue::String(Rc::from(text)));
    event.insert(
        "blocks".to_string(),
        VmValue::List(Rc::new(vec![VmValue::Dict(Rc::new(BTreeMap::from([
            ("type".to_string(), VmValue::String(Rc::from("text"))),
            ("text".to_string(), VmValue::String(Rc::from(text))),
            (
                "visibility".to_string(),
                VmValue::String(Rc::from(visibility)),
            ),
        ])))])),
    );
    if let Some(metadata) = metadata {
        event.insert(
            "metadata".to_string(),
            crate::stdlib::json_to_vm_value(&metadata),
        );
    }
    VmValue::Dict(Rc::new(event))
}

pub(crate) fn normalize_transcript_asset(value: &VmValue) -> VmValue {
    let mut asset = value.as_dict().cloned().unwrap_or_default();
    asset.insert(
        "_type".to_string(),
        VmValue::String(Rc::from(TRANSCRIPT_ASSET_TYPE)),
    );
    if !asset.contains_key("id") {
        asset.insert(
            "id".to_string(),
            VmValue::String(Rc::from(uuid::Uuid::now_v7().to_string())),
        );
    }
    if !asset.contains_key("kind") {
        asset.insert("kind".to_string(), VmValue::String(Rc::from("blob")));
    }
    if !asset.contains_key("visibility") {
        asset.insert(
            "visibility".to_string(),
            VmValue::String(Rc::from("internal")),
        );
    }
    if value.as_dict().is_none() {
        asset.insert(
            "storage".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([(
                "path".to_string(),
                VmValue::String(Rc::from(value.display())),
            )]))),
        );
    }
    VmValue::Dict(Rc::new(asset))
}

pub(crate) fn is_transcript_value(value: &VmValue) -> bool {
    value
        .as_dict()
        .and_then(|d| d.get("_type"))
        .map(|v| v.display())
        .as_deref()
        == Some(TRANSCRIPT_TYPE)
}

pub(crate) fn vm_add_role_message(args: &[VmValue], role: &str) -> Result<VmValue, VmError> {
    match args.first() {
        Some(VmValue::List(list)) => {
            let mut new_messages = (**list).clone();
            new_messages.push(vm_message_value(
                role,
                args.get(1)
                    .cloned()
                    .unwrap_or_else(|| VmValue::String(Rc::from(""))),
            ));
            Ok(VmValue::List(Rc::new(new_messages)))
        }
        Some(VmValue::Dict(d))
            if d.get("_type").map(|v| v.display()).as_deref() == Some(TRANSCRIPT_TYPE) =>
        {
            let mut messages = transcript_message_list(d)?;
            messages.push(vm_message_value(
                role,
                args.get(1)
                    .cloned()
                    .unwrap_or_else(|| VmValue::String(Rc::from(""))),
            ));
            Ok(new_transcript_with_events(
                transcript_id(d),
                messages,
                transcript_summary_text(d),
                d.get("metadata").cloned(),
                Vec::new(),
                transcript_asset_list(d)?,
                d.get("state").and_then(|value| match value {
                    VmValue::String(text) if !text.is_empty() => Some(text.as_ref()),
                    _ => None,
                }),
            ))
        }
        _ => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "add_{role}: first argument must be a message list or transcript"
        ))))),
    }
}

fn default_visibility_for_role(role: &str) -> &'static str {
    match role {
        "tool_result" => "internal",
        _ => "public",
    }
}

fn normalize_message_blocks(content: Option<&VmValue>, role: &str) -> Vec<VmValue> {
    let default_visibility = default_visibility_for_role(role);
    match content {
        Some(VmValue::List(items)) => items
            .iter()
            .map(|block| normalize_transcript_block(block, default_visibility))
            .collect(),
        Some(VmValue::Dict(block)) => {
            vec![normalize_transcript_block(
                &VmValue::Dict(block.clone()),
                default_visibility,
            )]
        }
        Some(VmValue::Nil) | None => Vec::new(),
        Some(other) => vec![VmValue::Dict(Rc::new(BTreeMap::from([
            ("type".to_string(), VmValue::String(Rc::from("text"))),
            (
                "text".to_string(),
                VmValue::String(Rc::from(other.display())),
            ),
            (
                "visibility".to_string(),
                VmValue::String(Rc::from(default_visibility)),
            ),
        ])))],
    }
}

fn normalize_transcript_block(block: &VmValue, default_visibility: &str) -> VmValue {
    let mut normalized = block.as_dict().cloned().unwrap_or_else(|| {
        BTreeMap::from([(
            "text".to_string(),
            VmValue::String(Rc::from(block.display())),
        )])
    });
    if !normalized.contains_key("type") {
        normalized.insert("type".to_string(), VmValue::String(Rc::from("text")));
    }
    if !normalized.contains_key("visibility") {
        normalized.insert(
            "visibility".to_string(),
            VmValue::String(Rc::from(default_visibility)),
        );
    }
    VmValue::Dict(Rc::new(normalized))
}

fn overall_visibility(blocks: &[VmValue], default_visibility: &str) -> String {
    let mut resolved = default_visibility.to_string();
    for block in blocks {
        let Some(dict) = block.as_dict() else {
            continue;
        };
        let visibility = dict
            .get("visibility")
            .map(|value| value.display())
            .unwrap_or_else(|| default_visibility.to_string());
        if visibility == "public" {
            return visibility;
        }
        if visibility == "internal" {
            resolved = visibility;
        }
    }
    resolved
}

fn render_blocks_text(blocks: &[VmValue]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        let Some(dict) = block.as_dict() else {
            continue;
        };
        let kind = dict
            .get("type")
            .map(|value| value.display())
            .unwrap_or_else(|| "text".to_string());
        let text = match kind.as_str() {
            "text" | "output_text" | "reasoning" => dict
                .get("text")
                .or_else(|| dict.get("content"))
                .map(|value| value.display())
                .unwrap_or_default(),
            "tool_call" => {
                let name = dict
                    .get("name")
                    .map(|value| value.display())
                    .unwrap_or_else(|| "tool".to_string());
                format!("<tool_call:{name}>")
            }
            "tool_result" => {
                let name = dict
                    .get("name")
                    .map(|value| value.display())
                    .unwrap_or_else(|| "tool".to_string());
                format!("<tool_result:{name}>")
            }
            "image" | "input_image" => render_assetish_label("image", dict),
            "file" | "document" | "attachment" => render_assetish_label(&kind, dict),
            other => format!("<{other}>"),
        };
        if !text.is_empty() {
            parts.push(text);
        }
    }
    parts.join(" ")
}

fn render_assetish_label(kind: &str, dict: &BTreeMap<String, VmValue>) -> String {
    let label = dict
        .get("name")
        .or_else(|| dict.get("title"))
        .or_else(|| dict.get("path"))
        .or_else(|| dict.get("asset_id"))
        .map(|value| value.display())
        .unwrap_or_else(|| kind.to_string());
    format!("<{kind}:{label}>")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    #[test]
    fn local_provider_is_selected_when_local_base_url_and_model_are_set() {
        // Share the crate-wide LLM env lock so this test cannot race with
        // sibling modules (e.g. llm::api streaming classification tests) that
        // also mutate LOCAL_LLM_BASE_URL.
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_base = std::env::var("LOCAL_LLM_BASE_URL").ok();
        let prev_model = std::env::var("LOCAL_LLM_MODEL").ok();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();

        unsafe {
            std::env::set_var("LOCAL_LLM_BASE_URL", "http://127.0.0.1:8000");
            std::env::set_var("LOCAL_LLM_MODEL", "qwen2.5-coder-32b");
            std::env::remove_var("HARN_LLM_PROVIDER");
            std::env::remove_var("HARN_LLM_MODEL");
        }
        reset_provider_key_cache();

        assert_eq!(vm_resolve_provider(&None), "local");
        assert_eq!(vm_resolve_model(&None, "local"), "qwen2.5-coder-32b");
        assert!(resolve_api_key("local").is_ok());

        unsafe {
            match prev_base {
                Some(value) => std::env::set_var("LOCAL_LLM_BASE_URL", value),
                None => std::env::remove_var("LOCAL_LLM_BASE_URL"),
            }
            match prev_model {
                Some(value) => std::env::set_var("LOCAL_LLM_MODEL", value),
                None => std::env::remove_var("LOCAL_LLM_MODEL"),
            }
            match prev_harn_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
            match prev_harn_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
        }
        reset_provider_key_cache();
    }

    #[test]
    fn vm_messages_to_json_preserves_tool_message_fields() {
        let message = VmValue::Dict(Rc::new(BTreeMap::from([
            ("role".to_string(), VmValue::String(Rc::from("tool"))),
            (
                "tool_call_id".to_string(),
                VmValue::String(Rc::from("call_123")),
            ),
            ("content".to_string(), VmValue::String(Rc::from("ok"))),
        ])));

        let json = vm_messages_to_json(&[message]).expect("message json");
        assert_eq!(json[0]["role"], "tool");
        assert_eq!(json[0]["tool_call_id"], "call_123");
        assert_eq!(json[0]["content"], "ok");
    }

    #[test]
    fn extract_llm_options_rejects_removed_transcript_key() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        unsafe {
            std::env::set_var("HARN_LLM_PROVIDER", "mock");
            std::env::remove_var("HARN_LLM_MODEL");
        }

        let transcript = new_transcript_with(None, Vec::new(), None, None);
        let options = VmValue::Dict(Rc::new(BTreeMap::from([(
            "transcript".to_string(),
            transcript,
        )])));
        let err = extract_llm_options(&[VmValue::String(Rc::from("")), VmValue::Nil, options])
            .err()
            .expect("transcript option is rejected");
        let msg = match err {
            crate::value::VmError::Thrown(VmValue::String(s)) => s.to_string(),
            other => panic!("unexpected error: {other:?}"),
        };
        assert!(
            msg.contains("transcript") && msg.contains("session_id"),
            "got: {msg}"
        );

        unsafe {
            match prev_harn_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
            match prev_harn_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
        }
    }

    #[test]
    fn model_tier_prefers_reachable_env_provider_and_model() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_local_model = std::env::var("LOCAL_LLM_MODEL").ok();
        let prev_local_base = std::env::var("LOCAL_LLM_BASE_URL").ok();

        unsafe {
            std::env::set_var("HARN_LLM_MODEL", "gemma-4-e4b-it");
            std::env::set_var("HARN_LLM_PROVIDER", "local");
            std::env::set_var("LOCAL_LLM_MODEL", "gemma-4-e4b-it");
            std::env::set_var("LOCAL_LLM_BASE_URL", "http://127.0.0.1:8000");
        }
        reset_provider_key_cache();

        let options = Some(BTreeMap::from([(
            "model_tier".to_string(),
            VmValue::String(Rc::from("small")),
        )]));
        let provider = vm_resolve_provider(&options);
        let resolved = vm_resolve_model(&options, &provider);

        unsafe {
            match prev_harn_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
            match prev_harn_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
            match prev_local_model {
                Some(value) => std::env::set_var("LOCAL_LLM_MODEL", value),
                None => std::env::remove_var("LOCAL_LLM_MODEL"),
            }
            match prev_local_base {
                Some(value) => std::env::set_var("LOCAL_LLM_BASE_URL", value),
                None => std::env::remove_var("LOCAL_LLM_BASE_URL"),
            }
        }
        assert_eq!(provider, "local");
        assert_eq!(resolved, "gemma-4-e4b-it");
    }

    #[test]
    fn model_tier_falls_back_to_reachable_local_provider_when_default_alias_is_unavailable() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_local_model = std::env::var("LOCAL_LLM_MODEL").ok();
        let prev_local_base = std::env::var("LOCAL_LLM_BASE_URL").ok();

        unsafe {
            std::env::remove_var("HARN_LLM_MODEL");
            std::env::remove_var("HARN_LLM_PROVIDER");
            std::env::set_var("LOCAL_LLM_MODEL", "gemma-4-e4b-it");
            std::env::set_var("LOCAL_LLM_BASE_URL", "http://127.0.0.1:8000");
        }
        reset_provider_key_cache();

        let options = Some(BTreeMap::from([(
            "model_tier".to_string(),
            VmValue::String(Rc::from("small")),
        )]));
        let provider = vm_resolve_provider(&options);
        let resolved = vm_resolve_model(&options, &provider);

        unsafe {
            match prev_harn_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
            match prev_harn_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
            match prev_local_model {
                Some(value) => std::env::set_var("LOCAL_LLM_MODEL", value),
                None => std::env::remove_var("LOCAL_LLM_MODEL"),
            }
            match prev_local_base {
                Some(value) => std::env::set_var("LOCAL_LLM_BASE_URL", value),
                None => std::env::remove_var("LOCAL_LLM_BASE_URL"),
            }
        }

        assert_eq!(provider, "local");
        assert_eq!(resolved, "gemma-4-e4b-it");
    }

    #[test]
    fn raw_env_model_is_accepted_when_env_provider_matches() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();

        unsafe {
            std::env::set_var("HARN_LLM_MODEL", "google/gemma-4-31B-it");
            std::env::set_var("HARN_LLM_PROVIDER", "together");
        }

        let resolved = vm_resolve_model(&None, "together");

        unsafe {
            match prev_harn_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
            match prev_harn_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
        }

        assert_eq!(resolved, "google/gemma-4-31B-it");
    }

    #[test]
    fn provider_auto_with_local_prefix_model_routes_to_local() {
        // `provider: "auto"` must fall through to inference. With a `local:`
        // model prefix that inference should resolve to the local provider
        // rather than anthropic/ollama.
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_harn_model = std::env::var("HARN_LLM_MODEL").ok();
        let prev_base = std::env::var("LOCAL_LLM_BASE_URL").ok();
        unsafe {
            std::env::remove_var("HARN_LLM_PROVIDER");
            std::env::remove_var("HARN_LLM_MODEL");
            std::env::remove_var("LOCAL_LLM_BASE_URL");
        }
        reset_provider_key_cache();

        let mut opts: BTreeMap<String, VmValue> = BTreeMap::new();
        opts.insert("provider".to_string(), VmValue::String(Rc::from("auto")));
        opts.insert(
            "model".to_string(),
            VmValue::String(Rc::from("local:gemma-4-e4b-it")),
        );
        assert_eq!(vm_resolve_provider(&Some(opts)), "local");

        // Case-insensitive: "AUTO" should behave the same.
        let mut opts2: BTreeMap<String, VmValue> = BTreeMap::new();
        opts2.insert("provider".to_string(), VmValue::String(Rc::from("AUTO")));
        opts2.insert(
            "model".to_string(),
            VmValue::String(Rc::from("local:foo/bar")),
        );
        assert_eq!(vm_resolve_provider(&Some(opts2)), "local");

        // Explicit non-auto provider still wins.
        let mut opts3: BTreeMap<String, VmValue> = BTreeMap::new();
        opts3.insert(
            "provider".to_string(),
            VmValue::String(Rc::from("anthropic")),
        );
        opts3.insert("model".to_string(), VmValue::String(Rc::from("local:foo")));
        assert_eq!(vm_resolve_provider(&Some(opts3)), "anthropic");

        unsafe {
            match prev_harn_provider {
                Some(v) => std::env::set_var("HARN_LLM_PROVIDER", v),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
            match prev_harn_model {
                Some(v) => std::env::set_var("HARN_LLM_MODEL", v),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
            match prev_base {
                Some(v) => std::env::set_var("LOCAL_LLM_BASE_URL", v),
                None => std::env::remove_var("LOCAL_LLM_BASE_URL"),
            }
        }
        reset_provider_key_cache();
    }
}
