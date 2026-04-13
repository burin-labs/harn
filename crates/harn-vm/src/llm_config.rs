use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;

static CONFIG: OnceLock<ProvidersConfig> = OnceLock::new();
static CONFIG_PATH: OnceLock<String> = OnceLock::new();

// =============================================================================
// Config structs
// =============================================================================

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProvidersConfig {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderDef>,
    #[serde(default)]
    pub aliases: BTreeMap<String, AliasDef>,
    #[serde(default)]
    pub inference_rules: Vec<InferenceRule>,
    #[serde(default)]
    pub tier_rules: Vec<TierRule>,
    #[serde(default)]
    pub tier_defaults: TierDefaults,
    #[serde(default)]
    pub model_defaults: BTreeMap<String, BTreeMap<String, toml::Value>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderDef {
    pub base_url: String,
    #[serde(default)]
    pub base_url_env: Option<String>,
    #[serde(default = "default_bearer")]
    pub auth_style: String,
    #[serde(default)]
    pub auth_header: Option<String>,
    #[serde(default)]
    pub auth_env: AuthEnv,
    #[serde(default)]
    pub extra_headers: BTreeMap<String, String>,
    #[serde(default)]
    pub chat_endpoint: String,
    #[serde(default)]
    pub completion_endpoint: Option<String>,
    #[serde(default)]
    pub healthcheck: Option<HealthcheckDef>,
    #[serde(default)]
    pub features: Vec<String>,
    /// Fallback provider name to try if this provider fails.
    #[serde(default)]
    pub fallback: Option<String>,
    /// Number of retries before falling back (default 0).
    #[serde(default)]
    pub retry_count: Option<u32>,
    /// Delay between retries in milliseconds (default 1000).
    #[serde(default)]
    pub retry_delay_ms: Option<u64>,
    /// Maximum requests per minute. None = unlimited.
    #[serde(default)]
    pub rpm: Option<u32>,
}

impl Default for ProviderDef {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            base_url_env: None,
            auth_style: default_bearer(),
            auth_header: None,
            auth_env: AuthEnv::None,
            extra_headers: BTreeMap::new(),
            chat_endpoint: String::new(),
            completion_endpoint: None,
            healthcheck: None,
            features: Vec::new(),
            fallback: None,
            retry_count: None,
            retry_delay_ms: None,
            rpm: None,
        }
    }
}

fn default_bearer() -> String {
    "bearer".to_string()
}

/// Auth env var name(s) for the provider. Can be a single string or an array
/// (tried in order until one is set).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(untagged)]
pub enum AuthEnv {
    #[default]
    None,
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Clone, Deserialize)]
pub struct HealthcheckDef {
    pub method: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AliasDef {
    pub id: String,
    pub provider: String,
    /// Per-model tool format override: "native" or "text". When set, this
    /// takes precedence over the provider-level default. Models with strong
    /// tool-calling fine-tuning (Kimi-K2.5, GPT-4o) should use "native";
    /// models better served by text-based tool calling use "text".
    #[serde(default)]
    pub tool_format: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InferenceRule {
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default)]
    pub exact: Option<String>,
    pub provider: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TierRule {
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default)]
    pub exact: Option<String>,
    pub tier: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TierDefaults {
    #[serde(default = "default_mid")]
    pub default: String,
}

impl Default for TierDefaults {
    fn default() -> Self {
        Self {
            default: default_mid(),
        }
    }
}

fn default_mid() -> String {
    "mid".to_string()
}

// =============================================================================
// Config loading
// =============================================================================

