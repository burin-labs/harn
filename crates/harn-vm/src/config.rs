//! Canonical layered Harn runtime configuration.
//!
//! The VM owns the typed shape and deterministic merge engine so hosts can
//! inspect, validate, and explain configuration without depending on the CLI's
//! `harn.toml` package manifest model.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map as JsonMap, Value as JsonValue};

use crate::redact::current_policy;

pub const CONFIG_SCHEMA_VERSION: u32 = 1;
pub const CONFIG_SCHEMA_ID: &str = "https://harnlang.com/schemas/harn-config.schema.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct HarnConfig {
    pub schema_version: u32,
    pub models: ModelPolicyConfig,
    pub permissions: PermissionConfig,
    pub endpoints: EndpointCatalogConfig,
    pub packages: PackageSourcesConfig,
    pub skills: SkillSourcesConfig,
    pub plugins: PluginSourcesConfig,
    pub logging: LoggingConfig,
    pub retention: RetentionConfig,
    pub redaction: RedactionConfig,
    pub replay: ReplayConfig,
    pub limits: RuntimeLimitsConfig,
    pub policy: ManagedPolicyConfig,
    pub security: SecurityConfig,
    pub identity: IdentityConfig,
}

impl Default for HarnConfig {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            models: ModelPolicyConfig::default(),
            permissions: PermissionConfig::default(),
            endpoints: EndpointCatalogConfig::default(),
            packages: PackageSourcesConfig::default(),
            skills: SkillSourcesConfig::default(),
            plugins: PluginSourcesConfig::default(),
            logging: LoggingConfig::default(),
            retention: RetentionConfig::default(),
            redaction: RedactionConfig::default(),
            replay: ReplayConfig::default(),
            limits: RuntimeLimitsConfig::default(),
            policy: ManagedPolicyConfig::default(),
            security: SecurityConfig::default(),
            identity: IdentityConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ModelPolicyConfig {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub capability_refs: Vec<String>,
    pub providers: BTreeMap<String, ProviderPolicyConfig>,
    pub aliases: BTreeMap<String, ModelAliasConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderPolicyConfig {
    pub base_url: Option<String>,
    pub auth_env: Vec<String>,
    pub capability_refs: Vec<String>,
    pub models: Vec<String>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ModelAliasConfig {
    pub model: String,
    pub provider: String,
    pub capability_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    Allow,
    #[default]
    Ask,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct PermissionConfig {
    pub default: PermissionMode,
    pub capabilities: BTreeMap<String, PermissionMode>,
}

impl Default for PermissionConfig {
    fn default() -> Self {
        Self {
            default: PermissionMode::Ask,
            capabilities: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct IdentityConfig {
    pub scope_attenuation: crate::actor_chain::ScopeAttenuationPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct EndpointCatalogConfig {
    pub mcp: BTreeMap<String, EndpointConfig>,
    pub a2a: BTreeMap<String, EndpointConfig>,
    pub acp: BTreeMap<String, EndpointConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct EndpointConfig {
    pub enabled: bool,
    pub url: Option<String>,
    pub command: Vec<String>,
    pub transport: Option<String>,
    pub headers: BTreeMap<String, String>,
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            url: None,
            command: Vec::new(),
            transport: None,
            headers: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct PackageSourcesConfig {
    pub sources: Vec<SourceConfig>,
    pub lockfile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SkillSourcesConfig {
    pub paths: Vec<String>,
    pub sources: Vec<SourceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct PluginSourcesConfig {
    pub sources: Vec<SourceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SourceConfig {
    pub name: String,
    pub kind: String,
    pub url: Option<String>,
    pub path: Option<String>,
    pub trust: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub level: LogLevel,
    pub format: String,
    pub file: Option<String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            format: "text".to_string(),
            file: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionConfig {
    pub days: Option<u64>,
    pub max_bytes: Option<u64>,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            days: Some(30),
            max_bytes: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RedactionMode {
    Off,
    #[default]
    Standard,
    Strict,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RedactionConfig {
    pub mode: RedactionMode,
    pub extra_fields: Vec<String>,
    pub extra_url_params: Vec<String>,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            mode: RedactionMode::Standard,
            extra_fields: Vec::new(),
            extra_url_params: Vec::new(),
        }
    }
}

/// Prompt-injection defense posture for the runtime (defense Layers 0/1).
///
/// `Off` disables every layer. `Spotlight` (the default) frames untrusted
/// external tool/MCP output as data and gates exfiltration when context is
/// tainted. `Strict` additionally datamarks every line of untrusted content.
/// `LocalMl` adds the on-device-classifier tier (Layer 2): untrusted content is
/// scored by an injection classifier (the built-in heuristic by default, or a
/// downloadable `harn-guard` neural model when installed), and a flagged score
/// tightens the trifecta gate. It is a superset of `Spotlight`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SecurityMode {
    Off,
    #[default]
    Spotlight,
    Strict,
    LocalMl,
}

impl SecurityMode {
    /// Parse from the stable wire string. Unknown values fall back to the
    /// safe default (`Spotlight`).
    pub fn parse(value: &str) -> Self {
        match value {
            "off" => Self::Off,
            "spotlight" => Self::Spotlight,
            "strict" => Self::Strict,
            "local-ml" | "local_ml" => Self::LocalMl,
            _ => Self::Spotlight,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Spotlight => "spotlight",
            Self::Strict => "strict",
            Self::LocalMl => "local-ml",
        }
    }
}

/// `[security]` configuration. The runtime substrate lives in
/// [`crate::security`]; this is the typed, layerable shape hosts persist and
/// merge. Defaults are on (deterministic, free) so the runtime is
/// secure-by-default; the trifecta gate only takes effect where an interactive
/// approval policy is installed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityConfig {
    pub mode: SecurityMode,
    /// Frame untrusted external tool/MCP output in spotlight delimiters.
    pub spotlight_external: bool,
    /// Neutralize reserved chat-template special tokens (`<|im_start|>`,
    /// `[INST]`, `<|eot_id|>`, …) inside untrusted spans so they cannot re-open
    /// turns or inject a system message (ChatBug / ChatInject / MetaBreak). On by
    /// default for every non-`off` mode.
    pub neutralize_special_tokens: bool,
    /// Destyle forged turn/reasoning markers (line-leading `User:`/`Assistant:`/
    /// `System:` labels and `<think>` tags) inside untrusted spans so injected
    /// content cannot read as a real turn or chain-of-thought. On by default for
    /// every non-`off` mode.
    pub destyle_untrusted: bool,
    /// Apply the lethal-trifecta gate: force confirmation when tainted context
    /// reaches an exfiltration-capable or destructive tool.
    pub trifecta_gate: bool,
    /// Pin + hash MCP tool schemas; require re-approval when a server mutates a
    /// tool description after first approval (rug-pull defense).
    pub pin_mcp_schemas: bool,
    /// Authenticate cross-agent / orchestration directives on the read path.
    /// A directive-looking span (`Orchestrator directive:` …) that lacks a valid
    /// process-scoped provenance stamp is tagged untrusted and quarantined via
    /// the taint/trifecta gate, so a forged directive planted in an untrusted
    /// subagent result cannot be obeyed as authoritative. Default OFF (net-new
    /// enforcement); byte-identical behaviour when disabled.
    pub authenticate_directives: bool,
    /// Also gate reads of well-known secret/credential files while tainted.
    pub gate_secret_reads: bool,
    /// Score untrusted content with an injection classifier (Layer 2). Implied
    /// by `mode = "local-ml"`; can be opted into under `spotlight`/`strict` too.
    /// The classifier is the built-in heuristic unless a `harn-guard` neural
    /// model is registered. A flagged score tightens the trifecta gate.
    pub detect_injection: bool,
    /// Malicious-probability threshold, as a percent in `[0, 100]`, at or above
    /// which the classifier marks content as flagged. Kept as an integer so the
    /// config stays `Eq`-comparable and round-trips losslessly.
    pub guard_threshold_percent: u8,
    /// Selector for the downloadable neural classifier used when
    /// `detect_injection` is on: a `harn guard` catalog name (the default) or a
    /// path to a model directory. Resolved lazily by the host's `harn-guard`
    /// loader; an empty value or an uninstalled model keeps the built-in
    /// heuristic. Ignored by binaries built without the guard inference backend.
    pub guard_model: String,
    /// MCP servers the operator has explicitly trusted (skip taint + pinning).
    pub trusted_mcp_servers: Vec<String>,
}

/// Default neural-classifier selector: the ungated, Apache-2.0 catalog default.
/// Mirrors `harn_guard::DEFAULT_MODEL` (asserted equal by a `harn-guard` test);
/// kept here so `harn-vm` stays free of a dependency on `harn-guard`.
pub const DEFAULT_GUARD_MODEL: &str = "deberta-v3-prompt-injection-v2";

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            mode: SecurityMode::Spotlight,
            spotlight_external: true,
            neutralize_special_tokens: true,
            destyle_untrusted: true,
            trifecta_gate: true,
            pin_mcp_schemas: true,
            authenticate_directives: false,
            gate_secret_reads: true,
            detect_injection: false,
            guard_threshold_percent: 50,
            guard_model: DEFAULT_GUARD_MODEL.to_owned(),
            trusted_mcp_servers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ReplayConfig {
    pub enabled: bool,
    pub directory: Option<String>,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            directory: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkMode {
    Allow,
    #[default]
    Ask,
    Deny,
    Offline,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FilesystemMode {
    ReadWrite,
    ReadOnly,
    #[default]
    Sandboxed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    Host,
    #[default]
    Process,
    Container,
    Worktree,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeLimitsConfig {
    pub budget_usd: Option<f64>,
    pub tokens: Option<u64>,
    pub concurrency: Option<u64>,
    pub network: NetworkMode,
    pub filesystem: FilesystemMode,
    pub sandbox: SandboxMode,
}

impl Default for RuntimeLimitsConfig {
    fn default() -> Self {
        Self {
            budget_usd: None,
            tokens: None,
            concurrency: None,
            network: NetworkMode::Ask,
            filesystem: FilesystemMode::Sandboxed,
            sandbox: SandboxMode::Process,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ManagedPolicyConfig {
    pub locked_fields: Vec<String>,
    pub denied_fields: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ConfigLayerKind {
    BuiltInDefaults,
    RuntimeInstallDefaults,
    RemoteDefaults,
    UserConfig,
    ProjectConfig,
    RepoConfig,
    ManagedPolicy,
    EnvironmentOverrides,
}

impl ConfigLayerKind {
    pub fn label(self) -> &'static str {
        match self {
            ConfigLayerKind::BuiltInDefaults => "built-in defaults",
            ConfigLayerKind::RuntimeInstallDefaults => "runtime install defaults",
            ConfigLayerKind::RemoteDefaults => "remote defaults",
            ConfigLayerKind::UserConfig => "user config",
            ConfigLayerKind::ProjectConfig => "project config",
            ConfigLayerKind::RepoConfig => "repo config",
            ConfigLayerKind::ManagedPolicy => "managed policy",
            ConfigLayerKind::EnvironmentOverrides => "environment overrides",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigLayer {
    pub kind: ConfigLayerKind,
    pub name: String,
    pub source: String,
    pub value: JsonValue,
}

impl ConfigLayer {
    pub fn new(
        kind: ConfigLayerKind,
        name: impl Into<String>,
        source: impl Into<String>,
        value: JsonValue,
    ) -> Self {
        Self {
            kind,
            name: name.into(),
            source: source.into(),
            value,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LayerSummary {
    pub name: String,
    pub kind: ConfigLayerKind,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FieldCandidate {
    pub layer: String,
    pub kind: ConfigLayerKind,
    pub source: String,
    pub status: CandidateStatus,
    pub value: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    Applied,
    Shadowed,
    Locked,
    Denied,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FieldExplanation {
    pub path: String,
    pub value: JsonValue,
    pub source: String,
    pub layer: String,
    pub kind: ConfigLayerKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied_by: Option<String>,
    pub candidates: Vec<FieldCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedConfig {
    #[serde(skip_serializing)]
    pub config: HarnConfig,
    pub redacted_config: JsonValue,
    pub layers: Vec<LayerSummary>,
    pub explain: Vec<FieldExplanation>,
}

#[derive(Debug)]
pub enum ConfigError {
    ParseToml { source: String, message: String },
    ParseJson { source: String, message: String },
    InvalidConfig { source: String, message: String },
    InvalidPath { path: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::ParseToml { source, message } => {
                write!(f, "failed to parse TOML config {source}: {message}")
            }
            ConfigError::ParseJson { source, message } => {
                write!(f, "failed to parse JSON config {source}: {message}")
            }
            ConfigError::InvalidConfig { source, message } => {
                write!(f, "invalid config {source}: {message}")
            }
            ConfigError::InvalidPath { path } => {
                write!(f, "invalid config field path `{path}`")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

pub fn built_in_defaults_layer() -> ConfigLayer {
    ConfigLayer::new(
        ConfigLayerKind::BuiltInDefaults,
        "built-in defaults",
        "harn-vm",
        serde_json::to_value(HarnConfig::default()).expect("default config serializes"),
    )
}

pub fn layer_from_providers_config(
    kind: ConfigLayerKind,
    name: impl Into<String>,
    source: impl Into<String>,
    providers: &crate::llm_config::ProvidersConfig,
) -> ConfigLayer {
    let mut canonical_providers = JsonMap::new();
    for (provider_name, provider) in &providers.providers {
        canonical_providers.insert(
            provider_name.clone(),
            json!({
                "base_url": provider.base_url,
                "auth_env": crate::llm_config::auth_env_names(&provider.auth_env),
                "capability_refs": provider.features,
                "models": [],
                "metadata": {
                    "auth_style": provider.auth_style,
                    "chat_endpoint": provider.chat_endpoint,
                    "completion_endpoint": provider.completion_endpoint,
                }
            }),
        );
    }
    for (model_id, model) in &providers.models {
        let entry = canonical_providers
            .entry(model.provider.clone())
            .or_insert_with(|| {
                json!({
                    "base_url": null,
                    "auth_env": [],
                    "capability_refs": [],
                    "models": [],
                    "metadata": {}
                })
            });
        if let Some(models) = entry.get_mut("models").and_then(JsonValue::as_array_mut) {
            models.push(JsonValue::String(model_id.clone()));
        }
    }
    let aliases = providers
        .aliases
        .iter()
        .map(|(alias, entry)| {
            (
                alias.clone(),
                json!({
                    "model": entry.id,
                    "provider": entry.provider,
                    "capability_refs": [],
                }),
            )
        })
        .collect::<JsonMap<String, JsonValue>>();
    ConfigLayer::new(
        kind,
        name,
        source,
        json!({
            "models": {
                "default_provider": providers.default_provider,
                "providers": canonical_providers,
                "aliases": aliases,
            }
        }),
    )
}

pub fn parse_config_toml(
    content: &str,
    source: impl Into<String>,
) -> Result<JsonValue, ConfigError> {
    let source = source.into();
    let value = toml::from_str::<toml::Value>(content).map_err(|error| ConfigError::ParseToml {
        source: source.clone(),
        message: sanitized_error_message(error),
    })?;
    let json = serde_json::to_value(value).map_err(|error| ConfigError::InvalidConfig {
        source: source.clone(),
        message: error.to_string(),
    })?;
    validate_layer_value(&json, &source)?;
    Ok(json)
}

pub fn parse_config_json(
    content: &str,
    source: impl Into<String>,
) -> Result<JsonValue, ConfigError> {
    let source = source.into();
    let json =
        serde_json::from_str::<JsonValue>(content).map_err(|error| ConfigError::ParseJson {
            source: source.clone(),
            message: sanitized_error_message(error),
        })?;
    validate_layer_value(&json, &source)?;
    Ok(json)
}

pub fn parse_manifest_config_table(
    content: &str,
    source: impl Into<String>,
) -> Result<Option<JsonValue>, ConfigError> {
    let source = source.into();
    let value = toml::from_str::<toml::Value>(content).map_err(|error| ConfigError::ParseToml {
        source: source.clone(),
        message: sanitized_error_message(error),
    })?;
    let Some(table) = value.as_table() else {
        return Ok(None);
    };
    let Some(config) = table.get("config") else {
        return Ok(None);
    };
    let json = serde_json::to_value(config).map_err(|error| ConfigError::InvalidConfig {
        source: source.clone(),
        message: error.to_string(),
    })?;
    validate_layer_value(&json, &source)?;
    Ok(Some(json))
}

pub fn environment_layer<I, K, V>(vars: I) -> Result<Option<ConfigLayer>, ConfigError>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    let vars: BTreeMap<String, String> = vars
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    let mut value = match vars.get("HARN_CONFIG_JSON") {
        Some(raw) if !raw.trim().is_empty() => parse_config_json(raw, "HARN_CONFIG_JSON")?,
        _ => JsonValue::Object(JsonMap::new()),
    };

    set_env_string(
        &mut value,
        &vars,
        "HARN_DEFAULT_PROVIDER",
        "models.default_provider",
    )?;
    set_env_string(
        &mut value,
        &vars,
        "HARN_DEFAULT_MODEL",
        "models.default_model",
    )?;
    set_env_enum(&mut value, &vars, "HARN_LOG_LEVEL", "logging.level")?;
    set_env_enum(&mut value, &vars, "HARN_REDACTION_MODE", "redaction.mode")?;
    set_env_enum(&mut value, &vars, "HARN_NETWORK_MODE", "limits.network")?;
    set_env_enum(
        &mut value,
        &vars,
        "HARN_FILESYSTEM_MODE",
        "limits.filesystem",
    )?;
    set_env_enum(&mut value, &vars, "HARN_SANDBOX_MODE", "limits.sandbox")?;
    set_env_u64(&mut value, &vars, "HARN_RETENTION_DAYS", "retention.days")?;
    set_env_u64(&mut value, &vars, "HARN_TOKEN_BUDGET", "limits.tokens")?;
    set_env_u64(
        &mut value,
        &vars,
        "HARN_MAX_CONCURRENCY",
        "limits.concurrency",
    )?;
    set_env_f64(&mut value, &vars, "HARN_BUDGET_USD", "limits.budget_usd")?;
    set_env_bool(&mut value, &vars, "HARN_REPLAY_ENABLED", "replay.enabled")?;

    if value.as_object().is_some_and(JsonMap::is_empty) {
        return Ok(None);
    }
    validate_layer_value(&value, "environment overrides")?;
    Ok(Some(ConfigLayer::new(
        ConfigLayerKind::EnvironmentOverrides,
        "environment overrides",
        "process environment",
        value,
    )))
}

pub fn merge_layers(layers: Vec<ConfigLayer>) -> Result<ResolvedConfig, ConfigError> {
    let mut merged = JsonValue::Object(JsonMap::new());
    let mut candidate_map: BTreeMap<String, Vec<FieldCandidate>> = BTreeMap::new();
    let mut winner_map: BTreeMap<String, (String, String, ConfigLayerKind)> = BTreeMap::new();
    let mut locked: BTreeMap<String, String> = BTreeMap::new();
    let mut denied: BTreeMap<String, String> = BTreeMap::new();
    let mut summaries = Vec::new();

    for layer in layers {
        validate_layer_value(&layer.value, &layer.source)?;
        let display_source = redact_display(&layer.source);
        summaries.push(LayerSummary {
            name: layer.name.clone(),
            kind: layer.kind,
            source: display_source.clone(),
        });

        let leaves = leaf_values(&layer.value);
        for (path, value) in leaves {
            if path == "policy.locked_fields" || path == "policy.denied_fields" {
                apply_candidate(
                    &mut merged,
                    &mut candidate_map,
                    &mut winner_map,
                    &layer,
                    &path,
                    value,
                )?;
                continue;
            }
            if let Some((policy_path, source)) = first_policy_match(&denied, &path) {
                push_blocked_candidate(
                    &mut candidate_map,
                    &layer,
                    &path,
                    value,
                    CandidateStatus::Denied,
                    format!("{source} denied {policy_path}"),
                );
                continue;
            }
            if let Some((policy_path, source)) = first_policy_match(&locked, &path) {
                push_blocked_candidate(
                    &mut candidate_map,
                    &layer,
                    &path,
                    value,
                    CandidateStatus::Locked,
                    format!("{source} locked {policy_path}"),
                );
                continue;
            }
            apply_candidate(
                &mut merged,
                &mut candidate_map,
                &mut winner_map,
                &layer,
                &path,
                value,
            )?;
        }

        if layer.kind == ConfigLayerKind::ManagedPolicy {
            for path in string_list_at(&layer.value, "policy.locked_fields") {
                validate_field_path(&path)?;
                locked.insert(path, display_source.clone());
            }
            for path in string_list_at(&layer.value, "policy.denied_fields") {
                validate_field_path(&path)?;
                denied.insert(path.clone(), display_source.clone());
                apply_denied_policy(
                    &mut merged,
                    &mut candidate_map,
                    &mut winner_map,
                    &path,
                    &display_source,
                )?;
            }
        }
    }

    let config: HarnConfig =
        serde_json::from_value(merged.clone()).map_err(|error| ConfigError::InvalidConfig {
            source: "merged config".to_string(),
            message: error.to_string(),
        })?;
    let redacted_config = current_policy().redact_json(&merged);
    let mut explain = Vec::new();
    for (path, value) in leaf_values(&merged) {
        let Some((source, layer, kind)) = winner_map.get(&path).cloned() else {
            continue;
        };
        let locked_by = first_policy_match(&locked, &path).map(|(_, source)| source);
        let denied_by = first_policy_match(&denied, &path).map(|(_, source)| source);
        let mut candidates = candidate_map.remove(&path).unwrap_or_default();
        for candidate in &mut candidates {
            candidate.value = redact_value_at_path(&path, candidate.value.clone());
        }
        explain.push(FieldExplanation {
            path: path.clone(),
            value: redact_value_at_path(&path, value),
            source,
            layer,
            kind,
            locked_by,
            denied_by,
            candidates,
        });
    }
    for (path, mut candidates) in candidate_map {
        if candidates.is_empty() {
            continue;
        }
        for candidate in &mut candidates {
            candidate.value = redact_value_at_path(&path, candidate.value.clone());
        }
        let locked_by = first_policy_match(&locked, &path).map(|(_, source)| source);
        let denied_by = first_policy_match(&denied, &path).map(|(_, source)| source);
        explain.push(FieldExplanation {
            path: path.clone(),
            value: JsonValue::Null,
            source: "<blocked>".to_string(),
            layer: "<blocked>".to_string(),
            kind: candidates
                .last()
                .map(|candidate| candidate.kind)
                .unwrap_or(ConfigLayerKind::BuiltInDefaults),
            locked_by,
            denied_by,
            candidates,
        });
    }
    explain.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(ResolvedConfig {
        config,
        redacted_config,
        layers: summaries,
        explain,
    })
}

pub fn validate_policy_paths(value: &JsonValue) -> Result<(), ConfigError> {
    for path in string_list_at(value, "policy.locked_fields")
        .into_iter()
        .chain(string_list_at(value, "policy.denied_fields"))
    {
        validate_field_path(&path)?;
    }
    Ok(())
}

pub fn schema_json() -> JsonValue {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": CONFIG_SCHEMA_ID,
        "title": "Harn runtime config",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schema_version": {"type": "integer", "const": CONFIG_SCHEMA_VERSION},
            "models": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "default_provider": {"type": ["string", "null"]},
                    "default_model": {"type": ["string", "null"]},
                    "capability_refs": {"type": "array", "items": {"type": "string"}},
                    "providers": {"type": "object", "additionalProperties": {"$ref": "#/$defs/provider"}},
                    "aliases": {"type": "object", "additionalProperties": {"$ref": "#/$defs/model_alias"}}
                }
            },
            "permissions": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "default": {"$ref": "#/$defs/permission_mode"},
                    "capabilities": {"type": "object", "additionalProperties": {"$ref": "#/$defs/permission_mode"}}
                }
            },
            "endpoints": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "mcp": {"type": "object", "additionalProperties": {"$ref": "#/$defs/endpoint"}},
                    "a2a": {"type": "object", "additionalProperties": {"$ref": "#/$defs/endpoint"}},
                    "acp": {"type": "object", "additionalProperties": {"$ref": "#/$defs/endpoint"}}
                }
            },
            "packages": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "sources": {"type": "array", "items": {"$ref": "#/$defs/source"}},
                    "lockfile": {"type": ["string", "null"]}
                }
            },
            "skills": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "paths": {"type": "array", "items": {"type": "string"}},
                    "sources": {"type": "array", "items": {"$ref": "#/$defs/source"}}
                }
            },
            "plugins": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "sources": {"type": "array", "items": {"$ref": "#/$defs/source"}}
                }
            },
            "logging": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "level": {"enum": ["error", "warn", "info", "debug", "trace"]},
                    "format": {"type": "string"},
                    "file": {"type": ["string", "null"]}
                }
            },
            "retention": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "days": {"type": ["integer", "null"], "minimum": 0},
                    "max_bytes": {"type": ["integer", "null"], "minimum": 0}
                }
            },
            "redaction": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "mode": {"enum": ["off", "standard", "strict"]},
                    "extra_fields": {"type": "array", "items": {"type": "string"}},
                    "extra_url_params": {"type": "array", "items": {"type": "string"}}
                }
            },
            "replay": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "enabled": {"type": "boolean"},
                    "directory": {"type": ["string", "null"]}
                }
            },
            "limits": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "budget_usd": {"type": ["number", "null"], "minimum": 0},
                    "tokens": {"type": ["integer", "null"], "minimum": 0},
                    "concurrency": {"type": ["integer", "null"], "minimum": 0},
                    "network": {"enum": ["allow", "ask", "deny", "offline"]},
                    "filesystem": {"enum": ["read-write", "read-only", "sandboxed"]},
                    "sandbox": {"enum": ["host", "process", "container", "worktree"]}
                }
            },
            "policy": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "locked_fields": {"type": "array", "items": {"type": "string"}},
                    "denied_fields": {"type": "array", "items": {"type": "string"}}
                }
            },
            "security": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "mode": {"enum": ["off", "spotlight", "strict", "local-ml"]},
                    "spotlight_external": {"type": "boolean"},
                    "neutralize_special_tokens": {"type": "boolean"},
                    "destyle_untrusted": {"type": "boolean"},
                    "trifecta_gate": {"type": "boolean"},
                    "pin_mcp_schemas": {"type": "boolean"},
                    "authenticate_directives": {"type": "boolean"},
                    "gate_secret_reads": {"type": "boolean"},
                    "detect_injection": {"type": "boolean"},
                    "guard_threshold_percent": {"type": "integer", "minimum": 0, "maximum": 100},
                    "guard_model": {"type": "string"},
                    "trusted_mcp_servers": {"type": "array", "items": {"type": "string"}}
                }
            },
            "identity": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "scope_attenuation": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "mode": {"enum": ["off", "non-increasing", "strict-subset"]},
                            "alert_on_violation": {"type": "boolean"}
                        }
                    }
                }
            }
        },
        "$defs": {
            "permission_mode": {"enum": ["allow", "ask", "deny"]},
            "provider": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "base_url": {"type": ["string", "null"]},
                    "auth_env": {"type": "array", "items": {"type": "string"}},
                    "capability_refs": {"type": "array", "items": {"type": "string"}},
                    "models": {"type": "array", "items": {"type": "string"}},
                    "metadata": {"type": "object"}
                }
            },
            "model_alias": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "model": {"type": "string"},
                    "provider": {"type": "string"},
                    "capability_refs": {"type": "array", "items": {"type": "string"}}
                }
            },
            "endpoint": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "enabled": {"type": "boolean"},
                    "url": {"type": ["string", "null"]},
                    "command": {"type": "array", "items": {"type": "string"}},
                    "transport": {"type": ["string", "null"]},
                    "headers": {"type": "object", "additionalProperties": {"type": "string"}}
                }
            },
            "source": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "name": {"type": "string"},
                    "kind": {"type": "string"},
                    "url": {"type": ["string", "null"]},
                    "path": {"type": ["string", "null"]},
                    "trust": {"type": ["string", "null"]}
                }
            }
        }
    })
}

