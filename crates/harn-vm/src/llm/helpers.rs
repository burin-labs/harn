use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use crate::value::{VmError, VmValue};

// =============================================================================
// Provider API-key availability cache
// =============================================================================

/// Cache of provider name → whether a usable API key is available.
/// Populated lazily on first check per provider and reused for the process
/// lifetime (env vars don't change mid-run).
static PROVIDER_KEY_CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();

/// Check whether `provider` has a usable API key (or needs none).
/// Results are cached so repeated resolution attempts for the same provider
/// don't redundantly probe environment variables.
pub(crate) fn provider_key_available(provider: &str) -> bool {
    let cache = PROVIDER_KEY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache.lock().unwrap();
    if let Some(&available) = map.get(provider) {
        return available;
    }
    // Probe: try resolving the key — if it succeeds the provider is usable.
    let available = vm_resolve_api_key(provider).is_ok();
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

const TRANSCRIPT_TYPE: &str = "transcript";
const TRANSCRIPT_ASSET_TYPE: &str = "transcript_asset";
const TRANSCRIPT_VERSION: i64 = 2;

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
    // First-class local OpenAI-compatible server support.
    if std::env::var("LOCAL_LLM_BASE_URL").is_ok()
        && (options.as_ref().and_then(|o| o.get("model")).is_some()
            || std::env::var("HARN_LLM_MODEL").is_ok()
            || std::env::var("LOCAL_LLM_MODEL").is_ok())
    {
        return "local".to_string();
    }
    // Try to infer from model
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
        if let Some((model, provider)) = llm_config::resolve_tier_model(&tier, None) {
            let _ = model;
            if provider_key_available(&provider) {
                return provider;
            }
            // Tier provider has no key — fall through to defaults.
        }
    }
    if let Ok(m) = std::env::var("HARN_LLM_MODEL") {
        return llm_config::infer_provider(&m);
    }
    // Default to anthropic — but if the key is missing, try providers that
    // don't need one (ollama, local) before giving up.  This avoids noisy
    // "Missing API key" errors when the user is running with a local model
    // and a sub-pipeline (e.g. enrichment) doesn't inherit the provider env.
    let default = "anthropic";
    if provider_key_available(default) {
        return default.to_string();
    }
    for fallback in ["ollama", "local"] {
        if provider_key_available(fallback) {
            return fallback.to_string();
        }
    }
    // No provider has a key — return the default so the caller gets the
    // usual descriptive error from vm_resolve_api_key.
    default.to_string()
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
        .or_else(|| std::env::var("HARN_LLM_MODEL").ok())
        .or_else(|| {
            if provider == "local" {
                std::env::var("LOCAL_LLM_MODEL").ok()
            } else {
                None
            }
        });

    if let Some(raw) = raw {
        let (resolved, _) = llm_config::resolve_model(&raw);
        return resolved;
    }
    if let Some(tier) = options
        .as_ref()
        .and_then(|o| o.get("model_tier"))
        .map(|v| v.display())
    {
        if let Some((resolved, _)) = llm_config::resolve_tier_model(&tier, Some(provider)) {
            return resolved;
        }
    }
    // Default model per provider
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

pub(crate) fn vm_resolve_api_key(provider: &str) -> Result<String, VmError> {
    use crate::llm_config;
    if provider == "mock" {
        return Ok(String::new());
    }

    // Build a short "why this provider?" explanation to append to error
    // messages so the user knows where the selection came from (env vars,
    // llm.toml, or the default fallback) and how to switch to the mock
    // provider for offline experimentation.
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
    // Fallback for unknown providers
    std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "Missing API key: set ANTHROPIC_API_KEY environment variable{selection_hint}"
        ))))
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
                messages.push(serde_json::json!({
                    "role": role,
                    "content": content_json,
                }));
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

