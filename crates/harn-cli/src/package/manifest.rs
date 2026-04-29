use super::*;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
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

pub(crate) fn default_orchestrator_drain_max_items() -> usize {
    1024
}

pub(crate) fn default_orchestrator_drain_deadline_seconds() -> u64 {
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

pub(crate) fn default_orchestrator_pump_max_outstanding() -> usize {
    64
}

#[derive(Debug, Clone, Deserialize)]
pub struct HookConfig {
    pub event: harn_vm::orchestration::HookEvent,
    #[serde(default = "default_hook_pattern")]
    pub pattern: String,
    pub handler: String,
}

pub(crate) fn default_hook_pattern() -> String {
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
    #[serde(default, alias = "dlq-alerts")]
    pub dlq_alerts: Vec<TriggerDlqAlertManifestSpec>,
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

pub(crate) fn default_trigger_retention_days() -> u32 {
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
pub struct TriggerDlqAlertManifestSpec {
    #[serde(default)]
    pub destinations: Vec<TriggerDlqAlertDestination>,
    #[serde(default)]
    pub threshold: TriggerDlqAlertThreshold,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TriggerDlqAlertThreshold {
    #[serde(default, alias = "entries-in-1h")]
    pub entries_in_1h: Option<u32>,
    #[serde(default, alias = "percent-of-dispatches")]
    pub percent_of_dispatches: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TriggerDlqAlertDestination {
    Slack {
        channel: String,
        #[serde(default)]
        webhook_url_env: Option<String>,
    },
    Email {
        address: String,
    },
    Webhook {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

impl TriggerDlqAlertDestination {
    pub fn label(&self) -> String {
        match self {
            Self::Slack { channel, .. } => format!("slack:{channel}"),
            Self::Email { address } => format!("email:{address}"),
            Self::Webhook { url, .. } => format!("webhook:{url}"),
        }
    }
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
    #[serde(default)]
    pub evals: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default, alias = "harn_version", alias = "harn_version_range")]
    pub harn: Option<String>,
    #[serde(default)]
    pub docs_url: Option<String>,
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
    pub(crate) fn git_url(&self) -> Option<&str> {
        match self {
            Dependency::Table(t) => t.git.as_deref(),
            Dependency::Path(_) => None,
        }
    }

    pub(crate) fn rev(&self) -> Option<&str> {
        match self {
            Dependency::Table(t) => t.rev.as_deref().or(t.tag.as_deref()),
            Dependency::Path(_) => None,
        }
    }

    pub(crate) fn branch(&self) -> Option<&str> {
        match self {
            Dependency::Table(t) => t.branch.as_deref(),
            Dependency::Path(_) => None,
        }
    }

    pub(crate) fn local_path(&self) -> Option<&str> {
        match self {
            Dependency::Table(t) => t.path.as_deref(),
            Dependency::Path(p) => Some(p.as_str()),
        }
    }
}

pub(crate) fn validate_package_alias(alias: &str) -> Result<(), String> {
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

pub(crate) fn toml_string_literal(value: &str) -> Result<String, String> {
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
    #[serde(default)]
    pub oauth: Option<ProviderOAuthManifest>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConnectorManifest {
    #[serde(default)]
    pub harn: Option<String>,
    #[serde(default)]
    pub rust: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ProviderOAuthManifest {
    #[serde(default, alias = "auth_url", alias = "authorization-endpoint")]
    pub authorization_endpoint: Option<String>,
    #[serde(default, alias = "token_url", alias = "token-endpoint")]
    pub token_endpoint: Option<String>,
    #[serde(default, alias = "registration_url", alias = "registration-endpoint")]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default, alias = "scope")]
    pub scopes: Option<String>,
    #[serde(default, alias = "client-id")]
    pub client_id: Option<String>,
    #[serde(default, alias = "client-secret")]
    pub client_secret: Option<String>,
    #[serde(default, alias = "token_auth_method", alias = "token-auth-method")]
    pub token_endpoint_auth_method: Option<String>,
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
    pub expect_dedupe_key: Option<String>,
    #[serde(default)]
    pub expect_signature_state: Option<String>,
    #[serde(default)]
    pub expect_payload_contains: Option<toml::Value>,
    #[serde(default)]
    pub expect_response_status: Option<u16>,
    #[serde(default)]
    pub expect_response_body: Option<toml::Value>,
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
    pub oauth: Option<ProviderOAuthManifest>,
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

pub(crate) type ManifestModuleCacheKey = (PathBuf, Option<String>, Option<String>);
pub(crate) type ManifestModuleExports = BTreeMap<String, Rc<harn_vm::VmClosure>>;

static MANIFEST_PROVIDER_SCHEMA_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub(crate) async fn lock_manifest_provider_schemas() -> tokio::sync::MutexGuard<'static, ()> {
    MANIFEST_PROVIDER_SCHEMA_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

pub(crate) fn read_manifest_from_path(path: &Path) -> Result<Manifest, String> {
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

pub(crate) fn write_manifest_content(path: &Path, content: &str) -> Result<(), String> {
    fs::write(path, content).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

pub(crate) fn absolutize_check_config_paths(
    mut config: CheckConfig,
    manifest_dir: &Path,
) -> CheckConfig {
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
pub(crate) fn find_nearest_manifest(start: &Path) -> Option<(Manifest, PathBuf)> {
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

pub fn load_package_eval_pack_paths(anchor: Option<&Path>) -> Result<Vec<PathBuf>, String> {
    let anchor = anchor
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let Some((manifest, dir)) = find_nearest_manifest(&anchor) else {
        return Err("no harn.toml found for package eval discovery".to_string());
    };

    let declared = manifest
        .package
        .as_ref()
        .map(|package| package.evals.clone())
        .unwrap_or_default();
    let mut paths = if declared.is_empty() {
        let default_pack = dir.join("harn.eval.toml");
        if default_pack.is_file() {
            vec![default_pack]
        } else {
            Vec::new()
        }
    } else {
        declared
            .iter()
            .map(|entry| {
                let path = PathBuf::from(entry);
                if path.is_absolute() {
                    path
                } else {
                    dir.join(path)
                }
            })
            .collect()
    };
    paths.sort();
    if paths.is_empty() {
        return Err(
            "package declares no eval packs; add [package].evals or harn.eval.toml".to_string(),
        );
    }
    for path in &paths {
        if !path.is_file() {
            return Err(format!("eval pack does not exist: {}", path.display()));
        }
    }
    Ok(paths)
}

#[derive(Debug, Clone)]
pub(crate) struct ManifestContext {
    pub(crate) manifest: Manifest,
    pub(crate) dir: PathBuf,
}

impl ManifestContext {
    pub(crate) fn manifest_path(&self) -> PathBuf {
        self.dir.join(MANIFEST)
    }

    pub(crate) fn lock_path(&self) -> PathBuf {
        self.dir.join(LOCK_FILE)
    }

    pub(crate) fn packages_dir(&self) -> PathBuf {
        self.dir.join(PKG_DIR)
    }
}

pub(crate) fn load_current_manifest_context() -> Result<ManifestContext, String> {
    let dir = std::env::current_dir().map_err(|error| format!("failed to read cwd: {error}"))?;
    let manifest_path = dir.join(MANIFEST);
    let manifest = read_manifest_from_path(&manifest_path)?;
    Ok(ManifestContext { manifest, dir })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_eval_pack_paths_use_package_manifest_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("evals")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
    [package]
    name = "demo"
    version = "0.1.0"
    evals = ["evals/webhook.toml"]
    "#,
        )
        .unwrap();
        fs::write(
            root.join("evals/webhook.toml"),
            "version = 1\n[[cases]]\nrun = \"run.json\"\n",
        )
        .unwrap();

        let paths = load_package_eval_pack_paths(Some(&root.join("src/main.harn"))).unwrap();

        assert_eq!(paths, vec![root.join("evals/webhook.toml")]);
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
}