pub fn install_config_path_for_os(os: &str, program_data: Option<&str>) -> PathBuf {
    if os == "windows" {
        PathBuf::from(program_data.unwrap_or(r"C:\ProgramData")).join(r"Harn\config.toml")
    } else {
        PathBuf::from("/etc/harn/config.toml")
    }
}

pub fn user_config_path_for_os(
    os: &str,
    home: Option<&str>,
    xdg_config_home: Option<&str>,
    appdata: Option<&str>,
) -> Option<PathBuf> {
    if os == "windows" {
        return appdata.map(|root| PathBuf::from(root).join(r"Harn\config.toml"));
    }
    if let Some(root) = xdg_config_home.filter(|value| !value.trim().is_empty()) {
        return Some(PathBuf::from(root).join("harn").join("config.toml"));
    }
    home.map(|root| {
        PathBuf::from(root)
            .join(".config")
            .join("harn")
            .join("config.toml")
    })
}

fn validate_layer_value(value: &JsonValue, source: &str) -> Result<(), ConfigError> {
    serde_json::from_value::<HarnConfig>(value.clone()).map_err(|error| {
        ConfigError::InvalidConfig {
            source: source.to_string(),
            message: error.to_string(),
        }
    })?;
    Ok(())
}

fn sanitized_error_message(error: impl ToString) -> String {
    let message = error
        .to_string()
        .lines()
        .next()
        .unwrap_or("parse error")
        .to_string();
    current_policy().redact_string(&message).into_owned()
}