/// Load and cache the providers config. Called once at VM startup.
pub fn load_config() -> &'static ProvidersConfig {
    CONFIG.get_or_init(|| {
        let verbose_config_logging = matches!(
            std::env::var("HARN_VERBOSE_CONFIG").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        ) || matches!(
            std::env::var("HARN_ACP_VERBOSE").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        );
        // Try explicit env var path first
        if let Ok(path) = std::env::var("HARN_PROVIDERS_CONFIG") {
            match std::fs::read_to_string(&path) {
                Ok(content) => match toml::from_str::<ProvidersConfig>(&content) {
                    Ok(config) => {
                        if verbose_config_logging {
                            eprintln!(
                                "[llm_config] Loaded {} providers, {} aliases from {}",
                                config.providers.len(),
                                config.aliases.len(),
                                path
                            );
                        }
                        let _ = CONFIG_PATH.set(path);
                        return config;
                    }
                    Err(e) => eprintln!("[llm_config] TOML parse error in {}: {}", path, e),
                },
                Err(e) => eprintln!("[llm_config] Cannot read {}: {}", path, e),
            }
        }
        // Try ~/.config/harn/providers.toml
        if let Some(home) = dirs_or_home() {
            let path = format!("{home}/.config/harn/providers.toml");
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(config) = toml::from_str::<ProvidersConfig>(&content) {
                    let _ = CONFIG_PATH.set(path);
                    return config;
                }
            }
        }
        // Fallback: built-in defaults
        default_config()
    })
}

/// Returns the filesystem path of the currently-loaded providers config, if
/// any. Returns `None` when built-in defaults are active.
pub fn loaded_config_path() -> Option<std::path::PathBuf> {
    // Trigger lazy init so CONFIG_PATH gets populated if a file was loaded.
    let _ = load_config();
    CONFIG_PATH.get().map(std::path::PathBuf::from)
}

/// Resolve a model alias to (model_id, provider_name).
pub fn resolve_model(alias: &str) -> (String, Option<String>) {
    let config = load_config();
    if let Some(a) = config.aliases.get(alias) {
        return (a.id.clone(), Some(a.provider.clone()));
    }
    (alias.to_string(), None)
}

/// Infer provider from a model ID using inference rules.
pub fn infer_provider(model_id: &str) -> String {
    let config = load_config();
    for rule in &config.inference_rules {
        if let Some(exact) = &rule.exact {
            if model_id == exact {
                return rule.provider.clone();
            }
        }
        if let Some(pattern) = &rule.pattern {
            if glob_match(pattern, model_id) {
                return rule.provider.clone();
            }
        }
        if let Some(substr) = &rule.contains {
            if model_id.contains(substr.as_str()) {
                return rule.provider.clone();
            }
        }
    }
    // Fallback to hardcoded inference.
    // Order matters: `local:` must beat the generic `:` → ollama rule, and
    // any prefix-based rule must beat the generic `/` → openrouter rule for
    // ids like `local:owner/model`.
    if model_id.starts_with("local:") {
        return "local".to_string();
    }
    if model_id.starts_with("claude-") {
        return "anthropic".to_string();
    }
    if model_id.starts_with("gpt-") || model_id.starts_with("o1") || model_id.starts_with("o3") {
        return "openai".to_string();
    }
    if model_id.contains('/') {
        return "openrouter".to_string();
    }
    if model_id.contains(':') {
        return "ollama".to_string();
    }
    "anthropic".to_string()
}

/// Get model tier ("small", "mid", "frontier").
pub fn model_tier(model_id: &str) -> String {
    let config = load_config();
    for rule in &config.tier_rules {
        if let Some(exact) = &rule.exact {
            if model_id == exact {
                return rule.tier.clone();
            }
        }
        if let Some(pattern) = &rule.pattern {
            if glob_match(pattern, model_id) {
                return rule.tier.clone();
            }
        }
        if let Some(substr) = &rule.contains {
            if model_id.contains(substr.as_str()) {
                return rule.tier.clone();
            }
        }
    }
    // Fallback
    let lower = model_id.to_lowercase();
    if lower.contains("9b") || lower.contains("a3b") {
        return "small".to_string();
    }
    if lower.starts_with("claude-") || lower == "gpt-4o" {
        return "frontier".to_string();
    }
    config.tier_defaults.default.clone()
}

