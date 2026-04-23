use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fs, process};

use chrono_tz::Tz;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::str::FromStr;
use url::Url;

const CONTENT_HASH_FILE: &str = ".harn-content-hash";
const CACHE_METADATA_FILE: &str = ".harn-package-cache.toml";
const HARN_CACHE_DIR_ENV: &str = "HARN_CACHE_DIR";
const HARN_PACKAGE_REGISTRY_ENV: &str = "HARN_PACKAGE_REGISTRY";
const DEFAULT_PACKAGE_REGISTRY_URL: &str = "https://packages.harnlang.com/index.toml";
const CACHE_METADATA_VERSION: u32 = 1;
const LOCK_FILE_VERSION: u32 = 1;
const REGISTRY_INDEX_VERSION: u32 = 1;
const PKG_DIR: &str = ".harn/packages";
const MANIFEST: &str = "harn.toml";
const LOCK_FILE: &str = "harn.lock";
const TRIGGER_RETRY_MAX_LIMIT: u32 = 100;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    #[allow(dead_code)]
    pub package: Option<PackageInfo>,
    #[serde(default)]
    pub dependencies: HashMap<String, Dependency>,
    #[serde(default)]
    pub mcp: Vec<McpServerConfig>,
    #[serde(default)]
    pub check: CheckConfig,
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    /// `[registry]` table — lightweight package discovery index
    /// configuration. The CLI also honors `HARN_PACKAGE_REGISTRY` and
    /// `--registry` flags for one-off overrides.
    #[serde(default)]
    pub registry: PackageRegistryConfig,
    /// `[skills]` table — per-project skill discovery configuration
    /// (paths, lookup_order, disable).
    #[serde(default)]
    pub skills: SkillsConfig,
    /// `[[skill.source]]` array-of-tables — declared skill sources
    /// (filesystem, git, reserved registry).
    #[serde(default)]
    pub skill: SkillTables,
    /// `[capabilities]` section — per-provider-per-model override of
    /// the shipped capability matrix (`defer_loading`, `tool_search`,
    /// `prompt_caching`, etc.). Entries under `[[capabilities.provider.<name>]]`
    /// are prepended to the built-in rules for the same provider so
    /// early adopters can flag proxied endpoints as supporting tool
    /// search without waiting for a Harn release. See
    /// `harn_vm::llm::capabilities` for the rule schema.
    #[serde(default)]
    pub capabilities: Option<harn_vm::llm::capabilities::CapabilitiesFile>,
    /// Stable exported package modules. Keys are the logical import
    /// suffixes (e.g. `providers/openai`) and values are package-root-
    /// relative file paths. Consumers import them via `<package>/<key>`.
    #[allow(dead_code)]
    #[serde(default)]
    pub exports: HashMap<String, String>,
    /// `[llm]` section — packaged provider definitions, aliases,
    /// inference rules, tier rules, and model defaults. Uses the same
    /// schema as `providers.toml`, but merges into the current run
    /// instead of replacing the global config file.
    #[serde(default)]
    pub llm: harn_vm::llm_config::ProvidersConfig,
    /// `[[hooks]]` array-of-tables — declarative runtime hooks installed
    /// once per process/thread before execution starts. Matches the
    /// manifest-extension ABI shape added by `[exports]` / `[llm]`, but
    /// the handlers themselves live in Harn modules.
    #[serde(default)]
    pub hooks: Vec<HookConfig>,
    /// `[[triggers]]` array-of-tables — declarative event-driven trigger
    /// registrations that resolve local handlers and predicates from Harn
    /// modules at load time and preserve remote URI schemes for later
    /// dispatcher work.
    #[serde(default)]
    pub triggers: Vec<TriggerManifestEntry>,
    /// `[[providers]]` array-of-tables — provider-specific connector
    /// overrides used by the orchestrator to load either builtin Rust
    /// connectors or `.harn` modules as connector implementations.
    #[serde(default)]
    pub providers: Vec<ProviderManifestEntry>,
    /// `[[personas]]` array-of-tables — durable, non-executing agent role
    /// manifests. Personas bind an entry workflow to tools, capabilities,
    /// autonomy, budgets, receipts, handoffs, evals, and rollout metadata.
    #[serde(default)]
    pub personas: Vec<PersonaManifestEntry>,
    /// `[connector_contract]` table — deterministic package-local fixtures
    /// consumed by `harn connector check` for pure-Harn connector packages.
    #[serde(default, alias = "connector-contract")]
    pub connector_contract: ConnectorContractConfig,
    /// `[orchestrator]` table — listener-level controls shared by
    /// manifest-driven ingress surfaces.
    #[serde(default)]
    pub orchestrator: OrchestratorConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OrchestratorConfig {
    #[serde(default, alias = "allowed-origins")]
    pub allowed_origins: Vec<String>,
    #[serde(default, alias = "max-body-bytes")]
    pub max_body_bytes: Option<usize>,
    #[serde(default)]
    pub budget: OrchestratorBudgetSpec,
    #[serde(default)]
    pub drain: OrchestratorDrainConfig,
    #[serde(default)]
    pub pumps: OrchestratorPumpConfig,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestratorBudgetSpec {
    #[serde(default)]
    pub daily_cost_usd: Option<f64>,
    #[serde(default)]
    pub hourly_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrchestratorDrainConfig {
    #[serde(default = "default_orchestrator_drain_max_items", alias = "max-items")]
    pub max_items: usize,
    #[serde(
        default = "default_orchestrator_drain_deadline_seconds",
        alias = "deadline-seconds"
    )]
    pub deadline_seconds: u64,
}

impl Default for OrchestratorDrainConfig {
    fn default() -> Self {
        Self {
            max_items: default_orchestrator_drain_max_items(),
            deadline_seconds: default_orchestrator_drain_deadline_seconds(),
        }
    }
}

fn default_orchestrator_drain_max_items() -> usize {
    1024
}

fn default_orchestrator_drain_deadline_seconds() -> u64 {
    30
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrchestratorPumpConfig {
    #[serde(
        default = "default_orchestrator_pump_max_outstanding",
        alias = "max-outstanding"
    )]
    pub max_outstanding: usize,
}

impl Default for OrchestratorPumpConfig {
    fn default() -> Self {
        Self {
            max_outstanding: default_orchestrator_pump_max_outstanding(),
        }
    }
}

fn default_orchestrator_pump_max_outstanding() -> usize {
    64
}

#[derive(Debug, Clone, Deserialize)]
pub struct HookConfig {
    pub event: harn_vm::orchestration::HookEvent,
    #[serde(default = "default_hook_pattern")]
    pub pattern: String,
    pub handler: String,
}