fn set_env_string(
    value: &mut JsonValue,
    vars: &BTreeMap<String, String>,
    env_key: &str,
    path: &str,
) -> Result<(), ConfigError> {
    if let Some(raw) = vars
        .get(env_key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        set_path(value, path, JsonValue::String(raw.to_string()))?;
    }
    Ok(())
}

fn set_env_enum(
    value: &mut JsonValue,
    vars: &BTreeMap<String, String>,
    env_key: &str,
    path: &str,
) -> Result<(), ConfigError> {
    if let Some(raw) = vars
        .get(env_key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        let normalized = raw.to_ascii_lowercase().replace('_', "-");
        set_path(value, path, JsonValue::String(normalized))?;
    }
    Ok(())
}

fn set_env_u64(
    value: &mut JsonValue,
    vars: &BTreeMap<String, String>,
    env_key: &str,
    path: &str,
) -> Result<(), ConfigError> {
    if let Some(raw) = vars
        .get(env_key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        let parsed = raw
            .parse::<u64>()
            .map_err(|error| ConfigError::InvalidConfig {
                source: env_key.to_string(),
                message: error.to_string(),
            })?;
        set_path(value, path, json!(parsed))?;
    }
    Ok(())
}

fn set_env_f64(
    value: &mut JsonValue,
    vars: &BTreeMap<String, String>,
    env_key: &str,
    path: &str,
) -> Result<(), ConfigError> {
    if let Some(raw) = vars
        .get(env_key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        let parsed = raw
            .parse::<f64>()
            .map_err(|error| ConfigError::InvalidConfig {
                source: env_key.to_string(),
                message: error.to_string(),
            })?;
        set_path(value, path, json!(parsed))?;
    }
    Ok(())
}