/// Get provider config for resolving base_url, auth, etc.
pub fn provider_config(name: &str) -> Option<&'static ProviderDef> {
    load_config().providers.get(name)
}

/// Get model-specific default parameters (temperature, etc.).
/// Matches glob patterns in model_defaults keys.
pub fn model_params(model_id: &str) -> BTreeMap<String, toml::Value> {
    let config = load_config();
    let mut params = BTreeMap::new();
    for (pattern, defaults) in &config.model_defaults {
        if glob_match(pattern, model_id) {
            for (k, v) in defaults {
                params.insert(k.clone(), v.clone());
            }
        }
    }
    params
}

/// Get list of configured provider names.
pub fn provider_names() -> Vec<String> {
    load_config().providers.keys().cloned().collect()
}

/// Check if a provider advertises a feature (e.g., "native_tools").
pub fn provider_has_feature(provider: &str, feature: &str) -> bool {
    provider_config(provider)
        .map(|p| p.features.iter().any(|f| f == feature))
        .unwrap_or(false)
}

/// Resolve the default tool format for a model+provider combination.
/// Priority: alias `tool_format` (matched by model ID) > provider feature > "text".
pub fn default_tool_format(model: &str, provider: &str) -> String {
    let config = load_config();
    // Check aliases — match by model ID + provider, or by alias name
    for (name, alias) in &config.aliases {
        let matches = (alias.id == model && alias.provider == provider) || name == model;
        if matches {
            if let Some(ref fmt) = alias.tool_format {
                return fmt.clone();
            }
        }
    }
    // Fall back to provider feature
    if provider_has_feature(provider, "native_tools") {
        "native".to_string()
    } else {
        "text".to_string()
    }
}

/// Resolve a tier or alias into a concrete model/provider pair.
pub fn resolve_tier_model(
    target: &str,
    preferred_provider: Option<&str>,
) -> Option<(String, String)> {
    let config = load_config();

    if let Some(alias) = config.aliases.get(target) {
        return Some((alias.id.clone(), alias.provider.clone()));
    }

    let candidate_aliases = if let Some(provider) = preferred_provider {
        vec![
            format!("{provider}/{target}"),
            format!("{provider}:{target}"),
            format!("tier/{target}"),
            target.to_string(),
        ]
    } else {
        vec![format!("tier/{target}"), target.to_string()]
    };

    for alias_name in candidate_aliases {
        if let Some(alias) = config.aliases.get(&alias_name) {
            return Some((alias.id.clone(), alias.provider.clone()));
        }
    }

    None
}

/// Return all configured alias-backed model/provider pairs whose resolved
/// model falls into the requested capability tier. The result is de-duplicated
/// and sorted deterministically by provider then model id.
pub fn tier_candidates(target: &str) -> Vec<(String, String)> {
    let config = load_config();
    let mut seen = std::collections::BTreeSet::new();
    let mut candidates = Vec::new();

    for alias in config.aliases.values() {
        let pair = (alias.id.clone(), alias.provider.clone());
        if seen.contains(&pair) {
            continue;
        }
        if model_tier(&alias.id) == target {
            seen.insert(pair.clone());
            candidates.push(pair);
        }
    }

    candidates.sort_by(|(model_a, provider_a), (model_b, provider_b)| {
        provider_a
            .cmp(provider_b)
            .then_with(|| model_a.cmp(model_b))
    });
    candidates
}

// =============================================================================
// Helpers
// =============================================================================

/// Simple glob matching for patterns like "claude-*", "qwen/*", "ollama:*".
fn glob_match(pattern: &str, input: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        input.starts_with(prefix)
    } else if let Some(suffix) = pattern.strip_prefix('*') {
        input.ends_with(suffix)
    } else if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            input.starts_with(parts[0]) && input.ends_with(parts[1])
        } else {
            input == pattern
        }
    } else {
        input == pattern
    }
}