fn default_hook_pattern() -> String {
    "*".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerManifestEntry {
    pub id: String,
    #[serde(default)]
    pub kind: Option<TriggerKind>,
    #[serde(default)]
    pub provider: Option<harn_vm::ProviderId>,
    #[serde(default, alias = "tier")]
    pub autonomy_tier: harn_vm::AutonomyTier,
    #[serde(default, rename = "match")]
    pub match_: Option<TriggerMatchExpr>,
    #[serde(default)]
    pub sources: Vec<TriggerSourceManifestEntry>,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub when_budget: Option<TriggerWhenBudgetSpec>,
    pub handler: String,
    #[serde(default)]
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub retry: TriggerRetrySpec,
    #[serde(default)]
    pub priority: Option<TriggerPriorityField>,
    #[serde(default)]
    pub budget: TriggerBudgetSpec,
    #[serde(default)]
    pub concurrency: Option<TriggerConcurrencyManifestSpec>,
    #[serde(default)]
    pub throttle: Option<TriggerThrottleManifestSpec>,
    #[serde(default)]
    pub rate_limit: Option<TriggerRateLimitManifestSpec>,
    #[serde(default)]
    pub debounce: Option<TriggerDebounceManifestSpec>,
    #[serde(default)]
    pub singleton: Option<TriggerSingletonManifestSpec>,
    #[serde(default)]
    pub batch: Option<TriggerBatchManifestSpec>,
    #[serde(default)]
    pub window: Option<TriggerStreamWindowManifestSpec>,
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(flatten, default)]
    pub kind_specific: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerSourceManifestEntry {
    #[serde(default)]
    pub id: Option<String>,
    pub kind: TriggerKind,
    pub provider: harn_vm::ProviderId,
    #[serde(default, rename = "match")]
    pub match_: Option<TriggerMatchExpr>,
    #[serde(default)]
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub retry: Option<TriggerRetrySpec>,
    #[serde(default)]
    pub priority: Option<TriggerPriorityField>,
    #[serde(default)]
    pub budget: Option<TriggerBudgetSpec>,
    #[serde(default)]
    pub concurrency: Option<TriggerConcurrencyManifestSpec>,
    #[serde(default)]
    pub throttle: Option<TriggerThrottleManifestSpec>,
    #[serde(default)]
    pub rate_limit: Option<TriggerRateLimitManifestSpec>,
    #[serde(default)]
    pub debounce: Option<TriggerDebounceManifestSpec>,
    #[serde(default)]
    pub singleton: Option<TriggerSingletonManifestSpec>,
    #[serde(default)]
    pub batch: Option<TriggerBatchManifestSpec>,
    #[serde(default)]
    pub window: Option<TriggerStreamWindowManifestSpec>,
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(flatten, default)]
    pub kind_specific: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriggerKind {
    Webhook,
    Cron,
    Poll,
    Stream,
    Predicate,
    A2aPush,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TriggerMatchExpr {
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerRetrySpec {
    #[serde(default)]
    pub max: u32,
    #[serde(default)]
    pub backoff: TriggerRetryBackoff,
    #[serde(default = "default_trigger_retention_days")]
    pub retention_days: u32,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriggerRetryBackoff {
    #[default]
    Immediate,
    Svix,
}

fn default_trigger_retention_days() -> u32 {
    harn_vm::DEFAULT_INBOX_RETENTION_DAYS
}

impl Default for TriggerRetrySpec {
    fn default() -> Self {
        Self {
            max: 0,
            backoff: TriggerRetryBackoff::default(),
            retention_days: default_trigger_retention_days(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerDispatchPriority {
    High,
    #[default]
    Normal,
    Low,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TriggerPriorityField {
    Dispatch(TriggerDispatchPriority),
    Flow(TriggerPriorityManifestSpec),
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerBudgetSpec {
    #[serde(default)]
    pub max_cost_usd: Option<f64>,
    #[serde(default, alias = "tokens_max")]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub daily_cost_usd: Option<f64>,
    #[serde(default)]
    pub hourly_cost_usd: Option<f64>,
    #[serde(default)]
    pub max_autonomous_decisions_per_hour: Option<u64>,
    #[serde(default)]
    pub max_autonomous_decisions_per_day: Option<u64>,
    #[serde(default)]
    pub max_concurrent: Option<u32>,
    #[serde(default)]
    pub on_budget_exhausted: harn_vm::TriggerBudgetExhaustionStrategy,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerWhenBudgetSpec {
    #[serde(default)]
    pub max_cost_usd: Option<f64>,
    #[serde(default)]
    pub tokens_max: Option<u64>,
    #[serde(default)]
    pub timeout: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerConcurrencyManifestSpec {
    #[serde(default)]
    pub key: Option<String>,
    pub max: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerThrottleManifestSpec {
    #[serde(default)]
    pub key: Option<String>,
    pub period: String,
    pub max: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerRateLimitManifestSpec {
    #[serde(default)]
    pub key: Option<String>,
    pub period: String,
    pub max: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerDebounceManifestSpec {
    pub key: String,
    pub period: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerSingletonManifestSpec {
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerBatchManifestSpec {
    #[serde(default)]
    pub key: Option<String>,
    pub size: u32,
    pub timeout: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerPriorityManifestSpec {
    pub key: String,
    #[serde(default)]
    pub order: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriggerStreamWindowMode {
    Tumbling,
    Sliding,
    Session,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerStreamWindowManifestSpec {
    pub mode: TriggerStreamWindowMode,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub every: Option<String>,
    #[serde(default)]
    pub gap: Option<String>,
    #[serde(default)]
    pub max_items: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaManifestEntry {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, alias = "entry", alias = "entry_pipeline")]
    pub entry_workflow: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, alias = "tier")]
    pub autonomy_tier: Option<harn_vm::AutonomyTier>,
    #[serde(default, alias = "receipts")]
    pub receipt_policy: Option<PersonaReceiptPolicy>,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub schedules: Vec<String>,
    #[serde(default)]
    pub model_policy: PersonaModelPolicy,
    #[serde(default)]
    pub budget: PersonaBudget,
    #[serde(default)]
    pub handoffs: Vec<String>,
    #[serde(default)]
    pub context_packs: Vec<String>,
    #[serde(default, alias = "eval_packs")]
    pub evals: Vec<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub package_source: PersonaPackageSource,
    #[serde(default)]
    pub rollout_policy: PersonaRolloutPolicy,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaReceiptPolicy {
    #[default]
    Optional,
    Required,
    Disabled,
}

impl PersonaReceiptPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Optional => "optional",
            Self::Required => "required",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaModelPolicy {
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub escalation_model: Option<String>,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaBudget {
    #[serde(default)]
    pub daily_usd: Option<f64>,
    #[serde(default)]
    pub hourly_usd: Option<f64>,
    #[serde(default)]
    pub run_usd: Option<f64>,
    #[serde(default)]
    pub frontier_escalations: Option<u32>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub max_runtime_seconds: Option<u64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaPackageSource {
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub git: Option<String>,
    #[serde(default)]
    pub rev: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaRolloutPolicy {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub percentage: Option<u8>,
    #[serde(default)]
    pub cohorts: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedPersonaManifest {
    pub manifest_path: PathBuf,
    pub manifest_dir: PathBuf,
    pub personas: Vec<PersonaManifestEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PersonaValidationError {
    pub manifest_path: PathBuf,
    pub field_path: String,
    pub message: String,
}

impl std::fmt::Display for PersonaValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}: {}",
            self.manifest_path.display(),
            self.field_path,
            self.message
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerHandlerUri {
    Local(TriggerFunctionRef),
    A2a {
        target: String,
        allow_cleartext: bool,
    },
    Worker {
        queue: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerFunctionRef {
    pub raw: String,
    pub module_name: Option<String>,
    pub function_name: String,
}

/// `[skills]` table body.
#[derive(Debug, Default, Clone, Deserialize)]
#[allow(dead_code)] // `defaults` is parsed now and consumed by a follow-up CLI wiring PR.
pub struct SkillsConfig {
    /// Additional filesystem roots to scan. Each entry may be a
    /// literal directory or a glob (`packages/*/skills`). Resolved
    /// relative to the directory holding harn.toml.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Override priority order. Values are layer labels —
    /// `cli`, `env`, `project`, `manifest`, `user`, `package`,
    /// `system`, `host`. Unlisted layers fall through to default
    /// priority after listed ones.
    #[serde(default)]
    pub lookup_order: Vec<String>,
    /// Disable entire layers. Same label set as `lookup_order`.
    #[serde(default)]
    pub disable: Vec<String>,
    /// Optional remote registry base URL used to resolve
    /// `<fingerprint>.pub` when a signer is not installed locally.
    #[serde(default)]
    pub signer_registry_url: Option<String>,
    /// `[skills.defaults]` inline sub-table — applied to every
    /// discovered skill when the field is unset in its SKILL.md
    /// frontmatter.
    #[serde(default)]
    pub defaults: SkillDefaults,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[allow(dead_code)] // Wired in the follow-up that threads defaults into the loader.
pub struct SkillDefaults {
    #[serde(default)]
    pub tool_search: Option<String>,
    #[serde(default)]
    pub always_loaded: Vec<String>,
}

/// Container for `[[skill.source]]` array-of-tables.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct SkillTables {
    #[serde(default, rename = "source")]
    pub sources: Vec<SkillSourceEntry>,
}

/// One `[[skill.source]]` entry. The `registry` variant is accepted
/// for forward-compat but inert — see issue #73 and `docs/src/skills.md`
/// for the marketplace timeline.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
#[allow(dead_code)]
pub enum SkillSourceEntry {
    Fs {
        path: String,
        #[serde(default)]
        namespace: Option<String>,
    },
    Git {
        url: String,
        #[serde(default)]
        tag: Option<String>,
        #[serde(default)]
        namespace: Option<String>,
    },
    Registry {
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        name: Option<String>,
    },
}

/// Severity override for preflight diagnostics. `error` (default) fails
/// `harn check`; `warning` reports but does not fail; `off` suppresses
/// entirely. Accepted via `[check].preflight_severity` in harn.toml so
/// repos with hosts that do not expose every capability statically can
/// keep the checker running on genuine type errors.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PreflightSeverity {
    #[default]
    Error,
    Warning,
    Off,
}

impl PreflightSeverity {
    pub fn from_opt(raw: Option<&str>) -> Self {
        match raw.map(|s| s.to_ascii_lowercase()) {
            Some(v) if v == "warning" || v == "warn" => Self::Warning,
            Some(v) if v == "off" || v == "allow" || v == "silent" => Self::Off,
            _ => Self::Error,
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct CheckConfig {
    #[serde(default)]
    pub strict: bool,
    #[serde(default)]
    pub strict_types: bool,
    #[serde(default)]
    pub disable_rules: Vec<String>,
    #[serde(default)]
    pub host_capabilities: HashMap<String, Vec<String>>,
    #[serde(default, alias = "host_capabilities_file")]
    pub host_capabilities_path: Option<String>,
    #[serde(default)]
    pub bundle_root: Option<String>,
    /// Downgrade or suppress preflight diagnostics. See
    /// [`PreflightSeverity`].
    #[serde(default, alias = "preflight-severity")]
    pub preflight_severity: Option<String>,
    /// List of `"capability.operation"` strings that should be accepted
    /// by preflight without emitting a diagnostic, even if the operation
    /// is not in the default or loaded capability manifest.
    #[serde(default, alias = "preflight-allow")]
    pub preflight_allow: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct WorkspaceConfig {
    /// Directory or file globs (repo-relative) that `harn check --workspace`
    /// walks to collect the full pipeline tree in one invocation.
    #[serde(default)]
    pub pipelines: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct PackageRegistryConfig {
    /// URL or filesystem path to a TOML package index.
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub scopes: Option<String>,
    #[serde(default)]
    pub protocol_version: Option<String>,
    #[serde(default)]
    pub proxy_server_name: Option<String>,
    /// When `true`, the server is NOT booted up-front. It boots on the
    /// first `mcp_call` or on skill activation that declares it in
    /// `requires_mcp`. See harn#75.
    #[serde(default)]
    pub lazy: bool,
    /// Optional pointer to a Server Card — either an HTTP(S) URL or a
    /// local filesystem path. When set, `mcp_server_card("name")` reads
    /// the card from this source (cached per-process with a TTL).
    #[serde(default)]
    pub card: Option<String>,
    /// How long (milliseconds) to keep a lazy server's process alive
    /// after its last binder releases. 0 / unset → disconnect
    /// immediately. Ignored for non-lazy servers.
    #[serde(default, alias = "keep-alive-ms", alias = "keep_alive")]
    pub keep_alive_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct PackageInfo {
    pub name: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Dependency {
    Table(DepTable),
    Path(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct DepTable {
    pub git: Option<String>,
    pub tag: Option<String>,
    pub rev: Option<String>,
    pub branch: Option<String>,
    pub path: Option<String>,
    pub package: Option<String>,
}

impl Dependency {
    fn git_url(&self) -> Option<&str> {
        match self {
            Dependency::Table(t) => t.git.as_deref(),
            Dependency::Path(_) => None,
        }
    }

    fn rev(&self) -> Option<&str> {
        match self {
            Dependency::Table(t) => t.rev.as_deref().or(t.tag.as_deref()),
            Dependency::Path(_) => None,
        }
    }

    fn branch(&self) -> Option<&str> {
        match self {
            Dependency::Table(t) => t.branch.as_deref(),
            Dependency::Path(_) => None,
        }
    }

    fn local_path(&self) -> Option<&str> {
        match self {
            Dependency::Table(t) => t.path.as_deref(),
            Dependency::Path(p) => Some(p.as_str()),
        }
    }
}

fn validate_package_alias(alias: &str) -> Result<(), String> {
    let valid = !alias.is_empty()
        && alias != "."
        && alias != ".."
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "invalid dependency alias {alias:?}; use ASCII letters, numbers, '.', '_' or '-'"
        ))
    }
}

fn toml_string_literal(value: &str) -> Result<String, String> {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(value.len() + 2);
    encoded.push('"');
    for ch in value.chars() {
        match ch {
            '\u{08}' => encoded.push_str("\\b"),
            '\t' => encoded.push_str("\\t"),
            '\n' => encoded.push_str("\\n"),
            '\u{0C}' => encoded.push_str("\\f"),
            '\r' => encoded.push_str("\\r"),
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            ch if ch <= '\u{1F}' || ch == '\u{7F}' => {
                write!(&mut encoded, "\\u{:04X}", ch as u32)
                    .map_err(|error| format!("failed to encode TOML string: {error}"))?;
            }
            ch => encoded.push(ch),
        }
    }
    encoded.push('"');
    Ok(encoded)
}

#[derive(Debug, Default, Clone)]
pub struct RuntimeExtensions {
    pub root_manifest: Option<Manifest>,
    pub llm: Option<harn_vm::llm_config::ProvidersConfig>,
    pub capabilities: Option<harn_vm::llm::capabilities::CapabilitiesFile>,
    pub hooks: Vec<ResolvedHookConfig>,
    pub triggers: Vec<ResolvedTriggerConfig>,
    pub provider_connectors: Vec<ResolvedProviderConnectorConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderManifestEntry {
    pub id: harn_vm::ProviderId,
    pub connector: ProviderConnectorManifest,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConnectorManifest {
    #[serde(default)]
    pub harn: Option<String>,
    #[serde(default)]
    pub rust: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConnectorContractConfig {
    #[serde(default)]
    pub version: Option<u32>,
    #[serde(default)]
    pub fixtures: Vec<ConnectorContractFixture>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConnectorContractFixture {
    pub provider: harn_vm::ProviderId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub metadata: Option<toml::Value>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub body_json: Option<toml::Value>,
    #[serde(default)]
    pub expect_type: Option<String>,
    #[serde(default)]
    pub expect_kind: Option<String>,
    #[serde(default)]
    pub expect_event_count: Option<usize>,
    #[serde(default)]
    pub expect_error_contains: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedProviderConnectorKind {
    Harn { module: String },
    RustBuiltin,
    Invalid(String),
}

#[derive(Debug, Clone)]
pub struct ResolvedProviderConnectorConfig {
    pub id: harn_vm::ProviderId,
    pub manifest_dir: PathBuf,
    pub connector: ResolvedProviderConnectorKind,
}

#[derive(Debug, Clone)]
pub struct ResolvedHookConfig {
    pub event: harn_vm::orchestration::HookEvent,
    pub pattern: String,
    pub handler: String,
    pub manifest_dir: PathBuf,
    pub package_name: Option<String>,
    pub exports: HashMap<String, String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Trigger metadata is carried forward for doctor output and downstream dispatcher work.
pub struct ResolvedTriggerConfig {
    pub id: String,
    pub kind: TriggerKind,
    pub provider: harn_vm::ProviderId,
    pub autonomy_tier: harn_vm::AutonomyTier,
    pub match_: TriggerMatchExpr,
    pub when: Option<String>,
    pub when_budget: Option<TriggerWhenBudgetSpec>,
    pub handler: String,
    pub dedupe_key: Option<String>,
    pub retry: TriggerRetrySpec,
    pub dispatch_priority: TriggerDispatchPriority,
    pub budget: TriggerBudgetSpec,
    pub concurrency: Option<TriggerConcurrencyManifestSpec>,
    pub throttle: Option<TriggerThrottleManifestSpec>,
    pub rate_limit: Option<TriggerRateLimitManifestSpec>,
    pub debounce: Option<TriggerDebounceManifestSpec>,
    pub singleton: Option<TriggerSingletonManifestSpec>,
    pub batch: Option<TriggerBatchManifestSpec>,
    pub window: Option<TriggerStreamWindowManifestSpec>,
    pub priority_flow: Option<TriggerPriorityManifestSpec>,
    pub secrets: BTreeMap<String, String>,
    pub filter: Option<String>,
    pub kind_specific: BTreeMap<String, toml::Value>,
    pub manifest_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub package_name: Option<String>,
    pub exports: HashMap<String, String>,
    pub table_index: usize,
    pub shape_error: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Collected trigger bindings are validated now and consumed by follow-up trigger dispatcher work.
pub struct CollectedManifestTrigger {
    pub config: ResolvedTriggerConfig,
    pub handler: CollectedTriggerHandler,
    pub when: Option<CollectedTriggerPredicate>,
    pub flow_control: harn_vm::TriggerFlowControlConfig,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Remote handler targets and resolved closures are retained for downstream trigger execution.
pub enum CollectedTriggerHandler {
    Local {
        reference: TriggerFunctionRef,
        closure: Rc<harn_vm::VmClosure>,
    },
    A2a {
        target: String,
        allow_cleartext: bool,
    },
    Worker {
        queue: String,
    },
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Predicate closures are validated now and reused by later trigger dispatch work.
pub struct CollectedTriggerPredicate {
    pub reference: TriggerFunctionRef,
    pub closure: Rc<harn_vm::VmClosure>,
}

type ManifestModuleCacheKey = (PathBuf, Option<String>, Option<String>);
type ManifestModuleExports = BTreeMap<String, Rc<harn_vm::VmClosure>>;

static MANIFEST_PROVIDER_SCHEMA_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub(crate) async fn lock_manifest_provider_schemas() -> tokio::sync::MutexGuard<'static, ()> {
    MANIFEST_PROVIDER_SCHEMA_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LockFile {
    version: u32,
    #[serde(default, rename = "package")]
    packages: Vec<LockEntry>,
}

impl Default for LockFile {
    fn default() -> Self {
        Self {
            version: LOCK_FILE_VERSION,
            packages: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LockEntry {
    name: String,
    source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rev_request: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PackageCacheMetadata {
    version: u32,
    source: String,
    commit: String,
    content_hash: String,
    cached_at_unix_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PackageRegistryIndex {
    version: u32,
    #[serde(default, rename = "package")]
    packages: Vec<RegistryPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RegistryPackage {
    name: String,
    #[serde(default)]
    description: Option<String>,
    repository: String,
    #[serde(default)]
    license: Option<String>,
    #[serde(default, alias = "harn_version", alias = "harn_version_range")]
    harn: Option<String>,
    #[serde(default)]
    exports: Vec<String>,
    #[serde(default, alias = "connector-contract")]
    connector_contract: Option<String>,
    #[serde(default)]
    docs_url: Option<String>,
    #[serde(default)]
    checksum: Option<String>,
    #[serde(default)]
    provenance: Option<String>,
    #[serde(default, rename = "version")]
    versions: Vec<RegistryPackageVersion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RegistryPackageVersion {
    version: String,
    git: String,
    #[serde(default)]
    rev: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    package: Option<String>,
    #[serde(default)]
    checksum: Option<String>,
    #[serde(default)]
    provenance: Option<String>,
    #[serde(default)]
    yanked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RegistryPackageInfo {
    package: RegistryPackage,
    selected_version: Option<RegistryPackageVersion>,
}

impl LockFile {
    fn load(path: &Path) -> Result<Option<Self>, String> {
        let content = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
        };

        match toml::from_str::<Self>(&content) {
            Ok(mut lock) => {
                if lock.version != LOCK_FILE_VERSION {
                    return Err(format!(
                        "unsupported {} version {} (expected {})",
                        path.display(),
                        lock.version,
                        LOCK_FILE_VERSION
                    ));
                }
                lock.sort_entries();
                Ok(Some(lock))
            }
            Err(_) => {
                let legacy = toml::from_str::<LegacyLockFile>(&content)
                    .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
                let mut lock = Self {
                    version: LOCK_FILE_VERSION,
                    packages: legacy
                        .packages
                        .into_iter()
                        .map(|entry| LockEntry {
                            name: entry.name,
                            source: entry
                                .path
                                .map(|path| format!("path+{path}"))
                                .or_else(|| entry.git.map(|git| format!("git+{git}")))
                                .unwrap_or_default(),
                            rev_request: entry.rev_request.or(entry.tag),
                            commit: entry.commit,
                            content_hash: None,
                        })
                        .collect(),
                };
                lock.sort_entries();
                Ok(Some(lock))
            }
        }
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        let mut normalized = self.clone();
        normalized.version = LOCK_FILE_VERSION;
        normalized.sort_entries();
        let body = toml::to_string_pretty(&normalized)
            .map_err(|error| format!("failed to encode {}: {error}", path.display()))?;
        let mut out = String::from("# This file is auto-generated by Harn. Do not edit.\n\n");
        out.push_str(&body);
        fs::write(path, out).map_err(|error| format!("failed to write {}: {error}", path.display()))
    }

    fn sort_entries(&mut self) {
        self.packages
            .sort_by(|left, right| left.name.cmp(&right.name));
    }

    fn find(&self, name: &str) -> Option<&LockEntry> {
        self.packages.iter().find(|entry| entry.name == name)
    }

    fn replace(&mut self, entry: LockEntry) {
        if let Some(existing) = self.packages.iter_mut().find(|pkg| pkg.name == entry.name) {
            *existing = entry;
        } else {
            self.packages.push(entry);
        }
        self.sort_entries();
    }

    fn remove(&mut self, name: &str) {
        self.packages.retain(|entry| entry.name != name);
    }
}

#[derive(Debug, Deserialize)]
struct LegacyLockFile {
    #[serde(default, rename = "package")]
    packages: Vec<LegacyLockEntry>,
}

#[derive(Debug, Deserialize)]
struct LegacyLockEntry {
    name: String,
    #[serde(default)]
    git: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    rev_request: Option<String>,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

fn read_manifest_from_path(path: &Path) -> Result<Manifest, String> {
    let content = fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!(
                "No {} found in {}.",
                MANIFEST,
                path.parent().unwrap_or_else(|| Path::new(".")).display()
            )
        } else {
            format!("failed to read {}: {error}", path.display())
        }
    })?;
    toml::from_str::<Manifest>(&content)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn write_manifest_content(path: &Path, content: &str) -> Result<(), String> {
    fs::write(path, content).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn merge_capability_overrides(
    target: &mut harn_vm::llm::capabilities::CapabilitiesFile,
    source: &harn_vm::llm::capabilities::CapabilitiesFile,
) {
    for (provider, rules) in &source.provider {
        target
            .provider
            .entry(provider.clone())
            .or_default()
            .extend(rules.clone());
    }
    target
        .provider_family
        .extend(source.provider_family.clone());
}

fn resolved_hooks_from_manifest(
    manifest: &Manifest,
    manifest_dir: &Path,
) -> Vec<ResolvedHookConfig> {
    manifest
        .hooks
        .iter()
        .map(|hook| ResolvedHookConfig {
            event: hook.event,
            pattern: hook.pattern.clone(),
            handler: hook.handler.clone(),
            manifest_dir: manifest_dir.to_path_buf(),
            package_name: manifest.package.as_ref().and_then(|pkg| pkg.name.clone()),
            exports: manifest.exports.clone(),
        })
        .collect()
}

fn resolved_triggers_from_manifest(
    manifest: &Manifest,
    manifest_dir: &Path,
) -> Vec<ResolvedTriggerConfig> {
    let manifest_path = manifest_dir.join(MANIFEST);
    let package_name = manifest.package.as_ref().and_then(|pkg| pkg.name.clone());
    manifest
        .triggers
        .iter()
        .enumerate()
        .flat_map(|(table_index, trigger)| {
            resolved_trigger_entries_from_manifest_table(
                trigger,
                manifest_dir,
                &manifest_path,
                package_name.clone(),
                manifest.exports.clone(),
                table_index,
            )
        })
        .collect()
}

fn resolved_trigger_entries_from_manifest_table(
    trigger: &TriggerManifestEntry,
    manifest_dir: &Path,
    manifest_path: &Path,
    package_name: Option<String>,
    exports: HashMap<String, String>,
    table_index: usize,
) -> Vec<ResolvedTriggerConfig> {
    if trigger.sources.is_empty() {
        return vec![resolved_single_trigger_entry(
            trigger,
            manifest_dir,
            manifest_path,
            package_name,
            exports,
            table_index,
        )];
    }

    trigger
        .sources
        .iter()
        .enumerate()
        .map(|(source_index, source)| {
            resolved_trigger_source_entry(
                trigger,
                source,
                manifest_dir,
                manifest_path,
                package_name.clone(),
                exports.clone(),
                table_index,
                source_index,
            )
        })
        .collect()
}

fn resolved_single_trigger_entry(
    trigger: &TriggerManifestEntry,
    manifest_dir: &Path,
    manifest_path: &Path,
    package_name: Option<String>,
    exports: HashMap<String, String>,
    table_index: usize,
) -> ResolvedTriggerConfig {
    let shape_error = match (&trigger.kind, &trigger.provider) {
        (None, None) => {
            Some("trigger table must set kind/provider or declare one or more sources".to_string())
        }
        (None, Some(_)) => Some("trigger table missing kind".to_string()),
        (Some(_), None) => Some("trigger table missing provider".to_string()),
        (Some(_), Some(_)) => None,
    }
    .or_else(|| {
        trigger
            .match_
            .is_none()
            .then_some("trigger table missing match".to_string())
    });
    let (dispatch_priority, priority_flow) = split_trigger_priority(trigger.priority.clone());
    ResolvedTriggerConfig {
        id: trigger.id.clone(),
        kind: trigger.kind.unwrap_or(TriggerKind::Webhook),
        provider: trigger
            .provider
            .clone()
            .unwrap_or_else(|| harn_vm::ProviderId::from("")),
        autonomy_tier: trigger.autonomy_tier,
        match_: trigger.match_.clone().unwrap_or_default(),
        when: trigger.when.clone(),
        when_budget: trigger.when_budget.clone(),
        handler: trigger.handler.clone(),
        dedupe_key: trigger.dedupe_key.clone(),
        retry: trigger.retry.clone(),
        dispatch_priority,
        budget: trigger.budget.clone(),
        concurrency: trigger.concurrency.clone(),
        throttle: trigger.throttle.clone(),
        rate_limit: trigger.rate_limit.clone(),
        debounce: trigger.debounce.clone(),
        singleton: trigger.singleton.clone(),
        batch: trigger.batch.clone(),
        window: trigger.window.clone(),
        priority_flow,
        secrets: trigger.secrets.clone(),
        filter: trigger.filter.clone(),
        kind_specific: trigger.kind_specific.clone(),
        manifest_dir: manifest_dir.to_path_buf(),
        manifest_path: manifest_path.to_path_buf(),
        package_name,
        exports,
        table_index,
        shape_error,
    }
}

fn resolved_trigger_source_entry(
    trigger: &TriggerManifestEntry,
    source: &TriggerSourceManifestEntry,
    manifest_dir: &Path,
    manifest_path: &Path,
    package_name: Option<String>,
    exports: HashMap<String, String>,
    table_index: usize,
    source_index: usize,
) -> ResolvedTriggerConfig {
    let (dispatch_priority, priority_flow) =
        split_trigger_priority(source.priority.clone().or_else(|| trigger.priority.clone()));
    let mut kind_specific = trigger.kind_specific.clone();
    kind_specific.extend(source.kind_specific.clone());
    let mut secrets = trigger.secrets.clone();
    secrets.extend(source.secrets.clone());
    let source_label = source
        .id
        .clone()
        .unwrap_or_else(|| format!("source-{}", source_index + 1));
    ResolvedTriggerConfig {
        id: format!("{}.{}", trigger.id, source_label),
        kind: source.kind,
        provider: source.provider.clone(),
        autonomy_tier: trigger.autonomy_tier,
        match_: source.match_.clone().unwrap_or_default(),
        when: trigger.when.clone(),
        when_budget: trigger.when_budget.clone(),
        handler: trigger.handler.clone(),
        dedupe_key: source
            .dedupe_key
            .clone()
            .or_else(|| trigger.dedupe_key.clone()),
        retry: source
            .retry
            .clone()
            .unwrap_or_else(|| trigger.retry.clone()),
        dispatch_priority,
        budget: source
            .budget
            .clone()
            .unwrap_or_else(|| trigger.budget.clone()),
        concurrency: source
            .concurrency
            .clone()
            .or_else(|| trigger.concurrency.clone()),
        throttle: source.throttle.clone().or_else(|| trigger.throttle.clone()),
        rate_limit: source
            .rate_limit
            .clone()
            .or_else(|| trigger.rate_limit.clone()),
        debounce: source.debounce.clone().or_else(|| trigger.debounce.clone()),
        singleton: source
            .singleton
            .clone()
            .or_else(|| trigger.singleton.clone()),
        batch: source.batch.clone().or_else(|| trigger.batch.clone()),
        window: source.window.clone().or_else(|| trigger.window.clone()),
        priority_flow,
        secrets,
        filter: source.filter.clone().or_else(|| trigger.filter.clone()),
        kind_specific,
        manifest_dir: manifest_dir.to_path_buf(),
        manifest_path: manifest_path.to_path_buf(),
        package_name,
        exports,
        table_index,
        shape_error: source
            .match_
            .is_none()
            .then(|| format!("trigger source '{source_label}' missing match")),
    }
}

fn resolved_provider_connectors_from_manifest(
    manifest: &Manifest,
    manifest_dir: &Path,
) -> Vec<ResolvedProviderConnectorConfig> {
    manifest
        .providers
        .iter()
        .map(|provider| {
            let connector = match (
                provider.connector.harn.as_deref(),
                provider.connector.rust.as_deref(),
            ) {
                (Some(module), None) => ResolvedProviderConnectorKind::Harn {
                    module: module.to_string(),
                },
                (None, Some("builtin")) | (None, None) => {
                    ResolvedProviderConnectorKind::RustBuiltin
                }
                (None, Some(other)) => ResolvedProviderConnectorKind::Invalid(format!(
                    "provider '{}' uses unsupported connector.rust value '{other}'",
                    provider.id.as_str()
                )),
                (Some(_), Some(_)) => ResolvedProviderConnectorKind::Invalid(format!(
                    "provider '{}' cannot set both connector.harn and connector.rust",
                    provider.id.as_str()
                )),
            };
            ResolvedProviderConnectorConfig {
                id: provider.id.clone(),
                manifest_dir: manifest_dir.to_path_buf(),
                connector,
            }
        })
        .collect()
}

fn split_trigger_priority(
    priority: Option<TriggerPriorityField>,
) -> (TriggerDispatchPriority, Option<TriggerPriorityManifestSpec>) {
    match priority {
        Some(TriggerPriorityField::Dispatch(priority)) => (priority, None),
        Some(TriggerPriorityField::Flow(spec)) => (TriggerDispatchPriority::Normal, Some(spec)),
        None => (TriggerDispatchPriority::Normal, None),
    }
}

#[derive(Debug, Clone)]
struct TriggerFunctionSignature {
    params: Vec<Option<harn_parser::TypeExpr>>,
    return_type: Option<harn_parser::TypeExpr>,
}

fn manifest_trigger_location(trigger: &ResolvedTriggerConfig) -> String {
    format!(
        "{} [[triggers]] table #{} (id = {})",
        trigger.manifest_path.display(),
        trigger.table_index + 1,
        trigger.id
    )
}

fn trigger_error(trigger: &ResolvedTriggerConfig, message: impl Into<String>) -> String {
    format!("{}: {}", manifest_trigger_location(trigger), message.into())
}

fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn parse_local_trigger_ref(
    raw: &str,
    field_name: &str,
    trigger: &ResolvedTriggerConfig,
) -> Result<TriggerFunctionRef, String> {
    if raw.trim().is_empty() {
        return Err(trigger_error(
            trigger,
            format!("{field_name} cannot be empty"),
        ));
    }
    if raw.contains("://") {
        return Err(trigger_error(
            trigger,
            format!("{field_name} must reference a local function, not a URI"),
        ));
    }
    if let Some((module_name, function_name)) = raw.rsplit_once("::") {
        if module_name.trim().is_empty() || function_name.trim().is_empty() {
            return Err(trigger_error(
                trigger,
                format!("{field_name} must use <module>::<function> when module-qualified"),
            ));
        }
        if !valid_identifier(function_name) {
            return Err(trigger_error(
                trigger,
                format!("{field_name} function name '{function_name}' is not a valid identifier"),
            ));
        }
        return Ok(TriggerFunctionRef {
            raw: raw.to_string(),
            module_name: Some(module_name.to_string()),
            function_name: function_name.to_string(),
        });
    }
    if !valid_identifier(raw) {
        return Err(trigger_error(
            trigger,
            format!("{field_name} '{raw}' is not a valid bare function identifier"),
        ));
    }
    Ok(TriggerFunctionRef {
        raw: raw.to_string(),
        module_name: None,
        function_name: raw.to_string(),
    })
}

fn parse_trigger_handler_uri(trigger: &ResolvedTriggerConfig) -> Result<TriggerHandlerUri, String> {
    let raw = trigger.handler.trim();
    if let Some(target) = raw.strip_prefix("a2a://") {
        if target.is_empty() {
            return Err(trigger_error(
                trigger,
                "handler a2a:// target cannot be empty",
            ));
        }
        let allow_cleartext = extract_kind_field(trigger, "allow_cleartext")
            .map(parse_trigger_allow_cleartext)
            .transpose()?
            .unwrap_or(false);
        return Ok(TriggerHandlerUri::A2a {
            target: target.to_string(),
            allow_cleartext,
        });
    }
    if let Some(queue) = raw.strip_prefix("worker://") {
        if queue.is_empty() {
            return Err(trigger_error(
                trigger,
                "handler worker:// queue cannot be empty",
            ));
        }
        return Ok(TriggerHandlerUri::Worker {
            queue: queue.to_string(),
        });
    }
    if raw.contains("://") {
        return Err(trigger_error(
            trigger,
            format!("handler URI scheme in '{raw}' is not implemented"),
        ));
    }
    Ok(TriggerHandlerUri::Local(parse_local_trigger_ref(
        raw, "handler", trigger,
    )?))
}

fn parse_secret_id(raw: &str) -> Option<harn_vm::secrets::SecretId> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (base, version) = match trimmed.rsplit_once('@') {
        Some((base, version_text)) => {
            let version = version_text.parse::<u64>().ok()?;
            (base, harn_vm::secrets::SecretVersion::Exact(version))
        }
        None => (trimmed, harn_vm::secrets::SecretVersion::Latest),
    };
    let (namespace, name) = base.split_once('/')?;
    if namespace.is_empty() || name.is_empty() {
        return None;
    }
    Some(harn_vm::secrets::SecretId::new(namespace, name).with_version(version))
}

fn extract_kind_field<'a>(
    trigger: &'a ResolvedTriggerConfig,
    field: &str,
) -> Option<&'a toml::Value> {
    trigger.kind_specific.get(field)
}

fn looks_like_utc_offset_timezone(raw: &str) -> bool {
    let value = raw.trim();
    if let Some(rest) = value
        .strip_prefix("UTC")
        .or_else(|| value.strip_prefix("utc"))
        .or_else(|| value.strip_prefix("GMT"))
        .or_else(|| value.strip_prefix("gmt"))
    {
        return rest.starts_with('+') || rest.starts_with('-');
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < 3 || !matches!(chars[0], '+' | '-') {
        return false;
    }
    chars[1..]
        .iter()
        .all(|ch| ch.is_ascii_digit() || *ch == ':')
}

fn parse_jmespath_expression(
    trigger: &ResolvedTriggerConfig,
    field_name: &str,
    expr: &str,
) -> Result<(), String> {
    jmespath::compile(expr).map(|_| ()).map_err(|error| {
        trigger_error(
            trigger,
            format!("{field_name} '{expr}' is invalid: {error}"),
        )
    })
}

fn parse_duration_millis(raw: &str) -> Result<u64, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("duration cannot be empty".to_string());
    }
    let (value, unit) = trimmed
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(index, _)| (&trimmed[..index], &trimmed[index..]))
        .unwrap_or((trimmed, "ms"));
    let amount = value
        .parse::<u64>()
        .map_err(|_| format!("invalid duration '{raw}'"))?;
    let multiplier = match unit.trim() {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => {
            return Err(format!(
                "invalid duration unit in '{raw}'; expected ms, s, m, or h"
            ))
        }
    };
    Ok(amount.saturating_mul(multiplier))
}

fn validate_static_trigger_config(trigger: &ResolvedTriggerConfig) -> Result<(), String> {
    if let Some(message) = &trigger.shape_error {
        return Err(trigger_error(trigger, message));
    }
    if trigger.id.trim().is_empty() {
        return Err(trigger_error(trigger, "id cannot be empty"));
    }
    let Some(provider_metadata) = harn_vm::provider_metadata(trigger.provider.as_str()) else {
        return Err(trigger_error(
            trigger,
            format!("provider '{}' is not registered", trigger.provider.as_str()),
        ));
    };
    let kind_name = trigger_kind_label(trigger.kind);
    if !provider_metadata.supports_kind(kind_name) {
        return Err(trigger_error(
            trigger,
            format!(
                "provider '{}' does not support trigger kind '{}'",
                trigger.provider.as_str(),
                kind_name
            ),
        ));
    }
    for secret_name in provider_metadata.required_secret_names() {
        if !trigger.secrets.contains_key(secret_name) {
            return Err(trigger_error(
                trigger,
                format!(
                    "provider '{}' requires secret '{}'",
                    trigger.provider.as_str(),
                    secret_name
                ),
            ));
        }
    }
    if let Some(dedupe_key) = &trigger.dedupe_key {
        parse_jmespath_expression(trigger, "dedupe_key", dedupe_key)?;
    }
    if let Some(filter) = &trigger.filter {
        parse_jmespath_expression(trigger, "filter", filter)?;
    }
    if let Some(value) = extract_kind_field(trigger, "allow_cleartext") {
        let _ = parse_trigger_allow_cleartext(value)?;
        if !trigger.handler.trim().starts_with("a2a://") {
            return Err(trigger_error(
                trigger,
                "`allow_cleartext` is only valid for `a2a://...` handlers",
            ));
        }
    }
    if trigger.when_budget.is_some() && trigger.when.is_none() {
        return Err(trigger_error(
            trigger,
            "when_budget requires a when predicate",
        ));
    }
    if let Some(daily_cost_usd) = trigger.budget.daily_cost_usd {
        if daily_cost_usd.is_sign_negative() {
            return Err(trigger_error(
                trigger,
                "budget.daily_cost_usd must be greater than or equal to 0",
            ));
        }
    }
    if let Some(hourly_cost_usd) = trigger.budget.hourly_cost_usd {
        if hourly_cost_usd.is_sign_negative() {
            return Err(trigger_error(
                trigger,
                "budget.hourly_cost_usd must be greater than or equal to 0",
            ));
        }
    }
    if trigger.budget.max_autonomous_decisions_per_hour == Some(0) {
        return Err(trigger_error(
            trigger,
            "budget.max_autonomous_decisions_per_hour must be greater than or equal to 1",
        ));
    }
    if trigger.budget.max_autonomous_decisions_per_day == Some(0) {
        return Err(trigger_error(
            trigger,
            "budget.max_autonomous_decisions_per_day must be greater than or equal to 1",
        ));
    }
    if let Some(max_cost_usd) = trigger.budget.max_cost_usd {
        if max_cost_usd.is_sign_negative() {
            return Err(trigger_error(
                trigger,
                "budget.max_cost_usd must be greater than or equal to 0",
            ));
        }
    }
    if trigger.budget.max_tokens == Some(0) {
        return Err(trigger_error(
            trigger,
            "budget.max_tokens must be greater than or equal to 1",
        ));
    }
    if trigger.budget.max_concurrent == Some(0) {
        return Err(trigger_error(
            trigger,
            "budget.max_concurrent must be greater than or equal to 1",
        ));
    }
    if let Some(when_budget) = trigger.when_budget.as_ref() {
        if when_budget
            .max_cost_usd
            .is_some_and(|value| value.is_sign_negative())
        {
            return Err(trigger_error(
                trigger,
                "when_budget.max_cost_usd must be greater than or equal to 0",
            ));
        }
        if when_budget.tokens_max == Some(0) {
            return Err(trigger_error(
                trigger,
                "when_budget.tokens_max must be greater than or equal to 1",
            ));
        }
        if let Some(timeout) = when_budget.timeout.as_deref() {
            parse_duration_millis(timeout)
                .map_err(|error| trigger_error(trigger, format!("when_budget.timeout {error}")))?;
        }
    }
    if trigger.retry.max > TRIGGER_RETRY_MAX_LIMIT {
        return Err(trigger_error(
            trigger,
            format!("retry.max must be less than or equal to {TRIGGER_RETRY_MAX_LIMIT}"),
        ));
    }
    if trigger.retry.retention_days == 0 {
        return Err(trigger_error(
            trigger,
            "retry.retention_days must be greater than or equal to 1",
        ));
    }
    if let Some(spec) = &trigger.concurrency {
        if spec.max == 0 {
            return Err(trigger_error(
                trigger,
                "concurrency.max must be greater than or equal to 1",
            ));
        }
    }
    if let Some(spec) = &trigger.throttle {
        if spec.max == 0 {
            return Err(trigger_error(
                trigger,
                "throttle.max must be greater than or equal to 1",
            ));
        }
        harn_vm::parse_flow_control_duration(&spec.period)
            .map_err(|error| trigger_error(trigger, format!("throttle.period {error}")))?;
    }
    if let Some(spec) = &trigger.rate_limit {
        if spec.max == 0 {
            return Err(trigger_error(
                trigger,
                "rate_limit.max must be greater than or equal to 1",
            ));
        }
        harn_vm::parse_flow_control_duration(&spec.period)
            .map_err(|error| trigger_error(trigger, format!("rate_limit.period {error}")))?;
    }
    if let Some(spec) = &trigger.debounce {
        harn_vm::parse_flow_control_duration(&spec.period)
            .map_err(|error| trigger_error(trigger, format!("debounce.period {error}")))?;
    }
    if let Some(spec) = &trigger.batch {
        if spec.size == 0 {
            return Err(trigger_error(
                trigger,
                "batch.size must be greater than or equal to 1",
            ));
        }
        harn_vm::parse_flow_control_duration(&spec.timeout)
            .map_err(|error| trigger_error(trigger, format!("batch.timeout {error}")))?;
    }
    if let Some(spec) = &trigger.priority_flow {
        if spec.order.is_empty() {
            return Err(trigger_error(
                trigger,
                "priority.order must contain at least one value",
            ));
        }
    }
    if trigger.priority_flow.is_some()
        && trigger.concurrency.is_none()
        && trigger.budget.max_concurrent.is_none()
    {
        return Err(trigger_error(
            trigger,
            "priority requires concurrency.max so queued dispatches have a slot to compete for",
        ));
    }
    if trigger.batch.is_some()
        && (trigger.debounce.is_some()
            || trigger.singleton.is_some()
            || trigger.concurrency.is_some()
            || trigger.priority_flow.is_some()
            || trigger.throttle.is_some()
            || trigger.rate_limit.is_some()
            || trigger.budget.max_concurrent.is_some())
    {
        return Err(trigger_error(
            trigger,
            "batch cannot currently be combined with debounce, singleton, concurrency, priority, throttle, or rate_limit",
        ));
    }
    for (name, secret_ref) in &trigger.secrets {
        let Some(secret_id) = parse_secret_id(secret_ref) else {
            return Err(trigger_error(
                trigger,
                format!("secret '{name}' must use <namespace>/<name> syntax"),
            ));
        };
        if secret_id.namespace != trigger.provider.as_str() {
            return Err(trigger_error(
                trigger,
                format!(
                    "secret '{name}' uses namespace '{}' but provider is '{}'",
                    secret_id.namespace,
                    trigger.provider.as_str()
                ),
            ));
        }
    }
    if matches!(trigger.kind, TriggerKind::Cron) {
        let Some(schedule) = extract_kind_field(trigger, "schedule").and_then(toml::Value::as_str)
        else {
            return Err(trigger_error(
                trigger,
                "cron triggers require a string schedule field",
            ));
        };
        croner::Cron::from_str(schedule).map_err(|error| {
            trigger_error(
                trigger,
                format!("invalid cron schedule '{schedule}': {error}"),
            )
        })?;
        if let Some(timezone) =
            extract_kind_field(trigger, "timezone").and_then(toml::Value::as_str)
        {
            if looks_like_utc_offset_timezone(timezone) {
                return Err(trigger_error(
                    trigger,
                    format!(
                        "invalid cron timezone '{timezone}': use an IANA timezone name like 'America/New_York', not a UTC offset"
                    ),
                ));
            }
            timezone.parse::<Tz>().map_err(|error| {
                trigger_error(
                    trigger,
                    format!("invalid cron timezone '{timezone}': {error}"),
                )
            })?;
        }
    }
    if matches!(trigger.kind, TriggerKind::Stream) {
        validate_stream_trigger_config(trigger)?;
    } else if trigger.window.is_some() {
        return Err(trigger_error(
            trigger,
            "window is only valid for stream triggers",
        ));
    }
    Ok(())
}

fn validate_orchestrator_budget(manifest: Option<&Manifest>) -> Result<(), String> {
    let Some(manifest) = manifest else {
        return Ok(());
    };
    if manifest
        .orchestrator
        .budget
        .daily_cost_usd
        .is_some_and(|value| value.is_sign_negative())
    {
        return Err(
            "orchestrator.budget.daily_cost_usd must be greater than or equal to 0".to_string(),
        );
    }
    if manifest
        .orchestrator
        .budget
        .hourly_cost_usd
        .is_some_and(|value| value.is_sign_negative())
    {
        return Err(
            "orchestrator.budget.hourly_cost_usd must be greater than or equal to 0".to_string(),
        );
    }
    Ok(())
}

fn validate_stream_trigger_config(trigger: &ResolvedTriggerConfig) -> Result<(), String> {
    if let Some(window) = &trigger.window {
        validate_stream_window(trigger, window)?;
    }
    let provider = trigger.provider.as_str();
    let has_any = |fields: &[&str]| {
        fields.iter().any(|field| {
            extract_kind_field(trigger, field).is_some_and(|value| {
                value.as_str().is_some_and(|text| !text.trim().is_empty())
                    || value.as_array().is_some_and(|items| !items.is_empty())
                    || value.as_table().is_some_and(|table| !table.is_empty())
            })
        })
    };
    let required = match provider {
        "kafka" => (!has_any(&["topic", "topics"])).then_some("topic or topics"),
        "nats" => (!has_any(&["subject", "subjects"])).then_some("subject or subjects"),
        "pulsar" => (!has_any(&["topic", "topics"])).then_some("topic or topics"),
        "postgres-cdc" => (!has_any(&["slot"])).then_some("slot"),
        "email" => {
            (!has_any(&["address", "domain", "routing"])).then_some("address, domain, or routing")
        }
        "websocket" => (!has_any(&["url", "path"])).then_some("url or path"),
        _ => None,
    };
    if let Some(required) = required {
        return Err(trigger_error(
            trigger,
            format!("stream provider '{provider}' requires {required}"),
        ));
    }
    Ok(())
}

fn validate_stream_window(
    trigger: &ResolvedTriggerConfig,
    window: &TriggerStreamWindowManifestSpec,
) -> Result<(), String> {
    if window.max_items == Some(0) {
        return Err(trigger_error(
            trigger,
            "window.max_items must be greater than or equal to 1",
        ));
    }
    if let Some(size) = window.size.as_deref() {
        harn_vm::parse_flow_control_duration(size)
            .map_err(|error| trigger_error(trigger, format!("window.size {error}")))?;
    }
    if let Some(every) = window.every.as_deref() {
        harn_vm::parse_flow_control_duration(every)
            .map_err(|error| trigger_error(trigger, format!("window.every {error}")))?;
    }
    if let Some(gap) = window.gap.as_deref() {
        harn_vm::parse_flow_control_duration(gap)
            .map_err(|error| trigger_error(trigger, format!("window.gap {error}")))?;
    }
    match window.mode {
        TriggerStreamWindowMode::Tumbling => {
            if window.size.is_none() {
                return Err(trigger_error(
                    trigger,
                    "tumbling stream windows require window.size",
                ));
            }
            if window.every.is_some() || window.gap.is_some() {
                return Err(trigger_error(
                    trigger,
                    "tumbling stream windows cannot set window.every or window.gap",
                ));
            }
        }
        TriggerStreamWindowMode::Sliding => {
            if window.size.is_none() || window.every.is_none() {
                return Err(trigger_error(
                    trigger,
                    "sliding stream windows require window.size and window.every",
                ));
            }
            if window.gap.is_some() {
                return Err(trigger_error(
                    trigger,
                    "sliding stream windows cannot set window.gap",
                ));
            }
        }
        TriggerStreamWindowMode::Session => {
            if window.gap.is_none() {
                return Err(trigger_error(
                    trigger,
                    "session stream windows require window.gap",
                ));
            }
            if window.every.is_some() {
                return Err(trigger_error(
                    trigger,
                    "session stream windows cannot set window.every",
                ));
            }
        }
    }
    Ok(())
}

fn validate_static_trigger_configs(triggers: &[ResolvedTriggerConfig]) -> Result<(), String> {
    let mut seen_ids = HashSet::new();
    for trigger in triggers {
        validate_static_trigger_config(trigger)?;
        if !seen_ids.insert(trigger.id.clone()) {
            return Err(trigger_error(
                trigger,
                format!(
                    "duplicate trigger id '{}' across loaded manifests",
                    trigger.id
                ),
            ));
        }
    }
    Ok(())
}

fn parse_trigger_allow_cleartext(value: &toml::Value) -> Result<bool, String> {
    value
        .as_bool()
        .ok_or_else(|| "`allow_cleartext` must be a boolean".to_string())
}

fn manifest_module_source_path(
    manifest_dir: &Path,
    package_name: Option<&str>,
    exports: &HashMap<String, String>,
    module_name: Option<&str>,
) -> Result<PathBuf, String> {
    match module_name {
        None => {
            let path = manifest_dir.join("lib.harn");
            if path.exists() {
                Ok(path)
            } else {
                Err(format!(
                    "no lib.harn found next to manifest in {}",
                    manifest_dir.display()
                ))
            }
        }
        Some(module_name) if package_name.is_some_and(|pkg| pkg == module_name) => {
            let path = manifest_dir.join("lib.harn");
            if path.exists() {
                Ok(path)
            } else {
                Err(format!(
                    "module '{}' resolves to local lib.harn, but {} is missing",
                    module_name,
                    path.display()
                ))
            }
        }
        Some(module_name) if exports.contains_key(module_name) => {
            let rel_path = exports.get(module_name).expect("checked export key exists");
            let path = manifest_dir.join(rel_path);
            if path.exists() {
                Ok(path)
            } else {
                Err(format!(
                    "export '{}' resolves to {}, but that path does not exist",
                    module_name,
                    path.display()
                ))
            }
        }
        Some(module_name) => {
            let path = harn_vm::resolve_module_import_path(manifest_dir, module_name);
            if path.exists() {
                Ok(path)
            } else {
                Err(format!(
                    "module '{}' could not be resolved from {}",
                    module_name,
                    manifest_dir.display()
                ))
            }
        }
    }
}

fn load_trigger_function_signatures(
    path: &Path,
) -> Result<BTreeMap<String, TriggerFunctionSignature>, String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let program = harn_parser::parse_source(&source)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    let mut signatures = BTreeMap::new();
    for node in &program {
        let (_, inner) = harn_parser::peel_attributes(node);
        if let harn_parser::Node::FnDecl {
            name,
            params,
            return_type,
            ..
        } = &inner.node
        {
            signatures.insert(
                name.clone(),
                TriggerFunctionSignature {
                    params: params.iter().map(|param| param.type_expr.clone()).collect(),
                    return_type: return_type.clone(),
                },
            );
        }
    }
    Ok(signatures)
}

async fn resolve_manifest_exports(
    vm: &mut harn_vm::Vm,
    manifest_dir: &Path,
    package_name: Option<&str>,
    exports: &HashMap<String, String>,
    module_name: Option<&str>,
) -> Result<ManifestModuleExports, String> {
    match module_name {
        None => {
            let lib_path = manifest_module_source_path(manifest_dir, package_name, exports, None)?;
            vm.load_module_exports(&lib_path)
                .await
                .map_err(|error| error.to_string())
        }
        Some(module_name) if package_name.is_some_and(|name| name == module_name) => {
            let lib_path = manifest_module_source_path(
                manifest_dir,
                package_name,
                exports,
                Some(module_name),
            )?;
            vm.load_module_exports(&lib_path)
                .await
                .map_err(|error| error.to_string())
        }
        Some(module_name) if exports.contains_key(module_name) => {
            let lib_path = manifest_module_source_path(
                manifest_dir,
                package_name,
                exports,
                Some(module_name),
            )?;
            vm.load_module_exports(&lib_path)
                .await
                .map_err(|error| error.to_string())
        }
        Some(module_name) => vm
            .load_module_exports_from_import(module_name)
            .await
            .map_err(|error| error.to_string()),
    }
}

struct ManifestExtensionProviderSchema {
    provider_id: &'static str,
    schema_name: &'static str,
    metadata: harn_vm::ProviderMetadata,
}

impl harn_vm::ProviderSchema for ManifestExtensionProviderSchema {
    fn provider_id(&self) -> &'static str {
        self.provider_id
    }

    fn harn_schema_name(&self) -> &'static str {
        self.schema_name
    }

    fn metadata(&self) -> harn_vm::ProviderMetadata {
        self.metadata.clone()
    }

    fn normalize(
        &self,
        _kind: &str,
        _headers: &BTreeMap<String, String>,
        raw: serde_json::Value,
    ) -> Result<harn_vm::ProviderPayload, harn_vm::ProviderCatalogError> {
        Ok(harn_vm::ProviderPayload::Extension(
            harn_vm::triggers::ExtensionProviderPayload {
                provider: self.metadata.provider.clone(),
                schema_name: self.metadata.schema_name.clone(),
                raw,
            },
        ))
    }
}

fn leak_static_string(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

pub(crate) async fn install_manifest_provider_schemas(
    extensions: &RuntimeExtensions,
) -> Result<(), String> {
    let mut schemas: Vec<Arc<dyn harn_vm::ProviderSchema>> = Vec::new();
    for provider in &extensions.provider_connectors {
        match &provider.connector {
            ResolvedProviderConnectorKind::RustBuiltin => continue,
            ResolvedProviderConnectorKind::Invalid(message) => {
                return Err(message.clone());
            }
            ResolvedProviderConnectorKind::Harn { module } => {
                let module_path =
                    harn_vm::resolve_module_import_path(&provider.manifest_dir, module);
                let contract = harn_vm::connectors::harn_module::load_contract(&module_path)
                    .await
                    .map_err(|error| {
                        format!(
                            "failed to load connector module '{}' for provider '{}': {error}",
                            module_path.display(),
                            provider.id.as_str()
                        )
                    })?;
                if contract.provider_id != provider.id {
                    return Err(format!(
                        "provider '{}' resolves to connector module '{}' which declares provider_id '{}'",
                        provider.id.as_str(),
                        module_path.display(),
                        contract.provider_id.as_str()
                    ));
                }
                let metadata = harn_vm::ProviderMetadata {
                    provider: contract.provider_id.as_str().to_string(),
                    kinds: contract
                        .kinds
                        .iter()
                        .map(|kind| kind.as_str().to_string())
                        .collect(),
                    schema_name: contract.payload_schema.harn_schema_name.clone(),
                    runtime: harn_vm::ProviderRuntimeMetadata::Placeholder,
                    ..harn_vm::ProviderMetadata::default()
                };
                let schema = ManifestExtensionProviderSchema {
                    provider_id: leak_static_string(metadata.provider.clone()),
                    schema_name: leak_static_string(metadata.schema_name.clone()),
                    metadata,
                };
                schemas.push(Arc::new(schema));
            }
        }
    }
    harn_vm::reset_provider_catalog_with(schemas).map_err(|error| error.to_string())?;
    Ok(())
}

fn is_trigger_event_type(ty: &harn_parser::TypeExpr) -> bool {
    matches!(ty, harn_parser::TypeExpr::Named(name) if name == "TriggerEvent")
}

fn is_bool_type(ty: &harn_parser::TypeExpr) -> bool {
    matches!(ty, harn_parser::TypeExpr::Named(name) if name == "bool")
}

fn is_predicate_return_type(ty: &harn_parser::TypeExpr) -> bool {
    if is_bool_type(ty) {
        return true;
    }
    matches!(
        ty,
        harn_parser::TypeExpr::Applied { name, args }
            if name == "Result"
                && args.len() == 2
                && args.first().is_some_and(is_bool_type)
    )
}

fn manifest_capabilities(
    manifest: &Manifest,
) -> Option<&harn_vm::llm::capabilities::CapabilitiesFile> {
    manifest.capabilities.as_ref()
}

fn is_empty_capabilities(file: &harn_vm::llm::capabilities::CapabilitiesFile) -> bool {
    file.provider.is_empty() && file.provider_family.is_empty()
}

/// Load the nearest project manifest plus any installed package manifests and
/// merge the root project's runtime extensions.
pub fn try_load_runtime_extensions(anchor: &Path) -> Result<RuntimeExtensions, String> {
    ensure_dependencies_materialized(anchor)?;
    let Some((root_manifest, manifest_dir)) = find_nearest_manifest(anchor) else {
        return Ok(RuntimeExtensions::default());
    };

    let mut llm = harn_vm::llm_config::ProvidersConfig::default();
    let mut capabilities = harn_vm::llm::capabilities::CapabilitiesFile::default();
    let mut hooks = Vec::new();
    let mut triggers = Vec::new();

    llm.merge_from(&root_manifest.llm);
    if let Some(file) = manifest_capabilities(&root_manifest) {
        merge_capability_overrides(&mut capabilities, file);
    }
    hooks.extend(resolved_hooks_from_manifest(&root_manifest, &manifest_dir));
    triggers.extend(resolved_triggers_from_manifest(
        &root_manifest,
        &manifest_dir,
    ));
    let provider_connectors =
        resolved_provider_connectors_from_manifest(&root_manifest, &manifest_dir);

    Ok(RuntimeExtensions {
        root_manifest: Some(root_manifest),
        llm: (!llm.is_empty()).then_some(llm),
        capabilities: (!is_empty_capabilities(&capabilities)).then_some(capabilities),
        hooks,
        triggers,
        provider_connectors,
    })
}

pub fn load_runtime_extensions(anchor: &Path) -> RuntimeExtensions {
    match try_load_runtime_extensions(anchor) {
        Ok(extensions) => extensions,
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

/// Install merged runtime extensions on the current thread.
pub fn install_runtime_extensions(extensions: &RuntimeExtensions) {
    harn_vm::llm_config::set_user_overrides(extensions.llm.clone());
    harn_vm::llm::capabilities::set_user_overrides(extensions.capabilities.clone());
    install_orchestrator_budget(extensions);
}

pub fn install_orchestrator_budget(extensions: &RuntimeExtensions) {
    let budget = extensions
        .root_manifest
        .as_ref()
        .map(|manifest| harn_vm::OrchestratorBudgetConfig {
            daily_cost_usd: manifest.orchestrator.budget.daily_cost_usd,
            hourly_cost_usd: manifest.orchestrator.budget.hourly_cost_usd,
        })
        .unwrap_or_default();
    harn_vm::install_orchestrator_budget(budget);
}

pub async fn install_manifest_hooks(
    vm: &mut harn_vm::Vm,
    extensions: &RuntimeExtensions,
) -> Result<(), String> {
    harn_vm::orchestration::clear_runtime_hooks();
    let mut loaded_exports: HashMap<ManifestModuleCacheKey, ManifestModuleExports> = HashMap::new();
    for hook in &extensions.hooks {
        let Some((module_name, function_name)) = hook.handler.rsplit_once("::") else {
            return Err(format!(
                "invalid hook handler '{}': expected <module>::<function>",
                hook.handler
            ));
        };
        let cache_key = (
            hook.manifest_dir.clone(),
            hook.package_name.clone(),
            Some(module_name.to_string()),
        );
        if !loaded_exports.contains_key(&cache_key) {
            let exports = resolve_manifest_exports(
                vm,
                &hook.manifest_dir,
                hook.package_name.as_deref(),
                &hook.exports,
                Some(module_name),
            )
            .await?;
            loaded_exports.insert(cache_key.clone(), exports);
        }
        let exports = loaded_exports
            .get(&cache_key)
            .expect("manifest hook exports cached");
        let Some(closure) = exports.get(function_name) else {
            return Err(format!(
                "hook handler '{}' is not exported by module '{}'",
                function_name, module_name
            ));
        };
        harn_vm::orchestration::register_vm_hook(
            hook.event,
            hook.pattern.clone(),
            hook.handler.clone(),
            closure.clone(),
        );
    }
    Ok(())
}

pub async fn collect_manifest_triggers(
    vm: &mut harn_vm::Vm,
    extensions: &RuntimeExtensions,
) -> Result<Vec<CollectedManifestTrigger>, String> {
    let _provider_schema_guard = lock_manifest_provider_schemas().await;
    install_manifest_provider_schemas(extensions).await?;
    validate_orchestrator_budget(extensions.root_manifest.as_ref())?;
    validate_static_trigger_configs(&extensions.triggers)?;
    let mut loaded_exports: HashMap<ManifestModuleCacheKey, ManifestModuleExports> = HashMap::new();
    let mut module_signatures: HashMap<PathBuf, BTreeMap<String, TriggerFunctionSignature>> =
        HashMap::new();
    let mut collected = Vec::new();

    for trigger in &extensions.triggers {
        let handler = parse_trigger_handler_uri(trigger)?;
        let collected_handler = match handler {
            TriggerHandlerUri::Local(reference) => {
                let cache_key = (
                    trigger.manifest_dir.clone(),
                    trigger.package_name.clone(),
                    reference.module_name.clone(),
                );
                if !loaded_exports.contains_key(&cache_key) {
                    let exports = resolve_manifest_exports(
                        vm,
                        &trigger.manifest_dir,
                        trigger.package_name.as_deref(),
                        &trigger.exports,
                        reference.module_name.as_deref(),
                    )
                    .await
                    .map_err(|error| trigger_error(trigger, error))?;
                    loaded_exports.insert(cache_key.clone(), exports);
                }
                let exports = loaded_exports
                    .get(&cache_key)
                    .expect("manifest trigger exports cached");
                let Some(closure) = exports.get(&reference.function_name) else {
                    return Err(trigger_error(
                        trigger,
                        format!(
                            "handler '{}' is not exported by the resolved module",
                            reference.raw
                        ),
                    ));
                };
                CollectedTriggerHandler::Local {
                    reference,
                    closure: closure.clone(),
                }
            }
            TriggerHandlerUri::A2a {
                target,
                allow_cleartext,
            } => CollectedTriggerHandler::A2a {
                target,
                allow_cleartext,
            },
            TriggerHandlerUri::Worker { queue } => CollectedTriggerHandler::Worker { queue },
        };

        let collected_when = if let Some(when_raw) = &trigger.when {
            let reference = parse_local_trigger_ref(when_raw, "when", trigger)?;
            let cache_key = (
                trigger.manifest_dir.clone(),
                trigger.package_name.clone(),
                reference.module_name.clone(),
            );
            if !loaded_exports.contains_key(&cache_key) {
                let exports = resolve_manifest_exports(
                    vm,
                    &trigger.manifest_dir,
                    trigger.package_name.as_deref(),
                    &trigger.exports,
                    reference.module_name.as_deref(),
                )
                .await
                .map_err(|error| trigger_error(trigger, error))?;
                loaded_exports.insert(cache_key.clone(), exports);
            }
            let exports = loaded_exports
                .get(&cache_key)
                .expect("manifest trigger predicate exports cached");
            let Some(closure) = exports.get(&reference.function_name) else {
                return Err(trigger_error(
                    trigger,
                    format!(
                        "when predicate '{}' is not exported by the resolved module",
                        reference.raw
                    ),
                ));
            };

            let source_path = manifest_module_source_path(
                &trigger.manifest_dir,
                trigger.package_name.as_deref(),
                &trigger.exports,
                reference.module_name.as_deref(),
            )
            .map_err(|error| trigger_error(trigger, error))?;
            if !module_signatures.contains_key(&source_path) {
                let signatures = load_trigger_function_signatures(&source_path)
                    .map_err(|error| trigger_error(trigger, error))?;
                module_signatures.insert(source_path.clone(), signatures);
            }
            let signatures = module_signatures
                .get(&source_path)
                .expect("module signatures cached");
            let Some(signature) = signatures.get(&reference.function_name) else {
                return Err(trigger_error(
                    trigger,
                    format!(
                        "when predicate '{}' must resolve to a function declaration",
                        reference.raw
                    ),
                ));
            };
            if signature.params.len() != 1
                || signature.params[0]
                    .as_ref()
                    .is_none_or(|param| !is_trigger_event_type(param))
            {
                return Err(trigger_error(
                    trigger,
                    format!(
                        "when predicate '{}' must have signature fn(TriggerEvent) -> bool",
                        reference.raw
                    ),
                ));
            }
            if signature
                .return_type
                .as_ref()
                .is_none_or(|return_type| !is_predicate_return_type(return_type))
            {
                return Err(trigger_error(
                    trigger,
                    format!(
                        "when predicate '{}' must have signature fn(TriggerEvent) -> bool or Result<bool, _>",
                        reference.raw
                    ),
                ));
            }

            Some(CollectedTriggerPredicate {
                reference,
                closure: closure.clone(),
            })
        } else {
            None
        };

        let flow_control = collect_trigger_flow_control(vm, trigger).await?;

        collected.push(CollectedManifestTrigger {
            config: trigger.clone(),
            handler: collected_handler,
            when: collected_when,
            flow_control,
        });
    }

    Ok(collected)
}

async fn collect_trigger_flow_control(
    vm: &mut harn_vm::Vm,
    trigger: &ResolvedTriggerConfig,
) -> Result<harn_vm::TriggerFlowControlConfig, String> {
    let mut flow = harn_vm::TriggerFlowControlConfig::default();

    let concurrency = if let Some(spec) = &trigger.concurrency {
        Some(spec.clone())
    } else if let Some(max) = trigger.budget.max_concurrent {
        eprintln!(
            "warning: {} uses deprecated budget.max_concurrent; prefer concurrency = {{ max = {} }}",
            manifest_trigger_location(trigger),
            max
        );
        Some(TriggerConcurrencyManifestSpec { key: None, max })
    } else {
        None
    };
    if let Some(spec) = concurrency {
        flow.concurrency = Some(harn_vm::TriggerConcurrencyConfig {
            key: compile_optional_trigger_expression(
                vm,
                trigger,
                "concurrency.key",
                spec.key.as_deref(),
            )
            .await?,
            max: spec.max,
        });
    }

    if let Some(spec) = &trigger.throttle {
        flow.throttle = Some(harn_vm::TriggerThrottleConfig {
            key: compile_optional_trigger_expression(
                vm,
                trigger,
                "throttle.key",
                spec.key.as_deref(),
            )
            .await?,
            period: harn_vm::parse_flow_control_duration(&spec.period)
                .map_err(|error| trigger_error(trigger, format!("throttle.period {error}")))?,
            max: spec.max,
        });
    }

    if let Some(spec) = &trigger.rate_limit {
        flow.rate_limit = Some(harn_vm::TriggerRateLimitConfig {
            key: compile_optional_trigger_expression(
                vm,
                trigger,
                "rate_limit.key",
                spec.key.as_deref(),
            )
            .await?,
            period: harn_vm::parse_flow_control_duration(&spec.period)
                .map_err(|error| trigger_error(trigger, format!("rate_limit.period {error}")))?,
            max: spec.max,
        });
    }

    if let Some(spec) = &trigger.debounce {
        flow.debounce = Some(harn_vm::TriggerDebounceConfig {
            key: compile_trigger_expression(vm, trigger, "debounce.key", &spec.key).await?,
            period: harn_vm::parse_flow_control_duration(&spec.period)
                .map_err(|error| trigger_error(trigger, format!("debounce.period {error}")))?,
        });
    }

    if let Some(spec) = &trigger.singleton {
        flow.singleton = Some(harn_vm::TriggerSingletonConfig {
            key: compile_optional_trigger_expression(
                vm,
                trigger,
                "singleton.key",
                spec.key.as_deref(),
            )
            .await?,
        });
    }

    if let Some(spec) = &trigger.batch {
        flow.batch = Some(harn_vm::TriggerBatchConfig {
            key: compile_optional_trigger_expression(vm, trigger, "batch.key", spec.key.as_deref())
                .await?,
            size: spec.size,
            timeout: harn_vm::parse_flow_control_duration(&spec.timeout)
                .map_err(|error| trigger_error(trigger, format!("batch.timeout {error}")))?,
        });
    }

    if let Some(spec) = &trigger.priority_flow {
        flow.priority = Some(harn_vm::TriggerPriorityOrderConfig {
            key: compile_trigger_expression(vm, trigger, "priority.key", &spec.key).await?,
            order: spec.order.clone(),
        });
    }

    Ok(flow)
}

async fn compile_optional_trigger_expression(
    vm: &mut harn_vm::Vm,
    trigger: &ResolvedTriggerConfig,
    field_name: &str,
    expr: Option<&str>,
) -> Result<Option<harn_vm::TriggerExpressionSpec>, String> {
    match expr {
        Some(expr) => compile_trigger_expression(vm, trigger, field_name, expr)
            .await
            .map(Some),
        None => Ok(None),
    }
}

async fn compile_trigger_expression(
    vm: &mut harn_vm::Vm,
    trigger: &ResolvedTriggerConfig,
    field_name: &str,
    expr: &str,
) -> Result<harn_vm::TriggerExpressionSpec, String> {
    let synthetic = PathBuf::from(format!(
        "<trigger-expr>/{}/{:04}-{}.harn",
        harn_vm::event_log::sanitize_topic_component(&trigger.id),
        trigger.table_index,
        harn_vm::event_log::sanitize_topic_component(field_name),
    ));
    let source = format!(
        "import \"std/triggers\"\n\npub fn __trigger_expr(event: TriggerEvent) -> any {{\n  return {expr}\n}}\n"
    );
    let exports = vm
        .load_module_exports_from_source(synthetic, &source)
        .await
        .map_err(|error| {
            trigger_error(
                trigger,
                format!("{field_name} '{expr}' is invalid Harn expression: {error}"),
            )
        })?;
    let closure = exports.get("__trigger_expr").ok_or_else(|| {
        trigger_error(
            trigger,
            format!("{field_name} '{expr}' did not compile into an exported closure"),
        )
    })?;
    Ok(harn_vm::TriggerExpressionSpec {
        raw: expr.to_string(),
        closure: closure.clone(),
    })
}

fn trigger_kind_label(kind: TriggerKind) -> &'static str {
    match kind {
        TriggerKind::Webhook => "webhook",
        TriggerKind::Cron => "cron",
        TriggerKind::Poll => "poll",
        TriggerKind::Stream => "stream",
        TriggerKind::Predicate => "predicate",
        TriggerKind::A2aPush => "a2a-push",
    }
}

fn worker_queue_priority(priority: TriggerDispatchPriority) -> harn_vm::WorkerQueuePriority {
    match priority {
        TriggerDispatchPriority::High => harn_vm::WorkerQueuePriority::High,
        TriggerDispatchPriority::Normal => harn_vm::WorkerQueuePriority::Normal,
        TriggerDispatchPriority::Low => harn_vm::WorkerQueuePriority::Low,
    }
}

pub fn manifest_trigger_binding_spec(
    trigger: CollectedManifestTrigger,
) -> harn_vm::TriggerBindingSpec {
    let flow_control = trigger.flow_control.clone();
    let config = trigger.config;
    let (handler, handler_descriptor) = match trigger.handler {
        CollectedTriggerHandler::Local { reference, closure } => (
            harn_vm::TriggerHandlerSpec::Local {
                raw: reference.raw.clone(),
                closure,
            },
            serde_json::json!({
                "kind": "local",
                "raw": reference.raw,
            }),
        ),
        CollectedTriggerHandler::A2a {
            target,
            allow_cleartext,
        } => (
            harn_vm::TriggerHandlerSpec::A2a {
                target: target.clone(),
                allow_cleartext,
            },
            serde_json::json!({
                "kind": "a2a",
                "target": target,
                "allow_cleartext": allow_cleartext,
            }),
        ),
        CollectedTriggerHandler::Worker { queue } => (
            harn_vm::TriggerHandlerSpec::Worker {
                queue: queue.clone(),
            },
            serde_json::json!({
                "kind": "worker",
                "queue": queue,
            }),
        ),
    };

    let when_raw = trigger
        .when
        .as_ref()
        .map(|predicate| predicate.reference.raw.clone());
    let when = trigger.when.map(|predicate| harn_vm::TriggerPredicateSpec {
        raw: predicate.reference.raw,
        closure: predicate.closure,
    });
    let mut when_budget = config
        .when_budget
        .as_ref()
        .map(|budget| {
            Ok::<harn_vm::TriggerPredicateBudget, String>(harn_vm::TriggerPredicateBudget {
                max_cost_usd: budget.max_cost_usd,
                tokens_max: budget.tokens_max,
                timeout_ms: budget
                    .timeout
                    .as_deref()
                    .map(parse_duration_millis)
                    .transpose()?,
            })
        })
        .transpose()
        .unwrap_or_default();
    if config.budget.max_cost_usd.is_some() || config.budget.max_tokens.is_some() {
        let budget = when_budget.get_or_insert_with(harn_vm::TriggerPredicateBudget::default);
        if budget.max_cost_usd.is_none() {
            budget.max_cost_usd = config.budget.max_cost_usd;
        }
        if budget.tokens_max.is_none() {
            budget.tokens_max = config.budget.max_tokens;
        }
    }
    let id = config.id.clone();
    let kind = trigger_kind_label(config.kind).to_string();
    let provider = config.provider.clone();
    let autonomy_tier = config.autonomy_tier;
    let match_events = config.match_.events.clone();
    let dedupe_key = config.dedupe_key.clone();
    let retry = harn_vm::TriggerRetryConfig::new(
        config.retry.max,
        match config.retry.backoff {
            TriggerRetryBackoff::Immediate => harn_vm::RetryPolicy::Linear { delay_ms: 0 },
            TriggerRetryBackoff::Svix => harn_vm::RetryPolicy::Svix,
        },
    );
    let filter = config.filter.clone();
    let dedupe_retention_days = config.retry.retention_days;
    let daily_cost_usd = config.budget.daily_cost_usd;
    let hourly_cost_usd = config.budget.hourly_cost_usd;
    let max_autonomous_decisions_per_hour = config.budget.max_autonomous_decisions_per_hour;
    let max_autonomous_decisions_per_day = config.budget.max_autonomous_decisions_per_day;
    let on_budget_exhausted = config.budget.on_budget_exhausted;
    let max_concurrent = flow_control.concurrency.as_ref().map(|config| config.max);
    let manifest_path = Some(config.manifest_path.clone());
    let package_name = config.package_name.clone();

    let fingerprint = serde_json::to_string(&serde_json::json!({
        "id": &id,
        "kind": &kind,
        "provider": provider.as_str(),
        "autonomy_tier": autonomy_tier,
        "match": config.match_,
        "when": when_raw,
        "when_budget": config.when_budget,
        "handler": handler_descriptor,
        "dedupe_key": &dedupe_key,
        "retry": config.retry,
        "dispatch_priority": config.dispatch_priority,
        "budget": config.budget,
        "flow_control": {
            "concurrency": config.concurrency,
            "throttle": config.throttle,
            "rate_limit": config.rate_limit,
            "debounce": config.debounce,
            "singleton": config.singleton,
            "batch": config.batch,
            "priority": config.priority_flow,
        },
        "window": config.window,
        "secrets": config.secrets,
        "filter": &filter,
        "kind_specific": config.kind_specific,
        "manifest_path": &manifest_path,
        "package_name": &package_name,
    }))
    .unwrap_or_else(|_| format!("{}:{}:{}", id, kind, provider.as_str()));

    harn_vm::TriggerBindingSpec {
        id,
        source: harn_vm::TriggerBindingSource::Manifest,
        kind,
        provider,
        autonomy_tier,
        handler,
        dispatch_priority: worker_queue_priority(config.dispatch_priority),
        when,
        when_budget,
        retry,
        match_events,
        dedupe_key,
        filter,
        dedupe_retention_days,
        daily_cost_usd,
        hourly_cost_usd,
        max_autonomous_decisions_per_hour,
        max_autonomous_decisions_per_day,
        on_budget_exhausted,
        max_concurrent,
        flow_control,
        manifest_path,
        package_name,
        definition_fingerprint: fingerprint,
    }
}

pub async fn install_manifest_triggers(
    vm: &mut harn_vm::Vm,
    extensions: &RuntimeExtensions,
) -> Result<(), String> {
    install_orchestrator_budget(extensions);
    let collected = collect_manifest_triggers(vm, extensions).await?;
    install_collected_manifest_triggers(&collected).await
}

pub async fn install_collected_manifest_triggers(
    collected: &[CollectedManifestTrigger],
) -> Result<(), String> {
    let bindings = collected
        .iter()
        .cloned()
        .map(manifest_trigger_binding_spec)
        .collect();
    harn_vm::install_manifest_triggers(bindings)
        .await
        .map_err(|error| error.to_string())
}

fn absolutize_check_config_paths(mut config: CheckConfig, manifest_dir: &Path) -> CheckConfig {
    if let Some(path) = config.host_capabilities_path.clone() {
        let candidate = PathBuf::from(&path);
        if !candidate.is_absolute() {
            config.host_capabilities_path =
                Some(manifest_dir.join(candidate).display().to_string());
        }
    }
    if let Some(path) = config.bundle_root.clone() {
        let candidate = PathBuf::from(&path);
        if !candidate.is_absolute() {
            config.bundle_root = Some(manifest_dir.join(candidate).display().to_string());
        }
    }
    config
}

/// Walk upward from `start` (or its parent if it's a file path that
/// does not yet exist) looking for the nearest `harn.toml`. Stops at
/// a `.git` boundary so a stray manifest in `$HOME` or a parent
/// project is never silently picked up. Returns `(manifest, manifest_dir)`
/// when found.
fn find_nearest_manifest(start: &Path) -> Option<(Manifest, PathBuf)> {
    const MAX_PARENT_DIRS: usize = 16;
    let base = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(start)
    };
    let mut cursor: Option<PathBuf> = if base.is_dir() {
        Some(base)
    } else {
        base.parent().map(Path::to_path_buf)
    };
    let mut steps = 0usize;
    while let Some(dir) = cursor {
        if steps >= MAX_PARENT_DIRS {
            break;
        }
        steps += 1;
        let candidate = dir.join(MANIFEST);
        if candidate.is_file() {
            match read_manifest_from_path(&candidate) {
                Ok(manifest) => return Some((manifest, dir)),
                Err(error) => {
                    eprintln!("warning: {error}");
                    return None;
                }
            }
        }
        if dir.join(".git").exists() {
            break;
        }
        cursor = dir.parent().map(Path::to_path_buf);
    }
    None
}

/// Load the `[check]` config from the nearest `harn.toml`.
/// Walks up from the given file (or from cwd if no file is given),
/// stopping at a `.git` boundary.
pub fn load_check_config(harn_file: Option<&std::path::Path>) -> CheckConfig {
    let anchor = harn_file
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    if let Some((manifest, dir)) = find_nearest_manifest(&anchor) {
        return absolutize_check_config_paths(manifest.check, &dir);
    }
    CheckConfig::default()
}

/// Load the `[workspace]` config and the directory of the `harn.toml`
/// it came from. Paths in the returned config are left as-is (callers
/// resolve them against the returned `manifest_dir`).
pub fn load_workspace_config(anchor: Option<&Path>) -> Option<(WorkspaceConfig, PathBuf)> {
    let anchor = anchor
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let (manifest, dir) = find_nearest_manifest(&anchor)?;
    Some((manifest.workspace, dir))
}

pub fn load_personas_from_manifest_path(
    manifest_path: &Path,
) -> Result<ResolvedPersonaManifest, Vec<PersonaValidationError>> {
    let manifest_path = if manifest_path.is_dir() {
        manifest_path.join(MANIFEST)
    } else {
        manifest_path.to_path_buf()
    };
    let manifest_dir = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let manifest = read_manifest_from_path(&manifest_path).map_err(|message| {
        vec![PersonaValidationError {
            manifest_path: manifest_path.clone(),
            field_path: "harn.toml".to_string(),
            message,
        }]
    })?;
    validate_and_resolve_personas(manifest, manifest_path, manifest_dir)
}

pub fn load_personas_config(
    anchor: Option<&Path>,
) -> Result<Option<ResolvedPersonaManifest>, Vec<PersonaValidationError>> {
    let anchor = anchor
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let Some((manifest, dir)) = find_nearest_manifest(&anchor) else {
        return Ok(None);
    };
    let manifest_path = dir.join(MANIFEST);
    validate_and_resolve_personas(manifest, manifest_path, dir).map(Some)
}

fn validate_and_resolve_personas(
    manifest: Manifest,
    manifest_path: PathBuf,
    manifest_dir: PathBuf,
) -> Result<ResolvedPersonaManifest, Vec<PersonaValidationError>> {
    let known_capabilities = known_persona_capabilities(&manifest, &manifest_dir);
    let known_tools = known_persona_tools(&manifest);
    let known_names: BTreeSet<String> = manifest
        .personas
        .iter()
        .filter_map(|persona| persona.name.as_ref())
        .filter(|name| !name.trim().is_empty())
        .cloned()
        .collect();
    let mut errors = Vec::new();
    for (index, persona) in manifest.personas.iter().enumerate() {
        validate_persona(
            persona,
            index,
            &manifest_path,
            &known_capabilities,
            &known_tools,
            &known_names,
            &mut errors,
        );
    }
    if errors.is_empty() {
        Ok(ResolvedPersonaManifest {
            manifest_path,
            manifest_dir,
            personas: manifest.personas,
        })
    } else {
        Err(errors)
    }
}

fn validate_persona(
    persona: &PersonaManifestEntry,
    index: usize,
    manifest_path: &Path,
    known_capabilities: &BTreeSet<String>,
    known_tools: &BTreeSet<String>,
    known_names: &BTreeSet<String>,
    errors: &mut Vec<PersonaValidationError>,
) {
    let root = format!("[[personas]][{index}]");
    for field in persona.extra.keys() {
        persona_error(
            manifest_path,
            format!("{root}.{field}"),
            "unknown persona field",
            errors,
        );
    }
    let name = validate_required_string(
        manifest_path,
        &root,
        "name",
        persona.name.as_deref(),
        errors,
    );
    if let Some(name) = name {
        validate_tokenish(manifest_path, &root, "name", name, errors);
    }
    validate_required_string(
        manifest_path,
        &root,
        "description",
        persona.description.as_deref(),
        errors,
    );
    validate_required_string(
        manifest_path,
        &root,
        "entry_workflow",
        persona.entry_workflow.as_deref(),
        errors,
    );
    if persona.tools.is_empty() && persona.capabilities.is_empty() {
        persona_error(
            manifest_path,
            format!("{root}.tools"),
            "persona requires at least one tool or capability",
            errors,
        );
    }
    if persona.autonomy_tier.is_none() {
        persona_error(
            manifest_path,
            format!("{root}.autonomy_tier"),
            "missing required autonomy tier",
            errors,
        );
    }
    if persona.receipt_policy.is_none() {
        persona_error(
            manifest_path,
            format!("{root}.receipt_policy"),
            "missing required receipt policy",
            errors,
        );
    }
    validate_string_list(manifest_path, &root, "tools", &persona.tools, errors);
    for tool in &persona.tools {
        if !known_tools.contains(tool) {
            persona_error(
                manifest_path,
                format!("{root}.tools"),
                format!("unknown tool '{tool}'"),
                errors,
            );
        }
    }
    for capability in &persona.capabilities {
        let Some((cap, op)) = capability.split_once('.') else {
            persona_error(
                manifest_path,
                format!("{root}.capabilities"),
                format!("capability '{capability}' must use capability.operation syntax"),
                errors,
            );
            continue;
        };
        if cap.trim().is_empty() || op.trim().is_empty() {
            persona_error(
                manifest_path,
                format!("{root}.capabilities"),
                format!("capability '{capability}' must use capability.operation syntax"),
                errors,
            );
        } else if !known_capabilities.contains(capability) {
            persona_error(
                manifest_path,
                format!("{root}.capabilities"),
                format!("unknown capability '{capability}'"),
                errors,
            );
        }
    }
    validate_string_list(
        manifest_path,
        &root,
        "context_packs",
        &persona.context_packs,
        errors,
    );
    validate_string_list(manifest_path, &root, "evals", &persona.evals, errors);
    for schedule in &persona.schedules {
        if schedule.trim().is_empty() {
            persona_error(
                manifest_path,
                format!("{root}.schedules"),
                "schedule entries must not be empty",
                errors,
            );
        } else if let Err(error) = croner::Cron::from_str(schedule) {
            persona_error(
                manifest_path,
                format!("{root}.schedules"),
                format!("invalid cron schedule '{schedule}': {error}"),
                errors,
            );
        }
    }
    for trigger in &persona.triggers {
        if !trigger.contains('.') {
            persona_error(
                manifest_path,
                format!("{root}.triggers"),
                format!("trigger '{trigger}' must use provider.event syntax"),
                errors,
            );
        }
    }
    for handoff in &persona.handoffs {
        if !known_names.contains(handoff) {
            persona_error(
                manifest_path,
                format!("{root}.handoffs"),
                format!("unknown handoff target '{handoff}'"),
                errors,
            );
        }
    }
    validate_persona_budget(manifest_path, &root, &persona.budget, errors);
    validate_persona_nested_extra(
        manifest_path,
        &root,
        "model_policy",
        &persona.model_policy.extra,
        errors,
    );
    validate_persona_nested_extra(
        manifest_path,
        &root,
        "package_source",
        &persona.package_source.extra,
        errors,
    );
    validate_persona_nested_extra(
        manifest_path,
        &root,
        "rollout_policy",
        &persona.rollout_policy.extra,
        errors,
    );
    if let Some(percentage) = persona.rollout_policy.percentage {
        if percentage > 100 {
            persona_error(
                manifest_path,
                format!("{root}.rollout_policy.percentage"),
                "rollout percentage must be between 0 and 100",
                errors,
            );
        }
    }
}

fn validate_required_string<'a>(
    manifest_path: &Path,
    root: &str,
    field: &str,
    value: Option<&'a str>,
    errors: &mut Vec<PersonaValidationError>,
) -> Option<&'a str> {
    match value.map(str::trim) {
        Some(value) if !value.is_empty() => Some(value),
        _ => {
            persona_error(
                manifest_path,
                format!("{root}.{field}"),
                format!("missing required {field}"),
                errors,
            );
            None
        }
    }
}

fn validate_string_list(
    manifest_path: &Path,
    root: &str,
    field: &str,
    values: &[String],
    errors: &mut Vec<PersonaValidationError>,
) {
    for value in values {
        if value.trim().is_empty() {
            persona_error(
                manifest_path,
                format!("{root}.{field}"),
                format!("{field} entries must not be empty"),
                errors,
            );
        } else {
            validate_tokenish(manifest_path, root, field, value, errors);
        }
    }
}

fn validate_tokenish(
    manifest_path: &Path,
    root: &str,
    field: &str,
    value: &str,
    errors: &mut Vec<PersonaValidationError>,
) {
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/'))
    {
        persona_error(
            manifest_path,
            format!("{root}.{field}"),
            format!("'{value}' must contain only letters, numbers, '.', '-', '_', or '/'"),
            errors,
        );
    }
}

fn validate_persona_budget(
    manifest_path: &Path,
    root: &str,
    budget: &PersonaBudget,
    errors: &mut Vec<PersonaValidationError>,
) {
    validate_persona_nested_extra(manifest_path, root, "budget", &budget.extra, errors);
    for (field, value) in [
        ("daily_usd", budget.daily_usd),
        ("hourly_usd", budget.hourly_usd),
        ("run_usd", budget.run_usd),
    ] {
        if value.is_some_and(|number| !number.is_finite() || number < 0.0) {
            persona_error(
                manifest_path,
                format!("{root}.budget.{field}"),
                "budget amounts must be finite non-negative numbers",
                errors,
            );
        }
    }
}

fn validate_persona_nested_extra(
    manifest_path: &Path,
    root: &str,
    field: &str,
    extra: &BTreeMap<String, toml::Value>,
    errors: &mut Vec<PersonaValidationError>,
) {
    for key in extra.keys() {
        persona_error(
            manifest_path,
            format!("{root}.{field}.{key}"),
            format!("unknown {field} field"),
            errors,
        );
    }
}

fn persona_error(
    manifest_path: &Path,
    field_path: String,
    message: impl Into<String>,
    errors: &mut Vec<PersonaValidationError>,
) {
    errors.push(PersonaValidationError {
        manifest_path: manifest_path.to_path_buf(),
        field_path,
        message: message.into(),
    });
}

fn known_persona_capabilities(manifest: &Manifest, manifest_dir: &Path) -> BTreeSet<String> {
    let mut capabilities = BTreeSet::new();
    for (capability, operations) in default_persona_capability_map() {
        for operation in operations {
            capabilities.insert(format!("{capability}.{operation}"));
        }
    }
    for (capability, operations) in &manifest.check.host_capabilities {
        for operation in operations {
            capabilities.insert(format!("{capability}.{operation}"));
        }
    }
    if let Some(path) = manifest.check.host_capabilities_path.as_deref() {
        let path = PathBuf::from(path);
        let path = if path.is_absolute() {
            path
        } else {
            manifest_dir.join(path)
        };
        if let Ok(content) = fs::read_to_string(path) {
            let parsed_json = serde_json::from_str::<serde_json::Value>(&content).ok();
            let parsed_toml = toml::from_str::<toml::Value>(&content)
                .ok()
                .and_then(|value| serde_json::to_value(value).ok());
            if let Some(value) = parsed_json.or(parsed_toml) {
                collect_persona_capabilities_from_json(&value, &mut capabilities);
            }
        }
    }
    capabilities
}

fn collect_persona_capabilities_from_json(value: &serde_json::Value, out: &mut BTreeSet<String>) {
    let root = value.get("capabilities").unwrap_or(value);
    let Some(capabilities) = root.as_object() else {
        return;
    };
    for (capability, entry) in capabilities {
        if let Some(list) = entry.as_array() {
            for item in list {
                if let Some(operation) = item.as_str() {
                    out.insert(format!("{capability}.{operation}"));
                }
            }
        } else if let Some(obj) = entry.as_object() {
            if let Some(list) = obj
                .get("operations")
                .or_else(|| obj.get("ops"))
                .and_then(|v| v.as_array())
            {
                for item in list {
                    if let Some(operation) = item.as_str() {
                        out.insert(format!("{capability}.{operation}"));
                    }
                }
            } else {
                for (operation, enabled) in obj {
                    if enabled.as_bool().unwrap_or(true) {
                        out.insert(format!("{capability}.{operation}"));
                    }
                }
            }
        }
    }
}

fn default_persona_capability_map() -> BTreeMap<&'static str, Vec<&'static str>> {
    BTreeMap::from([
        (
            "workspace",
            vec![
                "read_text",
                "write_text",
                "apply_edit",
                "delete",
                "exists",
                "file_exists",
                "list",
                "project_root",
                "roots",
            ],
        ),
        ("process", vec!["exec"]),
        ("template", vec!["render"]),
        ("interaction", vec!["ask"]),
        (
            "runtime",
            vec![
                "approved_plan",
                "dry_run",
                "pipeline_input",
                "record_run",
                "set_result",
                "task",
            ],
        ),
        (
            "project",
            vec![
                "agent_instructions",
                "code_patterns",
                "compute_content_hash",
                "ide_context",
                "lessons",
                "mcp_config",
                "metadata_get",
                "metadata_refresh_hashes",
                "metadata_save",
                "metadata_set",
                "metadata_stale",
                "scan",
                "scope_test_command",
                "test_commands",
            ],
        ),
        (
            "session",
            vec![
                "active_roots",
                "changed_paths",
                "preread_get",
                "preread_read_many",
            ],
        ),
        (
            "editor",
            vec!["get_active_file", "get_selection", "get_visible_files"],
        ),
        ("diagnostics", vec!["get_causal_traces", "get_errors"]),
        ("git", vec!["get_branch", "get_diff"]),
        ("learning", vec!["get_learned_rules", "report_correction"]),
    ])
}

fn known_persona_tools(manifest: &Manifest) -> BTreeSet<String> {
    let mut tools = BTreeSet::from([
        "a2a".to_string(),
        "acp".to_string(),
        "ci".to_string(),
        "filesystem".to_string(),
        "github".to_string(),
        "linear".to_string(),
        "mcp".to_string(),
        "notion".to_string(),
        "pagerduty".to_string(),
        "shell".to_string(),
        "slack".to_string(),
    ]);
    for server in &manifest.mcp {
        tools.insert(server.name.clone());
    }
    for provider in &manifest.providers {
        tools.insert(provider.id.as_str().to_string());
    }
    for trigger in &manifest.triggers {
        if let Some(provider) = trigger.provider.as_ref() {
            tools.insert(provider.as_str().to_string());
        }
        for source in &trigger.sources {
            tools.insert(source.provider.as_str().to_string());
        }
    }
    tools
}

#[derive(Debug, Clone)]
struct ManifestContext {
    manifest: Manifest,
    dir: PathBuf,
}

impl ManifestContext {
    fn manifest_path(&self) -> PathBuf {
        self.dir.join(MANIFEST)
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join(LOCK_FILE)
    }

    fn packages_dir(&self) -> PathBuf {
        self.dir.join(PKG_DIR)
    }
}

fn load_current_manifest_context() -> Result<ManifestContext, String> {
    let dir = std::env::current_dir().map_err(|error| format!("failed to read cwd: {error}"))?;
    let manifest_path = dir.join(MANIFEST);
    let manifest = read_manifest_from_path(&manifest_path)?;
    Ok(ManifestContext { manifest, dir })
}

fn manifest_has_git_dependencies(manifest: &Manifest) -> bool {
    manifest
        .dependencies
        .values()
        .any(|dependency| dependency.git_url().is_some())
}

fn ensure_git_available() -> Result<(), String> {
    process::Command::new("git")
        .arg("--version")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map(|_| ())
        .map_err(|_| "git is required for git dependencies but was not found in PATH".to_string())
}

fn cache_root() -> Result<PathBuf, String> {
    if let Ok(value) = std::env::var(HARN_CACHE_DIR_ENV) {
        if !value.trim().is_empty() {
            return Ok(PathBuf::from(value));
        }
    }

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set and HARN_CACHE_DIR was not provided".to_string())?;
    if cfg!(target_os = "macos") {
        return Ok(home.join("Library/Caches/harn"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(xdg).join("harn"));
    }
    Ok(home.join(".cache/harn"))
}

fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    hex_bytes(Sha256::digest(bytes.as_ref()))
}

fn hex_bytes(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn git_cache_dir(source: &str, commit: &str) -> Result<PathBuf, String> {
    Ok(cache_root()?
        .join("git")
        .join(sha256_hex(source))
        .join(commit))
}

fn git_cache_lock_path(source: &str, commit: &str) -> Result<PathBuf, String> {
    Ok(cache_root()?
        .join("locks")
        .join(format!("{}-{commit}.lock", sha256_hex(source))))
}

fn acquire_git_cache_lock(source: &str, commit: &str) -> Result<File, String> {
    let path = git_cache_lock_path(source, commit)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let file = File::create(&path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    file.lock_exclusive()
        .map_err(|error| format!("failed to lock {}: {error}", path.display()))?;
    Ok(file)
}

fn read_cached_content_hash(dir: &Path) -> Result<Option<String>, String> {
    let path = dir.join(CONTENT_HASH_FILE);
    match fs::read_to_string(&path) {
        Ok(value) => Ok(Some(value.trim().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("failed to read {}: {error}", path.display())),
    }
}

fn write_cached_content_hash(dir: &Path, hash: &str) -> Result<(), String> {
    fs::write(dir.join(CONTENT_HASH_FILE), format!("{hash}\n")).map_err(|error| {
        format!(
            "failed to write {}: {error}",
            dir.join(CONTENT_HASH_FILE).display()
        )
    })
}

fn read_cache_metadata(dir: &Path) -> Result<Option<PackageCacheMetadata>, String> {
    let path = dir.join(CACHE_METADATA_FILE);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    let metadata = toml::from_str::<PackageCacheMetadata>(&content)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    if metadata.version != CACHE_METADATA_VERSION {
        return Err(format!(
            "unsupported {} version {} (expected {})",
            path.display(),
            metadata.version,
            CACHE_METADATA_VERSION
        ));
    }
    Ok(Some(metadata))
}

fn write_cache_metadata(
    dir: &Path,
    source: &str,
    commit: &str,
    content_hash: &str,
) -> Result<(), String> {
    let cached_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock error: {error}"))?
        .as_millis();
    let metadata = PackageCacheMetadata {
        version: CACHE_METADATA_VERSION,
        source: source.to_string(),
        commit: commit.to_string(),
        content_hash: content_hash.to_string(),
        cached_at_unix_ms,
    };
    let body = toml::to_string_pretty(&metadata)
        .map_err(|error| format!("failed to encode cache metadata: {error}"))?;
    fs::write(dir.join(CACHE_METADATA_FILE), body).map_err(|error| {
        format!(
            "failed to write {}: {error}",
            dir.join(CACHE_METADATA_FILE).display()
        )
    })
}

fn normalized_relative_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn collect_hashable_files(
    root: &Path,
    cursor: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), String> {
    for entry in fs::read_dir(cursor)
        .map_err(|error| format!("failed to read {}: {error}", cursor.display()))?
    {
        let entry =
            entry.map_err(|error| format!("failed to read {} entry: {error}", cursor.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to stat {}: {error}", path.display()))?;
        let name = entry.file_name();
        if name == OsStr::new(".git")
            || name == OsStr::new(".gitignore")
            || name == OsStr::new(CONTENT_HASH_FILE)
            || name == OsStr::new(CACHE_METADATA_FILE)
        {
            continue;
        }
        if file_type.is_dir() {
            collect_hashable_files(root, &path, out)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| format!("failed to relativize {}: {error}", path.display()))?;
            out.push(relative.to_path_buf());
        }
    }
    Ok(())
}

fn compute_content_hash(dir: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    collect_hashable_files(dir, dir, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for relative in files {
        let normalized = normalized_relative_path(&relative);
        let contents = fs::read(dir.join(&relative)).map_err(|error| {
            format!("failed to read {}: {error}", dir.join(&relative).display())
        })?;
        hasher.update(normalized.as_bytes());
        hasher.update([0]);
        hasher.update(sha256_hex(contents).as_bytes());
    }
    Ok(format!("sha256:{}", hex_bytes(hasher.finalize())))
}

fn verify_content_hash_or_compute(dir: &Path, expected: &str) -> Result<(), String> {
    let actual = compute_content_hash(dir)?;
    if actual != expected {
        return Err(format!(
            "content hash mismatch for {}: expected {}, got {}",
            dir.display(),
            expected,
            actual
        ));
    }
    if read_cached_content_hash(dir)?.as_deref() != Some(expected) {
        write_cached_content_hash(dir, expected)?;
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst)
        .map_err(|error| format!("failed to create {}: {error}", dst.display()))?;
    for entry in
        fs::read_dir(src).map_err(|error| format!("failed to read {}: {error}", src.display()))?
    {
        let entry =
            entry.map_err(|error| format!("failed to read {} entry: {error}", src.display()))?;
        let ty = entry
            .file_type()
            .map_err(|error| format!("failed to stat {}: {error}", entry.path().display()))?;
        let name = entry.file_name();
        if name == OsStr::new(".git")
            || name == OsStr::new(CONTENT_HASH_FILE)
            || name == OsStr::new(CACHE_METADATA_FILE)
        {
            continue;
        }
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else if ty.is_file() {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
            }
            fs::copy(entry.path(), &dest_path).map_err(|error| {
                format!(
                    "failed to copy {} to {}: {error}",
                    entry.path().display(),
                    dest_path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn remove_materialized_package(packages_dir: &Path, alias: &str) -> Result<(), String> {
    let dir = packages_dir.join(alias);
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(&dir)
                .map_err(|error| format!("failed to remove {}: {error}", dir.display()))?;
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(&dir)
                .map_err(|error| format!("failed to remove {}: {error}", dir.display()))?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("failed to stat {}: {error}", dir.display())),
    }
    let file = packages_dir.join(format!("{alias}.harn"));
    match fs::symlink_metadata(&file) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(&file)
                .map_err(|error| format!("failed to remove {}: {error}", file.display()))?;
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(&file)
                .map_err(|error| format!("failed to remove {}: {error}", file.display()))?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("failed to stat {}: {error}", file.display())),
    }
    Ok(())
}

#[cfg(unix)]
fn symlink_path_dependency(source: &Path, dest: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(source, dest).map_err(|error| {
        format!(
            "failed to symlink {} to {}: {error}",
            source.display(),
            dest.display()
        )
    })
}

#[cfg(windows)]
fn symlink_path_dependency(source: &Path, dest: &Path) -> Result<(), String> {
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(source, dest)
    } else {
        std::os::windows::fs::symlink_file(source, dest)
    }
    .map_err(|error| {
        format!(
            "failed to symlink {} to {}: {error}",
            source.display(),
            dest.display()
        )
    })
}

#[cfg(not(any(unix, windows)))]
fn symlink_path_dependency(_source: &Path, _dest: &Path) -> Result<(), String> {
    Err("symlinks are not supported on this platform".to_string())
}

fn materialize_path_dependency(source: &Path, dest_root: &Path, alias: &str) -> Result<(), String> {
    remove_materialized_package(dest_root, alias)?;
    if source.is_dir() {
        let dest = dest_root.join(alias);
        match symlink_path_dependency(source, &dest) {
            Ok(()) => Ok(()),
            Err(_) => copy_dir_recursive(source, &dest),
        }
    } else {
        let dest = dest_root.join(format!("{alias}.harn"));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        match symlink_path_dependency(source, &dest) {
            Ok(()) => Ok(()),
            Err(_) => {
                fs::copy(source, &dest).map_err(|error| {
                    format!(
                        "failed to copy {} to {}: {error}",
                        source.display(),
                        dest.display()
                    )
                })?;
                Ok(())
            }
        }
    }
}

fn materialized_hash_matches(dir: &Path, expected: &str) -> bool {
    verify_content_hash_or_compute(dir, expected).is_ok()
}

fn resolve_path_dependency_source(manifest_dir: &Path, raw: &str) -> Result<PathBuf, String> {
    let source = {
        let candidate = PathBuf::from(raw);
        if candidate.is_absolute() {
            candidate
        } else {
            manifest_dir.join(candidate)
        }
    };
    if source.exists() {
        return source
            .canonicalize()
            .map_err(|error| format!("failed to canonicalize {}: {error}", source.display()));
    }
    if source.extension().is_none() {
        let with_ext = source.with_extension("harn");
        if with_ext.exists() {
            return with_ext.canonicalize().map_err(|error| {
                format!("failed to canonicalize {}: {error}", with_ext.display())
            });
        }
    }
    Err(format!("package source not found: {}", source.display()))
}

fn path_source_uri(path: &Path) -> Result<String, String> {
    let url = Url::from_file_path(path)
        .map_err(|_| format!("failed to convert {} to file:// URL", path.display()))?;
    Ok(format!("path+{}", url))
}

fn path_from_source_uri(source: &str) -> Result<PathBuf, String> {
    let raw = source
        .strip_prefix("path+")
        .ok_or_else(|| format!("invalid path source: {source}"))?;
    if let Ok(url) = Url::parse(raw) {
        return url
            .to_file_path()
            .map_err(|_| format!("invalid file:// path source: {source}"));
    }
    Ok(PathBuf::from(raw))
}

fn registry_file_url_or_path(raw: &str) -> Result<Option<PathBuf>, String> {
    if let Ok(url) = Url::parse(raw) {
        if url.scheme() == "file" {
            return url
                .to_file_path()
                .map(Some)
                .map_err(|_| format!("invalid file:// registry URL: {raw}"));
        }
        return Ok(None);
    }
    Ok(Some(PathBuf::from(raw)))
}

fn read_registry_source(source: &str) -> Result<String, String> {
    if let Some(path) = registry_file_url_or_path(source)? {
        return fs::read_to_string(&path).map_err(|error| {
            format!(
                "failed to read package registry {}: {error}",
                path.display()
            )
        });
    }

    let url = Url::parse(source)
        .map_err(|error| format!("invalid package registry URL {source:?}: {error}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("unsupported package registry URL scheme: {other}")),
    }
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| format!("failed to build package registry client: {error}"))?
        .get(url)
        .send()
        .map_err(|error| format!("failed to fetch package registry {source}: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GET {source} returned HTTP {status}"));
    }
    response
        .text()
        .map_err(|error| format!("failed to read package registry response: {error}"))
}

fn resolve_configured_registry_source(explicit: Option<&str>) -> Result<String, String> {
    if let Some(explicit) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(explicit.to_string());
    }
    if let Ok(value) = std::env::var(HARN_PACKAGE_REGISTRY_ENV) {
        let value = value.trim();
        if !value.is_empty() {
            return Ok(value.to_string());
        }
    }

    let cwd = std::env::current_dir().map_err(|error| format!("failed to read cwd: {error}"))?;
    if let Some((manifest, manifest_dir)) = find_nearest_manifest(&cwd) {
        if let Some(raw) = manifest
            .registry
            .url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if Url::parse(raw).is_ok() || PathBuf::from(raw).is_absolute() {
                return Ok(raw.to_string());
            }
            return Ok(manifest_dir.join(raw).display().to_string());
        }
    }

    Ok(DEFAULT_PACKAGE_REGISTRY_URL.to_string())
}

fn is_valid_registry_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

fn is_valid_registry_package_name(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed != name || trimmed.is_empty() || trimmed.contains("://") || trimmed.ends_with('/') {
        return false;
    }
    if let Some(scoped) = trimmed.strip_prefix('@') {
        let Some((scope, package)) = scoped.split_once('/') else {
            return false;
        };
        return !package.contains('/')
            && is_valid_registry_segment(scope)
            && is_valid_registry_segment(package);
    }
    !trimmed.contains('/') && is_valid_registry_segment(trimmed)
}

fn parse_registry_package_spec(spec: &str) -> Option<(&str, Option<&str>)> {
    let trimmed = spec.trim();
    if !trimmed.starts_with('@') {
        if let Some((name, version)) = trimmed.rsplit_once('@') {
            if is_valid_registry_package_name(name) && !version.trim().is_empty() {
                return Some((name, Some(version)));
            }
        }
        if is_valid_registry_package_name(trimmed) {
            return Some((trimmed, None));
        }
        return None;
    }

    if let Some((name, version)) = trimmed.rsplit_once('@') {
        if !name.is_empty()
            && name != trimmed
            && is_valid_registry_package_name(name)
            && !version.trim().is_empty()
        {
            return Some((name, Some(version)));
        }
    }
    if is_valid_registry_package_name(trimmed) {
        return Some((trimmed, None));
    }
    None
}

fn parse_package_registry_index(
    source: &str,
    content: &str,
) -> Result<PackageRegistryIndex, String> {
    let mut index = toml::from_str::<PackageRegistryIndex>(content)
        .map_err(|error| format!("failed to parse package registry {source}: {error}"))?;
    if index.version != REGISTRY_INDEX_VERSION {
        return Err(format!(
            "unsupported package registry {source} version {} (expected {})",
            index.version, REGISTRY_INDEX_VERSION
        ));
    }
    validate_package_registry_index(source, &mut index)?;
    Ok(index)
}

fn validate_package_registry_index(
    source: &str,
    index: &mut PackageRegistryIndex,
) -> Result<(), String> {
    let mut names = HashSet::new();
    for package in &mut index.packages {
        if !is_valid_registry_package_name(&package.name) {
            return Err(format!(
                "package registry {source} has invalid package name '{}'",
                package.name
            ));
        }
        if !names.insert(package.name.clone()) {
            return Err(format!(
                "package registry {source} declares '{}' more than once",
                package.name
            ));
        }
        normalize_git_url(&package.repository).map_err(|error| {
            format!(
                "package registry {source} has invalid repository for '{}': {error}",
                package.name
            )
        })?;
        let mut versions = HashSet::new();
        for version in &package.versions {
            if version.version.trim().is_empty() {
                return Err(format!(
                    "package registry {source} has empty version for '{}'",
                    package.name
                ));
            }
            if !versions.insert(version.version.clone()) {
                return Err(format!(
                    "package registry {source} declares '{}@{}' more than once",
                    package.name, version.version
                ));
            }
            if version.rev.is_none() && version.branch.is_none() {
                return Err(format!(
                    "package registry {source} entry '{}@{}' must specify rev or branch",
                    package.name, version.version
                ));
            }
            normalize_git_url(&version.git).map_err(|error| {
                format!(
                    "package registry {source} has invalid git source for '{}@{}': {error}",
                    package.name, version.version
                )
            })?;
        }
    }
    index
        .packages
        .sort_by(|left, right| left.name.cmp(&right.name));
    Ok(())
}

fn load_package_registry(explicit: Option<&str>) -> Result<(String, PackageRegistryIndex), String> {
    let source = resolve_configured_registry_source(explicit)?;
    let content = read_registry_source(&source)?;
    let index = parse_package_registry_index(&source, &content)?;
    Ok((source, index))
}

fn registry_package_matches(package: &RegistryPackage, query: &str) -> bool {
    if query.trim().is_empty() {
        return true;
    }
    let query = query.to_ascii_lowercase();
    package.name.to_ascii_lowercase().contains(&query)
        || package
            .description
            .as_deref()
            .is_some_and(|value| value.to_ascii_lowercase().contains(&query))
        || package.repository.to_ascii_lowercase().contains(&query)
        || package
            .exports
            .iter()
            .any(|export| export.to_ascii_lowercase().contains(&query))
}

fn latest_registry_version(package: &RegistryPackage) -> Option<&RegistryPackageVersion> {
    package
        .versions
        .iter()
        .rev()
        .find(|version| !version.yanked)
}

fn find_registry_package_version(
    index: &PackageRegistryIndex,
    name: &str,
    version: Option<&str>,
) -> Result<RegistryPackageInfo, String> {
    let package = index
        .packages
        .iter()
        .find(|package| package.name == name)
        .ok_or_else(|| format!("package registry does not contain {name}"))?;
    let selected_version = match version {
        Some(version) => Some(
            package
                .versions
                .iter()
                .find(|entry| entry.version == version)
                .ok_or_else(|| format!("package registry does not contain {name}@{version}"))?
                .clone(),
        ),
        None => latest_registry_version(package).cloned(),
    };
    Ok(RegistryPackageInfo {
        package: package.clone(),
        selected_version,
    })
}

fn search_package_registry_impl(
    query: Option<&str>,
    registry: Option<&str>,
) -> Result<Vec<RegistryPackage>, String> {
    let (_, index) = load_package_registry(registry)?;
    Ok(index
        .packages
        .into_iter()
        .filter(|package| registry_package_matches(package, query.unwrap_or("")))
        .collect())
}

fn package_registry_info_impl(
    spec: &str,
    registry: Option<&str>,
) -> Result<RegistryPackageInfo, String> {
    let Some((name, version)) = parse_registry_package_spec(spec) else {
        return Err(format!(
            "invalid registry package name '{spec}'; use names like @burin/notion-sdk or acme-lib"
        ));
    };
    let (_, index) = load_package_registry(registry)?;
    find_registry_package_version(&index, name, version)
}

fn registry_dependency_from_spec(
    spec: &str,
    alias: Option<&str>,
    registry: Option<&str>,
) -> Result<(String, Dependency), String> {
    let Some((name, Some(version))) = parse_registry_package_spec(spec) else {
        return Err(format!(
            "registry dependency '{spec}' must include a version, for example {spec}@1.2.3"
        ));
    };
    let info = package_registry_info_impl(&format!("{name}@{version}"), registry)?;
    let selected = info
        .selected_version
        .ok_or_else(|| format!("package registry does not contain {name}@{version}"))?;
    if selected.yanked {
        return Err(format!(
            "{name}@{version} is yanked in the package registry"
        ));
    }
    let git = normalize_git_url(&selected.git)?;
    let package_name = selected
        .package
        .clone()
        .map(Ok)
        .unwrap_or_else(|| derive_repo_name_from_source(&git))?;
    let alias = alias.unwrap_or(package_name.as_str()).to_string();
    Ok((
        alias.clone(),
        Dependency::Table(DepTable {
            git: Some(git),
            tag: None,
            rev: selected.rev,
            branch: selected.branch,
            path: None,
            package: (alias != package_name).then_some(package_name),
        }),
    ))
}

fn is_probable_shorthand_git_url(raw: &str) -> bool {
    !raw.contains("://")
        && !raw.starts_with("git@")
        && raw.contains('/')
        && raw
            .split('/')
            .next()
            .is_some_and(|segment| segment.contains('.'))
}

fn normalize_git_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("git URL cannot be empty".to_string());
    }

    let candidate_path = PathBuf::from(trimmed);
    if candidate_path.exists() {
        let canonical = candidate_path
            .canonicalize()
            .map_err(|error| format!("failed to canonicalize {}: {error}", trimmed))?;
        let url = Url::from_file_path(canonical)
            .map_err(|_| format!("failed to convert {} to file:// URL", trimmed))?;
        return Ok(url.to_string().trim_end_matches('/').to_string());
    }

    if let Some(rest) = trimmed.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            return Ok(format!(
                "ssh://git@{}/{}",
                host,
                path.trim_start_matches('/').trim_end_matches('/')
            ));
        }
    }

    let with_scheme = if is_probable_shorthand_git_url(trimmed) {
        format!("https://{trimmed}")
    } else {
        trimmed.to_string()
    };
    let parsed =
        Url::parse(&with_scheme).map_err(|error| format!("invalid git URL {trimmed}: {error}"))?;
    let mut normalized = parsed.to_string();
    while normalized.ends_with('/') {
        normalized.pop();
    }
    if parsed.scheme() != "file" && normalized.ends_with(".git") {
        normalized.truncate(normalized.len() - 4);
    }
    Ok(normalized)
}

fn derive_repo_name_from_source(source: &str) -> Result<String, String> {
    let url = Url::parse(source).map_err(|error| format!("invalid git URL {source}: {error}"))?;
    let segment = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .ok_or_else(|| format!("failed to derive package name from {source}"))?;
    Ok(segment.trim_end_matches(".git").to_string())
}

fn parse_positional_git_spec(spec: &str) -> (&str, Option<&str>) {
    if let Some((source, candidate_ref)) = spec.rsplit_once('@') {
        if !candidate_ref.is_empty()
            && !candidate_ref.contains('/')
            && !candidate_ref.contains(':')
            && !source.ends_with("://")
        {
            return (source, Some(candidate_ref));
        }
    }
    (spec, None)
}

fn existing_local_path_spec(spec: &str) -> Option<PathBuf> {
    if spec.trim().is_empty() || spec.contains("://") || spec.starts_with("git@") {
        return None;
    }
    let candidate = PathBuf::from(spec);
    if candidate.exists() {
        return Some(candidate);
    }
    if candidate.extension().is_none() {
        let with_ext = candidate.with_extension("harn");
        if with_ext.exists() {
            return Some(with_ext);
        }
    }
    if is_probable_shorthand_git_url(spec) {
        return None;
    }
    None
}

fn package_manifest_name(path: &Path) -> Option<String> {
    let manifest_path = if path.is_dir() {
        path.join(MANIFEST)
    } else {
        path.parent()?.join(MANIFEST)
    };
    let manifest = read_manifest_from_path(&manifest_path).ok()?;
    manifest
        .package
        .and_then(|pkg| pkg.name)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

fn derive_package_alias_from_path(path: &Path) -> Result<String, String> {
    if let Some(name) = package_manifest_name(path) {
        return Ok(name);
    }
    let fallback = if path.is_dir() {
        path.file_name()
    } else {
        path.file_stem()
    };
    fallback
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("failed to derive package alias from {}", path.display()))
}

fn is_full_git_sha(value: &str) -> bool {
    value.len() == 40 && value.as_bytes().iter().all(|byte| byte.is_ascii_hexdigit())
}

fn git_output<I, S>(args: I, cwd: Option<&Path>) -> Result<std::process::Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = process::Command::new("git");
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|error| format!("failed to run git: {error}"))
}

fn resolve_git_commit(
    url: &str,
    rev: Option<&str>,
    branch: Option<&str>,
) -> Result<String, String> {
    let requested = branch.or(rev).unwrap_or("HEAD");
    if branch.is_none() && is_full_git_sha(requested) {
        return Ok(requested.to_string());
    }

    let refs = if let Some(branch) = branch {
        vec![format!("refs/heads/{branch}")]
    } else if requested == "HEAD" {
        vec!["HEAD".to_string()]
    } else {
        vec![
            requested.to_string(),
            format!("refs/tags/{requested}^{{}}"),
            format!("refs/tags/{requested}"),
            format!("refs/heads/{requested}"),
        ]
    };

    let output = git_output(
        std::iter::once("ls-remote".to_string())
            .chain(std::iter::once(url.to_string()))
            .chain(refs.clone()),
        None,
    )?;
    if !output.status.success() {
        return Err(format!(
            "failed to resolve git ref from {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let commit = stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .find(|value| is_full_git_sha(value))
        .ok_or_else(|| format!("could not resolve {requested} from {url}"))?;
    Ok(commit.to_string())
}

fn clone_git_commit_to(url: &str, commit: &str, dest: &Path) -> Result<(), String> {
    if dest.exists() {
        fs::remove_dir_all(dest)
            .map_err(|error| format!("failed to reset {}: {error}", dest.display()))?;
    }
    fs::create_dir_all(dest)
        .map_err(|error| format!("failed to create {}: {error}", dest.display()))?;

    let init = git_output(["init", "--quiet"], Some(dest))?;
    if !init.status.success() {
        return Err(format!(
            "failed to initialize git repo in {}: {}",
            dest.display(),
            String::from_utf8_lossy(&init.stderr).trim()
        ));
    }

    let remote = git_output(["remote", "add", "origin", url], Some(dest))?;
    if !remote.status.success() {
        return Err(format!(
            "failed to add git remote {url}: {}",
            String::from_utf8_lossy(&remote.stderr).trim()
        ));
    }

    let fetch = git_output(["fetch", "--depth", "1", "origin", commit], Some(dest))?;
    if !fetch.status.success() {
        let fallback_dir = dest.with_extension("full-clone");
        if fallback_dir.exists() {
            fs::remove_dir_all(&fallback_dir)
                .map_err(|error| format!("failed to remove {}: {error}", fallback_dir.display()))?;
        }
        let clone = git_output(
            ["clone", url, fallback_dir.to_string_lossy().as_ref()],
            None,
        )?;
        if !clone.status.success() {
            return Err(format!(
                "failed to fetch {commit} from {url}: {}",
                String::from_utf8_lossy(&fetch.stderr).trim()
            ));
        }
        let checkout = git_output(["checkout", commit], Some(&fallback_dir))?;
        if !checkout.status.success() {
            return Err(format!(
                "failed to checkout {commit} in {}: {}",
                fallback_dir.display(),
                String::from_utf8_lossy(&checkout.stderr).trim()
            ));
        }
        fs::remove_dir_all(dest)
            .map_err(|error| format!("failed to remove {}: {error}", dest.display()))?;
        fs::rename(&fallback_dir, dest).map_err(|error| {
            format!(
                "failed to move {} to {}: {error}",
                fallback_dir.display(),
                dest.display()
            )
        })?;
    } else {
        let checkout = git_output(["checkout", "--detach", "FETCH_HEAD"], Some(dest))?;
        if !checkout.status.success() {
            return Err(format!(
                "failed to checkout FETCH_HEAD in {}: {}",
                dest.display(),
                String::from_utf8_lossy(&checkout.stderr).trim()
            ));
        }
    }

    let git_dir = dest.join(".git");
    if git_dir.exists() {
        fs::remove_dir_all(&git_dir)
            .map_err(|error| format!("failed to remove {}: {error}", git_dir.display()))?;
    }
    Ok(())
}

fn unique_temp_dir(base: &Path, label: &str) -> Result<PathBuf, String> {
    for _ in 0..16 {
        let suffix = uuid::Uuid::now_v7();
        let candidate = base.join(format!("{label}-{suffix}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "failed to allocate a unique temporary directory under {}",
        base.display()
    ))
}

fn ensure_git_cache_populated(
    url: &str,
    source: &str,
    commit: &str,
    expected_hash: Option<&str>,
    refetch: bool,
    offline: bool,
) -> Result<String, String> {
    let cache_dir = git_cache_dir(source, commit)?;
    let _lock = acquire_git_cache_lock(source, commit)?;
    if refetch && cache_dir.exists() {
        fs::remove_dir_all(&cache_dir)
            .map_err(|error| format!("failed to remove {}: {error}", cache_dir.display()))?;
    }
    if cache_dir.exists() {
        if let Some(expected) = expected_hash {
            verify_content_hash_or_compute(&cache_dir, expected)?;
            write_cache_metadata(&cache_dir, source, commit, expected)?;
            return Ok(expected.to_string());
        }
        let hash = compute_content_hash(&cache_dir)?;
        write_cached_content_hash(&cache_dir, &hash)?;
        write_cache_metadata(&cache_dir, source, commit, &hash)?;
        return Ok(hash);
    }

    if offline {
        return Err(format!(
            "package cache entry for {source} at {commit} is missing; cannot fetch in offline mode"
        ));
    }

    let parent = cache_dir
        .parent()
        .ok_or_else(|| format!("invalid cache path {}", cache_dir.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let temp_dir = unique_temp_dir(parent, "tmp")?;
    let populated = (|| -> Result<String, String> {
        clone_git_commit_to(url, commit, &temp_dir)?;
        let hash = compute_content_hash(&temp_dir)?;
        if let Some(expected) = expected_hash {
            if hash != expected {
                return Err(format!(
                    "content hash mismatch for {} at {}: expected {}, got {}",
                    source, commit, expected, hash
                ));
            }
        }
        write_cached_content_hash(&temp_dir, &hash)?;
        write_cache_metadata(&temp_dir, source, commit, &hash)?;
        fs::rename(&temp_dir, &cache_dir).map_err(|error| {
            format!(
                "failed to move {} to {}: {error}",
                temp_dir.display(),
                cache_dir.display()
            )
        })?;
        Ok(hash)
    })();
    let hash = match populated {
        Ok(hash) => hash,
        Err(error) => {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(error);
        }
    };
    Ok(hash)
}

fn compatible_locked_entry(
    alias: &str,
    dependency: &Dependency,
    lock: &LockEntry,
    manifest_dir: &Path,
) -> Result<bool, String> {
    if lock.name != alias {
        return Ok(false);
    }
    if let Some(path) = dependency.local_path() {
        let source = path_source_uri(&resolve_path_dependency_source(manifest_dir, path)?)?;
        return Ok(lock.source == source);
    }
    if let Some(url) = dependency.git_url() {
        let source = format!("git+{}", normalize_git_url(url)?);
        let requested = dependency
            .branch()
            .map(str::to_string)
            .or_else(|| dependency.rev().map(str::to_string));
        return Ok(lock.source == source
            && lock.rev_request == requested
            && lock.commit.is_some()
            && lock.content_hash.is_some());
    }
    Ok(false)
}

#[derive(Debug, Clone)]
struct PendingDependency {
    alias: String,
    dependency: Dependency,
    manifest_dir: PathBuf,
    parent: Option<String>,
    parent_is_git: bool,
}

fn git_rev_request(alias: &str, dependency: &Dependency) -> Result<String, String> {
    dependency
        .branch()
        .or_else(|| dependency.rev())
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "git dependency {alias} must specify `rev` or `branch`; use `harn add <url>@<tag-or-sha>` or add `rev = \"...\"` to {MANIFEST}"
            )
        })
}

fn dependency_manifest_dir(source: &Path) -> Option<PathBuf> {
    if source.is_dir() {
        return Some(source.to_path_buf());
    }
    source.parent().map(Path::to_path_buf)
}

fn read_package_manifest_from_dir(dir: &Path) -> Result<Option<Manifest>, String> {
    let manifest_path = dir.join(MANIFEST);
    if !manifest_path.exists() {
        return Ok(None);
    }
    read_manifest_from_path(&manifest_path).map(Some)
}

fn dependency_conflict_message(existing: &LockEntry, candidate: &LockEntry) -> String {
    format!(
        "dependency alias '{}' resolves to multiple packages ({} and {}); use distinct aliases in {MANIFEST}",
        candidate.name, existing.source, candidate.source
    )
}

fn replace_lock_entry(lock: &mut LockFile, candidate: LockEntry) -> Result<bool, String> {
    validate_package_alias(&candidate.name)?;
    if let Some(existing) = lock.find(&candidate.name) {
        if existing == &candidate {
            return Ok(false);
        }
        return Err(dependency_conflict_message(existing, &candidate));
    }
    lock.replace(candidate);
    Ok(true)
}

fn enqueue_manifest_dependencies(
    pending: &mut Vec<PendingDependency>,
    manifest: Manifest,
    manifest_dir: PathBuf,
    parent: String,
    parent_is_git: bool,
) {
    let mut aliases: Vec<String> = manifest.dependencies.keys().cloned().collect();
    aliases.sort();
    for alias in aliases.into_iter().rev() {
        if let Some(dependency) = manifest.dependencies.get(&alias).cloned() {
            pending.push(PendingDependency {
                alias,
                dependency,
                manifest_dir: manifest_dir.clone(),
                parent: Some(parent.clone()),
                parent_is_git,
            });
        }
    }
}

fn build_lockfile(
    ctx: &ManifestContext,
    existing: Option<&LockFile>,
    refresh_alias: Option<&str>,
    refresh_all: bool,
    allow_resolve: bool,
    offline: bool,
) -> Result<LockFile, String> {
    if manifest_has_git_dependencies(&ctx.manifest) {
        ensure_git_available()?;
    }

    let mut lock = LockFile::default();
    let mut pending: Vec<PendingDependency> = Vec::new();
    let mut aliases: Vec<String> = ctx.manifest.dependencies.keys().cloned().collect();
    aliases.sort();
    for alias in aliases.into_iter().rev() {
        let dependency = ctx
            .manifest
            .dependencies
            .get(&alias)
            .ok_or_else(|| format!("dependency {alias} disappeared while locking"))?
            .clone();
        pending.push(PendingDependency {
            alias,
            dependency,
            manifest_dir: ctx.dir.clone(),
            parent: None,
            parent_is_git: false,
        });
    }

    while let Some(next) = pending.pop() {
        let alias = next.alias;
        validate_package_alias(&alias)?;
        let dependency = next.dependency;
        if dependency.local_path().is_some() && next.parent_is_git {
            let parent = next.parent.as_deref().unwrap_or("a git package");
            return Err(format!(
                "package {parent} declares local path dependency {alias}, but path dependencies are not supported inside git-installed packages; publish {alias} as a git dependency with `rev` or `branch`"
            ));
        }
        if dependency.git_url().is_some() {
            ensure_git_available()?;
            git_rev_request(&alias, &dependency)?;
        }
        let refresh = refresh_all || refresh_alias == Some(alias.as_str());
        if let Some(existing_lock) = existing.and_then(|lock| lock.find(&alias)) {
            if !refresh
                && compatible_locked_entry(&alias, &dependency, existing_lock, &next.manifest_dir)?
            {
                let mut entry = existing_lock.clone();
                if entry.source.starts_with("git+") && entry.content_hash.is_none() {
                    let url = entry.source.trim_start_matches("git+");
                    let commit = entry
                        .commit
                        .as_deref()
                        .ok_or_else(|| format!("missing locked commit for {alias}"))?;
                    entry.content_hash = Some(ensure_git_cache_populated(
                        url,
                        &entry.source,
                        commit,
                        None,
                        false,
                        offline,
                    )?);
                }
                let inserted = replace_lock_entry(&mut lock, entry.clone())?;
                if entry.source.starts_with("git+") {
                    let url = entry.source.trim_start_matches("git+");
                    let commit = entry
                        .commit
                        .as_deref()
                        .ok_or_else(|| format!("missing locked commit for {alias}"))?;
                    let expected_hash = entry
                        .content_hash
                        .as_deref()
                        .ok_or_else(|| format!("missing content hash for {alias}"))?;
                    ensure_git_cache_populated(
                        url,
                        &entry.source,
                        commit,
                        Some(expected_hash),
                        false,
                        offline,
                    )?;
                    if inserted {
                        let cache_dir = git_cache_dir(&entry.source, commit)?;
                        if let Some(manifest) = read_package_manifest_from_dir(&cache_dir)? {
                            enqueue_manifest_dependencies(
                                &mut pending,
                                manifest,
                                cache_dir,
                                alias,
                                true,
                            );
                        }
                    }
                } else if inserted && entry.source.starts_with("path+") {
                    let source = path_from_source_uri(&entry.source)?;
                    if let Some(manifest_dir) = dependency_manifest_dir(&source) {
                        if let Some(manifest) = read_package_manifest_from_dir(&manifest_dir)? {
                            enqueue_manifest_dependencies(
                                &mut pending,
                                manifest,
                                manifest_dir,
                                alias,
                                false,
                            );
                        }
                    }
                }
                continue;
            }
        }

        if !allow_resolve {
            return Err(format!(
                "{} would need to change",
                ctx.lock_path().display()
            ));
        }

        if let Some(path) = dependency.local_path() {
            let source = resolve_path_dependency_source(&next.manifest_dir, path)?;
            let package_alias = alias.clone();
            let entry = LockEntry {
                name: alias.clone(),
                source: path_source_uri(&source)?,
                rev_request: None,
                commit: None,
                content_hash: None,
            };
            let inserted = replace_lock_entry(&mut lock, entry)?;
            if inserted {
                if let Some(manifest_dir) = dependency_manifest_dir(&source) {
                    if let Some(manifest) = read_package_manifest_from_dir(&manifest_dir)? {
                        enqueue_manifest_dependencies(
                            &mut pending,
                            manifest,
                            manifest_dir,
                            package_alias,
                            false,
                        );
                    }
                }
            }
            continue;
        }

        if let Some(url) = dependency.git_url() {
            let rev_request = git_rev_request(&alias, &dependency)?;
            let normalized_url = normalize_git_url(url)?;
            let source = format!("git+{normalized_url}");
            let commit =
                resolve_git_commit(&normalized_url, dependency.rev(), dependency.branch())?;
            let content_hash = ensure_git_cache_populated(
                &normalized_url,
                &source,
                &commit,
                None,
                false,
                offline,
            )?;
            let entry = LockEntry {
                name: alias.clone(),
                source: source.clone(),
                rev_request: Some(rev_request),
                commit: Some(commit.clone()),
                content_hash: Some(content_hash),
            };
            let inserted = replace_lock_entry(&mut lock, entry)?;
            if inserted {
                let cache_dir = git_cache_dir(&source, &commit)?;
                if let Some(manifest) = read_package_manifest_from_dir(&cache_dir)? {
                    enqueue_manifest_dependencies(&mut pending, manifest, cache_dir, alias, true);
                }
            }
            continue;
        }

        return Err(format!(
            "dependency {alias} is missing a git or path source"
        ));
    }
    Ok(lock)
}

fn materialize_dependencies_from_lock(
    ctx: &ManifestContext,
    lock: &LockFile,
    refetch: Option<&str>,
    offline: bool,
) -> Result<usize, String> {
    let packages_dir = ctx.packages_dir();
    fs::create_dir_all(&packages_dir)
        .map_err(|error| format!("failed to create {}: {error}", packages_dir.display()))?;

    let mut installed = 0usize;
    for entry in &lock.packages {
        let alias = &entry.name;
        validate_package_alias(alias)?;
        if entry.source.starts_with("path+") {
            let source = path_from_source_uri(&entry.source)?;
            materialize_path_dependency(&source, &packages_dir, alias)?;
            installed += 1;
            continue;
        }

        let commit = entry
            .commit
            .as_deref()
            .ok_or_else(|| format!("missing locked commit for {alias}"))?;
        let expected_hash = entry
            .content_hash
            .as_deref()
            .ok_or_else(|| format!("missing content hash for {alias}"))?;
        let source = entry.source.clone();
        let url = source.trim_start_matches("git+");
        let refetch_this = refetch == Some("all") || refetch == Some(alias.as_str());
        ensure_git_cache_populated(
            url,
            &source,
            commit,
            Some(expected_hash),
            refetch_this,
            offline,
        )?;
        let cache_dir = git_cache_dir(&source, commit)?;
        let dest_dir = packages_dir.join(alias);
        if !dest_dir.exists() || !materialized_hash_matches(&dest_dir, expected_hash) {
            remove_materialized_package(&packages_dir, alias)?;
            copy_dir_recursive(&cache_dir, &dest_dir)?;
            write_cached_content_hash(&dest_dir, expected_hash)?;
        }
        installed += 1;
    }
    Ok(installed)
}

fn validate_lock_matches_manifest(ctx: &ManifestContext, lock: &LockFile) -> Result<(), String> {
    for (alias, dependency) in &ctx.manifest.dependencies {
        validate_package_alias(alias)?;
        let entry = lock.find(alias).ok_or_else(|| {
            format!(
                "{} is missing an entry for {alias}",
                ctx.lock_path().display()
            )
        })?;
        if !compatible_locked_entry(alias, dependency, entry, &ctx.dir)? {
            return Err(format!(
                "{} is out of date for {alias}; run `harn install`",
                ctx.lock_path().display()
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct PackageCacheEntry {
    path: PathBuf,
    source_hash: String,
    commit: String,
    metadata: Option<PackageCacheMetadata>,
}

fn git_cache_root() -> Result<PathBuf, String> {
    Ok(cache_root()?.join("git"))
}

fn discover_git_cache_entries() -> Result<Vec<PackageCacheEntry>, String> {
    let root = git_cache_root()?;
    let mut entries = Vec::new();
    let source_dirs = match fs::read_dir(&root) {
        Ok(source_dirs) => source_dirs,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => return Err(format!("failed to read {}: {error}", root.display())),
    };
    for source_dir in source_dirs {
        let source_dir = source_dir
            .map_err(|error| format!("failed to read {} entry: {error}", root.display()))?;
        let source_type = source_dir
            .file_type()
            .map_err(|error| format!("failed to stat {}: {error}", source_dir.path().display()))?;
        if !source_type.is_dir() {
            continue;
        }
        let source_hash = source_dir.file_name().to_string_lossy().to_string();
        let commit_dirs = fs::read_dir(source_dir.path())
            .map_err(|error| format!("failed to read {}: {error}", source_dir.path().display()))?;
        for commit_dir in commit_dirs {
            let commit_dir = commit_dir.map_err(|error| {
                format!(
                    "failed to read {} entry: {error}",
                    source_dir.path().display()
                )
            })?;
            let commit_type = commit_dir.file_type().map_err(|error| {
                format!("failed to stat {}: {error}", commit_dir.path().display())
            })?;
            if !commit_type.is_dir() {
                continue;
            }
            let commit = commit_dir.file_name().to_string_lossy().to_string();
            if commit.starts_with("tmp-") || commit.ends_with(".full-clone") {
                continue;
            }
            let metadata = read_cache_metadata(&commit_dir.path())?;
            entries.push(PackageCacheEntry {
                path: commit_dir.path(),
                source_hash: source_hash.clone(),
                commit,
                metadata,
            });
        }
    }
    entries.sort_by(|left, right| {
        left.source_hash
            .cmp(&right.source_hash)
            .then_with(|| left.commit.cmp(&right.commit))
    });
    Ok(entries)
}

fn locked_git_cache_paths(lock: &LockFile) -> Result<HashSet<PathBuf>, String> {
    let mut keep = HashSet::new();
    for entry in &lock.packages {
        validate_package_alias(&entry.name)?;
        if !entry.source.starts_with("git+") {
            continue;
        }
        let commit = entry
            .commit
            .as_deref()
            .ok_or_else(|| format!("missing locked commit for {}", entry.name))?;
        keep.insert(git_cache_dir(&entry.source, commit)?);
    }
    Ok(keep)
}

fn verify_lock_entry_cache(entry: &LockEntry) -> Result<bool, String> {
    validate_package_alias(&entry.name)?;
    if !entry.source.starts_with("git+") {
        if entry.source.starts_with("path+") {
            let path = path_from_source_uri(&entry.source)?;
            if !path.exists() {
                return Err(format!(
                    "path dependency {} source is missing: {}",
                    entry.name,
                    path.display()
                ));
            }
        }
        return Ok(false);
    }
    let commit = entry
        .commit
        .as_deref()
        .ok_or_else(|| format!("missing locked commit for {}", entry.name))?;
    let expected_hash = entry
        .content_hash
        .as_deref()
        .ok_or_else(|| format!("missing content hash for {}", entry.name))?;
    let cache_dir = git_cache_dir(&entry.source, commit)?;
    if !cache_dir.is_dir() {
        return Err(format!(
            "package cache entry for {} is missing: {}",
            entry.name,
            cache_dir.display()
        ));
    }
    verify_content_hash_or_compute(&cache_dir, expected_hash)?;
    match read_cache_metadata(&cache_dir)? {
        Some(metadata)
            if metadata.source == entry.source
                && metadata.commit == commit
                && metadata.content_hash == expected_hash => {}
        Some(metadata) => {
            return Err(format!(
                "package cache metadata mismatch for {}: expected {} {} {}, got {} {} {}",
                entry.name,
                entry.source,
                commit,
                expected_hash,
                metadata.source,
                metadata.commit,
                metadata.content_hash
            ));
        }
        None => write_cache_metadata(&cache_dir, &entry.source, commit, expected_hash)?,
    }
    Ok(true)
}

fn verify_materialized_lock_entry(
    ctx: &ManifestContext,
    entry: &LockEntry,
) -> Result<bool, String> {
    validate_package_alias(&entry.name)?;
    let packages_dir = ctx.packages_dir();
    if entry.source.starts_with("path+") {
        let dir = packages_dir.join(&entry.name);
        let file = packages_dir.join(format!("{}.harn", entry.name));
        if !dir.exists() && !file.exists() {
            return Err(format!(
                "materialized path dependency {} is missing under {}",
                entry.name,
                packages_dir.display()
            ));
        }
        return Ok(true);
    }
    if !entry.source.starts_with("git+") {
        return Ok(false);
    }
    let expected_hash = entry
        .content_hash
        .as_deref()
        .ok_or_else(|| format!("missing content hash for {}", entry.name))?;
    let dest_dir = packages_dir.join(&entry.name);
    if !dest_dir.is_dir() {
        return Err(format!(
            "materialized package {} is missing: {}",
            entry.name,
            dest_dir.display()
        ));
    }
    verify_content_hash_or_compute(&dest_dir, expected_hash)?;
    Ok(true)
}

fn verify_package_cache_impl(materialized: bool) -> Result<usize, String> {
    let ctx = load_current_manifest_context()?;
    let lock = LockFile::load(&ctx.lock_path())?
        .ok_or_else(|| format!("{} is missing", ctx.lock_path().display()))?;
    validate_lock_matches_manifest(&ctx, &lock)?;
    let mut verified = 0usize;
    for entry in &lock.packages {
        if verify_lock_entry_cache(entry)? {
            verified += 1;
        }
        if materialized && verify_materialized_lock_entry(&ctx, entry)? {
            verified += 1;
        }
    }
    Ok(verified)
}

fn clean_package_cache_impl(all: bool) -> Result<usize, String> {
    let entries = discover_git_cache_entries()?;
    if entries.is_empty() {
        return Ok(0);
    }
    if all {
        let root = cache_root()?;
        for child in ["git", "locks"] {
            let path = root.join(child);
            if path.exists() {
                fs::remove_dir_all(&path)
                    .map_err(|error| format!("failed to remove {}: {error}", path.display()))?;
            }
        }
        return Ok(entries.len());
    }

    let ctx = load_current_manifest_context()?;
    let lock = LockFile::load(&ctx.lock_path())?.ok_or_else(|| {
        format!(
            "{} is missing; pass --all to clean every cache entry",
            LOCK_FILE
        )
    })?;
    validate_lock_matches_manifest(&ctx, &lock)?;
    let keep = locked_git_cache_paths(&lock)?;
    let mut removed = 0usize;
    for entry in entries {
        if keep.contains(&entry.path) {
            continue;
        }
        fs::remove_dir_all(&entry.path)
            .map_err(|error| format!("failed to remove {}: {error}", entry.path.display()))?;
        removed += 1;
        if let Some(parent) = entry.path.parent() {
            let is_empty = fs::read_dir(parent)
                .map(|mut children| children.next().is_none())
                .unwrap_or(false);
            if is_empty {
                fs::remove_dir(parent)
                    .map_err(|error| format!("failed to remove {}: {error}", parent.display()))?;
            }
        }
    }
    Ok(removed)
}

pub fn list_package_cache() {
    let result = (|| -> Result<(PathBuf, Vec<PackageCacheEntry>), String> {
        Ok((cache_root()?, discover_git_cache_entries()?))
    })();

    match result {
        Ok((root, entries)) => {
            println!("Cache root: {}", root.display());
            if entries.is_empty() {
                println!("No cached git packages.");
                return;
            }
            println!("commit\tcontent_hash\tsource\tpath");
            for entry in entries {
                let (source, content_hash) = entry
                    .metadata
                    .as_ref()
                    .map(|metadata| (metadata.source.as_str(), metadata.content_hash.as_str()))
                    .unwrap_or(("(unknown)", "(unknown)"));
                println!(
                    "{}\t{}\t{}\t{}",
                    entry.commit,
                    content_hash,
                    source,
                    entry.path.display()
                );
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn clean_package_cache(all: bool) {
    match clean_package_cache_impl(all) {
        Ok(removed) => println!("Removed {removed} cached package entries."),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn verify_package_cache(materialized: bool) {
    match verify_package_cache_impl(materialized) {
        Ok(verified) => println!("Verified {verified} package cache entries."),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn search_package_registry(query: Option<&str>, registry: Option<&str>, json: bool) {
    match search_package_registry_impl(query, registry) {
        Ok(packages) if json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&packages)
                    .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
            );
        }
        Ok(packages) => {
            if packages.is_empty() {
                println!("No packages found.");
                return;
            }
            println!("name\tlatest\tharn\tcontract\tdescription");
            for package in packages {
                let latest = latest_registry_version(&package)
                    .map(|version| version.version.as_str())
                    .unwrap_or("-");
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    package.name,
                    latest,
                    package.harn.as_deref().unwrap_or("-"),
                    package.connector_contract.as_deref().unwrap_or("-"),
                    package.description.as_deref().unwrap_or("")
                );
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn show_package_registry_info(spec: &str, registry: Option<&str>, json: bool) {
    match package_registry_info_impl(spec, registry) {
        Ok(info) if json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&info)
                    .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
            );
        }
        Ok(info) => {
            let package = info.package;
            println!("{}", package.name);
            if let Some(description) = package.description.as_deref() {
                println!("description: {description}");
            }
            println!("repository: {}", package.repository);
            if let Some(license) = package.license.as_deref() {
                println!("license: {license}");
            }
            if let Some(harn) = package.harn.as_deref() {
                println!("harn: {harn}");
            }
            if let Some(contract) = package.connector_contract.as_deref() {
                println!("connector_contract: {contract}");
            }
            if let Some(docs) = package.docs_url.as_deref() {
                println!("docs: {docs}");
            }
            if let Some(checksum) = package.checksum.as_deref() {
                println!("checksum: {checksum}");
            }
            if let Some(provenance) = package.provenance.as_deref() {
                println!("provenance: {provenance}");
            }
            if !package.exports.is_empty() {
                println!("exports: {}", package.exports.join(", "));
            }
            if let Some(version) = info.selected_version {
                println!("selected: {}", version.version);
                println!("git: {}", version.git);
                if let Some(rev) = version.rev.as_deref() {
                    println!("rev: {rev}");
                }
                if let Some(branch) = version.branch.as_deref() {
                    println!("branch: {branch}");
                }
                if let Some(package_name) = version.package.as_deref() {
                    println!("package: {package_name}");
                }
            }
            if !package.versions.is_empty() {
                let versions = package
                    .versions
                    .iter()
                    .map(|version| {
                        if version.yanked {
                            format!("{} (yanked)", version.version)
                        } else {
                            version.version.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("versions: {versions}");
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn ensure_dependencies_materialized(anchor: &Path) -> Result<(), String> {
    let Some((manifest, dir)) = find_nearest_manifest(anchor) else {
        return Ok(());
    };
    if manifest.dependencies.is_empty() {
        return Ok(());
    }
    let ctx = ManifestContext { manifest, dir };
    let lock = LockFile::load(&ctx.lock_path())?.ok_or_else(|| {
        format!(
            "{} is missing; run `harn install`",
            ctx.lock_path().display()
        )
    })?;
    validate_lock_matches_manifest(&ctx, &lock)?;
    materialize_dependencies_from_lock(&ctx, &lock, None, false)?;
    Ok(())
}

fn dependency_section_bounds(lines: &[String]) -> Option<(usize, usize)> {
    let start = lines
        .iter()
        .position(|line| line.trim() == "[dependencies]")?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, line)| line.trim_start().starts_with('['))
        .map(|(index, _)| index)
        .unwrap_or(lines.len());
    Some((start, end))
}

fn render_dependency_line(alias: &str, dependency: &Dependency) -> Result<String, String> {
    validate_package_alias(alias)?;
    match dependency {
        Dependency::Path(path) => Ok(format!(
            "{alias} = {{ path = {} }}",
            toml_string_literal(path)?
        )),
        Dependency::Table(table) => {
            let mut fields = Vec::new();
            if let Some(path) = table.path.as_deref() {
                fields.push(format!("path = {}", toml_string_literal(path)?));
            }
            if let Some(git) = table.git.as_deref() {
                fields.push(format!("git = {}", toml_string_literal(git)?));
            }
            if let Some(branch) = table.branch.as_deref() {
                fields.push(format!("branch = {}", toml_string_literal(branch)?));
            } else if let Some(rev) = table.rev.as_deref().or(table.tag.as_deref()) {
                fields.push(format!("rev = {}", toml_string_literal(rev)?));
            }
            if let Some(package) = table.package.as_deref() {
                fields.push(format!("package = {}", toml_string_literal(package)?));
            }
            Ok(format!("{alias} = {{ {} }}", fields.join(", ")))
        }
    }
}

fn ensure_manifest_exists(manifest_path: &Path) -> Result<String, String> {
    if manifest_path.exists() {
        return fs::read_to_string(manifest_path)
            .map_err(|error| format!("failed to read {}: {error}", manifest_path.display()));
    }
    Ok("[package]\nname = \"my-project\"\nversion = \"0.1.0\"\n".to_string())
}

fn upsert_dependency_in_manifest(
    manifest_path: &Path,
    alias: &str,
    dependency: &Dependency,
) -> Result<(), String> {
    let content = ensure_manifest_exists(manifest_path)?;
    let mut lines: Vec<String> = content.lines().map(|line| line.to_string()).collect();
    if dependency_section_bounds(&lines).is_none() {
        if !lines.is_empty() && !lines.last().is_some_and(|line| line.is_empty()) {
            lines.push(String::new());
        }
        lines.push("[dependencies]".to_string());
    }
    let (start, end) = dependency_section_bounds(&lines).ok_or_else(|| {
        format!(
            "failed to locate [dependencies] in {}",
            manifest_path.display()
        )
    })?;
    let rendered = render_dependency_line(alias, dependency)?;
    if let Some((index, _)) = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .take(end - start - 1)
        .find(|(_, line)| {
            line.split('=')
                .next()
                .is_some_and(|key| key.trim() == alias)
        })
    {
        lines[index] = rendered;
    } else {
        lines.insert(end, rendered);
    }
    write_manifest_content(manifest_path, &(lines.join("\n") + "\n"))
}

fn remove_dependency_from_manifest(manifest_path: &Path, alias: &str) -> Result<bool, String> {
    let content = fs::read_to_string(manifest_path)
        .map_err(|error| format!("failed to read {}: {error}", manifest_path.display()))?;
    let mut lines: Vec<String> = content.lines().map(|line| line.to_string()).collect();
    let Some((start, end)) = dependency_section_bounds(&lines) else {
        return Ok(false);
    };
    let mut removed = false;
    lines = lines
        .into_iter()
        .enumerate()
        .filter_map(|(index, line)| {
            if index <= start || index >= end {
                return Some(line);
            }
            let matches = line
                .split('=')
                .next()
                .is_some_and(|key| key.trim() == alias);
            if matches {
                removed = true;
                None
            } else {
                Some(line)
            }
        })
        .collect();
    if removed {
        write_manifest_content(manifest_path, &(lines.join("\n") + "\n"))?;
    }
    Ok(removed)
}

fn install_packages_impl(
    frozen: bool,
    refetch: Option<&str>,
    offline: bool,
) -> Result<usize, String> {
    let ctx = load_current_manifest_context()?;
    let existing = LockFile::load(&ctx.lock_path())?;
    if ctx.manifest.dependencies.is_empty() {
        if !frozen {
            LockFile::default().save(&ctx.lock_path())?;
        }
        return Ok(0);
    }

    if (frozen || offline) && existing.is_none() {
        return Err(format!("{} is missing", ctx.lock_path().display()));
    }

    let desired = build_lockfile(
        &ctx,
        existing.as_ref(),
        None,
        false,
        !frozen && !offline,
        offline,
    )?;
    if frozen || offline {
        if existing.as_ref() != Some(&desired) {
            return Err(format!(
                "{} would need to change",
                ctx.lock_path().display()
            ));
        }
    } else {
        desired.save(&ctx.lock_path())?;
    }
    materialize_dependencies_from_lock(&ctx, &desired, refetch, offline)
}

pub fn install_packages(frozen: bool, refetch: Option<&str>, offline: bool) {
    match install_packages_impl(frozen, refetch, offline) {
        Ok(0) => println!("No dependencies to install."),
        Ok(installed) => println!("Installed {installed} package(s) to {PKG_DIR}/"),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn lock_packages() {
    let result = (|| -> Result<usize, String> {
        let ctx = load_current_manifest_context()?;
        let existing = LockFile::load(&ctx.lock_path())?;
        let lock = build_lockfile(&ctx, existing.as_ref(), None, true, true, false)?;
        lock.save(&ctx.lock_path())?;
        Ok(lock.packages.len())
    })();

    match result {
        Ok(count) => println!("Wrote {} with {count} package(s).", LOCK_FILE),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn update_packages(alias: Option<&str>, all: bool) {
    if !all && alias.is_none() {
        eprintln!("error: specify a dependency alias or pass --all");
        process::exit(1);
    }

    let result = (|| -> Result<usize, String> {
        let ctx = load_current_manifest_context()?;
        if let Some(alias) = alias {
            validate_package_alias(alias)?;
            if !ctx.manifest.dependencies.contains_key(alias) {
                return Err(format!("{alias} is not present in [dependencies]"));
            }
        }
        let existing = LockFile::load(&ctx.lock_path())?;
        let lock = build_lockfile(&ctx, existing.as_ref(), alias, all, true, false)?;
        lock.save(&ctx.lock_path())?;
        materialize_dependencies_from_lock(&ctx, &lock, None, false)
    })();

    match result {
        Ok(installed) => println!("Updated {installed} package(s)."),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn remove_package(alias: &str) {
    let result = (|| -> Result<bool, String> {
        validate_package_alias(alias)?;
        let ctx = load_current_manifest_context()?;
        let removed = remove_dependency_from_manifest(&ctx.manifest_path(), alias)?;
        if !removed {
            return Ok(false);
        }
        let mut lock = LockFile::load(&ctx.lock_path())?.unwrap_or_default();
        lock.remove(alias);
        lock.save(&ctx.lock_path())?;
        remove_materialized_package(&ctx.packages_dir(), alias)?;
        Ok(true)
    })();

    match result {
        Ok(true) => println!("Removed {alias} from {MANIFEST} and {LOCK_FILE}."),
        Ok(false) => {
            eprintln!("error: {alias} is not present in [dependencies]");
            process::exit(1);
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

fn normalize_add_request(
    name_or_spec: &str,
    alias: Option<&str>,
    git_url: Option<&str>,
    tag: Option<&str>,
    rev: Option<&str>,
    branch: Option<&str>,
    local_path: Option<&str>,
    registry: Option<&str>,
) -> Result<(String, Dependency), String> {
    if local_path.is_some() && (rev.is_some() || tag.is_some() || branch.is_some()) {
        return Err("path dependencies do not accept --rev, --tag, or --branch".to_string());
    }
    if git_url.is_none()
        && local_path.is_none()
        && rev.is_none()
        && tag.is_none()
        && branch.is_none()
    {
        if let Some(path) = existing_local_path_spec(name_or_spec) {
            let alias = alias
                .map(str::to_string)
                .map(Ok)
                .unwrap_or_else(|| derive_package_alias_from_path(&path))?;
            validate_package_alias(&alias)?;
            return Ok((
                alias,
                Dependency::Table(DepTable {
                    git: None,
                    tag: None,
                    rev: None,
                    branch: None,
                    path: Some(name_or_spec.to_string()),
                    package: None,
                }),
            ));
        }
        if parse_registry_package_spec(name_or_spec).is_some() {
            return registry_dependency_from_spec(name_or_spec, alias, registry);
        }
    }
    if git_url.is_some() || local_path.is_some() {
        if let Some(path) = local_path {
            let alias = alias
                .map(str::to_string)
                .unwrap_or_else(|| name_or_spec.to_string());
            validate_package_alias(&alias)?;
            return Ok((
                alias,
                Dependency::Table(DepTable {
                    git: None,
                    tag: None,
                    rev: None,
                    branch: None,
                    path: Some(path.to_string()),
                    package: None,
                }),
            ));
        }
        let alias = alias.unwrap_or(name_or_spec).to_string();
        validate_package_alias(&alias)?;
        if rev.is_none() && tag.is_none() && branch.is_none() {
            return Err(format!(
                "git dependency {alias} must specify `rev` or `branch`; use `harn add <url>@<tag-or-sha>` or pass `--rev`/`--branch`"
            ));
        }
        let git = normalize_git_url(git_url.ok_or_else(|| "missing --git URL".to_string())?)?;
        let package_name = derive_repo_name_from_source(&git)?;
        return Ok((
            alias.clone(),
            Dependency::Table(DepTable {
                git: Some(git),
                tag: None,
                rev: rev.or(tag).map(str::to_string),
                branch: branch.map(str::to_string),
                path: None,
                package: (alias != package_name).then_some(package_name),
            }),
        ));
    }

    if rev.is_some() && tag.is_some() {
        return Err("use only one of --rev or --tag".to_string());
    }
    let (raw_source, inline_ref) = parse_positional_git_spec(name_or_spec);
    if inline_ref.is_some() && (rev.is_some() || tag.is_some() || branch.is_some()) {
        return Err("specify the git ref either inline as @ref or via --rev/--branch".to_string());
    }
    let git = normalize_git_url(raw_source)?;
    let package_name = derive_repo_name_from_source(&git)?;
    let alias = alias.unwrap_or(package_name.as_str()).to_string();
    validate_package_alias(&alias)?;
    if inline_ref.is_none() && rev.is_none() && tag.is_none() && branch.is_none() {
        return Err(format!(
            "git dependency {alias} must specify `rev` or `branch`; use `harn add {raw_source}@<tag-or-sha>` or pass `--rev`/`--branch`"
        ));
    }
    Ok((
        alias.clone(),
        Dependency::Table(DepTable {
            git: Some(git),
            tag: None,
            rev: inline_ref.or(rev).or(tag).map(str::to_string),
            branch: branch.map(str::to_string),
            path: None,
            package: (alias != package_name).then_some(package_name),
        }),
    ))
}

#[cfg(test)]
pub fn add_package(
    name_or_spec: &str,
    alias: Option<&str>,
    git_url: Option<&str>,
    tag: Option<&str>,
    rev: Option<&str>,
    branch: Option<&str>,
    local_path: Option<&str>,
) {
    add_package_with_registry(
        name_or_spec,
        alias,
        git_url,
        tag,
        rev,
        branch,
        local_path,
        None,
    )
}

pub fn add_package_with_registry(
    name_or_spec: &str,
    alias: Option<&str>,
    git_url: Option<&str>,
    tag: Option<&str>,
    rev: Option<&str>,
    branch: Option<&str>,
    local_path: Option<&str>,
    registry: Option<&str>,
) {
    let result = (|| -> Result<(String, usize), String> {
        let manifest_path = std::env::current_dir()
            .map_err(|error| format!("failed to read cwd: {error}"))?
            .join(MANIFEST);
        let (alias, dependency) = normalize_add_request(
            name_or_spec,
            alias,
            git_url,
            tag,
            rev,
            branch,
            local_path,
            registry,
        )?;
        upsert_dependency_in_manifest(&manifest_path, &alias, &dependency)?;
        let installed = install_packages_impl(false, None, false)?;
        Ok((alias, installed))
    })();

    match result {
        Ok((alias, installed)) => {
            println!("Added {alias} to {MANIFEST}.");
            println!("Installed {installed} package(s).");
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

/// Resolved `[skills]` section plus the directory the manifest came
/// from. Paths in `skills.paths` are joined against `manifest_dir`;
/// `[[skill.source]]` fs entries get absolutized here too.
pub struct ResolvedSkillsConfig {
    pub config: SkillsConfig,
    pub sources: Vec<SkillSourceEntry>,
    pub manifest_dir: PathBuf,
}

/// Load the `[skills]` + `[[skill.source]]` tables from the nearest
/// harn.toml, walking up from `anchor` like [`load_check_config`].
/// Returns `None` when there is no manifest on the walk path.
pub fn load_skills_config(anchor: Option<&Path>) -> Option<ResolvedSkillsConfig> {
    let anchor = anchor
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let (manifest, dir) = find_nearest_manifest(&anchor)?;

    // Absolutize `[[skill.source]]` fs paths relative to manifest_dir.
    let sources = manifest
        .skill
        .sources
        .into_iter()
        .map(|s| match s {
            SkillSourceEntry::Fs { path, namespace } => {
                let abs = if PathBuf::from(&path).is_absolute() {
                    path
                } else {
                    dir.join(&path).display().to_string()
                };
                SkillSourceEntry::Fs {
                    path: abs,
                    namespace,
                }
            }
            other => other,
        })
        .collect();

    let mut config = manifest.skills;
    if let Some(raw) = config.signer_registry_url.as_deref() {
        if !raw.is_empty() && Url::parse(raw).is_err() && !PathBuf::from(raw).is_absolute() {
            config.signer_registry_url = Some(dir.join(raw).display().to_string());
        }
    }

    Some(ResolvedSkillsConfig {
        config,
        sources,
        manifest_dir: dir,
    })
}

/// Expand `skills.paths` (which may include simple `*` globs) into
/// concrete directories relative to `manifest_dir`. We implement just
/// enough globbing for the documented `packages/*/skills` pattern so
/// we don't force a `glob`-crate dep on harn-cli.
pub fn resolve_skills_paths(cfg: &ResolvedSkillsConfig) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in &cfg.config.paths {
        let raw = PathBuf::from(entry);
        let absolute = if raw.is_absolute() {
            raw
        } else {
            cfg.manifest_dir.join(raw)
        };
        out.extend(expand_single_star_glob(&absolute));
    }
    out
}

fn expand_single_star_glob(path: &Path) -> Vec<PathBuf> {
    let as_str = path.to_string_lossy().to_string();
    if !as_str.contains('*') {
        return vec![path.to_path_buf()];
    }
    let components: Vec<&str> = as_str.split('/').collect();
    let mut results: Vec<PathBuf> = vec![PathBuf::new()];
    for comp in components {
        let mut next: Vec<PathBuf> = Vec::new();
        if comp == "*" {
            for parent in &results {
                if let Ok(entries) = fs::read_dir(parent) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            next.push(path);
                        }
                    }
                }
            }
        } else if comp.is_empty() {
            for parent in &results {
                if parent.as_os_str().is_empty() {
                    next.push(PathBuf::from("/"));
                } else {
                    next.push(parent.clone());
                }
            }
        } else {
            for parent in &results {
                let joined = parent.join(comp);
                // Filter branches whose literal suffix does not exist on
                // disk so downstream FS sources don't iterate over phantom
                // directories (one Rust round-trip cheaper than discovering
                // them at load time).
                if joined.exists() || parent.as_os_str().is_empty() {
                    next.push(joined);
                }
            }
        }
        results = next;
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tokio::sync::MutexGuard;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TriggerTables {
        #[serde(default)]
        triggers: Vec<TriggerManifestEntry>,
    }

    fn test_vm() -> harn_vm::Vm {
        let mut vm = harn_vm::Vm::new();
        harn_vm::register_vm_stdlib(&mut vm);
        vm
    }

    fn write_trigger_project(root: &Path, manifest: &str, lib_source: Option<&str>) -> PathBuf {
        std::fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(MANIFEST), manifest).unwrap();
        if let Some(source) = lib_source {
            fs::write(root.join("lib.harn"), source).unwrap();
        }
        let harn_file = root.join("main.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();
        harn_file
    }

    struct TestEnvGuard {
        previous_cwd: PathBuf,
        previous_cache: Option<std::ffi::OsString>,
        previous_registry: Option<std::ffi::OsString>,
        _cwd_lock: MutexGuard<'static, ()>,
        _env_lock: MutexGuard<'static, ()>,
    }

    impl Drop for TestEnvGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous_cwd).unwrap();
            if let Some(value) = self.previous_cache.clone() {
                std::env::set_var(HARN_CACHE_DIR_ENV, value);
            } else {
                std::env::remove_var(HARN_CACHE_DIR_ENV);
            }
            if let Some(value) = self.previous_registry.clone() {
                std::env::set_var(HARN_PACKAGE_REGISTRY_ENV, value);
            } else {
                std::env::remove_var(HARN_PACKAGE_REGISTRY_ENV);
            }
        }
    }

    fn with_test_env<T>(cwd: &Path, cache_dir: &Path, f: impl FnOnce() -> T) -> T {
        let cwd_lock = crate::tests::common::cwd_lock::lock_cwd();
        let env_lock = crate::tests::common::env_lock::lock_env().blocking_lock();
        let guard = TestEnvGuard {
            previous_cwd: std::env::current_dir().unwrap(),
            previous_cache: std::env::var_os(HARN_CACHE_DIR_ENV),
            previous_registry: std::env::var_os(HARN_PACKAGE_REGISTRY_ENV),
            _cwd_lock: cwd_lock,
            _env_lock: env_lock,
        };
        std::env::set_current_dir(cwd).unwrap();
        std::env::set_var(HARN_CACHE_DIR_ENV, cache_dir);
        std::env::remove_var(HARN_PACKAGE_REGISTRY_ENV);
        let result = f();
        drop(guard);
        result
    }

    fn run_git(repo: &Path, args: &[&str]) -> String {
        let output = test_git_command(repo).args(args).output().unwrap();
        if !output.status.success() {
            panic!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn test_git_command(repo: &Path) -> process::Command {
        let mut command = process::Command::new("git");
        command
            .current_dir(repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        command
    }

    fn create_git_package_repo_with(
        name: &str,
        manifest_tail: &str,
        lib_source: &str,
    ) -> (tempfile::TempDir, PathBuf, String) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join(name);
        fs::create_dir_all(&repo).unwrap();
        let init = test_git_command(&repo)
            .args(["init", "-b", "main"])
            .output()
            .unwrap();
        if !init.status.success() {
            let fallback = test_git_command(&repo).arg("init").output().unwrap();
            assert!(
                fallback.status.success(),
                "git init failed: {}",
                String::from_utf8_lossy(&fallback.stderr)
            );
        }
        run_git(&repo, &["config", "user.email", "tests@example.com"]);
        run_git(&repo, &["config", "user.name", "Harn Tests"]);
        run_git(&repo, &["config", "core.hooksPath", "/dev/null"]);
        fs::write(
            repo.join(MANIFEST),
            format!(
                r#"
[package]
name = "{name}"
version = "0.1.0"
"#
            ) + manifest_tail,
        )
        .unwrap();
        fs::write(repo.join("lib.harn"), lib_source).unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-m", "initial"]);
        run_git(&repo, &["tag", "v1.0.0"]);
        let branch = run_git(&repo, &["branch", "--show-current"]);
        (tmp, repo, branch)
    }

    fn create_git_package_repo() -> (tempfile::TempDir, PathBuf, String) {
        create_git_package_repo_with(
            "acme-lib",
            "",
            "pub fn value() -> string { return \"v1\" }\n",
        )
    }

    fn write_package_registry_index(
        path: &Path,
        registry_name: &str,
        git: &str,
        package_name: &str,
    ) {
        fs::write(
            path,
            format!(
                r#"
version = 1

[[package]]
name = "{registry_name}"
description = "Acme package for registry tests"
repository = "{git}"
license = "MIT OR Apache-2.0"
harn = ">=0.7,<0.8"
exports = ["lib"]
connector_contract = "v1"
docs_url = "https://docs.example.test/acme"
checksum = "sha256:index"
provenance = "https://provenance.example.test/acme"

[[package.version]]
version = "1.0.0"
git = "{git}"
rev = "v1.0.0"
package = "{package_name}"
checksum = "sha256:package"
provenance = "https://provenance.example.test/acme/1.0.0"
"#
            ),
        )
        .unwrap();
    }

    fn test_harn_connector_source(provider_id: &str) -> String {
        format!(
            r#"
pub fn provider_id() {{
  return "{provider_id}"
}}

pub fn kinds() {{
  return ["webhook"]
}}

pub fn payload_schema() {{
  return {{
    harn_schema_name: "EchoEventPayload",
    json_schema: {{
      type: "object",
      additionalProperties: true,
    }},
  }}
}}
"#
        )
    }

    #[test]
    fn preflight_severity_parsing_accepts_synonyms() {
        assert_eq!(
            PreflightSeverity::from_opt(Some("warning")),
            PreflightSeverity::Warning
        );
        assert_eq!(
            PreflightSeverity::from_opt(Some("WARN")),
            PreflightSeverity::Warning
        );
        assert_eq!(
            PreflightSeverity::from_opt(Some("off")),
            PreflightSeverity::Off
        );
        assert_eq!(
            PreflightSeverity::from_opt(Some("allow")),
            PreflightSeverity::Off
        );
        assert_eq!(
            PreflightSeverity::from_opt(Some("error")),
            PreflightSeverity::Error
        );
        assert_eq!(PreflightSeverity::from_opt(None), PreflightSeverity::Error);
        // Unknown values fall back to the safe default (error).
        assert_eq!(
            PreflightSeverity::from_opt(Some("bogus")),
            PreflightSeverity::Error
        );
    }

    #[test]
    fn load_check_config_walks_up_from_nested_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Mark root as project boundary so walk-up terminates here.
        std::fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[check]
preflight_severity = "warning"
preflight_allow = ["custom.scan", "runtime.*"]
host_capabilities_path = "./schemas/host-caps.json"

[workspace]
pipelines = ["pipelines", "scripts"]
"#,
        )
        .unwrap();
        let nested = root.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let harn_file = nested.join("pipeline.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();

        let cfg = load_check_config(Some(&harn_file));
        assert_eq!(cfg.preflight_severity.as_deref(), Some("warning"));
        assert_eq!(cfg.preflight_allow, vec!["custom.scan", "runtime.*"]);
        let caps_path = cfg.host_capabilities_path.expect("host caps path");
        assert!(
            caps_path.ends_with("schemas/host-caps.json")
                || caps_path.ends_with("schemas\\host-caps.json"),
            "unexpected absolutized path: {caps_path}"
        );

        let (workspace, manifest_dir) =
            load_workspace_config(Some(&harn_file)).expect("workspace manifest");
        assert_eq!(workspace.pipelines, vec!["pipelines", "scripts"]);
        // Walk-up lands on the directory containing the harn.toml.
        assert_eq!(manifest_dir, root);
    }

    #[test]
    fn orchestrator_drain_config_parses_defaults_and_overrides() {
        let default_manifest: Manifest = toml::from_str(
            r#"
[package]
name = "fixture"
"#,
        )
        .unwrap();
        assert_eq!(default_manifest.orchestrator.drain.max_items, 1024);
        assert_eq!(default_manifest.orchestrator.drain.deadline_seconds, 30);
        assert_eq!(default_manifest.orchestrator.pumps.max_outstanding, 64);

        let configured: Manifest = toml::from_str(
            r#"
[package]
name = "fixture"

[orchestrator]
drain.max_items = 77
drain.deadline_seconds = 12
pumps.max_outstanding = 3
"#,
        )
        .unwrap();
        assert_eq!(configured.orchestrator.drain.max_items, 77);
        assert_eq!(configured.orchestrator.drain.deadline_seconds, 12);
        assert_eq!(configured.orchestrator.pumps.max_outstanding, 3);
    }

    #[test]
    fn load_skills_config_parses_tables_and_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[skills]
paths = ["packages/*/skills", "../shared-skills"]
lookup_order = ["cli", "project", "host"]
disable = ["system"]
signer_registry_url = "https://skills.harnlang.com/signers/"

[skills.defaults]
tool_search = "bm25"
always_loaded = ["look", "edit"]

[[skill.source]]
type = "fs"
path = "../shared"

[[skill.source]]
type = "git"
url = "https://github.com/acme/harn-skills"
tag = "v1.2.0"

[[skill.source]]
type = "registry"
url = "https://skills.harnlang.com"
name = "acme/ops"
"#,
        )
        .unwrap();
        let harn_file = root.join("main.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();

        let resolved = load_skills_config(Some(&harn_file)).expect("skills config should load");
        assert_eq!(resolved.config.paths.len(), 2);
        assert_eq!(resolved.config.lookup_order, vec!["cli", "project", "host"]);
        assert_eq!(resolved.config.disable, vec!["system"]);
        assert_eq!(
            resolved.config.signer_registry_url.as_deref(),
            Some("https://skills.harnlang.com/signers/")
        );
        assert_eq!(
            resolved.config.defaults.tool_search.as_deref(),
            Some("bm25")
        );
        assert_eq!(resolved.config.defaults.always_loaded, vec!["look", "edit"]);

        assert_eq!(resolved.sources.len(), 3);
        match &resolved.sources[0] {
            SkillSourceEntry::Fs { path, .. } => {
                assert!(path.ends_with("shared"), "fs path absolutized: {path}");
            }
            other => panic!("expected fs source, got {other:?}"),
        }
        match &resolved.sources[1] {
            SkillSourceEntry::Git { url, tag, .. } => {
                assert!(url.contains("harn-skills"));
                assert_eq!(tag.as_deref(), Some("v1.2.0"));
            }
            other => panic!("expected git source, got {other:?}"),
        }
        assert!(matches!(
            &resolved.sources[2],
            SkillSourceEntry::Registry { .. }
        ));
    }

    #[test]
    fn expand_single_star_glob_handles_packages_pattern() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("packages/pkg-a/skills")).unwrap();
        fs::create_dir_all(root.join("packages/pkg-b/skills")).unwrap();
        fs::create_dir_all(root.join("packages/pkg-c")).unwrap();

        let raw = root.join("packages").join("*").join("skills");
        let expanded = expand_single_star_glob(&raw);
        assert_eq!(expanded.len(), 2);
    }

    #[test]
    fn load_check_config_stops_at_git_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        // An ancestor harn.toml above .git must NOT be picked up.
        fs::write(
            tmp.path().join(MANIFEST),
            "[check]\npreflight_severity = \"off\"\n",
        )
        .unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let inner = project.join("src");
        std::fs::create_dir_all(&inner).unwrap();
        let harn_file = inner.join("main.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();
        let cfg = load_check_config(Some(&harn_file));
        assert!(
            cfg.preflight_severity.is_none(),
            "must not inherit harn.toml from outside the .git boundary"
        );
    }

    #[test]
    fn lock_file_round_trips_typed_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(LOCK_FILE);
        let lock = LockFile {
            version: LOCK_FILE_VERSION,
            packages: vec![LockEntry {
                name: "acme-lib".to_string(),
                source: "git+https://github.com/acme/acme-lib".to_string(),
                rev_request: Some("v1.0.0".to_string()),
                commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
                content_hash: Some("sha256:deadbeef".to_string()),
            }],
        };
        lock.save(&path).unwrap();
        let loaded = LockFile::load(&path).unwrap().unwrap();
        assert_eq!(loaded, lock);
    }

    #[test]
    fn compute_content_hash_ignores_git_and_hash_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(root.join(".gitignore"), "ignored\n").unwrap();
        fs::write(root.join(CONTENT_HASH_FILE), "stale\n").unwrap();
        fs::write(
            root.join("lib.harn"),
            "pub fn value() -> number { return 1 }\n",
        )
        .unwrap();
        let first = compute_content_hash(root).unwrap();
        fs::write(root.join(".git/HEAD"), "changed\n").unwrap();
        fs::write(root.join(".gitignore"), "changed\n").unwrap();
        fs::write(root.join(CONTENT_HASH_FILE), "changed\n").unwrap();
        let second = compute_content_hash(root).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn add_and_remove_git_dependency_round_trip() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[package]
name = "workspace"
version = "0.1.0"
"#,
        )
        .unwrap();

        with_test_env(root, &cache_dir, || {
            let spec = format!("{}@v1.0.0", repo.display());
            add_package(&spec, None, None, None, None, None, None);

            let alias = "acme-lib";
            let manifest = fs::read_to_string(root.join(MANIFEST)).unwrap();
            assert!(manifest.contains("acme-lib"));
            assert!(manifest.contains("rev = \"v1.0.0\""));

            let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
            let entry = lock.find(alias).unwrap();
            assert_eq!(lock.version, LOCK_FILE_VERSION);
            assert!(entry.source.starts_with("git+file://"));
            assert!(entry.commit.as_deref().is_some_and(is_full_git_sha));
            assert!(entry
                .content_hash
                .as_deref()
                .is_some_and(|hash| hash.starts_with("sha256:")));
            assert!(root.join(PKG_DIR).join(alias).join("lib.harn").is_file());

            remove_package(alias);
            let updated_manifest = fs::read_to_string(root.join(MANIFEST)).unwrap();
            assert!(!updated_manifest.contains("acme-lib ="));
            let updated_lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
            assert!(updated_lock.find(alias).is_none());
            assert!(!root.join(PKG_DIR).join(alias).exists());
        });
    }

    #[test]
    fn update_branch_dependency_refreshes_locked_commit() {
        let (_repo_tmp, repo, branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
        fs::create_dir_all(root.join(".git")).unwrap();
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        fs::write(
            root.join(MANIFEST),
            format!(
                r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
acme-lib = {{ git = "{git}", branch = "{branch}" }}
"#
            ),
        )
        .unwrap();

        with_test_env(root, &cache_dir, || {
            let installed = install_packages_impl(false, None, false).unwrap();
            assert_eq!(installed, 1);
            let first_lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
            let first_commit = first_lock
                .find("acme-lib")
                .and_then(|entry| entry.commit.clone())
                .unwrap();

            fs::write(
                repo.join("lib.harn"),
                "pub fn value() -> string { return \"v2\" }\n",
            )
            .unwrap();
            run_git(&repo, &["add", "."]);
            run_git(&repo, &["commit", "-m", "update"]);

            update_packages(Some("acme-lib"), false);
            let second_lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
            let second_commit = second_lock
                .find("acme-lib")
                .and_then(|entry| entry.commit.clone())
                .unwrap();
            assert_ne!(first_commit, second_commit);
        });
    }

    #[test]
    fn add_positional_local_path_dependency_uses_manifest_name_and_live_link() {
        let dependency_tmp = tempfile::tempdir().unwrap();
        let dependency_root = dependency_tmp.path().join("harn-openapi");
        fs::create_dir_all(&dependency_root).unwrap();
        fs::write(
            dependency_root.join(MANIFEST),
            r#"
[package]
name = "openapi"
version = "0.1.0"
"#,
        )
        .unwrap();
        fs::write(
            dependency_root.join("lib.harn"),
            "pub fn version() -> string { return \"v1\" }\n",
        )
        .unwrap();

        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[package]
name = "workspace"
version = "0.1.0"
"#,
        )
        .unwrap();

        with_test_env(root, &cache_dir, || {
            add_package(
                dependency_root.to_string_lossy().as_ref(),
                None,
                None,
                None,
                None,
                None,
                None,
            );

            let manifest = fs::read_to_string(root.join(MANIFEST)).unwrap();
            assert!(
                manifest.contains("openapi = { path = "),
                "manifest should use package.name as alias: {manifest}"
            );
            let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
            let entry = lock.find("openapi").expect("openapi lock entry");
            assert!(entry.source.starts_with("path+file://"));
            let materialized = root.join(PKG_DIR).join("openapi");
            assert!(materialized.join("lib.harn").is_file());

            #[cfg(unix)]
            assert!(
                fs::symlink_metadata(&materialized)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "path dependencies should be live-linked on Unix"
            );

            fs::write(
                dependency_root.join("lib.harn"),
                "pub fn version() -> string { return \"v2\" }\n",
            )
            .unwrap();
            let live_source = fs::read_to_string(materialized.join("lib.harn")).unwrap();
            #[cfg(unix)]
            assert!(
                live_source.contains("v2"),
                "materialized path dependency should reflect sibling repo edits"
            );

            remove_package("openapi");
            assert!(!materialized.exists());
            assert!(dependency_root.join("lib.harn").exists());
        });
    }

    #[test]
    fn frozen_install_errors_when_lockfile_is_missing() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
        fs::create_dir_all(root.join(".git")).unwrap();
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        fs::write(
            root.join(MANIFEST),
            format!(
                r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
acme-lib = {{ git = "{git}", rev = "v1.0.0" }}
"#
            ),
        )
        .unwrap();

        with_test_env(root, &cache_dir, || {
            let error = install_packages_impl(true, None, false).unwrap_err();
            assert!(error.contains(LOCK_FILE));
        });
    }

    #[test]
    fn offline_locked_install_materializes_from_cache_without_source_repo() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
        fs::create_dir_all(root.join(".git")).unwrap();
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        fs::write(
            root.join(MANIFEST),
            format!(
                r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
acme-lib = {{ git = "{git}", rev = "v1.0.0" }}
"#
            ),
        )
        .unwrap();

        with_test_env(root, &cache_dir, || {
            let installed = install_packages_impl(false, None, false).unwrap();
            assert_eq!(installed, 1);
            fs::remove_dir_all(root.join(PKG_DIR)).unwrap();
            fs::remove_dir_all(&repo).unwrap();

            let installed = install_packages_impl(true, None, true).unwrap();
            assert_eq!(installed, 1);
            assert!(root
                .join(PKG_DIR)
                .join("acme-lib")
                .join("lib.harn")
                .is_file());
        });
    }

    #[test]
    fn offline_locked_install_fails_when_cache_is_missing() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
        fs::create_dir_all(root.join(".git")).unwrap();
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        fs::write(
            root.join(MANIFEST),
            format!(
                r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
acme-lib = {{ git = "{git}", rev = "v1.0.0" }}
"#
            ),
        )
        .unwrap();

        with_test_env(root, &cache_dir, || {
            install_packages_impl(false, None, false).unwrap();
            fs::remove_dir_all(cache_dir.join("git")).unwrap();
            let error = install_packages_impl(true, None, true).unwrap_err();
            assert!(error.contains("offline mode"));
        });
    }

    #[test]
    fn package_cache_verify_detects_tampering_even_with_stale_marker() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
        fs::create_dir_all(root.join(".git")).unwrap();
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        fs::write(
            root.join(MANIFEST),
            format!(
                r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
acme-lib = {{ git = "{git}", rev = "v1.0.0" }}
"#
            ),
        )
        .unwrap();

        with_test_env(root, &cache_dir, || {
            install_packages_impl(false, None, false).unwrap();
            let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
            let entry = lock.find("acme-lib").unwrap();
            let cache_dir = git_cache_dir(&entry.source, entry.commit.as_deref().unwrap()).unwrap();
            fs::write(
                cache_dir.join("lib.harn"),
                "pub fn value() { return \"pwned\" }\n",
            )
            .unwrap();

            let error = verify_package_cache_impl(false).unwrap_err();
            assert!(error.contains("content hash mismatch"));
        });
    }

    #[test]
    fn package_cache_clean_all_removes_cached_git_entries() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
        fs::create_dir_all(root.join(".git")).unwrap();
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        fs::write(
            root.join(MANIFEST),
            format!(
                r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
acme-lib = {{ git = "{git}", rev = "v1.0.0" }}
"#
            ),
        )
        .unwrap();

        with_test_env(root, &cache_dir, || {
            install_packages_impl(false, None, false).unwrap();
            assert_eq!(discover_git_cache_entries().unwrap().len(), 1);

            let removed = clean_package_cache_impl(true).unwrap();
            assert_eq!(removed, 1);
            assert!(discover_git_cache_entries().unwrap().is_empty());
        });
    }

    #[test]
    fn add_github_shorthand_requires_version_or_ref() {
        let error = normalize_add_request(
            "github.com/burin-labs/harn-openapi",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(error.contains("must specify `rev` or `branch`"));
    }

    #[test]
    fn add_github_shorthand_with_ref_writes_git_dependency() {
        let (alias, dependency) = normalize_add_request(
            "github.com/burin-labs/harn-openapi@v1.2.3",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(alias, "harn-openapi");
        assert_eq!(
            render_dependency_line(&alias, &dependency).unwrap(),
            "harn-openapi = { git = \"https://github.com/burin-labs/harn-openapi\", rev = \"v1.2.3\" }"
        );
    }

    #[test]
    fn registry_index_search_and_info_use_local_file_without_network() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
        let registry_path = root.join("index.toml");
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        write_package_registry_index(&registry_path, "@burin/acme-lib", &git, "acme-lib");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[package]
name = "workspace"
version = "0.1.0"
"#,
        )
        .unwrap();

        with_test_env(root, &cache_dir, || {
            let matches = search_package_registry_impl(Some("acme"), Some("index.toml")).unwrap();
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].name, "@burin/acme-lib");
            assert_eq!(matches[0].harn.as_deref(), Some(">=0.7,<0.8"));
            assert_eq!(matches[0].connector_contract.as_deref(), Some("v1"));
            assert_eq!(matches[0].exports, vec!["lib"]);

            let info =
                package_registry_info_impl("@burin/acme-lib@1.0.0", Some("index.toml")).unwrap();
            assert_eq!(info.package.license.as_deref(), Some("MIT OR Apache-2.0"));
            assert_eq!(
                info.selected_version
                    .as_ref()
                    .map(|version| version.git.as_str()),
                Some(git.as_str())
            );
        });
    }

    #[test]
    fn add_registry_dependency_writes_existing_git_dependency_shape() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
        let registry_path = root.join("index.toml");
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        write_package_registry_index(&registry_path, "@burin/acme-lib", &git, "acme-lib");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[package]
name = "workspace"
version = "0.1.0"
"#,
        )
        .unwrap();

        with_test_env(root, &cache_dir, || {
            std::env::set_var(HARN_PACKAGE_REGISTRY_ENV, "index.toml");
            add_package("@burin/acme-lib@1.0.0", None, None, None, None, None, None);

            let manifest = fs::read_to_string(root.join(MANIFEST)).unwrap();
            assert!(
                manifest.contains(&format!(
                    "acme-lib = {{ git = \"{git}\", rev = \"v1.0.0\" }}"
                )),
                "registry install should write the same dependency line as a direct git add: {manifest}"
            );
            let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
            let entry = lock.find("acme-lib").unwrap();
            assert_eq!(entry.source, format!("git+{git}"));
            assert!(root
                .join(PKG_DIR)
                .join("acme-lib")
                .join("lib.harn")
                .is_file());
        });
    }

    #[test]
    fn registry_index_rejects_invalid_names_and_duplicate_versions() {
        let content = r#"
version = 1

[[package]]
name = "@bad/"
repository = "https://github.com/acme/acme-lib"

[[package.version]]
version = "1.0.0"
git = "https://github.com/acme/acme-lib"
rev = "v1.0.0"
"#;
        let error = parse_package_registry_index("fixture", content).unwrap_err();
        assert!(error.contains("invalid package name"));

        let content = r#"
version = 1

[[package]]
name = "@burin/acme-lib"
repository = "https://github.com/acme/acme-lib"

[[package.version]]
version = "1.0.0"
git = "https://github.com/acme/acme-lib"
rev = "v1.0.0"

[[package.version]]
version = "1.0.0"
git = "https://github.com/acme/acme-lib"
rev = "v1.0.0"
"#;
        let error = parse_package_registry_index("fixture", content).unwrap_err();
        assert!(error.contains("more than once"));
    }

    #[test]
    fn install_resolves_transitive_git_dependencies_from_clean_cache() {
        let (_sdk_tmp, sdk_repo, _branch) = create_git_package_repo_with(
            "notion-sdk-harn",
            "",
            "pub fn sdk_value() -> string { return \"sdk\" }\n",
        );
        let sdk_git = normalize_git_url(sdk_repo.to_string_lossy().as_ref()).unwrap();
        let connector_tail = format!(
            r#"

[dependencies]
notion-sdk-harn = {{ git = "{sdk_git}", rev = "v1.0.0" }}
"#
        );
        let (_connector_tmp, connector_repo, _branch) = create_git_package_repo_with(
            "notion-connector-harn",
            &connector_tail,
            r#"
import "notion-sdk-harn"

pub fn connector_value() -> string {
  return "connector"
}
"#,
        );

        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
        fs::create_dir_all(root.join(".git")).unwrap();
        let connector_git = normalize_git_url(connector_repo.to_string_lossy().as_ref()).unwrap();
        fs::write(
            root.join(MANIFEST),
            format!(
                r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
notion-connector-harn = {{ git = "{connector_git}", rev = "v1.0.0" }}
"#
            ),
        )
        .unwrap();

        with_test_env(root, &cache_dir, || {
            let installed = install_packages_impl(false, None, false).unwrap();
            assert_eq!(installed, 2);

            let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
            assert!(lock.find("notion-connector-harn").is_some());
            assert!(lock.find("notion-sdk-harn").is_some());
            assert!(root
                .join(PKG_DIR)
                .join("notion-connector-harn")
                .join("lib.harn")
                .is_file());
            assert!(root
                .join(PKG_DIR)
                .join("notion-sdk-harn")
                .join("lib.harn")
                .is_file());

            let mut vm = test_vm();
            let exports = futures::executor::block_on(
                vm.load_module_exports(
                    &root
                        .join(PKG_DIR)
                        .join("notion-connector-harn")
                        .join("lib.harn"),
                ),
            )
            .expect("transitive import should load from the workspace package root");
            assert!(exports.contains_key("connector_value"));
        });
    }

    #[test]
    fn git_packages_reject_transitive_path_dependencies() {
        let connector_tail = r#"

[dependencies]
local-helper = { path = "../helper" }
"#;
        let (_connector_tmp, connector_repo, _branch) = create_git_package_repo_with(
            "notion-connector-harn",
            connector_tail,
            "pub fn connector_value() -> string { return \"connector\" }\n",
        );

        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
        fs::create_dir_all(root.join(".git")).unwrap();
        let connector_git = normalize_git_url(connector_repo.to_string_lossy().as_ref()).unwrap();
        fs::write(
            root.join(MANIFEST),
            format!(
                r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
notion-connector-harn = {{ git = "{connector_git}", rev = "v1.0.0" }}
"#
            ),
        )
        .unwrap();

        with_test_env(root, &cache_dir, || {
            let error = install_packages_impl(false, None, false).unwrap_err();
            assert!(
                error.contains("path dependencies are not supported inside git-installed packages")
            );
        });
    }

    #[test]
    fn load_runtime_extensions_uses_only_root_llm_config() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".harn/packages/acme")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[llm.aliases]
project-fast = { id = "project/model", provider = "project" }

[llm.providers.project]
base_url = "https://project.test/v1"
chat_endpoint = "/chat/completions"
"#,
        )
        .unwrap();
        fs::write(
            root.join(".harn/packages/acme/harn.toml"),
            r#"
[llm.aliases]
acme-fast = { id = "acme/model", provider = "acme" }

[llm.providers.acme]
base_url = "https://acme.test/v1"
chat_endpoint = "/chat/completions"
"#,
        )
        .unwrap();
        let harn_file = root.join("main.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();

        let extensions = load_runtime_extensions(&harn_file);
        let llm = extensions.llm.expect("merged llm config");
        assert!(llm.providers.contains_key("project"));
        assert!(llm.aliases.contains_key("project-fast"));
        assert!(!llm.providers.contains_key("acme"));
        assert!(!llm.aliases.contains_key("acme-fast"));
    }

    #[test]
    fn load_runtime_extensions_ignores_package_hooks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".harn/packages/acme")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[package]
name = "workspace"

[[hooks]]
event = "PostToolUse"
pattern = "tool.name =~ \"read\""
handler = "workspace::after_read"
"#,
        )
        .unwrap();
        fs::write(
            root.join(".harn/packages/acme/harn.toml"),
            r#"
[package]
name = "acme"

[[hooks]]
event = "PreToolUse"
pattern = "tool.name =~ \"edit|write\""
handler = "acme::audit_edit"
"#,
        )
        .unwrap();
        let harn_file = root.join("main.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();

        let extensions = load_runtime_extensions(&harn_file);
        assert_eq!(extensions.hooks.len(), 1);
        assert_eq!(extensions.hooks[0].handler, "workspace::after_read");
    }

    #[test]
    fn load_runtime_extensions_collects_manifest_provider_connectors() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[[providers]]
id = "echo"
connector = { harn = "./echo_connector.harn" }

[[providers]]
id = "github"
connector = { rust = "builtin" }
"#,
        )
        .unwrap();
        let harn_file = root.join("main.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();

        let extensions = load_runtime_extensions(&harn_file);
        assert_eq!(extensions.provider_connectors.len(), 2);
        assert!(matches!(
            &extensions.provider_connectors[0].connector,
            ResolvedProviderConnectorKind::Harn { module } if module == "./echo_connector.harn"
        ));
        assert!(matches!(
            extensions.provider_connectors[1].connector,
            ResolvedProviderConnectorKind::RustBuiltin
        ));
    }

    #[test]
    fn trigger_manifest_entries_round_trip_through_toml() {
        let source = r#"
[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
autonomy_tier = "act_with_approval"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
when_budget = { max_cost_usd = 0.001, tokens_max = 500, timeout = "5s" }
handler = "handlers::on_new_issue"
dedupe_key = "event.dedupe_key"
retry = { max = 7, backoff = "svix", retention_days = 7 }
priority = "high"
budget = { max_cost_usd = 0.001, max_tokens = 500, hourly_cost_usd = 1.0, daily_cost_usd = 5.0, max_concurrent = 10, on_budget_exhausted = "retry_later" }
secrets = { signing_secret = "github/webhook-secret" }
filter = "event.kind"

[[triggers]]
id = "daily-digest"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
handler = "worker://digest-queue"
schedule = "0 9 * * *"
timezone = "America/Los_Angeles"
"#;
        let parsed: TriggerTables = toml::from_str(source).expect("trigger tables parse");
        let encoded = toml::to_string(&parsed).expect("trigger tables encode");
        let reparsed: TriggerTables = toml::from_str(&encoded).expect("trigger tables reparse");
        assert_eq!(reparsed, parsed);
    }

    #[test]
    fn trigger_manifest_entries_round_trip_flow_control_tables() {
        let source = r#"
[[triggers]]
id = "github-priority"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
concurrency = { key = "event.headers.tenant", max = 2 }
throttle = { key = "event.headers.user", period = "1m", max = 30 }
rate_limit = { period = "1h", max = 1000 }
debounce = { key = "event.headers.pr_id", period = "30s" }
singleton = { key = "event.headers.repo" }
priority = { key = "event.headers.tier", order = ["gold", "silver", "bronze"] }
secrets = { signing_secret = "github/webhook-secret" }

[[triggers]]
id = "github-batch"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
batch = { key = "event.headers.repo", size = 50, timeout = "30s" }
secrets = { signing_secret = "github/webhook-secret" }
"#;
        let parsed: TriggerTables = toml::from_str(source).expect("trigger tables parse");
        let encoded = toml::to_string(&parsed).expect("trigger tables encode");
        let reparsed: TriggerTables = toml::from_str(&encoded).expect("trigger tables reparse");
        assert_eq!(reparsed, parsed);
    }

    #[test]
    fn trigger_manifest_entries_round_trip_stream_sources() {
        let source = r#"
[[triggers]]
id = "market-fan-in"
handler = "handlers::on_market_event"
when = "handlers::should_handle"
debounce = { key = "event.provider + \":\" + event.kind", period = "2s" }

[[triggers.sources]]
id = "open"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
schedule = "0 14 * * 1-5"
timezone = "America/New_York"

[[triggers.sources]]
id = "quotes"
kind = "stream"
provider = "kafka"
match = { events = ["quote.tick"] }
topic = "quotes"
consumer_group = "harn-market"
window = { mode = "sliding", key = "event.provider_payload.key", size = "5m", every = "1m", max_items = 5000 }
"#;
        let parsed: TriggerTables = toml::from_str(source).expect("trigger tables parse");
        assert_eq!(parsed.triggers.len(), 1);
        assert_eq!(parsed.triggers[0].sources.len(), 2);
        let encoded = toml::to_string(&parsed).expect("trigger tables encode");
        let reparsed: TriggerTables = toml::from_str(&encoded).expect("trigger tables reparse");
        assert_eq!(reparsed, parsed);
    }

    #[test]
    fn trigger_manifest_entries_parse_inline_sources() {
        let source = r#"
[[triggers]]
id = "ops-fan-in"
handler = "handlers::on_event"
sources = [
  { id = "tick", kind = "cron", provider = "cron", match = { events = ["cron.tick"] }, schedule = "*/5 * * * *", timezone = "UTC" },
  { id = "alerts", kind = "stream", provider = "nats", match = { events = ["alert.received"] }, subject = "alerts.>" },
]
"#;
        let parsed: TriggerTables = toml::from_str(source).expect("trigger tables parse");
        assert_eq!(parsed.triggers.len(), 1);
        assert_eq!(parsed.triggers[0].sources.len(), 2);
        assert_eq!(parsed.triggers[0].sources[1].provider.as_str(), "nats");
        assert_eq!(parsed.triggers[0].sources[1].kind, TriggerKind::Stream);
    }

    #[test]
    fn load_runtime_extensions_ignores_package_triggers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".harn/packages/acme")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[package]
name = "workspace"

[[triggers]]
id = "workspace-trigger"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "worker://workspace-queue"
"#,
        )
        .unwrap();
        fs::write(
            root.join(".harn/packages/acme/harn.toml"),
            r#"
[package]
name = "acme"

[[triggers]]
id = "acme-trigger"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
handler = "worker://acme-queue"
schedule = "0 9 * * *"
timezone = "UTC"
"#,
        )
        .unwrap();
        let harn_file = root.join("main.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();

        let extensions = load_runtime_extensions(&harn_file);
        assert_eq!(extensions.triggers.len(), 1);
        assert_eq!(extensions.triggers[0].id, "workspace-trigger");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_accepts_local_handler_and_when() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[orchestrator.budget]
hourly_cost_usd = 1.0
daily_cost_usd = 5.0

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
tier = "suggest"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
when_budget = { max_cost_usd = 0.001, tokens_max = 500, timeout = "5s" }
handler = "handlers::on_new_issue"
dedupe_key = "event.dedupe_key"
retry = { max = 7, backoff = "svix", retention_days = 7 }
priority = "normal"
budget = { max_cost_usd = 0.002, max_tokens = 250, hourly_cost_usd = 1.0, daily_cost_usd = 5.0, max_autonomous_decisions_per_hour = 25, max_autonomous_decisions_per_day = 100, max_concurrent = 10, on_budget_exhausted = "fail" }
secrets = { signing_secret = "github/webhook-secret" }
filter = "event.kind"
"#,
            Some(
                r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) {
  log(event.kind)
}

pub fn should_handle(event: TriggerEvent) -> Result<bool, string> {
  return Result.Ok(event.provider == "github")
}
"#,
            ),
        );
        let extensions = load_runtime_extensions(&harn_file);
        assert_eq!(
            extensions
                .root_manifest
                .as_ref()
                .and_then(|manifest| manifest.orchestrator.budget.hourly_cost_usd),
            Some(1.0)
        );
        let mut vm = test_vm();
        let collected = collect_manifest_triggers(&mut vm, &extensions)
            .await
            .expect("trigger collection succeeds");
        assert_eq!(collected.len(), 1);
        assert!(matches!(
            &collected[0].handler,
            CollectedTriggerHandler::Local { reference, .. } if reference.raw == "handlers::on_new_issue"
        ));
        assert_eq!(
            collected[0].config.dispatch_priority,
            TriggerDispatchPriority::Normal
        );
        assert_eq!(
            collected[0].config.autonomy_tier,
            harn_vm::AutonomyTier::Suggest
        );
        assert_eq!(
            collected[0]
                .flow_control
                .concurrency
                .as_ref()
                .map(|config| config.max),
            Some(10)
        );
        assert!(collected[0].when.is_some());
        assert_eq!(
            collected[0]
                .config
                .when_budget
                .as_ref()
                .and_then(|budget| budget.tokens_max),
            Some(500)
        );
        assert_eq!(collected[0].config.budget.hourly_cost_usd, Some(1.0));
        assert_eq!(
            collected[0].config.budget.max_autonomous_decisions_per_hour,
            Some(25)
        );
        assert_eq!(
            collected[0].config.budget.max_autonomous_decisions_per_day,
            Some(100)
        );
        assert_eq!(
            collected[0].config.budget.on_budget_exhausted,
            harn_vm::TriggerBudgetExhaustionStrategy::Fail
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_accepts_expression_keyed_flow_control() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-flow-control"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
concurrency = { key = "event.headers.tenant", max = 2 }
throttle = { key = "event.headers.user", period = "1m", max = 30 }
rate_limit = { period = "1h", max = 1000 }
debounce = { key = "event.headers.pr_id", period = "30s" }
singleton = { key = "event.headers.repo" }
priority = { key = "event.headers.tier", order = ["gold", "silver", "bronze"] }
secrets = { signing_secret = "github/webhook-secret" }
"#,
            Some(
                r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) -> string {
  return event.kind
}
"#,
            ),
        );
        let extensions = load_runtime_extensions(&harn_file);
        let mut vm = test_vm();
        let collected = collect_manifest_triggers(&mut vm, &extensions)
            .await
            .expect("trigger collection succeeds");
        assert_eq!(collected.len(), 1);
        let flow = &collected[0].flow_control;
        assert_eq!(
            flow.concurrency
                .as_ref()
                .and_then(|config| config.key.as_ref())
                .map(|expr| expr.raw.as_str()),
            Some("event.headers.tenant")
        );
        assert_eq!(flow.concurrency.as_ref().map(|config| config.max), Some(2));
        assert_eq!(
            flow.throttle
                .as_ref()
                .and_then(|config| config.key.as_ref())
                .map(|expr| expr.raw.as_str()),
            Some("event.headers.user")
        );
        assert_eq!(
            flow.throttle.as_ref().map(|config| config.period),
            Some(std::time::Duration::from_secs(60))
        );
        assert_eq!(flow.throttle.as_ref().map(|config| config.max), Some(30));
        assert!(flow
            .rate_limit
            .as_ref()
            .is_some_and(|config| config.key.is_none()));
        assert_eq!(
            flow.rate_limit.as_ref().map(|config| config.period),
            Some(std::time::Duration::from_secs(60 * 60))
        );
        assert_eq!(
            flow.rate_limit.as_ref().map(|config| config.max),
            Some(1000)
        );
        assert_eq!(
            flow.debounce.as_ref().map(|config| config.key.raw.as_str()),
            Some("event.headers.pr_id")
        );
        assert_eq!(
            flow.debounce.as_ref().map(|config| config.period),
            Some(std::time::Duration::from_secs(30))
        );
        assert_eq!(
            flow.singleton
                .as_ref()
                .and_then(|config| config.key.as_ref())
                .map(|expr| expr.raw.as_str()),
            Some("event.headers.repo")
        );
        assert_eq!(
            flow.priority.as_ref().map(|config| config.key.raw.as_str()),
            Some("event.headers.tier")
        );
        assert_eq!(
            flow.priority.as_ref().map(|config| config.order.clone()),
            Some(vec![
                "gold".to_string(),
                "silver".to_string(),
                "bronze".to_string(),
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_accepts_batch_flow_control() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-batch"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
batch = { key = "event.headers.repo", size = 50, timeout = "30s" }
secrets = { signing_secret = "github/webhook-secret" }
"#,
            Some(
                r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) -> string {
  return event.kind
}
"#,
            ),
        );
        let mut vm = test_vm();
        let collected = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .expect("trigger collection succeeds");
        assert_eq!(collected.len(), 1);
        assert_eq!(
            collected[0]
                .flow_control
                .batch
                .as_ref()
                .and_then(|config| config.key.as_ref())
                .map(|expr| expr.raw.as_str()),
            Some("event.headers.repo")
        );
        assert_eq!(
            collected[0]
                .flow_control
                .batch
                .as_ref()
                .map(|config| config.size),
            Some(50)
        );
        assert_eq!(
            collected[0]
                .flow_control
                .batch
                .as_ref()
                .map(|config| config.timeout),
            Some(std::time::Duration::from_secs(30))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_expands_stream_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "market-fan-in"
handler = "handlers::on_market_event"
when = "handlers::should_handle"
debounce = { key = "event.provider + \":\" + event.kind", period = "2s" }

[[triggers.sources]]
id = "open"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
schedule = "0 14 * * 1-5"
timezone = "America/New_York"

[[triggers.sources]]
id = "quotes"
kind = "stream"
provider = "kafka"
match = { events = ["quote.tick"] }
topic = "quotes"
consumer_group = "harn-market"
window = { mode = "sliding", key = "event.provider_payload.key", size = "5m", every = "1m", max_items = 5000 }
"#,
            Some(
                r#"
import "std/triggers"

pub fn should_handle(event: TriggerEvent) -> bool {
  return event.provider == "cron" || event.provider == "kafka"
}

pub fn on_market_event(event: TriggerEvent) -> string {
  return event.kind
}
"#,
            ),
        );
        let mut vm = test_vm();
        let collected = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .expect("trigger collection succeeds");
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].config.id, "market-fan-in.open");
        assert_eq!(collected[0].config.kind, TriggerKind::Cron);
        assert_eq!(collected[1].config.id, "market-fan-in.quotes");
        assert_eq!(collected[1].config.kind, TriggerKind::Stream);
        assert_eq!(collected[1].config.provider.as_str(), "kafka");
        assert_eq!(
            collected[1]
                .config
                .window
                .as_ref()
                .map(|window| window.mode),
            Some(TriggerStreamWindowMode::Sliding)
        );
        assert_eq!(
            collected[1]
                .flow_control
                .debounce
                .as_ref()
                .map(|config| config.period),
            Some(std::time::Duration::from_secs(2))
        );
        assert!(collected.iter().all(|trigger| trigger.when.is_some()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_missing_trigger_match() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
handler = "handlers::on_new_issue"
secrets = { signing_secret = "github/webhook-secret" }
"#,
            Some(
                r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) -> string {
  return event.kind
}
"#,
            ),
        );
        let error = collect_manifest_triggers(&mut test_vm(), &load_runtime_extensions(&harn_file))
            .await
            .expect_err("missing match should be rejected");
        assert!(error.contains("trigger table missing match"), "{error}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_missing_source_match() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "market-fan-in"
handler = "handlers::on_market_event"

[[triggers.sources]]
id = "quotes"
kind = "stream"
provider = "kafka"
topic = "quotes"
"#,
            Some(
                r#"
import "std/triggers"

pub fn on_market_event(event: TriggerEvent) -> string {
  return event.kind
}
"#,
            ),
        );
        let error = collect_manifest_triggers(&mut test_vm(), &load_runtime_extensions(&harn_file))
            .await
            .expect_err("missing source match should be rejected");
        assert!(
            error.contains("trigger source 'quotes' missing match"),
            "{error}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_accepts_a2a_allow_cleartext() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[[triggers]]
id = "local-a2a"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "a2a://127.0.0.1:8787/triage"
allow_cleartext = true
secrets = { signing_secret = "github/webhook-secret" }
"#,
            None,
        );
        let mut vm = test_vm();
        let collected = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .expect("trigger collection succeeds");
        assert_eq!(collected.len(), 1);
        assert!(matches!(
            &collected[0].handler,
            CollectedTriggerHandler::A2a {
                target,
                allow_cleartext: true,
            } if target == "127.0.0.1:8787/triage"
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_accepts_harn_provider_override() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[[providers]]
id = "echo"
connector = { harn = "./echo_connector.harn" }

[[triggers]]
id = "echo-webhook"
kind = "webhook"
provider = "echo"
path = "/hooks/echo"
match = { path = "/hooks/echo", events = ["echo.received"] }
handler = "worker://echo-queue"
"#,
            None,
        );
        fs::write(
            tmp.path().join("echo_connector.harn"),
            test_harn_connector_source("echo"),
        )
        .unwrap();

        let mut vm = test_vm();
        let collected = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .expect("trigger collection succeeds");
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].config.provider.as_str(), "echo");
        assert_eq!(
            harn_vm::provider_metadata("echo")
                .expect("provider metadata registered")
                .schema_name,
            "EchoEventPayload"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_duplicate_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[[triggers]]
id = "duplicate"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "worker://queue-a"
secrets = { signing_secret = "github/webhook-secret" }

[[triggers]]
id = "duplicate"
kind = "webhook"
provider = "github"
match = { events = ["issues.edited"] }
handler = "worker://queue-b"
secrets = { signing_secret = "github/webhook-secret" }
"#,
            None,
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("duplicate trigger id 'duplicate'"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_unknown_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[[triggers]]
id = "unknown-provider"
kind = "webhook"
provider = "made-up"
match = { events = ["issues.opened"] }
handler = "worker://queue"
"#,
            None,
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("provider 'made-up' is not registered"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_non_bool_allow_cleartext() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[[triggers]]
id = "bad-allow-cleartext-type"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "a2a://127.0.0.1:8787/triage"
allow_cleartext = "yes"
secrets = { signing_secret = "github/webhook-secret" }
"#,
            None,
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("`allow_cleartext` must be a boolean"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_priority_without_concurrency() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "priority-without-concurrency"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
priority = { key = "event.headers.tier", order = ["gold", "silver"] }
secrets = { signing_secret = "github/webhook-secret" }
"#,
            Some(
                r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) -> string {
  return event.kind
}
"#,
            ),
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("priority requires concurrency"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_allow_cleartext_on_non_a2a_handler() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[[triggers]]
id = "bad-allow-cleartext-target"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "worker://queue"
allow_cleartext = true
secrets = { signing_secret = "github/webhook-secret" }
"#,
            None,
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("only valid for `a2a://...` handlers"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_unsupported_provider_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[[triggers]]
id = "bad-kind"
kind = "cron"
provider = "github"
match = { events = ["cron.tick"] }
handler = "worker://queue"
schedule = "0 9 * * *"
timezone = "UTC"
secrets = { signing_secret = "github/webhook-secret" }
"#,
            None,
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("does not support trigger kind 'cron'"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_missing_required_provider_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[[triggers]]
id = "missing-secret"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "worker://queue"
"#,
            None,
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("requires secret 'signing_secret'"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_unresolved_handler() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "missing-handler"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::missing"
secrets = { signing_secret = "github/webhook-secret" }
"#,
            Some(
                r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) {
  log(event.kind)
}
"#,
            ),
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("handler 'handlers::missing' is not exported"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_malformed_cron() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[[triggers]]
id = "bad-cron"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
handler = "worker://queue"
schedule = "not a cron"
timezone = "America/Los_Angeles"
"#,
            None,
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("invalid cron schedule"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_utc_offset_timezone() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[[triggers]]
id = "bad-cron-timezone"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
handler = "worker://queue"
schedule = "0 9 * * *"
timezone = "+02:00"
"#,
            None,
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("use an IANA timezone name"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_invalid_dedupe_expression() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[[triggers]]
id = "bad-dedupe"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "worker://queue"
dedupe_key = "["
secrets = { signing_secret = "github/webhook-secret" }
"#,
            None,
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("dedupe_key"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_zero_retention_days() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[[triggers]]
id = "bad-retention"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "worker://queue"
secrets = { signing_secret = "github/webhook-secret" }
retry = { retention_days = 0 }
"#,
            None,
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(
            error.contains("retry.retention_days"),
            "actual error: {error}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_secret_namespace_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[[triggers]]
id = "bad-secret"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "worker://queue"
secrets = { signing_secret = "slack/webhook-secret" }
"#,
            None,
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("uses namespace 'slack'"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_invalid_when_signature() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "bad-when"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
handler = "worker://queue"
secrets = { signing_secret = "github/webhook-secret" }
"#,
            Some(
                r#"
import "std/triggers"

pub fn should_handle(event: TriggerEvent) -> string {
  return event.kind
}
"#,
            ),
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("must have signature fn(TriggerEvent) -> bool or Result<bool, _>"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_when_budget_without_when() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[[triggers]]
id = "bad-when-budget"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
when_budget = { timeout = "5s" }
handler = "worker://queue"
secrets = { signing_secret = "github/webhook-secret" }
"#,
            None,
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("when_budget requires a when predicate"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collect_manifest_triggers_rejects_invalid_when_budget_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "bad-when-timeout"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
when_budget = { timeout = "soon" }
handler = "worker://queue"
secrets = { signing_secret = "github/webhook-secret" }
"#,
            Some(
                r#"
import "std/triggers"

pub fn should_handle(event: TriggerEvent) -> bool {
  return true
}
"#,
            ),
        );
        let mut vm = test_vm();
        let error = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
            .await
            .unwrap_err();
        assert!(error.contains("when_budget.timeout"));
    }

    #[test]
    fn package_alias_validation_rejects_path_traversal_names() {
        for alias in [
            "../evil",
            "nested/evil",
            "nested\\evil",
            ".",
            "..",
            "bad alias",
        ] {
            assert!(
                validate_package_alias(alias).is_err(),
                "{alias:?} should be rejected"
            );
        }
        validate_package_alias("acme-lib_1.2").expect("ordinary alias should be accepted");
    }

    #[test]
    fn add_package_rejects_aliases_that_escape_packages_dir() {
        let error = normalize_add_request(
            "ignored",
            Some("../evil"),
            None,
            None,
            None,
            None,
            Some("./dep"),
            None,
        )
        .unwrap_err();
        assert!(error.contains("invalid dependency alias"));
    }

    #[test]
    fn rendered_dependency_values_are_toml_escaped() {
        let path = "dep\" \nmalicious = true";
        let line = render_dependency_line(
            "safe",
            &Dependency::Table(DepTable {
                git: None,
                tag: None,
                rev: None,
                branch: None,
                path: Some(path.to_string()),
                package: None,
            }),
        )
        .expect("dependency line");
        let parsed: Manifest = toml::from_str(&format!("[dependencies]\n{line}\n")).unwrap();
        assert_eq!(parsed.dependencies.len(), 1);
        assert_eq!(
            parsed
                .dependencies
                .get("safe")
                .and_then(Dependency::local_path),
            Some(path)
        );
    }

    #[test]
    fn materialization_rejects_lock_alias_path_traversal_before_removing_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep");
        fs::create_dir_all(&dep).unwrap();
        fs::write(dep.join("lib.harn"), "pub fn dep() { 1 }\n").unwrap();
        let victim = tmp.path().join("victim");
        fs::create_dir_all(&victim).unwrap();
        fs::write(victim.join("keep.txt"), "keep").unwrap();

        let manifest: Manifest = toml::from_str("[package]\nname = \"root\"\n").unwrap();
        let ctx = ManifestContext {
            manifest,
            dir: tmp.path().to_path_buf(),
        };
        let lock = LockFile {
            version: LOCK_FILE_VERSION,
            packages: vec![LockEntry {
                name: "../victim".to_string(),
                source: path_source_uri(&dep).unwrap(),
                rev_request: None,
                commit: None,
                content_hash: None,
            }],
        };

        let error = materialize_dependencies_from_lock(&ctx, &lock, None, false).unwrap_err();
        assert!(error.contains("invalid dependency alias"));
        assert!(
            victim.join("keep.txt").exists(),
            "malicious alias should not remove paths outside .harn/packages"
        );
    }
}