fn set_env_bool(
    value: &mut JsonValue,
    vars: &BTreeMap<String, String>,
    env_key: &str,
    path: &str,
) -> Result<(), ConfigError> {
    if let Some(raw) = vars
        .get(env_key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        let parsed = match raw.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => {
                return Err(ConfigError::InvalidConfig {
                    source: env_key.to_string(),
                    message: "expected one of true/false, yes/no, on/off, or 1/0".to_string(),
                });
            }
        };
        set_path(value, path, json!(parsed))?;
    }
    Ok(())
}

fn apply_candidate(
    merged: &mut JsonValue,
    candidate_map: &mut BTreeMap<String, Vec<FieldCandidate>>,
    winner_map: &mut BTreeMap<String, (String, String, ConfigLayerKind)>,
    layer: &ConfigLayer,
    path: &str,
    value: JsonValue,
) -> Result<(), ConfigError> {
    if let Some(candidates) = candidate_map.get_mut(path) {
        if let Some(previous) = candidates
            .iter_mut()
            .rev()
            .find(|candidate| candidate.status == CandidateStatus::Applied)
        {
            previous.status = CandidateStatus::Shadowed;
        }
    }
    set_path(merged, path, value.clone())?;
    candidate_map
        .entry(path.to_string())
        .or_default()
        .push(FieldCandidate {
            layer: layer.name.clone(),
            kind: layer.kind,
            source: redact_display(&layer.source),
            status: CandidateStatus::Applied,
            value,
            blocked_by: None,
        });
    winner_map.insert(
        path.to_string(),
        (
            redact_display(&layer.source),
            layer.name.clone(),
            layer.kind,
        ),
    );
    Ok(())
}