fn dirs_or_home() -> Option<String> {
    std::env::var("HOME").ok()
}

/// Resolve the effective base URL for a provider, checking the `base_url_env`
/// override first, then falling back to the configured `base_url`.
pub fn resolve_base_url(pdef: &ProviderDef) -> String {
    if let Some(env_name) = &pdef.base_url_env {
        if let Ok(val) = std::env::var(env_name) {
            // Strip surrounding quotes that some .env parsers leave intact.
            let trimmed = val.trim().trim_matches('"').trim_matches('\'');
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    pdef.base_url.clone()
}

// =============================================================================
// Built-in default config (matches current hardcoded behavior)
// =============================================================================

fn default_config() -> ProvidersConfig {
    let mut config = ProvidersConfig::default();

    // Anthropic
    config.providers.insert(
        "anthropic".to_string(),
        ProviderDef {
            base_url: "https://api.anthropic.com/v1".to_string(),
            auth_style: "header".to_string(),
            auth_header: Some("x-api-key".to_string()),
            auth_env: AuthEnv::Single("ANTHROPIC_API_KEY".to_string()),
            extra_headers: BTreeMap::from([(
                "anthropic-version".to_string(),
                "2023-06-01".to_string(),
            )]),
            chat_endpoint: "/messages".to_string(),
            completion_endpoint: None,
            healthcheck: Some(HealthcheckDef {
                method: "POST".to_string(),
                path: Some("/messages/count_tokens".to_string()),
                url: None,
                body: Some(
                    r#"{"model":"claude-sonnet-4-20250514","messages":[{"role":"user","content":"x"}]}"#
                        .to_string(),
                ),
            }),
            features: vec!["prompt_caching".to_string(), "thinking".to_string()],
            ..Default::default()
        },
    );

    // OpenAI
    config.providers.insert(
        "openai".to_string(),
        ProviderDef {
            base_url: "https://api.openai.com/v1".to_string(),
            auth_style: "bearer".to_string(),
            auth_env: AuthEnv::Single("OPENAI_API_KEY".to_string()),
            chat_endpoint: "/chat/completions".to_string(),
            completion_endpoint: Some("/completions".to_string()),
            healthcheck: Some(HealthcheckDef {
                method: "GET".to_string(),
                path: Some("/models".to_string()),
                url: None,
                body: None,
            }),
            ..Default::default()
        },
    );

    // OpenRouter
    config.providers.insert(
        "openrouter".to_string(),
        ProviderDef {
            base_url: "https://openrouter.ai/api/v1".to_string(),
            auth_style: "bearer".to_string(),
            auth_env: AuthEnv::Single("OPENROUTER_API_KEY".to_string()),
            chat_endpoint: "/chat/completions".to_string(),
            completion_endpoint: Some("/completions".to_string()),
            healthcheck: Some(HealthcheckDef {
                method: "GET".to_string(),
                path: Some("/auth/key".to_string()),
                url: None,
                body: None,
            }),
            ..Default::default()
        },
    );

    // HuggingFace
    config.providers.insert(
        "huggingface".to_string(),
        ProviderDef {
            base_url: "https://router.huggingface.co/v1".to_string(),
            auth_style: "bearer".to_string(),
            auth_env: AuthEnv::Multiple(vec![
                "HF_TOKEN".to_string(),
                "HUGGINGFACE_API_KEY".to_string(),
            ]),
            chat_endpoint: "/chat/completions".to_string(),
            completion_endpoint: Some("/completions".to_string()),
            healthcheck: Some(HealthcheckDef {
                method: "GET".to_string(),
                url: Some("https://huggingface.co/api/whoami-v2".to_string()),
                path: None,
                body: None,
            }),
            ..Default::default()
        },
    );

    // Ollama
    config.providers.insert(
        "ollama".to_string(),
        ProviderDef {
            base_url: "http://localhost:11434".to_string(),
            base_url_env: Some("OLLAMA_HOST".to_string()),
            auth_style: "none".to_string(),
            chat_endpoint: "/api/chat".to_string(),
            completion_endpoint: Some("/api/generate".to_string()),
            healthcheck: Some(HealthcheckDef {
                method: "GET".to_string(),
                path: Some("/api/tags".to_string()),
                url: None,
                body: None,
            }),
            ..Default::default()
        },
    );

    // Together AI (OpenAI-compatible)
    config.providers.insert(
        "together".to_string(),
        ProviderDef {
            base_url: "https://api.together.xyz/v1".to_string(),
            base_url_env: Some("TOGETHER_AI_BASE_URL".to_string()),
            auth_style: "bearer".to_string(),
            auth_env: AuthEnv::Single("TOGETHER_AI_API_KEY".to_string()),
            chat_endpoint: "/chat/completions".to_string(),
            completion_endpoint: Some("/completions".to_string()),
            healthcheck: Some(HealthcheckDef {
                method: "GET".to_string(),
                path: Some("/models".to_string()),
                url: None,
                body: None,
            }),
            ..Default::default()
        },
    );

    // Local OpenAI-compatible server
    config.providers.insert(
        "local".to_string(),
        ProviderDef {
            base_url: "http://localhost:8000".to_string(),
            base_url_env: Some("LOCAL_LLM_BASE_URL".to_string()),
            auth_style: "none".to_string(),
            chat_endpoint: "/v1/chat/completions".to_string(),
            completion_endpoint: Some("/v1/completions".to_string()),
            healthcheck: Some(HealthcheckDef {
                method: "GET".to_string(),
                path: Some("/v1/models".to_string()),
                url: None,
                body: None,
            }),
            ..Default::default()
        },
    );

    // Default inference rules
    config.inference_rules = vec![
        InferenceRule {
            pattern: Some("claude-*".to_string()),
            contains: None,
            exact: None,
            provider: "anthropic".to_string(),
        },
        InferenceRule {
            pattern: Some("gpt-*".to_string()),
            contains: None,
            exact: None,
            provider: "openai".to_string(),
        },
        InferenceRule {
            pattern: Some("o1*".to_string()),
            contains: None,
            exact: None,
            provider: "openai".to_string(),
        },
        InferenceRule {
            pattern: Some("o3*".to_string()),
            contains: None,
            exact: None,
            provider: "openai".to_string(),
        },
        InferenceRule {
            pattern: Some("local:*".to_string()),
            contains: None,
            exact: None,
            provider: "local".to_string(),
        },
        InferenceRule {
            pattern: None,
            contains: Some("/".to_string()),
            exact: None,
            provider: "openrouter".to_string(),
        },
        InferenceRule {
            pattern: None,
            contains: Some(":".to_string()),
            exact: None,
            provider: "ollama".to_string(),
        },
    ];

    // Default tier rules
    config.tier_rules = vec![
        TierRule {
            contains: Some("9b".to_string()),
            pattern: None,
            exact: None,
            tier: "small".to_string(),
        },
        TierRule {
            contains: Some("a3b".to_string()),
            pattern: None,
            exact: None,
            tier: "small".to_string(),
        },
        TierRule {
            contains: Some("gemma-4-e2b".to_string()),
            pattern: None,
            exact: None,
            tier: "small".to_string(),
        },
        TierRule {
            contains: Some("gemma-4-e4b".to_string()),
            pattern: None,
            exact: None,
            tier: "small".to_string(),
        },
        TierRule {
            contains: Some("gemma-4-26b".to_string()),
            pattern: None,
            exact: None,
            tier: "mid".to_string(),
        },
        TierRule {
            contains: Some("gemma-4-31b".to_string()),
            pattern: None,
            exact: None,
            tier: "frontier".to_string(),
        },
        TierRule {
            contains: Some("gemma4:26b".to_string()),
            pattern: None,
            exact: None,
            tier: "mid".to_string(),
        },
        TierRule {
            contains: Some("gemma4:31b".to_string()),
            pattern: None,
            exact: None,
            tier: "frontier".to_string(),
        },
        TierRule {
            pattern: Some("claude-*".to_string()),
            contains: None,
            exact: None,
            tier: "frontier".to_string(),
        },
        TierRule {
            exact: Some("gpt-4o".to_string()),
            contains: None,
            pattern: None,
            tier: "frontier".to_string(),
        },
    ];

    config.tier_defaults = TierDefaults {
        default: "mid".to_string(),
    };

    config.aliases.insert(
        "frontier".to_string(),
        AliasDef {
            id: "claude-sonnet-4-20250514".to_string(),
            provider: "anthropic".to_string(),
            tool_format: None,
        },
    );
    config.aliases.insert(
        "tier/frontier".to_string(),
        AliasDef {
            id: "claude-sonnet-4-20250514".to_string(),
            provider: "anthropic".to_string(),
            tool_format: None,
        },
    );
    config.aliases.insert(
        "mid".to_string(),
        AliasDef {
            id: "gpt-4o-mini".to_string(),
            provider: "openai".to_string(),
            tool_format: None,
        },
    );
    config.aliases.insert(
        "tier/mid".to_string(),
        AliasDef {
            id: "gpt-4o-mini".to_string(),
            provider: "openai".to_string(),
            tool_format: None,
        },
    );
    config.aliases.insert(
        "small".to_string(),
        AliasDef {
            id: "Qwen/Qwen3.5-9B".to_string(),
            provider: "openrouter".to_string(),
            tool_format: None,
        },
    );
    config.aliases.insert(
        "tier/small".to_string(),
        AliasDef {
            id: "Qwen/Qwen3.5-9B".to_string(),
            provider: "openrouter".to_string(),
            tool_format: None,
        },
    );
    config.aliases.insert(
        "local-gemma4".to_string(),
        AliasDef {
            id: "gemma-4-26b-a4b-it".to_string(),
            provider: "local".to_string(),
            tool_format: None,
        },
    );
    config.aliases.insert(
        "local-gemma4-26b".to_string(),
        AliasDef {
            id: "gemma-4-26b-a4b-it".to_string(),
            provider: "local".to_string(),
            tool_format: None,
        },
    );
    config.aliases.insert(
        "local-gemma4-31b".to_string(),
        AliasDef {
            id: "gemma-4-31b-it".to_string(),
            provider: "local".to_string(),
            tool_format: None,
        },
    );
    config.aliases.insert(
        "local-gemma4-e4b".to_string(),
        AliasDef {
            id: "gemma-4-e4b-it".to_string(),
            provider: "local".to_string(),
            tool_format: None,
        },
    );
    config.aliases.insert(
        "local-gemma4-e2b".to_string(),
        AliasDef {
            id: "gemma-4-e2b-it".to_string(),
            provider: "local".to_string(),
            tool_format: None,
        },
    );

    config
}

// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_match_prefix() {
        assert!(glob_match("claude-*", "claude-sonnet-4-20250514"));
        assert!(glob_match("gpt-*", "gpt-4o"));
        assert!(!glob_match("claude-*", "gpt-4o"));
    }

    #[test]
    fn test_glob_match_suffix() {
        assert!(glob_match("*-latest", "llama3.2-latest"));
        assert!(!glob_match("*-latest", "llama3.2"));
    }

    #[test]
    fn test_glob_match_middle() {
        assert!(glob_match("claude-*-latest", "claude-sonnet-latest"));
        assert!(!glob_match("claude-*-latest", "claude-sonnet-beta"));
    }

    #[test]
    fn test_glob_match_exact() {
        assert!(glob_match("gpt-4o", "gpt-4o"));
        assert!(!glob_match("gpt-4o", "gpt-4o-mini"));
    }

    #[test]
    fn test_infer_provider_from_defaults() {
        // These test the fallback logic (after rules)
        assert_eq!(infer_provider("claude-sonnet-4-20250514"), "anthropic");
        assert_eq!(infer_provider("gpt-4o"), "openai");
        assert_eq!(infer_provider("o1-preview"), "openai");
        assert_eq!(infer_provider("o3-mini"), "openai");
        assert_eq!(infer_provider("qwen/qwen3-coder"), "openrouter");
        assert_eq!(infer_provider("llama3.2:latest"), "ollama");
        assert_eq!(infer_provider("unknown-model"), "anthropic");
    }

    #[test]
    fn test_infer_provider_local_prefix() {
        // `local:` must route to the local OpenAI-compatible provider, not
        // ollama (which would otherwise swallow everything containing `:`).
        assert_eq!(infer_provider("local:gemma-4-e4b-it"), "local");
        assert_eq!(infer_provider("local:qwen2.5"), "local");
        // Even when the id also contains `/`, the `local:` prefix wins.
        assert_eq!(infer_provider("local:owner/model"), "local");
    }

    #[test]
    fn test_model_tier_from_defaults() {
        assert_eq!(model_tier("claude-sonnet-4-20250514"), "frontier");
        assert_eq!(model_tier("gpt-4o"), "frontier");
        assert_eq!(model_tier("Qwen3.5-9B"), "small");
        assert_eq!(model_tier("deepseek-v3"), "mid");
    }

    #[test]
    fn test_resolve_model_unknown_alias() {
        let (id, provider) = resolve_model("gpt-4o");
        assert_eq!(id, "gpt-4o");
        assert!(provider.is_none());
    }

    #[test]
    fn test_provider_names() {
        let names = provider_names();
        assert!(names.len() >= 7);
        assert!(names.contains(&"anthropic".to_string()));
        assert!(names.contains(&"together".to_string()));
        assert!(names.contains(&"local".to_string()));
        assert!(names.contains(&"openai".to_string()));
        assert!(names.contains(&"ollama".to_string()));
    }

    #[test]
    fn test_resolve_tier_model_default_aliases() {
        let (model, provider) = resolve_tier_model("frontier", None).unwrap();
        assert_eq!(model, "claude-sonnet-4-20250514");
        assert_eq!(provider, "anthropic");

        let (model, provider) = resolve_tier_model("small", None).unwrap();
        assert_eq!(model, "Qwen/Qwen3.5-9B");
        assert_eq!(provider, "openrouter");
    }

    #[test]
    fn test_resolve_tier_model_prefers_provider_scoped_aliases() {
        let (model, provider) = resolve_tier_model("mid", Some("openai")).unwrap();
        assert_eq!(model, "gpt-4o-mini");
        assert_eq!(provider, "openai");
    }

    #[test]
    fn test_provider_config_anthropic() {
        let pdef = provider_config("anthropic").unwrap();
        assert_eq!(pdef.auth_style, "header");
        assert_eq!(pdef.auth_header.as_deref(), Some("x-api-key"));
    }

    #[test]
    fn test_resolve_base_url_no_env() {
        let pdef = ProviderDef {
            base_url: "https://example.com".to_string(),
            ..Default::default()
        };
        assert_eq!(resolve_base_url(&pdef), "https://example.com");
    }

    #[test]
    fn test_default_config_roundtrip() {
        let config = default_config();
        assert!(!config.providers.is_empty());
        assert!(!config.inference_rules.is_empty());
        assert!(!config.tier_rules.is_empty());
        assert_eq!(config.tier_defaults.default, "mid");
    }

    #[test]
    fn test_model_params_empty() {
        let params = model_params("claude-sonnet-4-20250514");
        // Default config has no model_defaults, so should be empty
        assert!(params.is_empty());
    }
}
