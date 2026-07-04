//! Provider serving definitions: the `ProviderDef` runtime shape, its wire
//! deserialization form, overlay merge, auth-env selector, and base-URL
//! resolution.
use std::collections::BTreeMap;

use serde::Deserialize;

use super::*;

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
    /// How this provider's credentials are resolved. `"env"` (default) means
    /// the generic `auth_env` lookup is authoritative: missing env vars are a
    /// hard "missing API key" error. `"platform_managed"` means the provider's
    /// own shim resolves credentials through a multi-step chain the generic
    /// `auth_env` lookup cannot see (e.g. Bedrock's AWS credential chain —
    /// env/profile/container/instance-role — or Vertex's bearer token /
    /// service-account JSON / ADC). Callers must skip the generic `auth_env`
    /// requirement for these providers and let the shim fail on its own if
    /// credentials are truly absent, instead of hardcoding provider names.
    pub credential_resolution: String,
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
    /// Optional provider-level serving performance observations.
    pub performance: Option<ServingPerformanceDef>,
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
    credential_resolution: Option<String>,
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
    #[serde(default)]
    performance: Option<ServingPerformanceDef>,
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
            credential_resolution: wire
                .credential_resolution
                .unwrap_or_else(default_credential_resolution),
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
            performance: wire.performance,
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
            credential_resolution: default_credential_resolution(),
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
            performance: None,
            auth_style_explicit: false,
        }
    }
}

impl ProviderDef {
    pub(crate) fn merge_from(&mut self, overlay: &ProviderDef) {
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
        if overlay.credential_resolution != default_credential_resolution() {
            self.credential_resolution = overlay.credential_resolution.clone();
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
        merge_option(&mut self.performance, &overlay.performance);
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

fn default_credential_resolution() -> String {
    "env".to_string()
}

impl ProviderDef {
    /// Whether this provider resolves its own credentials through a
    /// multi-step chain (AWS SigV4 credential chain, GCP ADC / service
    /// account JSON, etc.) rather than the generic `auth_env` lookup.
    /// Callers that would otherwise hardcode a provider-name match (e.g.
    /// "does this provider need the generic missing-API-key error") should
    /// read this instead.
    pub fn is_credential_resolution_platform_managed(&self) -> bool {
        self.credential_resolution == "platform_managed"
    }
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