fn push_blocked_candidate(
    candidate_map: &mut BTreeMap<String, Vec<FieldCandidate>>,
    layer: &ConfigLayer,
    path: &str,
    value: JsonValue,
    status: CandidateStatus,
    blocked_by: String,
) {
    candidate_map
        .entry(path.to_string())
        .or_default()
        .push(FieldCandidate {
            layer: layer.name.clone(),
            kind: layer.kind,
            source: redact_display(&layer.source),
            status,
            value,
            blocked_by: Some(blocked_by),
        });
}

fn apply_denied_policy(
    merged: &mut JsonValue,
    candidate_map: &mut BTreeMap<String, Vec<FieldCandidate>>,
    winner_map: &mut BTreeMap<String, (String, String, ConfigLayerKind)>,
    policy_path: &str,
    policy_source: &str,
) -> Result<(), ConfigError> {
    remove_path(merged, policy_path)?;
    let blocked_by = format!("{policy_source} denied {policy_path}");
    let keys = candidate_map
        .keys()
        .filter(|candidate_path| policy_path_matches(policy_path, candidate_path))
        .cloned()
        .collect::<Vec<_>>();

    for path in keys {
        let mut fallback = None;
        if let Some(candidates) = candidate_map.get_mut(&path) {
            for candidate in candidates.iter_mut() {
                if candidate.kind == ConfigLayerKind::BuiltInDefaults {
                    candidate.status = CandidateStatus::Applied;
                    candidate.blocked_by = None;
                    fallback = Some((
                        candidate.value.clone(),
                        candidate.source.clone(),
                        candidate.layer.clone(),
                        candidate.kind,
                    ));
                } else {
                    candidate.status = CandidateStatus::Denied;
                    candidate.blocked_by = Some(blocked_by.clone());
                }
            }
        }

        if let Some((value, source, layer, kind)) = fallback {
            set_path(merged, &path, value)?;
            winner_map.insert(path, (source, layer, kind));
        } else {
            remove_path(merged, &path)?;
            winner_map.remove(&path);
        }
    }
    Ok(())
}

