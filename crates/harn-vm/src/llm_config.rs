use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{OnceLock, RwLock};

static CONFIG: OnceLock<ProvidersConfig> = OnceLock::new();
static CONFIG_PATH: OnceLock<String> = OnceLock::new();
static RUNTIME_CATALOG_OVERLAY: OnceLock<RwLock<Option<ProvidersConfig>>> = OnceLock::new();

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
    #[serde(default)]
    pub model_roles: BTreeMap<String, BTreeMap<String, toml::Value>>,
    #[serde(default)]
    pub suppress: SuppressDef,
}

/// Routes hidden from the exported/served provider catalog artifact.
///
/// Lets an overlay drop baseline routes that are broken or unusable for the
/// embedding product (e.g. a dedicated-only serving route, or a local image
/// with a broken server-side tool parser) without forking the baseline
/// catalog. Suppression is artifact-level presentation: it removes the model
/// row, its aliases, and any recommendation variant derived from it, but does
/// not block runtime resolution of an explicitly requested model id.
///
/// Combined with the overlay's whole-row `models` replacement, this also
/// expresses route renames: define the row under the new id and suppress the
/// old one.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct SuppressDef {
    /// `"provider:model_id"` selectors. Split on the FIRST colon only —
    /// model ids may themselves contain colons (e.g. Ollama image tags such
    /// as `ollama:qwen3.6:35b-a3b-coding-nvfp4`). Entries without a colon
    /// match nothing.
    #[serde(default)]
    pub routes: Vec<String>,
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
            && self.model_roles.is_empty()
            && self.suppress.routes.is_empty()
            && self.tier_defaults.default == default_mid()
    }

    pub fn merge_from(&mut self, overlay: &ProvidersConfig) {
        for (name, provider) in &overlay.providers {
            match self.providers.get_mut(name) {
                Some(existing) => existing.merge_from(provider),
                None => {
                    self.providers.insert(name.clone(), provider.clone());
                }
            }
        }
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

        for (role, defaults) in &overlay.model_roles {
            self.model_roles
                .entry(role.clone())
                .or_default()
                .extend(defaults.clone());
        }

        for route in &overlay.suppress.routes {
            if !self.suppress.routes.contains(route) {
                self.suppress.routes.push(route.clone());
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderDef {
    pub display_name: Option<String>,
    pub icon: Option<String>,
    /// Provider protocol. Omitted providers use Harn's normal HTTP provider
    /// path; `acp` launches an Agent Client Protocol server and drives it as
    /// an agent-backed provider.
    pub protocol: Option<String>,
    pub base_url: String,
    pub base_url_env: Option<String>,
    pub auth_style: String,
    pub auth_header: Option<String>,
    pub auth_env: AuthEnv,
    pub extra_headers: BTreeMap<String, String>,
    pub chat_endpoint: String,
    pub completion_endpoint: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub mcp_servers: Vec<serde_json::Value>,
    pub healthcheck: Option<HealthcheckDef>,
    /// Local runtime lifecycle metadata used by `harn local launch/stop`.
    /// This is intentionally separate from provider process fields such as
    /// `command`/`args`, which are used for ACP or external provider adapters.
    pub local_runtime: Option<LocalRuntimeDef>,
    pub features: Vec<String>,
    /// Fallback provider name to try if this provider fails.
    pub fallback: Option<String>,
    /// Number of retries before falling back (default 0).
    pub retry_count: Option<u32>,
    /// Delay between retries in milliseconds (default 1000).
    pub retry_delay_ms: Option<u64>,
    /// Maximum requests per minute. None = unlimited.
    pub rpm: Option<u32>,
    /// Rich provider quota metadata. `rpm` remains as a legacy shorthand;
    /// when both are present, this nested shape is the authoritative catalog
    /// record and callers can still read the flattened `rpm`.
    pub rate_limits: Option<RateLimitsDef>,
    /// Provider/catalog pricing in USD per 1k input tokens.
    pub cost_per_1k_in: Option<f64>,
    /// Provider/catalog pricing in USD per 1k output tokens.
    pub cost_per_1k_out: Option<f64>,
    /// Observed or configured p50 latency in milliseconds.
    pub latency_p50_ms: Option<u64>,
    #[doc(hidden)]
    pub auth_style_explicit: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ProviderDefWire {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    base_url_env: Option<String>,
    #[serde(default)]
    auth_style: Option<String>,
    #[serde(default)]
    auth_header: Option<String>,
    #[serde(default)]
    auth_env: AuthEnv,
    #[serde(default)]
    extra_headers: BTreeMap<String, String>,
    #[serde(default)]
    chat_endpoint: String,
    #[serde(default)]
    completion_endpoint: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    mcp_servers: Vec<serde_json::Value>,
    #[serde(default)]
    healthcheck: Option<HealthcheckDef>,
    #[serde(default)]
    local_runtime: Option<LocalRuntimeDef>,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    fallback: Option<String>,
    #[serde(default)]
    retry_count: Option<u32>,
    #[serde(default)]
    retry_delay_ms: Option<u64>,
    #[serde(default)]
    rpm: Option<u32>,
    #[serde(default)]
    rate_limits: Option<RateLimitsDef>,
    #[serde(default)]
    cost_per_1k_in: Option<f64>,
    #[serde(default)]
    cost_per_1k_out: Option<f64>,
    #[serde(default)]
    latency_p50_ms: Option<u64>,
}

impl<'de> Deserialize<'de> for ProviderDef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ProviderDefWire::deserialize(deserializer)?;
        let auth_style_explicit = wire.auth_style.is_some();
        Ok(Self {
            display_name: wire.display_name,
            icon: wire.icon,
            protocol: wire.protocol,
            base_url: wire.base_url,
            base_url_env: wire.base_url_env,
            auth_style: wire.auth_style.unwrap_or_else(default_bearer),
            auth_header: wire.auth_header,
            auth_env: wire.auth_env,
            extra_headers: wire.extra_headers,
            chat_endpoint: wire.chat_endpoint,
            completion_endpoint: wire.completion_endpoint,
            command: wire.command,
            args: wire.args,
            env: wire.env,
            cwd: wire.cwd,
            mcp_servers: wire.mcp_servers,
            healthcheck: wire.healthcheck,
            local_runtime: wire.local_runtime,
            features: wire.features,
            fallback: wire.fallback,
            retry_count: wire.retry_count,
            retry_delay_ms: wire.retry_delay_ms,
            rpm: wire.rpm,
            rate_limits: wire.rate_limits,
            cost_per_1k_in: wire.cost_per_1k_in,
            cost_per_1k_out: wire.cost_per_1k_out,
            latency_p50_ms: wire.latency_p50_ms,
            auth_style_explicit,
        })
    }
}

impl Default for ProviderDef {
    fn default() -> Self {
        Self {
            display_name: None,
            icon: None,
            protocol: None,
            base_url: String::new(),
            base_url_env: None,
            auth_style: default_bearer(),
            auth_header: None,
            auth_env: AuthEnv::None,
            extra_headers: BTreeMap::new(),
            chat_endpoint: String::new(),
            completion_endpoint: None,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            mcp_servers: Vec::new(),
            healthcheck: None,
            local_runtime: None,
            features: Vec::new(),
            fallback: None,
            retry_count: None,
            retry_delay_ms: None,
            rpm: None,
            rate_limits: None,
            cost_per_1k_in: None,
            cost_per_1k_out: None,
            latency_p50_ms: None,
            auth_style_explicit: false,
        }
    }
}

impl ProviderDef {
    fn merge_from(&mut self, overlay: &ProviderDef) {
        merge_option(&mut self.display_name, &overlay.display_name);
        merge_option(&mut self.icon, &overlay.icon);
        merge_option(&mut self.protocol, &overlay.protocol);
        merge_string(&mut self.base_url, &overlay.base_url);
        merge_option(&mut self.base_url_env, &overlay.base_url_env);
        let overlay_uses_default_auth_style = overlay.auth_style == default_bearer();
        if overlay.auth_style_explicit
            || !overlay_uses_default_auth_style
            || self.auth_style == default_bearer()
        {
            self.auth_style = overlay.auth_style.clone();
            self.auth_style_explicit |=
                overlay.auth_style_explicit || !overlay_uses_default_auth_style;
        }
        merge_option(&mut self.auth_header, &overlay.auth_header);
        if !overlay.auth_env.is_none() {
            self.auth_env = overlay.auth_env.clone();
        }
        self.extra_headers.extend(overlay.extra_headers.clone());
        merge_string(&mut self.chat_endpoint, &overlay.chat_endpoint);
        merge_option(&mut self.completion_endpoint, &overlay.completion_endpoint);
        merge_option(&mut self.command, &overlay.command);
        merge_vec(&mut self.args, &overlay.args);
        self.env.extend(overlay.env.clone());
        merge_option(&mut self.cwd, &overlay.cwd);
        merge_vec(&mut self.mcp_servers, &overlay.mcp_servers);
        merge_option(&mut self.healthcheck, &overlay.healthcheck);
        merge_option(&mut self.local_runtime, &overlay.local_runtime);
        merge_vec(&mut self.features, &overlay.features);
        merge_option(&mut self.fallback, &overlay.fallback);
        merge_option(&mut self.retry_count, &overlay.retry_count);
        merge_option(&mut self.retry_delay_ms, &overlay.retry_delay_ms);
        merge_option(&mut self.rpm, &overlay.rpm);
        merge_option(&mut self.rate_limits, &overlay.rate_limits);
        merge_option(&mut self.cost_per_1k_in, &overlay.cost_per_1k_in);
        merge_option(&mut self.cost_per_1k_out, &overlay.cost_per_1k_out);
        merge_option(&mut self.latency_p50_ms, &overlay.latency_p50_ms);
    }
}

fn merge_option<T: Clone>(base: &mut Option<T>, overlay: &Option<T>) {
    if overlay.is_some() {
        *base = overlay.clone();
    }
}

fn merge_string(base: &mut String, overlay: &str) {
    if !overlay.is_empty() {
        *base = overlay.to_string();
    }
}

fn merge_vec<T: Clone>(base: &mut Vec<T>, overlay: &[T]) {
    if !overlay.is_empty() {
        *base = overlay.to_vec();
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

impl AuthEnv {
    fn is_none(&self) -> bool {
        matches!(self, AuthEnv::None)
    }
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

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct LocalRuntimeDef {
    /// Lifecycle style: `daemon_api` for runtimes with their own resident
    /// daemon (Ollama), `managed_process` for Harn-spawned servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Command Harn should execute for managed-process runtimes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Default model source/path/repo. User overlays may set this; embedded
    /// catalog rows avoid machine-specific absolute paths except examples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_source: Option<String>,
    /// Environment variable that can provide a model source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_source_env: Option<String>,
    /// Default port when the provider base URL has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_port: Option<u16>,
    /// Argument names used by the runtime CLI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_model_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_layers_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_type_k_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_type_v_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_ram_arg: Option<String>,
    /// Extra arguments Harn applies by default when launching this runtime.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_args: Vec<String>,
    /// Stop strategy: `keep_alive_zero`, `pid`, or `external`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<String>,
    /// Official docs/source URL for the lifecycle contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// YYYY-MM-DD date when the local runtime row was last verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<String>,
    /// Short operational note surfaced by CLI docs/help.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct LocalMemoryDef {
    /// Empirical resident memory observed for this route/runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_resident_gib: Option<f64>,
    /// Context size used for the empirical measurement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_context_window: Option<u64>,
    /// KV-cache type used for the empirical measurement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_cache_type: Option<String>,
    /// Approximate non-context resident footprint for this model/runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_resident_gib: Option<f64>,
    /// Approximate GiB consumed by KV cache per 1,000 context tokens at the
    /// default cache type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kv_cache_gib_per_1k_ctx: Option<f64>,
    /// Cache-type multiplier relative to `kv_cache_gib_per_1k_ctx`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cache_type_multipliers: BTreeMap<String, f64>,
    /// Cache type assumed when the launch command does not set K/V cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cache_type: Option<String>,
    /// Minimum headroom Harn should leave for the OS and other apps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_margin_gib: Option<f64>,
    /// Highest context Harn should recommend automatically from this row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_recommended_context: Option<u64>,
    /// Official or empirical source for the sizing row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// YYYY-MM-DD date when the sizing row was last verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<String>,
    /// Short operational note surfaced by CLI diagnostics/docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl LocalMemoryDef {
    pub fn is_empty(&self) -> bool {
        self.measured_resident_gib.is_none()
            && self.measured_context_window.is_none()
            && self.measured_cache_type.is_none()
            && self.base_resident_gib.is_none()
            && self.kv_cache_gib_per_1k_ctx.is_none()
            && self.cache_type_multipliers.is_empty()
            && self.default_cache_type.is_none()
            && self.safety_margin_gib.is_none()
            && self.max_recommended_context.is_none()
            && self.source_url.is_none()
            && self.last_verified.is_none()
            && self.notes.is_none()
    }
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

/// Provider or model quota metadata. Providers publish these along several
/// axes, and any one exhausted bucket can trigger throttling.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct RateLimitsDef {
    /// Requests per minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u32>,
    /// Requests per hour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rph: Option<u32>,
    /// Requests per day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpd: Option<u32>,
    /// Total tokens per minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpm: Option<u64>,
    /// Total tokens per hour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tph: Option<u64>,
    /// Total tokens per day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpd: Option<u64>,
    /// Input tokens per minute, when the provider splits input/output quotas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tpm: Option<u64>,
    /// Output tokens per minute, when the provider splits input/output quotas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tpm: Option<u64>,
    /// Concurrent in-flight requests, if published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<u32>,
    /// Account tier or route class these limits describe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Official source URL for the row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// YYYY-MM-DD date when the row was last verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<String>,
    /// Free-text caveat for account-dependent or burst limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl RateLimitsDef {
    pub fn is_empty(&self) -> bool {
        self.rpm.is_none()
            && self.rph.is_none()
            && self.rpd.is_none()
            && self.tpm.is_none()
            && self.tph.is_none()
            && self.tpd.is_none()
            && self.input_tpm.is_none()
            && self.output_tpm.is_none()
            && self.concurrency.is_none()
            && self.tier.is_none()
            && self.source_url.is_none()
            && self.last_verified.is_none()
            && self.notes.is_none()
    }

    pub fn with_rpm_fallback(mut self, rpm: Option<u32>) -> Option<Self> {
        if self.rpm.is_none() {
            self.rpm = rpm;
        }
        (!self.is_empty()).then_some(self)
    }
}

/// Logical-model facts separated from provider serving routes. These fields
/// describe the underlying weights or public model family, not Harn's alias or
/// provider/model selector.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ModelArchitectureDef {
    /// Total parameter count in billions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter_count_b: Option<f64>,
    /// Active parameter count in billions for MoE models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_parameter_count_b: Option<f64>,
    /// True for mixture-of-experts models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moe: Option<bool>,
    /// Quantization advertised by this route, if route-specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    /// Numeric precision advertised by this route, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision: Option<String>,
    /// License identifier or short label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Tokenizer family or implementation hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokenizer: Option<String>,
    /// Public knowledge cutoff claim, when published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_cutoff: Option<String>,
    /// Official source URL for these facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// YYYY-MM-DD date when these facts were last verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<String>,
}

impl ModelArchitectureDef {
    pub fn is_empty(&self) -> bool {
        self.parameter_count_b.is_none()
            && self.active_parameter_count_b.is_none()
            && self.moe.is_none()
            && self.quantization.is_none()
            && self.precision.is_none()
            && self.license.is_none()
            && self.tokenizer.is_none()
            && self.knowledge_cutoff.is_none()
            && self.source_url.is_none()
            && self.last_verified.is_none()
    }
}

/// Optional accelerated-serving ("fast mode") tier for a model. Off by
/// default: its presence only *describes* that the provider offers a
/// faster, premium-priced serving path running the same weights — callers
/// must explicitly opt in via the provider's request knob, so nothing here
/// changes default behavior. Deliberately provider-agnostic: Anthropic
/// exposes the tier as `speed = "fast"` (beta-gated), while OpenAI uses
/// `service_tier = "fast"` / `"priority"`. Premium pricing is stored as
/// absolute per-MTok rates rather than a single multiplier because
/// providers price the tier asymmetrically (Anthropic Opus 4.8 is 2x
/// standard; Opus 4.6/4.7 fast mode is 6x).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct FastModeDef {
    /// Request field that opts into the fast tier (e.g. "speed" for
    /// Anthropic, "service_tier" for OpenAI).
    pub param: String,
    /// Value to send on `param` (e.g. "fast", "priority").
    pub value: String,
    /// Provider beta/feature header required to use the tier, if any
    /// (e.g. Anthropic "fast-mode-2026-02-01").
    #[serde(default)]
    pub beta_header: Option<String>,
    /// Output-tokens-per-second speedup vs standard serving (e.g. 2.5).
    #[serde(default)]
    pub otps_speedup: Option<f64>,
    /// Lifecycle of the fast tier: "ga" | "research_preview" |
    /// "deprecated". None when unspecified.
    #[serde(default)]
    pub status: Option<String>,
    /// Premium pricing charged while the fast tier is active (absolute
    /// per-MTok rates, not a multiplier on standard pricing).
    #[serde(default)]
    pub pricing: Option<ModelPricing>,
    /// Free-text note: constraints, deprecation timeline, etc.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ModelDef {
    pub name: String,
    pub provider: String,
    pub context_window: u64,
    /// Provider-independent logical model id, when multiple serving routes map
    /// to the same weights or model family.
    #[serde(default)]
    pub logical_model: Option<String>,
    /// Equivalence class for failover/escalation candidates. Entries in the
    /// same group are capability-compatible alternatives, not byte-identical
    /// APIs; callers must still re-render transcripts for the target provider.
    #[serde(default)]
    pub equivalence_group: Option<String>,
    /// Serving-route detail such as "serverless", "priority", "fp8", or a
    /// provider route slug. This is intentionally separate from `name`.
    #[serde(default)]
    pub served_variant: Option<String>,
    /// Provider-native model id to send on the wire. Defaults to the catalog
    /// key. Required when two providers expose the same native id and Harn
    /// needs a unique catalog key for each route.
    #[serde(default)]
    pub wire_model: Option<String>,
    /// Preferred API dialect for the route, e.g. `openai_chat`,
    /// `openai_responses`, `anthropic_messages`, `gemini_generate_content`.
    #[serde(default)]
    pub api_dialect: Option<String>,
    /// Route-specific token/request quota metadata.
    #[serde(default)]
    pub rate_limits: Option<RateLimitsDef>,
    /// Underlying model architecture facts separated from the provider id.
    #[serde(default)]
    pub architecture: Option<ModelArchitectureDef>,
    /// Local launch memory-sizing hints used by `harn local launch`.
    #[serde(default)]
    pub local_memory: Option<LocalMemoryDef>,
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
    /// Structured replacement pointer: the catalog id of the model that
    /// supersedes this one (e.g. an older Opus row points at the newest
    /// Opus). Lets release tooling express "migrate to X" in a
    /// machine-readable way instead of burying it in `deprecation_note`
    /// free text. A model may be superseded without being `deprecated`
    /// (a newer option exists but this one is still fully supported);
    /// pair it with `deprecated = true` once a sunset is announced.
    #[serde(default)]
    pub superseded_by: Option<String>,
    /// Accelerated-serving ("fast mode") tier metadata, when the model's
    /// provider offers one. Off by default — see [`FastModeDef`]. None for
    /// models with no faster serving path.
    #[serde(default)]
    pub fast_mode: Option<FastModeDef>,
    #[serde(default)]
    pub quality_tags: Vec<String>,
    /// Whether the model can be reached over a normal API-key serverless call,
    /// or only via a dedicated/provisioned endpoint that the caller must spin
    /// up out-of-band. Providers like Together list dedicated-only routes
    /// alongside serverless ones in `/v1/models`, so this metadata lets clients
    /// avoid presenting them as one-click options.
    #[serde(default)]
    pub availability: ModelAvailability,
    /// Popular-consensus tier label. Enum-typed string: "small" | "mid" |
    /// "frontier" | "reasoning". Self-declared per model (no pattern-matched
    /// rule table) so the catalog is the single source of truth. When None
    /// the resolver returns the catalog default ("mid"). Use the richer
    /// `strengths` + `benchmarks` fields to pick models for specific
    /// workloads — `tier` exists only as a coarse popular-consensus shortcut.
    #[serde(default)]
    pub tier: Option<String>,
    /// True when the model weights are downloadable / self-hostable
    /// (open-weight / open-source license, regardless of commercial-use
    /// restrictions). False when weights are closed (Anthropic, OpenAI,
    /// Google, etc.). None when the catalog row predates the migration.
    #[serde(default)]
    pub open_weight: Option<bool>,
    /// Workload-shaped strength tags. Conventional values include
    /// `coding`, `summarization`, `long_context`, `tool_use`, `reasoning`,
    /// `vision`, `speed`, `cheap`, `agentic`. Selectors should treat
    /// missing entries as "no claim" rather than "no strength."
    #[serde(default)]
    pub strengths: Vec<String>,
    /// Public benchmark numbers, keyed by a snake_case identifier
    /// (`swe_bench_verified`, `humaneval`, `aa_intelligence_index`, etc.).
    /// Values are the raw published scores. The selector layer is free
    /// to normalize per benchmark; the catalog records the canonical
    /// score so future readers can audit the source.
    #[serde(default)]
    pub benchmarks: BTreeMap<String, f64>,
    /// Normalized model-family token used as a diversity signal for
    /// reviewer selection. Distinct from provider: hosted wrappers should
    /// keep the underlying family (for example OpenRouter-hosted Claude
    /// still uses `anthropic-claude`).
    #[serde(default)]
    pub family: Option<String>,
    /// Narrower family lineage used by option-pack calibration.
    #[serde(default)]
    pub lineage: Option<String>,
    /// Preferred reviewer families for critique/review workloads.
    #[serde(default)]
    pub complementary_with: Vec<String>,
    /// Author families, lineages, model ids, or provider/model selectors
    /// this row should not review.
    #[serde(default)]
    pub avoid_as_reviewer_for: Vec<String>,
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
    pub family: String,
    pub lineage: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComplementaryReviewerOptions {
    pub author_model: String,
    pub author_provider: Option<String>,
    pub intent: ComplementaryReviewerIntent,
    pub max_price_multiplier: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComplementaryReviewerIntent {
    Review,
    Critique,
    PlanReview,
}

impl ComplementaryReviewerIntent {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "review" => Some(Self::Review),
            "critique" => Some(Self::Critique),
            "plan_review" => Some(Self::PlanReview),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Critique => "critique",
            Self::PlanReview => "plan_review",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ComplementaryReviewerSelection {
    pub intent: String,
    pub author: ComplementaryModelIdentity,
    pub reviewer: ComplementaryModelIdentity,
    pub fallback: bool,
    pub fallback_reason: Option<String>,
    /// Machine-readable reason a caller can branch on when `fallback` is
    /// `true`, distinct from the human-readable `fallback_reason`/`reason`
    /// prose. `None` on the success path. Lets a caller hard-fail an
    /// independent-review step rather than silently degrade to self-review.
    /// See [`ReviewerFallbackCode`] for the stable set of values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_code: Option<String>,
    pub reason: String,
    pub estimated_incremental_cost: Option<ComplementaryCostEstimate>,
}

/// Stable, machine-readable reasons `pick_complementary_reviewer` falls back
/// to the author model. Serialized as the `fallback_code` string so harn
/// pipelines and Rust callers can branch deterministically instead of parsing
/// prose. New variants are additive; existing codes are append-only contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewerFallbackCode {
    /// The author model's family could not be resolved, so no independent
    /// family comparison is possible.
    UnknownAuthorFamily,
    /// Different-family candidates exist but none satisfy `max_price_multiplier`.
    NoDiffFamilyWithinPrice,
    /// No active, serverless, different-family reviewer is cataloged at all.
    NoDiffFamilyServerless,
    /// Different-family candidates exist but were all excluded (e.g. every
    /// one declares `avoid_as_reviewer_for` the author).
    AllDiffFamilyExcluded,
}

impl ReviewerFallbackCode {
    pub fn as_code(self) -> &'static str {
        match self {
            Self::UnknownAuthorFamily => "unknown_author_family",
            Self::NoDiffFamilyWithinPrice => "no_diff_family_within_price",
            Self::NoDiffFamilyServerless => "no_diff_family_serverless",
            Self::AllDiffFamilyExcluded => "all_diff_family_excluded",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ComplementaryModelIdentity {
    pub id: String,
    pub provider: String,
    pub family: String,
    pub lineage: String,
    pub tier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ComplementaryCostEstimate {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub total_per_mtok: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiplier_vs_author: Option<f64>,
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
        if should_load_home_config() {
            if let Some(home) = dirs_or_home() {
                let path = format!("{home}/.config/harn/providers.toml");
                if let Some(overlay) = read_external_config(&path, false) {
                    config.merge_from(&overlay);
                    let _ = CONFIG_PATH.set(path);
                    return config;
                }
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
                eprintln!("[llm_config] TOML parse error in {path}: {error}");
                None
            }
        },
        Err(error) => {
            if verbose {
                eprintln!("[llm_config] Cannot read {path}: {error}");
            }
            None
        }
    }
}

fn should_load_home_config() -> bool {
    // Unit tests should cover embedded defaults plus explicit overlays, not
    // whichever provider file happens to exist on the developer machine.
    !cfg!(test)
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

/// Install the process-wide runtime catalog overlay used by
/// `provider_catalog::refresh_runtime_catalog`. Per-run user overlays still
/// merge last so project-local provider config can override hosted catalog
/// updates.
pub fn set_runtime_catalog_overlay(config: Option<ProvidersConfig>) {
    *runtime_catalog_overlay()
        .write()
        .expect("runtime catalog overlay poisoned") = config;
}

pub fn clear_runtime_catalog_overlay() {
    set_runtime_catalog_overlay(None);
}

pub(crate) fn effective_config() -> ProvidersConfig {
    let user_overrides = USER_OVERRIDES.with(|cell| cell.borrow().clone());
    effective_config_with_user_overrides(user_overrides.as_ref())
}

/// Provider config built purely from the compiled-in `EMBEDDED_PROVIDERS_TOML`
/// snapshot, ignoring every ambient layer: the developer's
/// `~/.config/harn/providers.toml`, `HARN_PROVIDERS_CONFIG`, the process
/// runtime-catalog overlay, and thread-local user overrides.
///
/// This is the hermetic source of truth for *generating* the checked-in
/// `spec/provider-catalog/*` artifacts. Artifact generation must be a pure
/// function of the source tree so a developer's personal aliases/providers
/// never leak into shipped artifacts (which then makes clean CI flag drift).
/// Runtime catalog presentation must keep using [`effective_config`] /
/// [`effective_config_with_user_overrides`], which legitimately reflect the
/// host's live configuration.
///
/// An optional explicit overlay (e.g. a `--overlay` file named on the command
/// line) is merged on top of the embedded base. Unlike the home file and env
/// layers, that overlay is a declared, reproducible input rather than ambient
/// machine state, so it is safe to honor while staying hermetic.
pub fn embedded_config(explicit_overlay: Option<&ProvidersConfig>) -> ProvidersConfig {
    let mut config = default_config();
    if let Some(overlay) = explicit_overlay {
        config.merge_from(overlay);
    }
    config
}

pub(crate) fn effective_config_with_user_overrides(
    user_overrides: Option<&ProvidersConfig>,
) -> ProvidersConfig {
    let mut merged = load_config().clone();
    if let Some(overlay) = runtime_catalog_overlay()
        .read()
        .expect("runtime catalog overlay poisoned")
        .as_ref()
    {
        merged.merge_from(overlay);
    }
    if let Some(overlay) = user_overrides {
        merged.merge_from(overlay);
    }
    merged
}

fn runtime_catalog_overlay() -> &'static RwLock<Option<ProvidersConfig>> {
    RUNTIME_CATALOG_OVERLAY.get_or_init(|| RwLock::new(None))
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
        let requested = alias
            .tool_format
            .clone()
            .unwrap_or_else(|| default_tool_format_with_config(&config, &id, &provider));
        let tool_format = guard_tool_format(&provider, &id, &requested, Some(selector));
        return ResolvedModel {
            tier: model_tier_with_config(&config, &id),
            family: model_family_with_config(&config, &provider, &id),
            lineage: model_lineage_with_config(&config, &provider, &id),
            id,
            provider,
            alias: Some(selector.to_string()),
            tool_format,
        };
    }

    let id = normalize_model_id(selector);
    let inference = infer_provider_with_config(&config, selector);
    let source = inference.source;
    let provider = inference.provider;
    let requested = default_tool_format_with_config(&config, &id, &provider);
    let tool_format = guard_tool_format(&provider, &id, &requested, None);
    let tier = model_tier_with_config(&config, &id);
    let family = model_family_with_inference_source(&config, &provider, &id, source);
    let lineage = model_lineage_with_inference_source(&config, &provider, &id, source);
    ResolvedModel {
        id,
        provider,
        alias: None,
        tool_format,
        tier,
        family,
        lineage,
    }
}

/// Run the requested `tool_format` through the capability registry's
/// dialect-validity gate, returning the safe format to actually use. When the
/// registry auto-corrects a known-broken combo (e.g. a `native` pin on a
/// `native_unreliable` route that silently drops to unparsed DSML text), the
/// correction is logged once at resolution time so a harness developer sees
/// *why* their pinned format was not honored — never a silent vanishing.
fn guard_tool_format(provider: &str, model: &str, requested: &str, alias: Option<&str>) -> String {
    let decision = crate::llm::capabilities::validate_tool_format(provider, model, requested);
    if let Some(reason) = &decision.correction {
        tracing::warn!(
            target: "harn::llm::tool_format",
            alias = alias.unwrap_or(""),
            "{reason}"
        );
    }
    decision.effective
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

pub(crate) fn model_tier_with_config(config: &ProvidersConfig, model_id: &str) -> String {
    // Per-model self-declared tier wins. This is the only path.
    if let Some(model) = config.models.get(model_id) {
        if let Some(tier) = model.tier.as_deref() {
            let trimmed = tier.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    // Legacy pattern-rules: still consulted while we finish migrating the
    // long tail of models to per-row `tier = "..."`. Newly added rows
    // should set `tier` directly; the rule table is a fallback only.
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
    config.tier_defaults.default.clone()
}

/// Return the normalized model-family token used for cross-family review.
pub fn model_family(provider: &str, model_id: &str) -> String {
    let config = effective_config();
    model_family_with_config(&config, provider, model_id)
}

pub(crate) fn model_family_with_config(
    config: &ProvidersConfig,
    provider: &str,
    model_id: &str,
) -> String {
    catalog_family_token(config, model_id)
        .unwrap_or_else(|| derive_model_family(provider, model_id))
}

fn model_family_with_inference_source(
    config: &ProvidersConfig,
    provider: &str,
    model_id: &str,
    source: crate::llm::provider::ProviderInferenceSource,
) -> String {
    if let Some(family) = catalog_family_token(config, model_id) {
        return family;
    }
    let id_family = derive_model_family("", model_id);
    if id_family != "unknown" {
        return id_family;
    }
    if matches!(
        source,
        crate::llm::provider::ProviderInferenceSource::DefaultFallback
    ) {
        return "unknown".to_string();
    }
    derive_model_family(provider, model_id)
}

/// Return the narrower lineage token used for model-aware option packs.
pub fn model_lineage(provider: &str, model_id: &str) -> String {
    let config = effective_config();
    model_lineage_with_config(&config, provider, model_id)
}

pub(crate) fn model_lineage_with_config(
    config: &ProvidersConfig,
    provider: &str,
    model_id: &str,
) -> String {
    catalog_lineage_token(config, model_id)
        .unwrap_or_else(|| derive_model_lineage(provider, model_id))
}

fn model_lineage_with_inference_source(
    config: &ProvidersConfig,
    provider: &str,
    model_id: &str,
    source: crate::llm::provider::ProviderInferenceSource,
) -> String {
    if let Some(lineage) = catalog_lineage_token(config, model_id) {
        return lineage;
    }
    let id_lineage = derive_model_lineage("", model_id);
    if id_lineage != "unknown" {
        return id_lineage;
    }
    if matches!(
        source,
        crate::llm::provider::ProviderInferenceSource::DefaultFallback
    ) {
        return "unknown".to_string();
    }
    derive_model_lineage(provider, model_id)
}

fn catalog_family_token(config: &ProvidersConfig, model_id: &str) -> Option<String> {
    config
        .models
        .get(model_id)
        .and_then(|model| normalized_catalog_token(model.family.as_deref()))
}

fn catalog_lineage_token(config: &ProvidersConfig, model_id: &str) -> Option<String> {
    config
        .models
        .get(model_id)
        .and_then(|model| normalized_catalog_token(model.lineage.as_deref()))
}

fn normalized_catalog_token(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase().replace('_', "-"))
}

fn derive_model_family(provider: &str, model_id: &str) -> String {
    let id = model_id.to_ascii_lowercase();
    if contains_any(&id, &["claude", "anthropic.claude"]) {
        return "anthropic-claude".to_string();
    }
    if contains_any(&id, &["gemini", "google/gemini"]) {
        return "google-gemini".to_string();
    }
    if contains_any(&id, &["deepseek"]) {
        return "deepseek".to_string();
    }
    if contains_any(&id, &["qwen"]) {
        return "qwen".to_string();
    }
    if contains_any(&id, &["kimi", "moonshot"]) {
        return "kimi".to_string();
    }
    if contains_any(&id, &["glm", "z-ai/glm", "zhipu"]) {
        return "glm".to_string();
    }
    if contains_any(&id, &["mistral", "mixtral", "devstral"]) {
        return "mistral".to_string();
    }
    if contains_any(&id, &["minimax"]) {
        return "minimax".to_string();
    }
    if contains_any(&id, &["llama"]) {
        return "llama".to_string();
    }
    if contains_any(&id, &["gemma"]) {
        return "gemma".to_string();
    }
    if is_openai_reasoning_model(&id) {
        return "openai-reasoning".to_string();
    }
    if contains_any(&id, &["gpt-oss", "openai/gpt", "gpt-"]) {
        return "openai-gpt".to_string();
    }
    match provider {
        "anthropic" | "bedrock" | "vertex-anthropic" => "anthropic-claude".to_string(),
        "openai" | "azure" | "azure_openai" => "openai-gpt".to_string(),
        "gemini" | "vertex" | "google" => "google-gemini".to_string(),
        "deepseek" => "deepseek".to_string(),
        "zai" => "glm".to_string(),
        "minimax" => "minimax".to_string(),
        other if !other.is_empty() => normalize_identifier_token(other),
        _ => "unknown".to_string(),
    }
}

fn derive_model_lineage(provider: &str, model_id: &str) -> String {
    let id = model_id.to_ascii_lowercase();
    if contains_any(&id, &["haiku"]) {
        return "claude-haiku".to_string();
    }
    if contains_any(&id, &["opus-4-7", "opus-4-8", "opus-mythos"]) {
        return "claude-opus-adaptive".to_string();
    }
    if contains_any(&id, &["claude"]) {
        return "claude-sonnet-opus".to_string();
    }
    if contains_any(&id, &["gpt-5"]) {
        return "openai-gpt5".to_string();
    }
    if is_openai_reasoning_model(&id) {
        return "openai-reasoning".to_string();
    }
    if contains_any(&id, &["gpt-", "gpt_"]) {
        return "openai-legacy".to_string();
    }
    if contains_any(&id, &["gemini"]) {
        if contains_any(&id, &["flash"]) {
            return "gemini-flash".to_string();
        }
        return "gemini-pro".to_string();
    }
    if contains_any(&id, &["qwen3", "qwen/qwen3"]) {
        return "qwen3".to_string();
    }
    if contains_any(&id, &["gemma4", "gemma-4"]) {
        return "gemma4".to_string();
    }
    let family = derive_model_family(provider, model_id);
    if family == "unknown" {
        "unknown".to_string()
    } else {
        family
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn starts_with_any(haystack: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| haystack.starts_with(prefix))
}

fn is_openai_reasoning_model(id: &str) -> bool {
    starts_with_any(id, &["o1", "o3", "o4"])
        || contains_any(
            id,
            &[
                "/o1", "/o3", "/o4", ":o1", ":o3", ":o4", ".o1", ".o3", ".o4",
            ],
        )
}

fn normalize_identifier_token(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Get provider config for resolving base_url, auth, etc.
pub fn provider_config(name: &str) -> Option<ProviderDef> {
    effective_config().providers.get(name).cloned()
}

pub fn provider_protocol(name: &str) -> Option<String> {
    provider_config(name).and_then(|def| def.protocol)
}

pub fn provider_uses_acp(name: &str) -> bool {
    provider_protocol(name)
        .as_deref()
        .is_some_and(|protocol| protocol.eq_ignore_ascii_case("acp"))
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

/// Get per-role LLM defaults, e.g. `[model_roles.merge]`.
///
/// Role defaults are intentionally shaped like ordinary `llm_call` options:
/// callers can pin `provider`/`model`, install `route_policy` or `prefer`,
/// and tune budget/latency knobs without creating a parallel routing stack.
/// Environment variables provide a lightweight operational override for
/// merge/fast-apply workers:
///
/// - `HARN_LLM_MERGE_PROVIDER`, `HARN_LLM_MERGE_MODEL`,
///   `HARN_LLM_MERGE_ROUTE_POLICY`
/// - `HARN_LLM_FAST_APPLY_PROVIDER`, `HARN_LLM_FAST_APPLY_MODEL`,
///   `HARN_LLM_FAST_APPLY_ROUTE_POLICY`
/// - `HARN_LLM_ROLE_<ROLE>_PROVIDER`, `_MODEL`, `_ROUTE_POLICY`
pub fn model_role_defaults(role: &str) -> BTreeMap<String, toml::Value> {
    let normalized = normalize_model_role_name(role);
    if normalized.is_empty() {
        return BTreeMap::new();
    }
    let config = effective_config();
    let mut params = BTreeMap::new();
    for key in role_lookup_keys(&normalized) {
        extend_model_role_defaults(&config, &key, &mut params);
    }
    apply_model_role_env_overrides(&normalized, &mut params);
    params
}

fn extend_model_role_defaults(
    config: &ProvidersConfig,
    role: &str,
    params: &mut BTreeMap<String, toml::Value>,
) {
    for (configured_role, defaults) in &config.model_roles {
        if normalize_model_role_name(configured_role) == role {
            params.extend(defaults.clone());
        }
    }
    if let Some(defaults) = config.model_roles.get(role) {
        params.extend(defaults.clone());
    }
}

fn normalize_model_role_name(role: &str) -> String {
    role.trim().to_ascii_lowercase().replace('-', "_")
}

fn role_lookup_keys(role: &str) -> Vec<String> {
    if role == "merge" {
        vec!["fast_apply".to_string(), "merge".to_string()]
    } else if role == "fast_apply" {
        vec!["merge".to_string(), "fast_apply".to_string()]
    } else {
        vec![role.to_string()]
    }
}

fn role_env_token(role: &str) -> String {
    role.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn apply_model_role_env_overrides(role: &str, params: &mut BTreeMap<String, toml::Value>) {
    for alias in role_env_aliases(role) {
        apply_model_role_env_var(&format!("HARN_LLM_{alias}_PROVIDER"), "provider", params);
        apply_model_role_env_var(&format!("HARN_LLM_{alias}_MODEL"), "model", params);
        apply_model_role_env_var(
            &format!("HARN_LLM_{alias}_ROUTE_POLICY"),
            "route_policy",
            params,
        );
        apply_model_role_env_var(
            &format!("HARN_LLM_ROLE_{alias}_PROVIDER"),
            "provider",
            params,
        );
        apply_model_role_env_var(&format!("HARN_LLM_ROLE_{alias}_MODEL"), "model", params);
        apply_model_role_env_var(
            &format!("HARN_LLM_ROLE_{alias}_ROUTE_POLICY"),
            "route_policy",
            params,
        );
    }
}

fn role_env_aliases(role: &str) -> Vec<String> {
    let token = role_env_token(role);
    if token.is_empty() {
        return Vec::new();
    }
    if token == "MERGE" {
        vec!["FAST_APPLY".to_string(), "MERGE".to_string()]
    } else if token == "FAST_APPLY" {
        vec!["MERGE".to_string(), "FAST_APPLY".to_string()]
    } else {
        vec![token]
    }
}

fn apply_model_role_env_var(
    env_name: &str,
    option_name: &str,
    params: &mut BTreeMap<String, toml::Value>,
) {
    let Ok(value) = std::env::var(env_name) else {
        return;
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    params.insert(
        option_name.to_string(),
        toml::Value::String(trimmed.to_string()),
    );
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
    let config = effective_config();
    model_catalog_entries_with_config(&config)
}

pub(crate) fn model_catalog_entries_with_config(
    config: &ProvidersConfig,
) -> Vec<(String, ModelDef)> {
    sorted_model_entries_with_config(config)
        .into_iter()
        .map(|(id, model)| {
            let provider = model.provider.clone();
            (
                id.clone(),
                with_effective_capability_tags(id, provider, model),
            )
        })
        .collect()
}

pub(crate) fn sorted_model_entries_with_config(
    config: &ProvidersConfig,
) -> Vec<(String, ModelDef)> {
    let mut entries: Vec<_> = config
        .models
        .iter()
        .map(|(id, model)| (id.clone(), model.clone()))
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

pub fn model_rate_limits(model_id: &str) -> Option<RateLimitsDef> {
    model_catalog_entry(model_id).and_then(|model| model.rate_limits)
}

pub fn wire_model_id(model_id: &str) -> String {
    model_catalog_entry(model_id)
        .and_then(|model| model.wire_model)
        .unwrap_or_else(|| model_id.to_string())
}

pub fn provider_rate_limits(provider: &str) -> Option<RateLimitsDef> {
    provider_config(provider).and_then(|provider| {
        provider
            .rate_limits
            .unwrap_or_default()
            .with_rpm_fallback(provider.rpm)
    })
}

pub fn model_equivalence_group(model_id: &str) -> Option<String> {
    model_catalog_entry(model_id).and_then(|model| {
        model
            .equivalence_group
            .or(model.logical_model)
            .filter(|group| !group.trim().is_empty())
    })
}

/// Return same-logical-model routes that can be considered for explicit
/// failover or cross-provider experiments. Equivalence is a catalog assertion
/// about compatible model weights/family, not wire-level identity.
pub fn equivalent_model_catalog_entries(selector: &str) -> Vec<(String, ModelDef)> {
    let resolved = resolve_model_info(selector);
    let Some(group) = model_equivalence_group(&resolved.id) else {
        return Vec::new();
    };
    let config = effective_config();
    let Some(source) = config.models.get(&resolved.id) else {
        return Vec::new();
    };
    let source_caps = crate::llm::capabilities::lookup(&source.provider, &resolved.id);
    let source_context = source
        .runtime_context_window
        .unwrap_or(source.context_window);

    sorted_model_entries_with_config(&config)
        .into_iter()
        .filter(|(id, model)| !(id == &resolved.id && model.provider == resolved.provider))
        .filter(|(_, model)| !model.deprecated)
        .filter(|(_, model)| model.availability != ModelAvailability::Dedicated)
        .filter(|(_, model)| {
            model.equivalence_group.as_deref() == Some(group.as_str())
                || model.logical_model.as_deref() == Some(group.as_str())
        })
        .filter(|(id, model)| {
            let caps = crate::llm::capabilities::lookup(&model.provider, id);
            let candidate_context = model.runtime_context_window.unwrap_or(model.context_window);
            candidate_context >= source_context
                && (!source_caps.native_tools || caps.native_tools)
                && (!source_caps.text_tool_wire_format_supported
                    || caps.text_tool_wire_format_supported)
                && (!source_caps.reasoning_effort_supported || caps.reasoning_effort_supported)
                && source_caps.structured_output_mode == caps.structured_output_mode
        })
        .map(|(id, model)| {
            let provider = model.provider.clone();
            (
                id.clone(),
                with_effective_capability_tags(id, provider, model),
            )
        })
        .collect()
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
    if provider_uses_acp(provider) {
        return "default".to_string();
    }
    match provider {
        "local" => std::env::var("LOCAL_LLM_MODEL")
            .or_else(|_| std::env::var("HARN_LLM_MODEL"))
            .unwrap_or_else(|_| "gemma-4-26b-a4b-it".to_string()),
        "mlx" => std::env::var("MLX_MODEL_ID")
            .unwrap_or_else(|_| "unsloth/Qwen3.6-35B-A3B-UD-MLX-4bit".to_string()),
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

/// Premium per-MTok pricing for a model's accelerated-serving ("fast mode")
/// tier, when the catalog declares one. Returns `None` for models with no
/// fast tier or a tier that omits explicit pricing — callers fall back to
/// standard pricing in that case.
pub fn model_fast_pricing_per_mtok(model_id: &str) -> Option<ModelPricing> {
    effective_config()
        .models
        .get(model_id)
        .and_then(|model| model.fast_mode.as_ref())
        .and_then(|fast_mode| fast_mode.pricing.clone())
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

/// The tool-call channel a `tool_format` string addresses.
///
/// `native` is the provider JSON tool-calling channel; `text` (the canonical
/// tagged/heredoc grammar) and `json` (fenced-JSON) are both TEXT-channel
/// formats — they ride in the assistant's visible content and parse with a
/// text parser. This is the single source of truth for "is this format a
/// text-channel format?" so the parity gates, native-tools resolution, and
/// tool-result message role all agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFormatChannel {
    /// Provider native JSON tool calling.
    Native,
    /// A text-channel grammar carried in assistant content (`text` or `json`).
    Text,
}

/// Classify a `tool_format` string into its channel, or `None` for an unknown
/// value (a typo, or a not-yet-wired format). Callers use this to reject
/// unknown formats loudly instead of silently defaulting.
///
/// EXHAUSTIVE-MATCH GUARD: this `match` is the canonical place tool_format is
/// switched. Adding a new format requires a branch here, so a half-wired
/// format fails to compile rather than silently reading as text.
pub fn tool_format_channel(format: &str) -> Option<ToolFormatChannel> {
    match format {
        "native" => Some(ToolFormatChannel::Native),
        "text" | "json" => Some(ToolFormatChannel::Text),
        _ => None,
    }
}

/// True when `format` is a tool_format Harn understands (`native`, `text`, or
/// `json`). Used to gate the capability-matrix `preferred_tool_format` so a
/// pinned format is honored, while an unknown value falls through to the
/// native/text heuristic.
pub fn is_known_tool_format(format: &str) -> bool {
    tool_format_channel(format).is_some()
}

/// Resolve the default tool format for a model+provider combination.
/// Priority: alias `tool_format` (matched by model ID) > provider/model
/// capability matrix > legacy provider feature > "json" (the global
/// text-channel default; heredoc "text" is opt-in via a pin or explicit
/// request).
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
        // A capability row may pin any known tool_format, including `text`
        // (heredoc) — the reverse safety valve a regressing model uses to pin
        // OFF the global json default. `json` is also honored when a row sets
        // it. The exhaustive match below is the EXHAUSTIVE-MATCH GUARD: a new
        // tool_format that isn't classified here fails loudly rather than
        // silently falling through to the native/json heuristic.
        if is_known_tool_format(format) {
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
        // GLOBAL DEFAULT: a text-channel model with no pinned format resolves
        // to fenced-json (`json`), not heredoc (`text`). The win is STRUCTURAL
        // — a JSON string can't carry a raw newline, so a `<<EOF` content
        // delimiter never collides with the call wrapper (heredoc's known
        // production defect: models leak `<<EOF` into file content → the
        // `line 0: <<` thrash). Fenced-json swept a clean 1.0/1.0/1.0
        // (compliance/parse-determinism/expressiveness) across every model
        // measured, and the structural guarantee generalizes to unmeasured
        // models. Heredoc (`text`) stays selectable explicitly and via a
        // per-model `preferred_tool_format = "text"` pin (the reverse valve).
        "json".to_string()
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
    capability_tags_from_capabilities(&caps)
}

pub(crate) fn capability_tags_from_capabilities(
    caps: &crate::llm::capabilities::Capabilities,
) -> Vec<String> {
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
    if caps.video {
        tags.push("video".to_string());
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

pub fn pick_complementary_reviewer(
    options: ComplementaryReviewerOptions,
) -> ComplementaryReviewerSelection {
    let config = effective_config();
    let mut author = resolve_model_info(&options.author_model);
    if let Some(provider) = options
        .author_provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
    {
        author.provider = provider.to_string();
        author.family = model_family_with_config(&config, &author.provider, &author.id);
        author.lineage = model_lineage_with_config(&config, &author.provider, &author.id);
        author.tool_format = default_tool_format_with_config(&config, &author.id, &author.provider);
    }
    let author_entry = config.models.get(&author.id);
    let author_identity = complementary_identity(
        author.id.clone(),
        author.provider.clone(),
        author.family.clone(),
        author.lineage.clone(),
        author.tier.clone(),
        author_entry.and_then(|model| model.pricing.clone()),
    );

    let fallback =
        |code: ReviewerFallbackCode, fallback_reason: String| ComplementaryReviewerSelection {
            intent: options.intent.as_str().to_string(),
            reviewer: author_identity.clone(),
            estimated_incremental_cost: cost_estimate(
                author_identity.pricing.as_ref(),
                author_identity.pricing.as_ref(),
            ),
            author: author_identity.clone(),
            fallback: true,
            reason: format!(
                "using author model {} because {fallback_reason}",
                author_identity.id
            ),
            fallback_reason: Some(fallback_reason),
            fallback_code: Some(code.as_code().to_string()),
        };

    if author_identity.family == "unknown" {
        return fallback(
            ReviewerFallbackCode::UnknownAuthorFamily,
            "author model family is unknown".to_string(),
        );
    }

    let preferred_families = author_entry
        .map(|model| model.complementary_with.clone())
        .unwrap_or_default();
    let author_refs = reviewer_match_refs(&author_identity);
    let mut rejected_by_price = 0usize;
    let mut diff_family_seen = 0usize;
    let mut candidates = Vec::new();

    for (id, model) in config.models.iter() {
        if id == &author_identity.id && model.provider == author_identity.provider {
            continue;
        }
        if model.deprecated || model.availability != ModelAvailability::Serverless {
            continue;
        }
        let family = model_family_with_config(&config, &model.provider, id);
        if family == "unknown" || family == author_identity.family {
            continue;
        }
        diff_family_seen += 1;
        let lineage = model_lineage_with_config(&config, &model.provider, id);
        let candidate_identity = complementary_identity(
            id.clone(),
            model.provider.clone(),
            family,
            lineage,
            model_tier_with_config(&config, id),
            model.pricing.clone(),
        );
        if model
            .avoid_as_reviewer_for
            .iter()
            .any(|selector| refs_contain_selector(&author_refs, selector))
        {
            continue;
        }
        if exceeds_price_cap(
            author_identity.pricing.as_ref(),
            candidate_identity.pricing.as_ref(),
            options.max_price_multiplier,
        ) {
            rejected_by_price += 1;
            continue;
        }
        let score = reviewer_score(
            &options,
            &author_identity,
            &candidate_identity,
            model,
            &preferred_families,
        );
        candidates.push(ReviewerCandidate {
            identity: candidate_identity,
            score,
        });
    }

    candidates.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.identity.provider.cmp(&right.identity.provider))
            .then_with(|| left.identity.id.cmp(&right.identity.id))
    });

    let Some(best) = candidates.into_iter().next() else {
        if rejected_by_price > 0 {
            let cap = options.max_price_multiplier.unwrap_or_default();
            return fallback(
                ReviewerFallbackCode::NoDiffFamilyWithinPrice,
                format!("no different-family reviewer satisfied max_price_multiplier {cap}"),
            );
        }
        if diff_family_seen == 0 {
            return fallback(
                ReviewerFallbackCode::NoDiffFamilyServerless,
                "no active serverless different-family reviewer is cataloged".to_string(),
            );
        }
        return fallback(
            ReviewerFallbackCode::AllDiffFamilyExcluded,
            "all different-family reviewer candidates were excluded".to_string(),
        );
    };

    let estimate = cost_estimate(
        best.identity.pricing.as_ref(),
        author_identity.pricing.as_ref(),
    );
    ComplementaryReviewerSelection {
        intent: options.intent.as_str().to_string(),
        reason: reviewer_reason(&author_identity, &best.identity, estimate.as_ref()),
        estimated_incremental_cost: estimate,
        author: author_identity,
        reviewer: best.identity,
        fallback: false,
        fallback_reason: None,
        fallback_code: None,
    }
}

#[derive(Debug, Clone)]
struct ReviewerCandidate {
    identity: ComplementaryModelIdentity,
    score: f64,
}

fn complementary_identity(
    id: String,
    provider: String,
    family: String,
    lineage: String,
    tier: String,
    pricing: Option<ModelPricing>,
) -> ComplementaryModelIdentity {
    ComplementaryModelIdentity {
        id,
        provider,
        family,
        lineage,
        tier,
        pricing,
    }
}

fn reviewer_score(
    options: &ComplementaryReviewerOptions,
    author: &ComplementaryModelIdentity,
    candidate: &ComplementaryModelIdentity,
    model: &ModelDef,
    preferred_families: &[String],
) -> f64 {
    let candidate_refs = reviewer_match_refs(candidate);
    let mut score = 0.0;
    if let Some(rank) = preferred_families
        .iter()
        .position(|selector| refs_contain_selector(&candidate_refs, selector))
    {
        score += 1_000.0 - rank as f64;
    }
    if candidate.provider != author.provider {
        score += 100.0;
    }
    score += match tier_distance(&author.tier, &candidate.tier) {
        0 => 80.0,
        1 => 45.0,
        2 => 15.0,
        _ => 0.0,
    };
    for strength in intent_strengths(options.intent) {
        if model.strengths.iter().any(|tag| tag == strength) {
            score += 8.0;
        }
    }
    if model.capabilities.iter().any(|tag| tag == "tools") {
        score += 4.0;
    }
    if let (Some(author_total), Some(candidate_total)) = (
        pricing_total(author.pricing.as_ref()),
        pricing_total(candidate.pricing.as_ref()),
    ) {
        if author_total > 0.0 {
            let ratio = candidate_total / author_total;
            if ratio <= 1.0 {
                score += 20.0;
            }
            score -= (ratio - 1.0).abs().min(10.0) * 8.0;
        }
    }
    score
}

fn intent_strengths(intent: ComplementaryReviewerIntent) -> &'static [&'static str] {
    match intent {
        ComplementaryReviewerIntent::Review => &["reasoning", "coding", "tool_use"],
        ComplementaryReviewerIntent::Critique => &["reasoning", "long_context", "tool_use"],
        ComplementaryReviewerIntent::PlanReview => {
            &["reasoning", "coding", "agentic", "long_context", "tool_use"]
        }
    }
}

fn tier_distance(left: &str, right: &str) -> u8 {
    let left = tier_rank(left);
    let right = tier_rank(right);
    left.abs_diff(right)
}

fn tier_rank(tier: &str) -> u8 {
    match tier {
        "small" => 0,
        "mid" => 1,
        "frontier" | "reasoning" => 2,
        _ => 1,
    }
}

fn exceeds_price_cap(
    author_pricing: Option<&ModelPricing>,
    candidate_pricing: Option<&ModelPricing>,
    max_price_multiplier: Option<f64>,
) -> bool {
    let Some(max_price_multiplier) = max_price_multiplier else {
        return false;
    };
    let Some(author_total) = pricing_total(author_pricing) else {
        return false;
    };
    let Some(candidate_total) = pricing_total(candidate_pricing) else {
        return true;
    };
    author_total > 0.0 && candidate_total > author_total * max_price_multiplier
}

fn cost_estimate(
    reviewer_pricing: Option<&ModelPricing>,
    author_pricing: Option<&ModelPricing>,
) -> Option<ComplementaryCostEstimate> {
    let reviewer_pricing = reviewer_pricing?;
    let total_per_mtok = reviewer_pricing.input_per_mtok + reviewer_pricing.output_per_mtok;
    let multiplier_vs_author = pricing_total(author_pricing)
        .filter(|author_total| *author_total > 0.0)
        .map(|author_total| total_per_mtok / author_total);
    Some(ComplementaryCostEstimate {
        input_per_mtok: reviewer_pricing.input_per_mtok,
        output_per_mtok: reviewer_pricing.output_per_mtok,
        total_per_mtok,
        multiplier_vs_author,
    })
}

fn pricing_total(pricing: Option<&ModelPricing>) -> Option<f64> {
    pricing.map(|pricing| pricing.input_per_mtok + pricing.output_per_mtok)
}

fn reviewer_reason(
    author: &ComplementaryModelIdentity,
    reviewer: &ComplementaryModelIdentity,
    estimate: Option<&ComplementaryCostEstimate>,
) -> String {
    let cost = estimate
        .and_then(|estimate| estimate.multiplier_vs_author)
        .map(|multiplier| format!("{multiplier:.2}x the author model price"))
        .unwrap_or_else(|| "price ratio unavailable".to_string());
    format!(
        "selected {} via {} because family {} differs from author family {}, tier {} matches author tier {}, and {}",
        reviewer.id,
        reviewer.provider,
        reviewer.family,
        author.family,
        reviewer.tier,
        author.tier,
        cost
    )
}

fn reviewer_match_refs(identity: &ComplementaryModelIdentity) -> BTreeSet<String> {
    BTreeSet::from([
        identity.id.to_ascii_lowercase(),
        identity.provider.to_ascii_lowercase(),
        format!("{}/{}", identity.provider, identity.id).to_ascii_lowercase(),
        format!("{}:{}", identity.provider, identity.id).to_ascii_lowercase(),
        identity.family.to_ascii_lowercase(),
        identity.lineage.to_ascii_lowercase(),
    ])
}

fn refs_contain_selector(refs: &BTreeSet<String>, selector: &str) -> bool {
    normalized_catalog_token(Some(selector))
        .or_else(|| Some(selector.trim().to_ascii_lowercase()))
        .is_some_and(|selector| refs.contains(&selector))
}

// Model-pattern matching for forms like "claude-*", "qwen/*", "ollama:*".
// Shared workspace semantics live in `harn-glob`.
use harn_glob::match_name as glob_match;

fn dirs_or_home() -> Option<String> {
    crate::user_dirs::home_dir().map(|home| home.to_string_lossy().into_owned())
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

/// Embedded copy of generated `llm/providers.toml`, built from
/// `llm/catalog_sources/**/*.toml` by `harn providers build-config`.
/// Edit the fragments, not this generated snapshot or this string.
const EMBEDDED_PROVIDERS_TOML: &str = include_str!("llm/providers.toml");

/// Parse the embedded generated `providers.toml` into the runtime
/// `ProvidersConfig`.
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
    fn resolve_model_info_guards_bad_native_pin_on_unreliable_route() {
        reset_overrides();
        // An alias that pins tool_format = "native" for DeepSeek V3.2 on
        // OpenRouter — a route the capability registry knows is
        // native_unreliable (drops to unparsed DSML text). Before the
        // footgun-removal gate this bad pin survived resolution verbatim and
        // produced vanishing tool calls; now it is steered to the route's safe
        // text-channel format.
        let overlay = parse_config_toml(
            "[aliases.guard-ds]\nid = \"deepseek/deepseek-v3.2\"\nprovider = \"openrouter\"\ntool_format = \"native\"\n",
        )
        .expect("overlay parses");
        set_user_overrides(Some(overlay));
        let resolved = resolve_model_info("guard-ds");
        assert_eq!(
            resolved.tool_format, "text",
            "a native pin on a native_unreliable route must be auto-corrected to text"
        );
        clear_user_overrides();

        // A safe native pin (a route with no adverse parity) is untouched.
        let overlay_ok = parse_config_toml(
            "[aliases.guard-ds-ok]\nid = \"deepseek/deepseek-v3-base\"\nprovider = \"openrouter\"\ntool_format = \"native\"\n",
        )
        .expect("overlay parses");
        set_user_overrides(Some(overlay_ok));
        let resolved_ok = resolve_model_info("guard-ds-ok");
        assert_eq!(resolved_ok.tool_format, "native");
        clear_user_overrides();
    }

    #[test]
    fn suppress_routes_parse_and_merge_dedupe() {
        let mut base =
            parse_config_toml("[suppress]\nroutes = [\"together:Qwen/Qwen3-Coder-Next-FP8\"]\n")
                .expect("base parses");
        assert!(!base.is_empty(), "a suppress-only overlay is not empty");
        let overlay = parse_config_toml(
            "[suppress]\nroutes = [\"together:Qwen/Qwen3-Coder-Next-FP8\", \"ollama:img:tag\"]\n",
        )
        .expect("overlay parses");
        base.merge_from(&overlay);
        assert_eq!(
            base.suppress.routes,
            vec![
                "together:Qwen/Qwen3-Coder-Next-FP8".to_string(),
                "ollama:img:tag".to_string(),
            ],
            "merge appends new selectors without duplicating existing ones"
        );
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
        let _guard = crate::llm::env_guard();
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
        let _guard = crate::llm::env_guard();
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
        let _guard = crate::llm::env_guard();
        let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
        unsafe {
            std::env::remove_var("HARN_DEFAULT_PROVIDER");
        }

        assert_eq!(infer_provider("cerebras/gpt-oss-120b"), "cerebras");
        assert_eq!(infer_provider("cerebras/zai-glm-4.7"), "cerebras");
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
        let _guard = crate::llm::env_guard();
        let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
        unsafe {
            std::env::remove_var("HARN_DEFAULT_PROVIDER");
        }

        for model in ["gpt-oss-120b", "zai-glm-4.7", "llama-3.3-70b"] {
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
    fn test_equivalent_model_catalog_entries_use_capability_compatible_routes() {
        reset_overrides();

        assert_eq!(
            wire_model_id("groq/openai/gpt-oss-120b"),
            "openai/gpt-oss-120b"
        );
        assert_eq!(wire_model_id("gpt-oss-120b"), "gpt-oss-120b");

        let equivalents = equivalent_model_catalog_entries("gpt-oss-120b");
        let ids = equivalents
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>();

        assert!(
            ids.contains(&"groq/openai/gpt-oss-120b"),
            "Cerebras GPT-OSS should surface the Groq serving variant"
        );
        assert!(
            !ids.contains(&"gpt-oss-120b"),
            "equivalence results should not include the source row"
        );
        assert!(equivalents.iter().all(|(_, model)| {
            model.equivalence_group.as_deref() == Some("openai-gpt-oss-120b")
        }));
    }

    #[test]
    fn fireworks_gpt_oss_route_has_real_context_window() {
        // Regression: the Fireworks-served `accounts/fireworks/models/gpt-oss-120b`
        // wire id had NO catalog row, so its context window resolved to None and
        // the agent's auto-compaction budget had nothing to enforce — the prompt
        // grew until Fireworks rejected the turn with HTTP 400 [context_overflow]
        // (session 019ee303: 197467 tokens > 131071 max). Cataloging the real
        // 131072 window lets compaction trigger before the hard limit.
        reset_overrides();

        let entry = model_catalog_entry("accounts/fireworks/models/gpt-oss-120b")
            .expect("Fireworks gpt-oss-120b must be in the model catalog");
        assert_eq!(entry.context_window, 131_072);
        assert_eq!(entry.provider, "fireworks");
        assert_eq!(
            entry.equivalence_group.as_deref(),
            Some("openai-gpt-oss-120b"),
        );
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
                logical_model: None,
                equivalence_group: None,
                served_variant: None,
                wire_model: None,
                api_dialect: None,
                rate_limits: None,
                architecture: None,
                local_memory: None,
                runtime_context_window: None,
                stream_timeout: None,
                capabilities: Vec::new(),
                pricing: None,
                deprecated: false,
                deprecation_note: None,
                superseded_by: None,
                fast_mode: None,
                quality_tags: Vec::new(),
                availability: ModelAvailability::default(),
                tier: None,
                open_weight: None,
                strengths: Vec::new(),
                benchmarks: std::collections::BTreeMap::new(),
                family: None,
                lineage: None,
                complementary_with: Vec::new(),
                avoid_as_reviewer_for: Vec::new(),
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

        let cerebras_glm = resolve_model_info("cerebras/zai-glm-4.7");
        assert_eq!(cerebras_glm.id, "zai-glm-4.7");
        assert_eq!(cerebras_glm.provider, "cerebras");
    }

    #[test]
    fn test_model_tier_from_defaults() {
        // Tier is now self-declared per model row in providers.toml.
        // Models that match an entry use the declared value; unknown
        // model ids fall through to `tier_defaults.default` ("mid").
        assert_eq!(model_tier("claude-sonnet-4-20250514"), "frontier");
        assert_eq!(model_tier("gpt-4o"), "frontier");
        assert_eq!(model_tier("Qwen/Qwen3.5-9B"), "small");
        assert_eq!(model_tier("deepseek-v4-flash"), "mid");
        assert_eq!(model_tier("deepseek-v4-pro"), "frontier");
        assert_eq!(model_tier("MiniMax-M2.7"), "frontier");
        assert_eq!(model_tier("glm-5.1"), "frontier");
        // Unknown ids resolve to the default.
        assert_eq!(model_tier("definitely-not-a-real-model"), "mid");
    }

    #[test]
    fn test_model_family_preserves_underlying_hosted_lineage() {
        assert_eq!(
            model_family("openrouter", "anthropic/claude-sonnet-4-6"),
            "anthropic-claude"
        );
        assert_eq!(
            model_family("openrouter", "google/gemini-2.5-flash"),
            "google-gemini"
        );
        assert_eq!(
            model_family("openrouter", "openai/o3-mini"),
            "openai-reasoning"
        );
        assert_eq!(model_lineage("openrouter", "openai/gpt-5.5"), "openai-gpt5");
        assert_eq!(
            model_lineage("openrouter", "openai/o3-mini"),
            "openai-reasoning"
        );
        assert_eq!(
            model_lineage("anthropic", "claude-opus-4-8"),
            "claude-opus-adaptive"
        );
        assert_eq!(model_lineage("llamacpp", "qwen3.6-35b-a3b"), "qwen3");
    }

    #[test]
    fn test_complementary_reviewer_uses_different_family() {
        let selection = pick_complementary_reviewer(ComplementaryReviewerOptions {
            author_model: "claude-sonnet-4-6".to_string(),
            author_provider: None,
            intent: ComplementaryReviewerIntent::PlanReview,
            max_price_multiplier: Some(3.0),
        });

        assert!(!selection.fallback, "{selection:?}");
        assert_eq!(selection.author.family, "anthropic-claude");
        assert_ne!(selection.reviewer.family, selection.author.family);
        assert_eq!(selection.reviewer.tier, "frontier");
        assert!(selection.estimated_incremental_cost.is_some());
        // Success path carries no machine-readable fallback code, so a caller
        // can treat `fallback_code.is_some()` as "must not self-review".
        assert_eq!(selection.fallback_code, None, "{selection:?}");
    }

    #[test]
    fn test_complementary_reviewer_falls_back_deterministically_on_price_cap() {
        let selection = pick_complementary_reviewer(ComplementaryReviewerOptions {
            author_model: "gpt-4o-mini".to_string(),
            author_provider: Some("openai".to_string()),
            intent: ComplementaryReviewerIntent::Review,
            max_price_multiplier: Some(0.01),
        });

        assert!(selection.fallback, "{selection:?}");
        assert_eq!(selection.reviewer.id, "gpt-4o-mini");
        assert_eq!(selection.reviewer.family, selection.author.family);
        assert!(selection
            .fallback_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("max_price_multiplier")));
        // The machine-readable code is stable regardless of the prose; a caller
        // hard-fails an independent-review step by branching on this, never by
        // parsing `fallback_reason`.
        assert_eq!(
            selection.fallback_code.as_deref(),
            Some(ReviewerFallbackCode::NoDiffFamilyWithinPrice.as_code()),
            "{selection:?}"
        );
        assert_eq!(
            ReviewerFallbackCode::NoDiffFamilyWithinPrice.as_code(),
            "no_diff_family_within_price"
        );
    }

    #[test]
    fn test_reviewer_fallback_codes_are_stable_strings() {
        // Append-only contract: harn pipelines and Rust callers branch on these
        // exact strings, so changing one is a breaking change.
        assert_eq!(
            ReviewerFallbackCode::UnknownAuthorFamily.as_code(),
            "unknown_author_family"
        );
        assert_eq!(
            ReviewerFallbackCode::NoDiffFamilyWithinPrice.as_code(),
            "no_diff_family_within_price"
        );
        assert_eq!(
            ReviewerFallbackCode::NoDiffFamilyServerless.as_code(),
            "no_diff_family_serverless"
        );
        assert_eq!(
            ReviewerFallbackCode::AllDiffFamilyExcluded.as_code(),
            "all_diff_family_excluded"
        );
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
    fn partial_provider_overlay_preserves_builtin_provider_metadata() {
        let overlay = parse_config_toml(
            r#"
            [providers.ollama]
            base_url = "http://localhost:11435"
            extra_headers = { "x-local" = "1" }
            "#,
        )
        .expect("provider overlay parses");

        let merged = merge_global_config(overlay);
        let ollama = merged
            .providers
            .get("ollama")
            .expect("ollama remains configured");

        assert_eq!(ollama.base_url, "http://localhost:11435");
        assert_eq!(ollama.auth_style, "none");
        assert_eq!(ollama.chat_endpoint, "/api/chat");
        assert_eq!(ollama.completion_endpoint.as_deref(), Some("/api/generate"));
        assert_eq!(ollama.cost_per_1k_in, Some(0.0));
        assert_eq!(ollama.cost_per_1k_out, Some(0.0));
        assert_eq!(
            ollama
                .healthcheck
                .as_ref()
                .and_then(|healthcheck| healthcheck.path.as_deref()),
            Some("/api/tags")
        );
        assert_eq!(
            ollama.extra_headers.get("x-local").map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn partial_provider_overlay_can_explicitly_replace_default_auth_style() {
        let overlay = parse_config_toml(
            r#"
            [providers.ollama]
            auth_style = "bearer"
            auth_env = "OLLAMA_API_KEY"
            "#,
        )
        .expect("provider overlay parses");

        let merged = merge_global_config(overlay);
        let ollama = merged
            .providers
            .get("ollama")
            .expect("ollama remains configured");

        assert_eq!(ollama.auth_style, "bearer");
        assert_eq!(auth_env_names(&ollama.auth_env), vec!["OLLAMA_API_KEY"]);
        assert_eq!(ollama.chat_endpoint, "/api/chat");
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
        assert_eq!(model, "unsloth/Qwen3.6-35B-A3B-UD-MLX-4bit");
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
        let _guard = crate::llm::env_guard();
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
    fn test_unknown_model_family_ignores_default_provider_fallback() {
        let _guard = crate::llm::env_guard();
        let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
        unsafe {
            std::env::set_var("HARN_DEFAULT_PROVIDER", "ollama");
        }

        let unknown = resolve_model_info("mystery-model-xyz");
        let known_family = resolve_model_info("deepseek-mystery-model");

        unsafe {
            match prev_default_provider {
                Some(value) => std::env::set_var("HARN_DEFAULT_PROVIDER", value),
                None => std::env::remove_var("HARN_DEFAULT_PROVIDER"),
            }
        }

        assert_eq!(unknown.provider, "ollama");
        assert_eq!(unknown.family, "unknown");
        assert_eq!(unknown.lineage, "unknown");
        assert_eq!(known_family.family, "deepseek");
        assert_eq!(known_family.lineage, "deepseek");
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
        // Tier is now declared on each model row; tier_rules is allowed
        // to be empty (the rule table is a legacy fallback only).
        assert_eq!(config.tier_defaults.default, "mid");
        // At least the new open-weight frontiers should have explicit tiers.
        let frontiers = config
            .models
            .iter()
            .filter(|(_, m)| m.tier.as_deref() == Some("frontier"))
            .count();
        assert!(
            frontiers >= 4,
            "expected at least 4 frontier-tagged models, got {frontiers}"
        );
    }

    #[test]
    fn test_local_ollama_catalog_metadata() {
        reset_overrides();

        let devstral =
            model_catalog_entry("devstral-small-2:24b").expect("devstral-small-2 catalog entry");
        assert_eq!(devstral.context_window, 262_144);
        assert!(!devstral.capabilities.iter().any(|cap| cap == "vision"));

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
            "native"
        );
        // devstral dropped its stale heredoc `text` pin (it has no reserved-token
        // constraint, so there was no structural reason to stay on heredoc) and
        // now inherits the global `json` text-channel default. Heredoc is still
        // reachable via an explicit `preferred_tool_format = "text"` pin.
        assert_eq!(
            default_tool_format("devstral-small-2:24b", "ollama"),
            "json"
        );
        // vLLM/SGLang-served Gemma 4 exposes OpenAI-compatible function calling,
        // so the local route declares native tools (matching every hosted gemma-4
        // sibling) rather than degrading to a text tool format.
        assert_eq!(default_tool_format("gemma-4-26b-a4b-it", "local"), "native");
        // deepseek-v3.2 and qwen3-coder both pin `text` in the capability
        // matrix, so they keep heredoc rather than inheriting the json default.
        assert_eq!(
            default_tool_format("deepseek/deepseek-v3.2", "openrouter"),
            "text"
        );
        assert_eq!(
            default_tool_format("qwen/qwen3-coder-flash", "openrouter"),
            "text"
        );
        // GPT-OSS tool defaults are provider-specific: aggregate OpenRouter and
        // Fireworks use Harn's heredoc text tools, while direct native-capable
        // hosts stay on provider-native tool calls.
        assert_eq!(
            default_tool_format("openai/gpt-oss-120b", "openrouter"),
            "text"
        );
        assert_eq!(
            default_tool_format("accounts/fireworks/models/gpt-oss-120b", "fireworks"),
            "text"
        );
        assert_eq!(default_tool_format("gpt-oss-120b", "cerebras"), "native");
        assert_eq!(
            default_tool_format("openai/gpt-oss-120b", "deepinfra"),
            "native"
        );
        assert_eq!(default_tool_format("openai/gpt-oss-120b", "groq"), "native");
    }

    #[test]
    fn test_default_tool_format_unpinned_text_channel_is_json() {
        reset_overrides();

        // GLOBAL DEFAULT FLIP: a model with no capability-matrix pin and no
        // native tool support resolves to fenced-json (`json`), not heredoc
        // (`text`). This is the behavior change — an unknown text-channel model
        // gets the delimiter-safe default. (Native-capable unknowns still get
        // `native`; pinned models still honor their pin, covered above.)
        assert_eq!(default_tool_format("mystery-model-xyz", "ollama"), "json");
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
                logical_model: None,
                equivalence_group: None,
                served_variant: None,
                wire_model: None,
                api_dialect: None,
                rate_limits: None,
                architecture: None,
                local_memory: None,
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
                superseded_by: None,
                fast_mode: None,
                quality_tags: Vec::new(),
                availability: ModelAvailability::default(),
                tier: None,
                open_weight: None,
                strengths: Vec::new(),
                benchmarks: std::collections::BTreeMap::new(),
                family: None,
                lineage: None,
                complementary_with: Vec::new(),
                avoid_as_reviewer_for: Vec::new(),
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
    fn embedded_cerebras_catalog_separates_public_and_dedicated_routes() {
        let config = default_config();
        for id in ["gpt-oss-120b", "zai-glm-4.7"] {
            let model = config.models.get(id).expect("current public Cerebras row");
            assert_eq!(model.provider, "cerebras");
            assert_eq!(model.availability, ModelAvailability::Serverless);
            assert!(!model.deprecated);
        }

        let llama = config
            .models
            .get("llama-3.3-70b")
            .expect("legacy Cerebras row");
        assert_eq!(llama.provider, "cerebras");
        assert_eq!(llama.availability, ModelAvailability::Dedicated);
        assert!(llama.deprecated);
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

    #[test]
    fn opus_alias_tracks_claude_opus_4_8_with_fast_mode() {
        // The `opus` alias must follow the newest Opus release, and that
        // release advertises its (off-by-default) fast-mode tier.
        let (model, provider) = resolve_model("opus");
        assert_eq!(model, "claude-opus-4-8");
        assert_eq!(provider.as_deref(), Some("anthropic"));

        let opus48 = model_catalog_entry("claude-opus-4-8").expect("opus 4.8 catalog entry");
        assert!(!opus48.deprecated, "newest Opus must not be deprecated");
        let fast = opus48.fast_mode.expect("opus 4.8 advertises fast mode");
        assert_eq!(fast.param, "speed");
        assert_eq!(fast.value, "fast");
        assert_eq!(fast.status.as_deref(), Some("research_preview"));
        let fast_pricing = fast.pricing.expect("fast mode carries premium pricing");
        let standard = opus48.pricing.expect("opus 4.8 standard pricing");
        assert!(
            fast_pricing.input_per_mtok > standard.input_per_mtok,
            "fast mode must be premium-priced relative to standard"
        );
    }

    #[test]
    fn superseded_opus_models_point_at_claude_opus_4_8() {
        // Earlier Opus rows are deprecated and carry a structured
        // `superseded_by` pointer to the current flagship.
        for model in ["claude-opus-4-7", "claude-opus-4-6"] {
            let entry =
                model_catalog_entry(model).unwrap_or_else(|| panic!("{model} catalog entry"));
            assert!(entry.deprecated, "{model} should be deprecated");
            assert_eq!(
                entry.superseded_by.as_deref(),
                Some("claude-opus-4-8"),
                "{model} should be superseded by claude-opus-4-8"
            );
        }
    }

    #[test]
    fn gpt_5_5_fast_mode_rides_service_tier() {
        // Fast mode is provider-agnostic: OpenAI exposes it through the
        // `service_tier` knob rather than Anthropic's `speed`.
        let entry = model_catalog_entry("gpt-5.5").expect("gpt-5.5 catalog entry");
        let fast = entry.fast_mode.expect("gpt-5.5 advertises a fast tier");
        assert_eq!(fast.param, "service_tier");
        assert_eq!(fast.status.as_deref(), Some("ga"));
    }
}