pub(crate) fn transcript_metadata(
    transcript: &BTreeMap<String, VmValue>,
) -> Option<serde_json::Value> {
    transcript
        .get("metadata")
        .and_then(|v| v.as_dict())
        .map(vm_value_dict_to_json)
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

pub(crate) fn vm_message_list_to_json(
    msg_list: &[VmValue],
) -> Result<Vec<serde_json::Value>, VmError> {
    vm_messages_to_json(msg_list)
}

pub(crate) fn json_messages_to_vm(msg_list: &[serde_json::Value]) -> Vec<VmValue> {
    msg_list
        .iter()
        .filter_map(|msg| {
            let role = msg.get("role").and_then(|v| v.as_str())?;
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

// =============================================================================
// Helper: add a role message to a conversation list
// =============================================================================

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
pub(crate) fn extract_llm_options(args: &[VmValue]) -> Result<super::api::LlmCallOptions, VmError> {
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

    // Default output ceiling. A value of 0 means "omit from request" (let
    // provider decide). 8192 prevents degenerate repetition loops while
    // leaving headroom on providers that allocate output tokens to internal
    // reasoning (e.g. DeepInfra's 16K limit for Qwen models).
    // Apply model_defaults from providers.toml as fallbacks for parameters
    // the caller didn't specify. This ensures recommended defaults (e.g.
    // presence_penalty=1.5 for Qwen) are applied automatically.
    let model_defaults = crate::llm_config::model_params(&model);
    let default_float =
        |key: &str| -> Option<f64> { model_defaults.get(key).and_then(|v| v.as_float()) };
    let default_int =
        |key: &str| -> Option<i64> { model_defaults.get(key).and_then(|v| v.as_integer()) };

    let max_tokens = opt_int(&options, "max_tokens").unwrap_or(16384);
    let temperature = opt_float(&options, "temperature").or_else(|| default_float("temperature"));
    let top_p = opt_float(&options, "top_p").or_else(|| default_float("top_p"));
    let top_k = opt_int(&options, "top_k").or_else(|| default_int("top_k"));
    let stop = opt_str_list(&options, "stop");
    let seed = opt_int(&options, "seed");
    let frequency_penalty =
        opt_float(&options, "frequency_penalty").or_else(|| default_float("frequency_penalty"));
    let presence_penalty =
        opt_float(&options, "presence_penalty").or_else(|| default_float("presence_penalty"));
    let response_format = opt_str(&options, "response_format");
    let timeout = opt_int(&options, "timeout").map(|t| t as u64);
    let idle_timeout = opt_int(&options, "idle_timeout").map(|t| t as u64);
    let cache = opt_bool(&options, "cache");
    let stream = options
        .as_ref()
        .and_then(|o| o.get("stream"))
        .map(|v| v.is_truthy())
        .unwrap_or_else(|| {
            std::env::var("HARN_LLM_STREAM")
                .map(|v| v != "0" && v.to_lowercase() != "false")
                .unwrap_or(true)
        });
    let output_validation = opt_str(&options, "output_validation");

    // Thinking: bool or {budget_tokens: N}
    let thinking = options
        .as_ref()
        .and_then(|o| o.get("thinking"))
        .and_then(|v| match v {
            VmValue::Bool(true) => Some(ThinkingConfig::Enabled),
            VmValue::Dict(d) => {
                let budget = d
                    .get("budget_tokens")
                    .and_then(|b| b.as_int())
                    .unwrap_or(10000);
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
    let output_schema = options
        .as_ref()
        .and_then(|o| o.get("output_schema").or_else(|| o.get("schema")))
        .and_then(|v| v.as_dict())
        .map(vm_value_dict_to_json);

    let transcript_val = options.as_ref().and_then(|o| o.get("transcript")).cloned();
    let transcript_dict = transcript_val
        .as_ref()
        .and_then(|v| v.as_dict())
        .filter(|d| d.get("_type").map(|v| v.display()).as_deref() == Some(TRANSCRIPT_TYPE));
    let transcript_id = transcript_dict.and_then(transcript_id);
    let transcript_summary = transcript_dict.and_then(transcript_summary_text);
    let transcript_metadata = transcript_dict.and_then(transcript_metadata);

    // Messages: options.messages > options.transcript > prompt
    let messages_val = options.as_ref().and_then(|o| o.get("messages")).cloned();
    let messages = if let Some(VmValue::List(msg_list)) = &messages_val {
        vm_messages_to_json(msg_list)?
    } else if let Some(transcript) = transcript_dict {
        let mut messages = vm_message_list_to_json(&transcript_message_list(transcript)?)?;
        if !prompt.is_empty() {
            messages.push(serde_json::json!({
                "role": "user",
                "content": prompt,
            }));
        }
        messages
    } else {
        vec![serde_json::json!({"role": "user", "content": prompt})]
    };

    let system = match (system, transcript_summary.clone()) {
        (Some(system), Some(summary)) => {
            Some(format!("{system}\n\nConversation summary:\n{summary}"))
        }
        (Some(system), None) => Some(system),
        (None, Some(summary)) => Some(format!("Conversation summary:\n{summary}")),
        (None, None) => None,
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
        transcript_id,
        transcript_summary,
        transcript_metadata,
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
        output_schema,
        output_validation,
        thinking,
        tools: tools_val,
        native_tools,
        tool_choice,
        cache,
        timeout,
        idle_timeout,
        stream,
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
        crate::events::log_warn(
            "llm",
            &format!("\"{param}\" is not supported by provider \"{p}\", ignoring"),
        );
    };

    match p {
        "anthropic" => {
            if opts.seed.is_some() {
                warn("seed");
            }
            if opts.frequency_penalty.is_some() {
                warn("frequency_penalty");
            }
            if opts.presence_penalty.is_some() {
                warn("presence_penalty");
            }
        }
        "openai" | "openrouter" | "huggingface" | "local" => {
            if opts.top_k.is_some() {
                warn("top_k");
            }
            if opts.thinking.is_some() {
                warn("thinking");
            }
            if opts.cache {
                warn("cache");
            }
        }
        "ollama" => {
            if opts.frequency_penalty.is_some() {
                warn("frequency_penalty");
            }
            if opts.presence_penalty.is_some() {
                warn("presence_penalty");
            }
            if opts.cache {
                warn("cache");
            }
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(vm_resolve_api_key("local").is_ok());

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
}