fn leaf_values(value: &JsonValue) -> Vec<(String, JsonValue)> {
    let mut leaves = Vec::new();
    collect_leaf_values(value, "", &mut leaves);
    leaves
}

fn collect_leaf_values(value: &JsonValue, prefix: &str, leaves: &mut Vec<(String, JsonValue)>) {
    match value {
        JsonValue::Object(map) if !map.is_empty() => {
            for (key, child) in map {
                let next = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                collect_leaf_values(child, &next, leaves);
            }
        }
        JsonValue::Object(_) if prefix.is_empty() => {}
        _ if !prefix.is_empty() => leaves.push((prefix.to_string(), value.clone())),
        _ => {}
    }
}

fn set_path(root: &mut JsonValue, path: &str, value: JsonValue) -> Result<(), ConfigError> {
    validate_field_path(path)?;
    let parts: Vec<&str> = path.split('.').collect();
    if !root.is_object() {
        *root = JsonValue::Object(JsonMap::new());
    }
    let mut cursor = root;
    for part in &parts[..parts.len() - 1] {
        let object = cursor
            .as_object_mut()
            .ok_or_else(|| ConfigError::InvalidPath {
                path: path.to_string(),
            })?;
        cursor = object
            .entry((*part).to_string())
            .or_insert_with(|| JsonValue::Object(JsonMap::new()));
    }
    let object = cursor
        .as_object_mut()
        .ok_or_else(|| ConfigError::InvalidPath {
            path: path.to_string(),
        })?;
    object.insert(parts[parts.len() - 1].to_string(), value);
    Ok(())
}

