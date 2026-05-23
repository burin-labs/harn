use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::OnceLock;

static CONFIG: OnceLock<ProvidersConfig> = OnceLock::new();
static CONFIG_PATH: OnceLock<String> = OnceLock::new();

thread_local! {
    /// Thread-local provider config overlays installed by the CLI after it
    /// reads the nearest `harn.toml` plus any installed package manifests.
    /// Kept thread-local so tests and multi-VM hosts can scope extensions to
    /// the current run without mutating the process-wide default config.
    static USER_OVERRIDES: RefCell<Option<ProvidersConfig>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProvidersConfig {
    #[serde(default)]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderDef>,
    #[serde(default)]
    pub aliases: BTreeMap<String, AliasDef>,
    #[serde(default)]
    pub alias_tool_calling: BTreeMap<String, AliasToolCallingDef>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelDef>,
    #[serde(default)]
    pub qc_defaults: BTreeMap<String, String>,
    #[serde(default)]
    pub inference_rules: Vec<InferenceRule>,
    #[serde(default)]
    pub tier_rules: Vec<TierRule>,
    #[serde(default)]
    pub tier_defaults: TierDefaults,
    #[serde(default)]
    pub model_defaults: BTreeMap<String, BTreeMap<String, toml::Value>>,
}

impl ProvidersConfig {
    pub fn is_empty(&self) -> bool {
        self.default_provider.is_none()
            && self.providers.is_empty()
            && self.aliases.is_empty()
            && self.alias_tool_calling.is_empty()
            && self.models.is_empty()
            && self.qc_defaults.is_empty()
            && self.inference_rules.is_empty()
            && self.tier_rules.is_empty()
            && self.model_defaults.is_empty()
            && self.tier_defaults.default == default_mid()
    }

    pub fn merge_from(&mut self, overlay: &ProvidersConfig) {
        self.providers.extend(overlay.providers.clone());
        self.aliases.extend(overlay.aliases.clone());
        self.alias_tool_calling
            .extend(overlay.alias_tool_calling.clone());
        self.models.extend(overlay.models.clone());
        self.qc_defaults.extend(overlay.qc_defaults.clone());

        if overlay.default_provider.is_some() {
            self.default_provider = overlay.default_provider.clone();
        }

        if !overlay.inference_rules.is_empty() {
            let mut merged = overlay.inference_rules.clone();
            merged.extend(self.inference_rules.clone());
            self.inference_rules = merged;
        }

        if !overlay.tier_rules.is_empty() {
            let mut merged = overlay.tier_rules.clone();
            merged.extend(self.tier_rules.clone());
            self.tier_rules = merged;
        }

        if overlay.tier_defaults.default != default_mid() {
            self.tier_defaults = overlay.tier_defaults.clone();
        }

        for (pattern, defaults) in &overlay.model_defaults {
            self.model_defaults
                .entry(pattern.clone())
                .or_default()
                .extend(defaults.clone());
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderDef {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
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
    /// Provider/catalog pricing in USD per 1k input tokens.
    #[serde(default)]
    pub cost_per_1k_in: Option<f64>,
    /// Provider/catalog pricing in USD per 1k output tokens.
    #[serde(default)]
    pub cost_per_1k_out: Option<f64>,
    /// Observed or configured p50 latency in milliseconds.
    #[serde(default)]
    pub latency_p50_ms: Option<u64>,
}

impl Default for ProviderDef {
    fn default() -> Self {
        Self {
            display_name: None,
            icon: None,
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
            cost_per_1k_in: None,
            cost_per_1k_out: None,
            latency_p50_ms: None,
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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AliasToolCallingDef {
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streaming_native: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_mode: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_probe_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    #[serde(default)]
    pub cache_read_per_mtok: Option<f64>,
    #[serde(default)]
    pub cache_write_per_mtok: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ModelDef {
    pub name: String,
    pub provider: String,
    pub context_window: u64,
    #[serde(default)]
    pub runtime_context_window: Option<u64>,
    #[serde(default)]
    pub stream_timeout: Option<f64>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub pricing: Option<ModelPricing>,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub deprecation_note: Option<String>,
    #[serde(default)]
    pub quality_tags: Vec<String>,
    /// Whether the model can be reached over a normal API-key serverless call,
    /// or only via a dedicated/provisioned endpoint that the caller must spin
    /// up out-of-band. Providers like Together list dedicated-only routes
    /// alongside serverless ones in `/v1/models`, so this metadata lets clients
    /// avoid presenting them as one-click options.
    #[serde(default)]
    pub availability: ModelAvailability,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability {
    /// Reachable through the provider's normal API-key path with no extra
    /// setup. The default for cataloged hosted/local models: by cataloging a
    /// row we are claiming the route works out of the box.
    #[default]
    Serverless,
    /// Requires the caller to provision a dedicated endpoint before requests
    /// will succeed. The catalog row exists for selection/pricing UI, but
    /// hosts must not auto-route to it.
    Dedicated,
    /// Availability is not known ahead of time. Used for routes that were
    /// surfaced dynamically (e.g. through `/v1/models`) without a static
    /// claim from Harn or the user.
    Unknown,
}

impl ModelAvailability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Serverless => "serverless",
            Self::Dedicated => "dedicated",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "serverless" => Some(Self::Serverless),
            "dedicated" => Some(Self::Dedicated),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResolvedModel {
    pub id: String,
    pub provider: String,
    pub alias: Option<String>,
    pub tool_format: String,
    pub tier: String,
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

/// Load and cache the providers config. Called once at VM startup.
pub fn load_config() -> &'static ProvidersConfig {
    CONFIG.get_or_init(|| {
        let mut config = default_config();
        let verbose_config_logging = matches!(
            std::env::var("HARN_VERBOSE_CONFIG").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        ) || matches!(
            std::env::var("HARN_ACP_VERBOSE").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        );
        if let Ok(path) = std::env::var("HARN_PROVIDERS_CONFIG") {
            if let Some(overlay) = read_external_config(&path, verbose_config_logging) {
                config.merge_from(&overlay);
                let _ = CONFIG_PATH.set(path);
                return config;
            }
        }
        if let Some(home) = dirs_or_home() {
            let path = format!("{home}/.config/harn/providers.toml");
            if let Some(overlay) = read_external_config(&path, false) {
                config.merge_from(&overlay);
                let _ = CONFIG_PATH.set(path);
                return config;
            }
        }
        config
    })
}

fn read_external_config(path: &str, verbose: bool) -> Option<ProvidersConfig> {
    match std::fs::read_to_string(path) {
        Ok(content) => match toml::from_str::<ProvidersConfig>(&content) {
            Ok(config) => {
                if verbose {
                    eprintln!(
                        "[llm_config] Loaded {} providers, {} aliases from {}",
                        config.providers.len(),
                        config.aliases.len(),
                        path
                    );
                }
                Some(config)
            }
            Err(error) => {
                eprintln!("[llm_config] TOML parse error in {}: {}", path, error);
                None
            }
        },
        Err(error) => {
            if verbose {
                eprintln!("[llm_config] Cannot read {}: {}", path, error);
            }
            None
        }
    }
}

/// Parse a provider/model catalog overlay in the same shape as
/// `providers.toml` or `[llm]` package-manifest sections.
pub fn parse_config_toml(src: &str) -> Result<ProvidersConfig, toml::de::Error> {
    toml::from_str::<ProvidersConfig>(src)
}

/// Returns the filesystem path of the currently-loaded providers config, if
/// any. Returns `None` when built-in defaults are active.
pub fn loaded_config_path() -> Option<std::path::PathBuf> {
    // Force lazy init so CONFIG_PATH is populated if a file was loaded.
    let _ = load_config();
    CONFIG_PATH.get().map(std::path::PathBuf::from)
}

/// Install per-run provider config overlays. The overlay uses the same shape as
/// `providers.toml`, but lives under `[llm]` in `harn.toml` and package
/// manifests. Passing `None` clears the overlay.
pub fn set_user_overrides(config: Option<ProvidersConfig>) {
    USER_OVERRIDES.with(|cell| *cell.borrow_mut() = config);
}

/// Clear per-run provider config overlays.
pub fn clear_user_overrides() {
    set_user_overrides(None);
}

fn effective_config() -> ProvidersConfig {
    let mut merged = load_config().clone();
    USER_OVERRIDES.with(|cell| {
        if let Some(overlay) = cell.borrow().as_ref() {
            merged.merge_from(overlay);
        }
    });
    merged
}

/// Resolve a model alias to (model_id, provider_name).
pub fn resolve_model(alias: &str) -> (String, Option<String>) {
    let config = effective_config();
    if let Some(a) = config.aliases.get(alias) {
        return (a.id.clone(), Some(a.provider.clone()));
    }
    (normalize_model_id(alias), None)
}

/// Strip host/provider selector prefixes that identify transport, not the
/// provider-native model id. This mirrors Burin's existing normalization so
/// `ollama:qwen3:30b` reaches Ollama as `qwen3:30b` instead of an invalid
/// model named `ollama`. Cerebras follows the same convention but uses a
/// slash separator (`cerebras/gpt-oss-120b`) because its own /v1/models
/// endpoint returns bare names that overlap OpenAI's families.
pub fn normalize_model_id(raw: &str) -> String {
    for prefix in PROVIDER_SELECTOR_PREFIXES {
        if let Some(stripped) = raw.strip_prefix(prefix) {
            return stripped.to_string();
        }
    }
    raw.to_string()
}

const PROVIDER_SELECTOR_PREFIXES: &[&str] =
    &["ollama:", "local:", "huggingface:", "hf:", "cerebras/"];

/// Resolve an alias or selector into the complete catalog identity hosts need:
/// provider inference, prefix-normalized model id, default tool format, and tier.
pub fn resolve_model_info(selector: &str) -> ResolvedModel {
    let config = effective_config();
    if let Some(alias) = config.aliases.get(selector) {
        let id = alias.id.clone();
        let provider = alias.provider.clone();
        let tool_format = alias
            .tool_format
            .clone()
            .unwrap_or_else(|| default_tool_format_with_config(&config, &id, &provider));
        return ResolvedModel {
            tier: model_tier_with_config(&config, &id),
            id,
            provider,
            alias: Some(selector.to_string()),
            tool_format,
        };
    }

    let id = normalize_model_id(selector);
    let provider = infer_provider_with_config(&config, selector).provider;
    let tool_format = default_tool_format_with_config(&config, &id, &provider);
    let tier = model_tier_with_config(&config, &id);
    ResolvedModel {
        id,
        provider,
        alias: None,
        tool_format,
        tier,
    }
}

/// Infer provider from a model ID using inference rules.
pub fn infer_provider(model_id: &str) -> String {
    infer_provider_detail(model_id).provider
}

/// Infer provider from a model ID and retain whether the configured default was used.
pub(crate) fn infer_provider_detail(model_id: &str) -> crate::llm::provider::ProviderInference {
    let config = effective_config();
    infer_provider_with_config(&config, model_id)
}

fn infer_provider_with_config(
    config: &ProvidersConfig,
    model_id: &str,
) -> crate::llm::provider::ProviderInference {
    if model_id.starts_with("local:") || model_id.starts_with("ollama:") {
        return crate::llm::provider::ProviderInference::builtin("ollama");
    }
    if model_id.starts_with("huggingface:") || model_id.starts_with("hf:") {
        return crate::llm::provider::ProviderInference::builtin("huggingface");
    }
    // Exact catalog rows are the most authoritative declaration of where
    // a model is hosted: any pattern-based inference rule is necessarily
    // less specific than `[models."<id>"].provider = "<name>"`. Catalogs
    // include user overlays, so users can still re-home a model by
    // setting a catalog entry in their own providers.toml.
    let normalized_id = normalize_model_id(model_id);
    if let Some(model) = config
        .models
        .get(model_id)
        .or_else(|| config.models.get(&normalized_id))
    {
        return crate::llm::provider::ProviderInference::builtin(model.provider.clone());
    }
    for rule in &config.inference_rules {
        if let Some(exact) = &rule.exact {
            if model_id == exact {
                return crate::llm::provider::ProviderInference::builtin(rule.provider.clone());
            }
        }
        if let Some(pattern) = &rule.pattern {
            if glob_match(pattern, model_id) {
                return crate::llm::provider::ProviderInference::builtin(rule.provider.clone());
            }
        }
        if let Some(substr) = &rule.contains {
            if model_id.contains(substr.as_str()) {
                return crate::llm::provider::ProviderInference::builtin(rule.provider.clone());
            }
        }
    }
    crate::llm::provider::infer_provider_from_model_id(
        model_id,
        &default_provider_with_config(config),
    )
}

pub fn default_provider() -> String {
    let config = effective_config();
    default_provider_with_config(&config)
}

fn default_provider_with_config(config: &ProvidersConfig) -> String {
    std::env::var("HARN_DEFAULT_PROVIDER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("auto"))
        .or_else(|| {
            config
                .default_provider
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("auto"))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "anthropic".to_string())
}

/// Get model tier ("small", "mid", "frontier").
pub fn model_tier(model_id: &str) -> String {
    let config = effective_config();
    model_tier_with_config(&config, model_id)
}

fn model_tier_with_config(config: &ProvidersConfig, model_id: &str) -> String {
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
pub fn provider_config(name: &str) -> Option<ProviderDef> {
    effective_config().providers.get(name).cloned()
}

/// Get model-specific default parameters (temperature, etc.).
/// Matches glob patterns in model_defaults keys.
pub fn model_params(model_id: &str) -> BTreeMap<String, toml::Value> {
    let config = effective_config();
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
    effective_config().providers.keys().cloned().collect()
}

/// Return every configured alias name, sorted deterministically.
pub fn known_model_names() -> Vec<String> {
    effective_config().aliases.keys().cloned().collect()
}

pub fn alias_entries() -> Vec<(String, AliasDef)> {
    effective_config().aliases.into_iter().collect()
}

pub fn alias_tool_calling_entry(alias: &str) -> Option<AliasToolCallingDef> {
    effective_config().alias_tool_calling.get(alias).cloned()
}

/// Return every configured model-catalog entry, sorted by provider then id.
pub fn model_catalog_entries() -> Vec<(String, ModelDef)> {
    let mut entries: Vec<_> = effective_config()
        .models
        .into_iter()
        .map(|(id, model)| {
            let provider = model.provider.clone();
            (
                id.clone(),
                with_effective_capability_tags(id, provider, model),
            )
        })
        .collect();
    entries.sort_by(|(id_a, model_a), (id_b, model_b)| {
        model_a
            .provider
            .cmp(&model_b.provider)
            .then_with(|| id_a.cmp(id_b))
    });
    entries
}

pub fn model_catalog_entry(model_id: &str) -> Option<ModelDef> {
    effective_config()
        .models
        .get(model_id)
        .cloned()
        .map(|model| {
            let provider = model.provider.clone();
            with_effective_capability_tags(model_id.to_string(), provider, model)
        })
}

pub fn qc_default_model(provider: &str) -> Option<String> {
    std::env::var("BURIN_QC_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            effective_config()
                .qc_defaults
                .get(&provider.to_lowercase())
                .cloned()
        })
}

pub fn default_model_for_provider(provider: &str) -> String {
    match provider {
        "local" => std::env::var("LOCAL_LLM_MODEL")
            .or_else(|_| std::env::var("HARN_LLM_MODEL"))
            .unwrap_or_else(|_| "gemma-4-26b-a4b-it".to_string()),
        "mlx" => std::env::var("MLX_MODEL_ID")
            .unwrap_or_else(|_| "unsloth/Qwen3.6-27B-UD-MLX-4bit".to_string()),
        "openai" => "gpt-4o-mini".to_string(),
        "ollama" => "llama3.2".to_string(),
        "openrouter" => "anthropic/claude-sonnet-4.6".to_string(),
        _ => "claude-sonnet-4-6".to_string(),
    }
}

pub fn qc_defaults() -> BTreeMap<String, String> {
    effective_config().qc_defaults
}

pub fn model_pricing_per_mtok(model_id: &str) -> Option<ModelPricing> {
    effective_config()
        .models
        .get(model_id)
        .and_then(|model| model.pricing.clone())
}

pub fn pricing_per_1k_for(provider: &str, model_id: &str) -> Option<(f64, f64)> {
    model_pricing_per_mtok(model_id)
        .map(|pricing| {
            (
                pricing.input_per_mtok / 1000.0,
                pricing.output_per_mtok / 1000.0,
            )
        })
        .or_else(|| {
            let (input, output, _) = provider_economics(provider);
            match (input, output) {
                (Some(input), Some(output)) => Some((input, output)),
                _ => None,
            }
        })
}

pub fn auth_env_names(auth_env: &AuthEnv) -> Vec<String> {
    match auth_env {
        AuthEnv::None => Vec::new(),
        AuthEnv::Single(name) => vec![name.clone()],
        AuthEnv::Multiple(names) => names.clone(),
    }
}

pub fn provider_key_available(provider: &str) -> bool {
    let Some(pdef) = provider_config(provider) else {
        return provider == "ollama";
    };
    if pdef.auth_style == "none" || matches!(pdef.auth_env, AuthEnv::None) {
        return true;
    }
    auth_env_names(&pdef.auth_env).into_iter().any(|env_name| {
        std::env::var(env_name)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    })
}

pub fn available_provider_names() -> Vec<String> {
    provider_names()
        .into_iter()
        .filter(|provider| provider_key_available(provider))
        .collect()
}

/// Check if a provider advertises a legacy provider-level feature.
pub fn provider_has_feature(provider: &str, feature: &str) -> bool {
    provider_config(provider)
        .map(|p| p.features.iter().any(|f| f == feature))
        .unwrap_or(false)
}

/// Provider-level catalog pricing/latency. Model-specific catalog pricing
/// wins when available; this is the adapter-level fallback used by routing
/// and portal summaries when a model has no explicit catalog entry.
pub fn provider_economics(provider: &str) -> (Option<f64>, Option<f64>, Option<u64>) {
    provider_config(provider)
        .map(|p| (p.cost_per_1k_in, p.cost_per_1k_out, p.latency_p50_ms))
        .unwrap_or((None, None, None))
}

/// Resolve the default tool format for a model+provider combination.
/// Priority: alias `tool_format` (matched by model ID) > provider/model
/// capability matrix > legacy provider feature > "text".
pub fn default_tool_format(model: &str, provider: &str) -> String {
    let config = effective_config();
    default_tool_format_with_config(&config, model, provider)
}

fn default_tool_format_with_config(
    config: &ProvidersConfig,
    model: &str,
    provider: &str,
) -> String {
    // Aliases match by model ID + provider, or by alias name.
    for (name, alias) in &config.aliases {
        let matches = (alias.id == model && alias.provider == provider) || name == model;
        if matches {
            if let Some(ref fmt) = alias.tool_format {
                return fmt.clone();
            }
        }
    }
    let capabilities = crate::llm::capabilities::lookup(provider, model);
    if let Some(format) = capabilities.preferred_tool_format.as_deref() {
        if matches!(format, "native" | "text") {
            return format.to_string();
        }
    }
    let capability_matrix_native = capabilities.native_tools;
    let legacy_provider_native = config
        .providers
        .get(provider)
        .map(|p| p.features.iter().any(|f| f == "native_tools"))
        .unwrap_or(false);
    if capability_matrix_native || legacy_provider_native {
        "native".to_string()
    } else {
        "text".to_string()
    }
}

fn with_effective_capability_tags(
    model_id: String,
    provider: String,
    mut model: ModelDef,
) -> ModelDef {
    model.capabilities = effective_model_capability_tags(&provider, &model_id);
    model
}

/// Legacy display tags derived from the canonical provider/model capability
/// matrix. The matrix is the source of truth; `models.*.capabilities` in
/// providers.toml is accepted only for backwards-compatible parsing.
pub fn effective_model_capability_tags(provider: &str, model_id: &str) -> Vec<String> {
    let caps = crate::llm::capabilities::lookup(provider, model_id);
    let mut tags = Vec::new();
    // Today all Harn chat providers expose streaming. Keep this as a
    // transport baseline rather than a duplicated per-model declaration.
    tags.push("streaming".to_string());
    if caps.native_tools || caps.text_tool_wire_format_supported {
        tags.push("tools".to_string());
    }
    if !caps.tool_search.is_empty() {
        tags.push("tool_search".to_string());
    }
    if caps.vision || caps.vision_supported {
        tags.push("vision".to_string());
    }
    if caps.audio {
        tags.push("audio".to_string());
    }
    if caps.pdf {
        tags.push("pdf".to_string());
    }
    if caps.files_api_supported {
        tags.push("files".to_string());
    }
    if caps.prompt_caching {
        tags.push("prompt_caching".to_string());
    }
    if !caps.thinking_modes.is_empty() {
        tags.push("thinking".to_string());
    }
    if caps.interleaved_thinking_supported
        || caps
            .thinking_modes
            .iter()
            .any(|mode| mode == "adaptive" || mode == "effort")
    {
        tags.push("extended_thinking".to_string());
    }
    if caps.json_schema.is_some() {
        tags.push("structured_output".to_string());
    }
    tags
}

/// Resolve a tier or alias into a concrete model/provider pair.
pub fn resolve_tier_model(
    target: &str,
    preferred_provider: Option<&str>,
) -> Option<(String, String)> {
    let config = effective_config();

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
    let config = effective_config();
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

/// Return all configured alias-backed model/provider pairs. Used by routing
/// policies that need to compare alternatives across tiers.
pub fn all_model_candidates() -> Vec<(String, String)> {
    let config = effective_config();
    let mut seen = std::collections::BTreeSet::new();
    let mut candidates = Vec::new();

    for alias in config.aliases.values() {
        let pair = (alias.id.clone(), alias.provider.clone());
        if seen.insert(pair.clone()) {
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

/// Embedded copy of `llm/providers.toml`, the single source of truth for
/// Harn's bundled provider/model catalog. Edit the TOML, not this string.
const EMBEDDED_PROVIDERS_TOML: &str = include_str!("llm/providers.toml");

/// Parse the embedded `providers.toml` into the runtime `ProvidersConfig`.
///
/// Hosts overlay this base via `HARN_PROVIDERS_CONFIG`,
/// `~/.config/harn/providers.toml`, `harn.toml`, package-manifest
/// `[llm]` sections, and per-run `set_user_overrides(...)`. The same
/// Serde shape applies at every layer, so there is exactly one schema to
/// keep coherent — no parallel Rust-literal catalog.
///
/// We `expect` on parse failure because the file is bundled into the
/// binary at compile time; a malformed embedded catalog is a build-time
/// invariant violation that should fail every test, not silently
/// degrade in production.
fn default_config() -> ProvidersConfig {
    parse_config_toml(EMBEDDED_PROVIDERS_TOML)
        .expect("embedded providers.toml must parse — invariant checked by harn-vm tests")
}

#[cfg(test)]
fn merge_global_config(overlay: ProvidersConfig) -> ProvidersConfig {
    let mut config = default_config();
    config.merge_from(&overlay);
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_overrides() {
        clear_user_overrides();
    }

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
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
        unsafe {
            std::env::remove_var("HARN_DEFAULT_PROVIDER");
        }

        assert_eq!(infer_provider("claude-sonnet-4-20250514"), "anthropic");
        assert_eq!(infer_provider("gpt-4o"), "openai");
        assert_eq!(infer_provider("o1-preview"), "openai");
        assert_eq!(infer_provider("o3-mini"), "openai");
        assert_eq!(infer_provider("o4-mini"), "openai");
        assert_eq!(infer_provider("gemini-2.5-pro"), "gemini");
        assert_eq!(infer_provider("qwen/qwen3-coder"), "openrouter");
        assert_eq!(infer_provider("llama3.2:latest"), "ollama");
        assert_eq!(infer_provider("unknown-model"), "anthropic");

        unsafe {
            match prev_default_provider {
                Some(value) => std::env::set_var("HARN_DEFAULT_PROVIDER", value),
                None => std::env::remove_var("HARN_DEFAULT_PROVIDER"),
            }
        }
    }

    #[test]
    fn test_infer_provider_prefix_rules() {
        assert_eq!(infer_provider("local:gemma-4-e4b-it"), "ollama");
        assert_eq!(infer_provider("ollama:qwen3:30b-a3b"), "ollama");
        // Even when the id also contains `/`, the local transport prefix wins.
        assert_eq!(infer_provider("local:owner/model"), "ollama");
        assert_eq!(infer_provider("hf:Qwen/Qwen3.6-35B-A3B"), "huggingface");
    }

    #[test]
    fn test_openrouter_inference_requires_one_slash() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
        unsafe {
            std::env::remove_var("HARN_DEFAULT_PROVIDER");
        }

        assert_eq!(infer_provider("org/model"), "openrouter");
        assert_eq!(infer_provider("org/team/model"), "anthropic");

        unsafe {
            match prev_default_provider {
                Some(value) => std::env::set_var("HARN_DEFAULT_PROVIDER", value),
                None => std::env::remove_var("HARN_DEFAULT_PROVIDER"),
            }
        }
    }

    #[test]
    fn test_cerebras_inference_beats_openrouter_slash_fallback() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
        unsafe {
            std::env::remove_var("HARN_DEFAULT_PROVIDER");
        }

        assert_eq!(infer_provider("cerebras/gpt-oss-120b"), "cerebras");
        assert_eq!(infer_provider("cerebras/llama-3.3-70b"), "cerebras");

        unsafe {
            match prev_default_provider {
                Some(value) => std::env::set_var("HARN_DEFAULT_PROVIDER", value),
                None => std::env::remove_var("HARN_DEFAULT_PROVIDER"),
            }
        }
    }

    #[test]
    fn test_direct_catalog_model_id_resolves_to_catalog_provider() {
        // Bare model IDs that the embedded catalog hosts on Cerebras must
        // not be misrouted by the generic `gpt-*` / single-slash inference
        // fallbacks. Regression for harn#2142 (model-info routed
        // `gpt-oss-120b` to openai, breaking Burin TUI credential checks).
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
        unsafe {
            std::env::remove_var("HARN_DEFAULT_PROVIDER");
        }

        for model in ["gpt-oss-120b", "llama-3.3-70b", "qwen-3-coder-480b"] {
            assert_eq!(
                infer_provider(model),
                "cerebras",
                "{model} should route to its catalog provider"
            );
            let resolved = resolve_model_info(model);
            assert_eq!(resolved.id, model);
            assert_eq!(resolved.provider, "cerebras");
        }

        unsafe {
            match prev_default_provider {
                Some(value) => std::env::set_var("HARN_DEFAULT_PROVIDER", value),
                None => std::env::remove_var("HARN_DEFAULT_PROVIDER"),
            }
        }
    }

    #[test]
    fn test_user_catalog_overlay_re_homes_model_provider() {
        // Users can re-home a built-in model by overlaying a catalog row;
        // the exact-match catalog lookup must honor overlays as well as the
        // embedded TOML.
        reset_overrides();
        let mut overlay = ProvidersConfig::default();
        overlay.models.insert(
            "gpt-4o".to_string(),
            ModelDef {
                name: "GPT-4o via OpenRouter".to_string(),
                provider: "openrouter".to_string(),
                context_window: 128_000,
                runtime_context_window: None,
                stream_timeout: None,
                capabilities: Vec::new(),
                pricing: None,
                deprecated: false,
                deprecation_note: None,
                quality_tags: Vec::new(),
                availability: ModelAvailability::default(),
            },
        );
        set_user_overrides(Some(overlay));

        assert_eq!(infer_provider("gpt-4o"), "openrouter");

        reset_overrides();
    }

    #[test]
    fn test_resolve_model_info_normalizes_provider_prefixes() {
        let local = resolve_model_info("local:gemma-4-e4b-it");
        assert_eq!(local.id, "gemma-4-e4b-it");
        assert_eq!(local.provider, "ollama");

        let ollama = resolve_model_info("ollama:qwen3:30b-a3b");
        assert_eq!(ollama.id, "qwen3:30b-a3b");
        assert_eq!(ollama.provider, "ollama");

        let hf = resolve_model_info("hf:Qwen/Qwen3.6-35B-A3B");
        assert_eq!(hf.id, "Qwen/Qwen3.6-35B-A3B");
        assert_eq!(hf.provider, "huggingface");

        let cerebras = resolve_model_info("cerebras/gpt-oss-120b");
        assert_eq!(cerebras.id, "gpt-oss-120b");
        assert_eq!(cerebras.provider, "cerebras");
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
        assert!(names.contains(&"mlx".to_string()));
        assert!(names.contains(&"openai".to_string()));
        assert!(names.contains(&"ollama".to_string()));
        assert!(names.contains(&"bedrock".to_string()));
        assert!(names.contains(&"azure_openai".to_string()));
        assert!(names.contains(&"vertex".to_string()));
    }

    #[test]
    fn global_provider_file_is_an_overlay_on_builtin_defaults() {
        let mut overlay = ProvidersConfig {
            default_provider: Some("ollama".to_string()),
            ..Default::default()
        };
        overlay.aliases.insert(
            "quickstart".to_string(),
            AliasDef {
                id: "llama3.2".to_string(),
                provider: "ollama".to_string(),
                tool_format: None,
            },
        );

        let merged = merge_global_config(overlay);

        assert_eq!(merged.default_provider.as_deref(), Some("ollama"));
        assert!(merged.providers.contains_key("anthropic"));
        assert!(merged.providers.contains_key("ollama"));
        assert_eq!(merged.aliases["quickstart"].id, "llama3.2");
    }

    #[test]
    fn test_resolve_tier_model_default_aliases() {
        // Exercise the alias-resolution machinery, not the specific catalog
        // value: the model under each tier alias evolves as the embedded
        // providers.toml is updated. The invariants worth pinning are the
        // provider routing + catalog-registration of the resolved model.
        let (model, provider) = resolve_tier_model("frontier", None)
            .expect("frontier alias must resolve from the embedded catalog");
        assert_eq!(provider, "anthropic");
        assert!(
            model_catalog_entry(&model)
                .is_some_and(|entry| entry.provider == "anthropic" && !entry.deprecated),
            "frontier alias must point at a registered, non-deprecated anthropic model (got {model})"
        );

        let (model, provider) = resolve_tier_model("small", None)
            .expect("small alias must resolve from the embedded catalog");
        assert!(
            [
                "openrouter",
                "huggingface",
                "local",
                "llamacpp",
                "mlx",
                "ollama"
            ]
            .contains(&provider.as_str()),
            "small tier should resolve to an open-weight provider (got {provider} / {model})"
        );
    }

    #[test]
    fn test_resolve_tier_model_prefers_provider_scoped_aliases() {
        // tier/<provider> takes precedence over generic tier when the
        // caller scopes by provider. Don't pin the specific model — the
        // catalog evolves.
        let (model, provider) = resolve_tier_model("mid", Some("openai"))
            .expect("mid tier scoped to openai must resolve");
        assert_eq!(provider, "openai");
        assert!(
            model_catalog_entry(&model).is_some(),
            "mid/openai alias must point at a registered model (got {model})"
        );
    }

    #[test]
    fn test_provider_config_anthropic() {
        let pdef = provider_config("anthropic").unwrap();
        assert_eq!(pdef.auth_style, "header");
        assert_eq!(pdef.auth_header.as_deref(), Some("x-api-key"));
    }

    #[test]
    fn test_provider_config_mlx() {
        let pdef = provider_config("mlx").unwrap();
        assert_eq!(pdef.base_url, "http://127.0.0.1:8002");
        assert_eq!(pdef.base_url_env.as_deref(), Some("MLX_BASE_URL"));
        assert_eq!(
            pdef.healthcheck.unwrap().path.as_deref(),
            Some("/v1/models")
        );

        let (model, provider) = resolve_model("mlx-qwen36-27b");
        assert_eq!(model, "unsloth/Qwen3.6-27B-UD-MLX-4bit");
        assert_eq!(provider.as_deref(), Some("mlx"));
    }

    #[test]
    fn test_enterprise_provider_defaults_and_inference() {
        let bedrock = provider_config("bedrock").unwrap();
        assert_eq!(bedrock.auth_style, "aws_sigv4");
        assert_eq!(bedrock.base_url_env.as_deref(), Some("BEDROCK_BASE_URL"));
        assert_eq!(
            infer_provider("anthropic.claude-3-5-sonnet-20240620-v1:0"),
            "bedrock"
        );
        assert_eq!(infer_provider("meta.llama3-70b-instruct-v1:0"), "bedrock");

        let azure = provider_config("azure_openai").unwrap();
        assert_eq!(azure.base_url_env.as_deref(), Some("AZURE_OPENAI_ENDPOINT"));
        assert_eq!(
            auth_env_names(&azure.auth_env),
            vec![
                "AZURE_OPENAI_API_KEY".to_string(),
                "AZURE_OPENAI_AD_TOKEN".to_string(),
                "AZURE_OPENAI_BEARER_TOKEN".to_string(),
            ]
        );

        let vertex = provider_config("vertex").unwrap();
        assert_eq!(vertex.base_url, "https://aiplatform.googleapis.com/v1");
        assert_eq!(infer_provider("gemini-1.5-pro-002"), "gemini");
    }

    #[test]
    fn test_default_provider_env_override_for_unknown_model() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
        unsafe {
            std::env::set_var("HARN_DEFAULT_PROVIDER", "openai");
        }

        let inference = infer_provider_detail("unknown-model");

        unsafe {
            match prev_default_provider {
                Some(value) => std::env::set_var("HARN_DEFAULT_PROVIDER", value),
                None => std::env::remove_var("HARN_DEFAULT_PROVIDER"),
            }
        }

        assert_eq!(inference.provider, "openai");
        assert_eq!(
            inference.source,
            crate::llm::provider::ProviderInferenceSource::DefaultFallback
        );
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
    fn test_local_ollama_catalog_metadata() {
        reset_overrides();

        let qwen_coding = model_catalog_entry("qwen3.6:35b-a3b-coding-nvfp4")
            .expect("qwen3.6 coding catalog entry");
        assert_eq!(qwen_coding.context_window, 262_144);
        assert!(!qwen_coding.capabilities.iter().any(|cap| cap == "vision"));

        let gemma4 = model_catalog_entry("gemma4:26b").expect("gemma4 catalog entry");
        assert_eq!(gemma4.context_window, 262_144);
        assert!(gemma4.capabilities.iter().any(|cap| cap == "vision"));
    }

    #[test]
    fn test_external_config_overlays_default_catalog() {
        let mut config = default_config();
        let mut overlay = ProvidersConfig {
            default_provider: Some("ollama".to_string()),
            ..Default::default()
        };
        overlay.providers.insert(
            "custom".to_string(),
            ProviderDef {
                base_url: "https://llm.example.test/v1".to_string(),
                chat_endpoint: "/chat/completions".to_string(),
                ..Default::default()
            },
        );

        config.merge_from(&overlay);

        assert_eq!(config.default_provider.as_deref(), Some("ollama"));
        assert!(config.providers.contains_key("custom"));
        assert!(config.providers.contains_key("anthropic"));
        assert!(config.providers.contains_key("ollama"));
    }

    #[test]
    fn test_model_params_empty() {
        let params = model_params("claude-sonnet-4-20250514");
        assert!(params.is_empty());
    }

    #[test]
    fn test_user_overrides_add_provider_and_alias() {
        reset_overrides();
        let mut overlay = ProvidersConfig::default();
        overlay.providers.insert(
            "acme".to_string(),
            ProviderDef {
                base_url: "https://llm.acme.test/v1".to_string(),
                chat_endpoint: "/chat/completions".to_string(),
                ..Default::default()
            },
        );
        overlay.aliases.insert(
            "acme-fast".to_string(),
            AliasDef {
                id: "acme/model-fast".to_string(),
                provider: "acme".to_string(),
                tool_format: Some("native".to_string()),
            },
        );
        set_user_overrides(Some(overlay));

        let (model, provider) = resolve_model("acme-fast");
        assert_eq!(model, "acme/model-fast");
        assert_eq!(provider.as_deref(), Some("acme"));
        assert!(provider_names().contains(&"acme".to_string()));
        assert_eq!(
            provider_config("acme").map(|provider| provider.base_url),
            Some("https://llm.acme.test/v1".to_string())
        );

        reset_overrides();
    }

    #[test]
    fn test_default_tool_format_uses_capability_matrix() {
        reset_overrides();

        assert_eq!(
            default_tool_format("qwen3.6-35b-a3b-ud-q4-k-xl", "llamacpp"),
            "text"
        );
        assert_eq!(
            default_tool_format("devstral-small-2:24b", "ollama"),
            "text"
        );
        assert_eq!(
            default_tool_format("ollama-devstral-small-2-native", "ollama"),
            "native"
        );
        assert_eq!(default_tool_format("gemma-4-26b-a4b-it", "local"), "text");
        assert_eq!(
            default_tool_format("deepseek/deepseek-v3.2", "openrouter"),
            "text"
        );
        assert_eq!(
            default_tool_format("qwen/qwen3-coder-flash", "openrouter"),
            "text"
        );
    }

    #[test]
    fn test_user_overrides_add_model_catalog_pricing_and_qc_defaults() {
        reset_overrides();
        let mut overlay = ProvidersConfig::default();
        overlay.models.insert(
            "acme/model-fast".to_string(),
            ModelDef {
                name: "Acme Fast".to_string(),
                provider: "acme".to_string(),
                context_window: 65_536,
                runtime_context_window: None,
                stream_timeout: Some(42.0),
                capabilities: vec!["tools".to_string(), "streaming".to_string()],
                pricing: Some(ModelPricing {
                    input_per_mtok: 1.25,
                    output_per_mtok: 2.5,
                    cache_read_per_mtok: Some(0.25),
                    cache_write_per_mtok: None,
                }),
                deprecated: false,
                deprecation_note: None,
                quality_tags: Vec::new(),
                availability: ModelAvailability::default(),
            },
        );
        overlay
            .qc_defaults
            .insert("acme".to_string(), "acme/model-cheap".to_string());
        set_user_overrides(Some(overlay));

        let entry = model_catalog_entry("acme/model-fast").expect("catalog entry");
        assert_eq!(entry.context_window, 65_536);
        assert_eq!(
            entry.capabilities,
            vec!["streaming".to_string(), "tools".to_string()]
        );
        assert_eq!(
            entry.pricing.as_ref().map(|pricing| pricing.input_per_mtok),
            Some(1.25)
        );
        assert_eq!(
            pricing_per_1k_for("acme", "acme/model-fast"),
            Some((0.00125, 0.0025))
        );
        assert_eq!(
            qc_default_model("acme").as_deref(),
            Some("acme/model-cheap")
        );

        reset_overrides();
    }

    #[test]
    fn test_user_overrides_prepend_inference_rules() {
        reset_overrides();
        let mut overlay = ProvidersConfig::default();
        overlay.inference_rules.push(InferenceRule {
            pattern: Some("internal-*".to_string()),
            contains: None,
            exact: None,
            provider: "openai".to_string(),
        });
        set_user_overrides(Some(overlay));

        assert_eq!(infer_provider("internal-foo"), "openai");

        reset_overrides();
    }

    // ── Embedded providers.toml invariants ───────────────────────────────────
    // These tests pin properties of the *system* — TOML parses, every
    // alias resolves, every deprecated model has a note — without
    // pinning specific catalog values. They survive future catalog
    // churn and surface real schema breakage.

    #[test]
    fn embedded_providers_toml_parses_and_is_not_trivially_empty() {
        let config = default_config();
        assert!(
            config.providers.len() >= 10,
            "expected >=10 providers in embedded catalog, got {}",
            config.providers.len()
        );
        assert!(
            config.models.len() >= 20,
            "expected >=20 models in embedded catalog, got {}",
            config.models.len()
        );
        assert!(
            config.aliases.len() >= 15,
            "expected >=15 aliases in embedded catalog, got {}",
            config.aliases.len()
        );
        assert_eq!(config.default_provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn embedded_catalog_every_deprecated_model_has_a_note() {
        let config = default_config();
        let offenders: Vec<&str> = config
            .models
            .iter()
            .filter(|(_, model)| {
                model.deprecated
                    && model
                        .deprecation_note
                        .as_deref()
                        .unwrap_or("")
                        .trim()
                        .is_empty()
            })
            .map(|(id, _)| id.as_str())
            .collect();
        assert!(
            offenders.is_empty(),
            "deprecated models missing a deprecation_note: {offenders:?}"
        );
    }

    #[test]
    fn embedded_catalog_every_model_targets_a_registered_provider() {
        let config = default_config();
        let known: std::collections::BTreeSet<&str> =
            config.providers.keys().map(String::as_str).collect();
        let orphans: Vec<(&str, &str)> = config
            .models
            .iter()
            .filter(|(_, model)| !known.contains(model.provider.as_str()))
            .map(|(id, model)| (id.as_str(), model.provider.as_str()))
            .collect();
        assert!(
            orphans.is_empty(),
            "models reference unknown providers: {orphans:?}"
        );
    }

    #[test]
    fn embedded_catalog_every_alias_targets_a_registered_provider() {
        let config = default_config();
        let known: std::collections::BTreeSet<&str> =
            config.providers.keys().map(String::as_str).collect();
        let orphans: Vec<(&str, &str)> = config
            .aliases
            .iter()
            .filter(|(_, alias)| !known.contains(alias.provider.as_str()))
            .map(|(name, alias)| (name.as_str(), alias.provider.as_str()))
            .collect();
        assert!(
            orphans.is_empty(),
            "aliases reference unknown providers: {orphans:?}"
        );
    }

    #[test]
    fn embedded_catalog_every_qc_default_targets_a_known_model() {
        let config = default_config();
        let orphans: Vec<(&str, &str)> = config
            .qc_defaults
            .iter()
            .filter(|(_, model_id)| !config.models.contains_key(model_id.as_str()))
            .map(|(provider, model_id)| (provider.as_str(), model_id.as_str()))
            .collect();
        assert!(
            orphans.is_empty(),
            "qc_defaults reference unknown models: {orphans:?}"
        );
    }

    #[test]
    fn embedded_catalog_pricing_rates_are_non_negative() {
        let config = default_config();
        for (id, model) in &config.models {
            let Some(pricing) = &model.pricing else {
                continue;
            };
            assert!(
                pricing.input_per_mtok >= 0.0 && pricing.output_per_mtok >= 0.0,
                "{id}: negative pricing — in={} out={}",
                pricing.input_per_mtok,
                pricing.output_per_mtok
            );
            if let Some(rate) = pricing.cache_read_per_mtok {
                assert!(rate >= 0.0, "{id}: negative cache_read rate {rate}");
            }
            if let Some(rate) = pricing.cache_write_per_mtok {
                assert!(rate >= 0.0, "{id}: negative cache_write rate {rate}");
            }
        }
    }

    #[test]
    fn model_availability_parses_known_strings() {
        assert_eq!(
            ModelAvailability::parse("serverless"),
            Some(ModelAvailability::Serverless)
        );
        assert_eq!(
            ModelAvailability::parse("dedicated"),
            Some(ModelAvailability::Dedicated)
        );
        assert_eq!(
            ModelAvailability::parse("unknown"),
            Some(ModelAvailability::Unknown)
        );
        assert_eq!(ModelAvailability::parse("provisioned"), None);
        for value in [
            ModelAvailability::Serverless,
            ModelAvailability::Dedicated,
            ModelAvailability::Unknown,
        ] {
            assert_eq!(ModelAvailability::parse(value.as_str()), Some(value));
        }
    }

    #[test]
    fn embedded_catalog_marks_together_dedicated_route_as_dedicated() {
        let config = default_config();
        let model = config
            .models
            .get("Qwen/Qwen3-Coder-Next-FP8")
            .expect("Together Qwen3 Coder Next FP8 is cataloged");
        assert_eq!(model.provider, "together");
        assert_eq!(model.availability, ModelAvailability::Dedicated);
    }

    #[test]
    fn embedded_catalog_dedicated_models_are_not_targeted_by_tier_aliases() {
        // A dedicated-only model behind a tier alias would silently fail
        // every serverless caller; the catalog must keep those routes
        // separated.
        let config = default_config();
        let dedicated: std::collections::BTreeSet<(&str, &str)> = config
            .models
            .iter()
            .filter(|(_, model)| model.availability == ModelAvailability::Dedicated)
            .map(|(id, model)| (model.provider.as_str(), id.as_str()))
            .collect();
        for (name, alias) in &config.aliases {
            if matches!(
                name.as_str(),
                "frontier"
                    | "mid"
                    | "small"
                    | "tier/frontier"
                    | "tier/mid"
                    | "tier/small"
                    | "sonnet"
                    | "opus"
                    | "haiku"
            ) {
                assert!(
                    !dedicated.contains(&(alias.provider.as_str(), alias.id.as_str())),
                    "tier alias `{name}` targets dedicated-only route `{}/{}`",
                    alias.provider,
                    alias.id,
                );
            }
        }
    }

    #[test]
    fn embedded_catalog_tier_aliases_resolve_to_active_models() {
        // The three canonical tier aliases (frontier / mid / small) MUST
        // resolve to non-deprecated catalog entries; a default that
        // routes the loop into a sunsetted model is a release blocker.
        for alias in ["frontier", "mid", "small"] {
            let (model, _provider) = resolve_tier_model(alias, None)
                .unwrap_or_else(|| panic!("tier alias `{alias}` must resolve"));
            let entry = model_catalog_entry(&model).unwrap_or_else(|| {
                panic!("tier alias `{alias}` -> `{model}` must be a registered catalog entry")
            });
            assert!(
                !entry.deprecated,
                "tier alias `{alias}` resolves to deprecated model `{model}` ({:?})",
                entry.deprecation_note
            );
        }
    }
}
