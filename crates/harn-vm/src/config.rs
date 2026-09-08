//! Canonical layered Harn runtime configuration.
//!
//! The VM owns the typed shape and deterministic merge engine so hosts can
//! inspect, validate, and explain configuration without depending on the CLI's
//! `harn.toml` package manifest model.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map as JsonMap, Value as JsonValue};

use crate::redact::current_policy;

mod provider_layer;

pub use provider_layer::layer_from_providers_config;

pub const CONFIG_SCHEMA_VERSION: u32 = 1;
pub const CONFIG_SCHEMA_ID: &str = "https://harnlang.com/schemas/harn-config.schema.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct HarnConfig {
    pub schema_version: u32,
    pub models: ModelPolicyConfig,
    pub endpoints: EndpointCatalogConfig,
    pub packages: PackageSourcesConfig,
    pub skills: SkillSourcesConfig,
    pub plugins: PluginSourcesConfig,
    pub logging: LoggingConfig,
    pub retention: RetentionConfig,
    pub redaction: RedactionConfig,
    pub replay: ReplayConfig,
    pub identity: IdentityConfig,
}

impl Default for HarnConfig {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            models: ModelPolicyConfig::default(),
            endpoints: EndpointCatalogConfig::default(),
            packages: PackageSourcesConfig::default(),
            skills: SkillSourcesConfig::default(),
            plugins: PluginSourcesConfig::default(),
            logging: LoggingConfig::default(),
            retention: RetentionConfig::default(),
            redaction: RedactionConfig::default(),
            replay: ReplayConfig::default(),
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
///
/// Both hardened tiers (`Strict` and `LocalMl`) additionally bundle the
/// origin-provenance defenses — `authenticate_directives`,
/// `taint_file_provenance`, `taint_command_reads`, and the precise
/// (destination-aware) exfil gate — on. See [`SecurityPolicy::from_config`].
/// The individual `SecurityConfig` booleans remain for tests and fine-grained
/// config, but the mode ladder is the coherent product surface.
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

/// Prompt-injection defense posture. The runtime substrate lives in
/// [`crate::security`]; this is the typed shape [`crate::security::SecurityPolicy::from_config`]
/// resolves into an installed policy. Hosts drive it per-run through the
/// `security_policy(...)` pipeline surface, not a persisted config section — so
/// there is a single source of truth for the posture and no silently-inert
/// persisted copy. Defaults are on (deterministic, free) so the runtime is
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
    /// Track untrusted-origin file provenance. A file written while untrusted
    /// content is in the session's context — or by a fetch/clone/MCP step — is
    /// recorded, and a later read of that path is classified untrusted so a
    /// deferred on-disk injection (a cloned dependency's README, a downloaded
    /// dataset) is quarantined by the same taint/trifecta gate as a live fetch.
    /// First-party file reads stay trusted. Default OFF (net-new enforcement);
    /// byte-identical behaviour when disabled.
    pub taint_file_provenance: bool,
    /// Extend untrusted-origin file provenance to the command surface. An
    /// `Execute`-kind tool whose command string names a tainted-origin path
    /// (`cat vendor/dep/README`) launders that content back into context outside
    /// a structured `read_file` call; classify it untrusted by the same file
    /// origin so the laundering read is quarantined too. Closes the `tool_result`
    /// residual (fetch-to-disk then `cat`). Fires only on paths already known
    /// untrusted, so a first-party `cat src/main.rs` stays trusted. Default OFF
    /// (net-new enforcement); byte-identical behaviour when disabled.
    pub taint_command_reads: bool,
    /// Narrow the exfil axis of the lethal-trifecta gate to attacker-originated
    /// destinations. When on, an exfil-capable tool only forces confirmation if
    /// its destination was named in untrusted content (the injection controls
    /// where data goes) or its payload references a secret — so benign research
    /// and synthesis to a user-named / configured destination is not gated.
    /// Fail-safe: an unknown / unextractable destination still gates. Default
    /// OFF (coarse gate is byte-identical when disabled).
    pub precise_exfil_gate: bool,
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
            taint_file_provenance: false,
            taint_command_reads: false,
            precise_exfil_gate: false,
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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    Applied,
    Shadowed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FieldExplanation {
    pub path: String,
    pub value: JsonValue,
    pub source: String,
    pub layer: String,
    pub kind: ConfigLayerKind,
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
    set_env_u64(&mut value, &vars, "HARN_RETENTION_DAYS", "retention.days")?;
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
            apply_candidate(
                &mut merged,
                &mut candidate_map,
                &mut winner_map,
                &layer,
                &path,
                value,
            )?;
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
        explain.push(FieldExplanation {
            path: path.clone(),
            value: JsonValue::Null,
            source: "<blocked>".to_string(),
            layer: "<blocked>".to_string(),
            kind: candidates
                .last()
                .map(|candidate| candidate.kind)
                .unwrap_or(ConfigLayerKind::BuiltInDefaults),
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
            ("HARN_REPLAY_ENABLED", "false"),
        ])
        .unwrap()
        .expect("env layer");
        let config: HarnConfig = serde_json::from_value(env.value).unwrap();
        assert_eq!(config.logging.level, LogLevel::Debug);
        assert!(!config.replay.enabled);
    }

    #[test]
    fn environment_bool_overrides_reject_unknown_values() {
        let error = environment_layer([("HARN_REPLAY_ENABLED", "sometimes")]).unwrap_err();
        assert!(error.to_string().contains("expected one of"));
    }

    /// The retired sections are refused by name rather than quietly accepted.
    ///
    /// A run never read `limits`, `permissions` or `policy`, so an operator who
    /// set one could not tell a respected ceiling from an ignored one. They are
    /// gone from the typed shape, and because the shape denies unknown fields a
    /// file that still carries one now fails at the parse boundary and names the
    /// section. Reinstating any of the three makes these parses succeed again.
    #[test]
    fn retired_sections_are_refused_by_name() {
        for section in ["limits", "permissions", "policy"] {
            let error = parse_config_toml(
                &format!("schema_version = 1\n\n[{section}]\n"),
                "harn.config.toml",
            )
            .expect_err("a retired section must not parse");
            let message = error.to_string();
            assert!(
                message.contains(section),
                "error for [{section}] should name it, got: {message}"
            );
        }
    }

    /// An unset retired section is not an error, so the refusal above is
    /// specific to the section rather than to config parsing in general.
    #[test]
    fn a_config_without_the_retired_sections_still_parses() {
        parse_config_toml(
            "schema_version = 1\n\n[logging]\nlevel = \"debug\"\n",
            "harn.config.toml",
        )
        .expect("a config with only live sections parses");
    }

    /// The retired environment names no longer reach a config field.
    ///
    /// They were registered, which made them look supported while nothing read
    /// the value they set. Setting one now contributes no layer at all.
    #[test]
    fn retired_environment_names_contribute_no_layer() {
        let layer = environment_layer([
            ("HARN_BUDGET_USD", "0.01"),
            ("HARN_TOKEN_BUDGET", "1"),
            ("HARN_MAX_CONCURRENCY", "1"),
            ("HARN_NETWORK_MODE", "offline"),
            ("HARN_FILESYSTEM_MODE", "read-only"),
            ("HARN_SANDBOX_MODE", "worktree"),
        ])
        .expect("retired names must not error");
        assert!(
            layer.is_none(),
            "retired environment names must not build a config layer"
        );
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