fn remove_path(root: &mut JsonValue, path: &str) -> Result<(), ConfigError> {
    validate_field_path(path)?;
    let parts = path.split('.').collect::<Vec<_>>();
    remove_path_parts(root, &parts);
    Ok(())
}

fn remove_path_parts(value: &mut JsonValue, parts: &[&str]) -> bool {
    let Some((part, rest)) = parts.split_first() else {
        return false;
    };
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    if rest.is_empty() {
        object.remove(*part);
    } else if let Some(child) = object.get_mut(*part) {
        if remove_path_parts(child, rest) {
            object.remove(*part);
        }
    }
    object.is_empty()
}

fn validate_field_path(path: &str) -> Result<(), ConfigError> {
    let valid = !path.trim().is_empty()
        && path
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(valid_path_char));
    if valid {
        Ok(())
    } else {
        Err(ConfigError::InvalidPath {
            path: path.to_string(),
        })
    }
}

fn valid_path_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}

fn first_policy_match(policies: &BTreeMap<String, String>, path: &str) -> Option<(String, String)> {
    policies
        .iter()
        .find(|(policy_path, _)| policy_path_matches(policy_path, path))
        .map(|(policy_path, source)| (policy_path.clone(), source.clone()))
}

fn policy_path_matches(policy_path: &str, candidate_path: &str) -> bool {
    candidate_path == policy_path
        || candidate_path
            .strip_prefix(policy_path)
            .is_some_and(|suffix| suffix.starts_with('.'))
        || policy_path
            .strip_prefix(candidate_path)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

fn string_list_at(value: &JsonValue, path: &str) -> Vec<String> {
    let mut cursor = value;
    for part in path.split('.') {
        let Some(next) = cursor.get(part) else {
            return Vec::new();
        };
        cursor = next;
    }
    cursor
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_string))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn redact_value_at_path(path: &str, value: JsonValue) -> JsonValue {
    let key = path.rsplit('.').next().unwrap_or(path);
    let mut object = JsonMap::new();
    object.insert(key.to_string(), value);
    let redacted = current_policy().redact_json(&JsonValue::Object(object));
    redacted
        .get(key)
        .cloned()
        .unwrap_or(JsonValue::String("[redacted]".to_string()))
}

fn redact_display(value: &str) -> String {
    let policy = current_policy();
    if value.starts_with("http://") || value.starts_with("https://") {
        if url::Url::parse(value).is_ok() {
            return policy.redact_url(value);
        }
        return "[redacted]".to_string();
    }
    policy.redact_string(value).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(kind: ConfigLayerKind, name: &str, value: JsonValue) -> ConfigLayer {
        ConfigLayer::new(kind, name, name, value)
    }

    #[test]
    fn precedence_tracks_winner_and_shadowed_candidates() {
        let resolved = merge_layers(vec![
            built_in_defaults_layer(),
            layer(
                ConfigLayerKind::UserConfig,
                "user",
                json!({"logging": {"level": "warn"}}),
            ),
            layer(
                ConfigLayerKind::ProjectConfig,
                "project",
                json!({"logging": {"level": "debug"}}),
            ),
        ])
        .unwrap();

        assert_eq!(resolved.config.logging.level, LogLevel::Debug);
        let level = resolved
            .explain
            .iter()
            .find(|field| field.path == "logging.level")
            .expect("logging.level explanation");
        assert_eq!(level.source, "project");
        assert!(level
            .candidates
            .iter()
            .any(|candidate| candidate.source == "user"
                && candidate.status == CandidateStatus::Shadowed));
    }

    #[test]
    fn managed_lock_blocks_later_environment_override() {
        let resolved = merge_layers(vec![
            built_in_defaults_layer(),
            layer(
                ConfigLayerKind::ManagedPolicy,
                "managed",
                json!({
                    "limits": {"network": "offline"},
                    "policy": {"locked_fields": ["limits.network"]}
                }),
            ),
            layer(
                ConfigLayerKind::EnvironmentOverrides,
                "env",
                json!({"limits": {"network": "allow"}}),
            ),
        ])
        .unwrap();

        assert_eq!(resolved.config.limits.network, NetworkMode::Offline);
        let network = resolved
            .explain
            .iter()
            .find(|field| field.path == "limits.network")
            .expect("network explanation");
        assert_eq!(network.locked_by.as_deref(), Some("managed"));
        assert!(network
            .candidates
            .iter()
            .any(|candidate| candidate.source == "env"
                && candidate.status == CandidateStatus::Locked));
    }

    #[test]
    fn managed_deny_blocks_later_field() {
        let resolved = merge_layers(vec![
            built_in_defaults_layer(),
            layer(
                ConfigLayerKind::ManagedPolicy,
                "managed",
                json!({"policy": {"denied_fields": ["endpoints.mcp.untrusted"]}}),
            ),
            layer(
                ConfigLayerKind::ProjectConfig,
                "project",
                json!({"endpoints": {"mcp": {"untrusted": {"url": "https://example.com"}}}}),
            ),
        ])
        .unwrap();

        assert!(!resolved.config.endpoints.mcp.contains_key("untrusted"));
        let candidates = resolved
            .explain
            .iter()
            .flat_map(|field| field.candidates.iter())
            .collect::<Vec<_>>();
        assert!(candidates
            .iter()
            .any(|candidate| candidate.status == CandidateStatus::Denied));
    }

    #[test]
    fn managed_deny_masks_lower_precedence_dynamic_fields() {
        let resolved = merge_layers(vec![
            built_in_defaults_layer(),
            layer(
                ConfigLayerKind::ProjectConfig,
                "project",
                json!({"endpoints": {"mcp": {"untrusted": {"url": "https://example.com"}}}}),
            ),
            layer(
                ConfigLayerKind::ManagedPolicy,
                "managed",
                json!({"policy": {"denied_fields": ["endpoints.mcp.untrusted"]}}),
            ),
        ])
        .unwrap();

        assert!(!resolved.config.endpoints.mcp.contains_key("untrusted"));
        let untrusted = resolved
            .explain
            .iter()
            .find(|field| field.path == "endpoints.mcp.untrusted.url")
            .expect("blocked endpoint explanation");
        assert_eq!(untrusted.denied_by.as_deref(), Some("managed"));
        assert!(untrusted
            .candidates
            .iter()
            .any(|candidate| candidate.source == "project"
                && candidate.status == CandidateStatus::Denied));
    }

    #[test]
    fn secrets_are_redacted_in_config_and_explain() {
        let resolved = merge_layers(vec![
            built_in_defaults_layer(),
            layer(
                ConfigLayerKind::UserConfig,
                "user",
                json!({
                    "endpoints": {
                        "mcp": {
                            "secret": {
                                "headers": {"authorization": "Bearer sk_live_1234567890abcdef"}
                            }
                        }
                    }
                }),
            ),
        ])
        .unwrap();

        let rendered = serde_json::to_string(&resolved).unwrap();
        assert!(!rendered.contains("sk_live_1234567890abcdef"));
        assert!(rendered.contains("[redacted]"));
    }

    #[test]
    fn sources_are_redacted_in_explain_output() {
        let resolved = merge_layers(vec![
            built_in_defaults_layer(),
            ConfigLayer::new(
                ConfigLayerKind::RemoteDefaults,
                "remote",
                "https://example.com/.well-known/harn?api_key=sk_live_1234567890abcdef",
                json!({"logging": {"level": "debug"}}),
            ),
        ])
        .unwrap();

        let rendered = serde_json::to_string(&resolved).unwrap();
        assert!(!rendered.contains("sk_live_1234567890abcdef"));
        assert!(rendered.contains("api_key=%5Bredacted%5D"));
    }

    #[test]
    fn parses_config_table_from_manifest() {
        let value = parse_manifest_config_table(
            r#"
[package]
name = "demo"

[config.logging]
level = "trace"
"#,
            "harn.toml",
        )
        .unwrap()
        .expect("config table");
        assert_eq!(value["logging"]["level"], "trace");
    }

    #[test]
    fn scope_attenuation_policy_merges_from_toml() {
        let project = parse_config_toml(
            r#"
[identity.scope_attenuation]
mode = "strict-subset"
alert_on_violation = false
"#,
            "harn.config.toml",
        )
        .unwrap();
        let resolved = merge_layers(vec![
            built_in_defaults_layer(),
            layer(ConfigLayerKind::ProjectConfig, "project", project),
        ])
        .unwrap();

        assert_eq!(
            resolved.config.identity.scope_attenuation.mode,
            crate::actor_chain::ScopeAttenuationMode::StrictSubset
        );
        assert!(
            !resolved
                .config
                .identity
                .scope_attenuation
                .alert_on_violation
        );
    }

    #[test]
    fn environment_overrides_are_typed() {
        let env = environment_layer([
            ("HARN_LOG_LEVEL", "debug"),
            ("HARN_TOKEN_BUDGET", "1200"),
            ("HARN_REPLAY_ENABLED", "false"),
        ])
        .unwrap()
        .expect("env layer");
        let config: HarnConfig = serde_json::from_value(env.value).unwrap();
        assert_eq!(config.logging.level, LogLevel::Debug);
        assert_eq!(config.limits.tokens, Some(1200));
        assert!(!config.replay.enabled);
    }

    #[test]
    fn environment_bool_overrides_reject_unknown_values() {
        let error = environment_layer([("HARN_REPLAY_ENABLED", "sometimes")]).unwrap_err();
        assert!(error.to_string().contains("expected one of"));
    }

    #[test]
    fn parse_errors_do_not_echo_source_lines() {
        let error = parse_config_toml(
            "secret = \"sk_live_1234567890abcdef\"\n[",
            "bad-config.toml",
        )
        .unwrap_err();
        let rendered = error.to_string();
        assert!(!rendered.contains("sk_live_1234567890abcdef"));
    }

    #[test]
    fn schema_is_valid_json_schema_document() {
        let schema = schema_json();
        assert_eq!(schema["$id"], CONFIG_SCHEMA_ID);
        assert_eq!(
            schema["properties"]["limits"]["properties"]["network"]["enum"][3],
            "offline"
        );
        assert_eq!(
            schema["properties"]["identity"]["properties"]["scope_attenuation"]["properties"]
                ["mode"]["enum"][1],
            "non-increasing"
        );
        assert_eq!(
            schema["properties"]["security"]["properties"]["mode"]["enum"][1],
            "spotlight"
        );
    }

    #[test]
    fn config_locations_are_cross_platform() {
        assert_eq!(
            install_config_path_for_os("linux", None),
            PathBuf::from("/etc/harn/config.toml")
        );
        assert_eq!(
            user_config_path_for_os("linux", Some("/home/me"), None, None),
            Some(PathBuf::from("/home/me/.config/harn/config.toml"))
        );
        assert_eq!(
            user_config_path_for_os("linux", Some("/home/me"), Some("/xdg"), None),
            Some(PathBuf::from("/xdg/harn/config.toml"))
        );
        assert_eq!(
            install_config_path_for_os("windows", Some(r"D:\ProgramData")),
            PathBuf::from(r"D:\ProgramData").join(r"Harn\config.toml")
        );
        assert_eq!(
            user_config_path_for_os("windows", None, None, Some(r"C:\Users\me\AppData\Roaming")),
            Some(PathBuf::from(r"C:\Users\me\AppData\Roaming").join(r"Harn\config.toml"))
        );
    }
}
