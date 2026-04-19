use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::{fs, process};

use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

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
    pub kind: TriggerKind,
    pub provider: harn_vm::ProviderId,
    #[serde(rename = "match")]
    pub match_: TriggerMatchExpr,
    #[serde(default)]
    pub when: Option<String>,
    pub handler: String,
    #[serde(default)]
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub retry: TriggerRetrySpec,
    #[serde(default)]
    pub priority: TriggerPriority,
    #[serde(default)]
    pub budget: TriggerBudgetSpec,
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
pub enum TriggerPriority {
    High,
    #[default]
    Normal,
    Low,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerBudgetSpec {
    #[serde(default)]
    pub daily_cost_usd: Option<f64>,
    #[serde(default)]
    pub max_concurrent: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerHandlerUri {
    Local(TriggerFunctionRef),
    A2a { target: String },
    Worker { queue: String },
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
    pub path: Option<String>,
}

impl Dependency {
    fn git_url(&self) -> Option<&str> {
        match self {
            Dependency::Table(t) => t.git.as_deref(),
            Dependency::Path(_) => None,
        }
    }

    fn tag(&self) -> Option<&str> {
        match self {
            Dependency::Table(t) => t.tag.as_deref(),
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

#[derive(Debug, Default, Clone)]
pub struct RuntimeExtensions {
    pub root_manifest: Option<Manifest>,
    pub llm: Option<harn_vm::llm_config::ProvidersConfig>,
    pub capabilities: Option<harn_vm::llm::capabilities::CapabilitiesFile>,
    pub hooks: Vec<ResolvedHookConfig>,
    pub triggers: Vec<ResolvedTriggerConfig>,
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
    pub match_: TriggerMatchExpr,
    pub when: Option<String>,
    pub handler: String,
    pub dedupe_key: Option<String>,
    pub retry: TriggerRetrySpec,
    pub priority: TriggerPriority,
    pub budget: TriggerBudgetSpec,
    pub secrets: BTreeMap<String, String>,
    pub filter: Option<String>,
    pub kind_specific: BTreeMap<String, toml::Value>,
    pub manifest_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub package_name: Option<String>,
    pub exports: HashMap<String, String>,
    pub table_index: usize,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Collected trigger bindings are validated now and consumed by follow-up trigger dispatcher work.
pub struct CollectedManifestTrigger {
    pub config: ResolvedTriggerConfig,
    pub handler: CollectedTriggerHandler,
    pub when: Option<CollectedTriggerPredicate>,
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

#[derive(Debug, Clone)]
struct LocatedManifest {
    manifest: Manifest,
    dir: PathBuf,
}

type ManifestModuleCacheKey = (PathBuf, Option<String>, Option<String>);
type ManifestModuleExports = BTreeMap<String, Rc<harn_vm::VmClosure>>;

#[derive(Debug, Default)]
struct LockFile {
    entries: HashMap<String, LockEntry>,
}

#[derive(Debug, Clone)]
struct LockEntry {
    git: Option<String>,
    tag: Option<String>,
    commit: Option<String>,
    path: Option<String>,
}

impl LockFile {
    fn load(path: &Path) -> Self {
        let content = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return Self::default(),
        };

        let mut entries = HashMap::new();
        let mut current_name: Option<String> = None;
        let mut current = LockEntry {
            git: None,
            tag: None,
            commit: None,
            path: None,
        };

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("[[package]]") {
                if let Some(name) = current_name.take() {
                    entries.insert(name, current.clone());
                }
                current = LockEntry {
                    git: None,
                    tag: None,
                    commit: None,
                    path: None,
                };
            } else if let Some((key, value)) = trimmed.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"');
                match key {
                    "name" => current_name = Some(value.to_string()),
                    "git" => current.git = Some(value.to_string()),
                    "tag" => current.tag = Some(value.to_string()),
                    "commit" => current.commit = Some(value.to_string()),
                    "path" => current.path = Some(value.to_string()),
                    _ => {}
                }
            }
        }
        if let Some(name) = current_name {
            entries.insert(name, current);
        }

        LockFile { entries }
    }

    fn save(&self, path: &Path) {
        let mut out =
            String::from("# This file is auto-generated by `harn install`. Do not edit.\n\n");
        let mut names: Vec<&String> = self.entries.keys().collect();
        names.sort();
        for name in names {
            let entry = &self.entries[name];
            out.push_str("[[package]]\n");
            out.push_str(&format!("name = \"{name}\"\n"));
            if let Some(git) = &entry.git {
                out.push_str(&format!("git = \"{git}\"\n"));
            }
            if let Some(tag) = &entry.tag {
                out.push_str(&format!("tag = \"{tag}\"\n"));
            }
            if let Some(commit) = &entry.commit {
                out.push_str(&format!("commit = \"{commit}\"\n"));
            }
            if let Some(path) = &entry.path {
                out.push_str(&format!("path = \"{path}\"\n"));
            }
            out.push('\n');
        }
        if let Err(e) = fs::write(path, &out) {
            eprintln!("Failed to write lock file: {e}");
        }
    }
}

pub fn read_manifest() -> Manifest {
    let content = match fs::read_to_string(MANIFEST) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("No harn.toml found in current directory.");
            eprintln!("Create one with `harn init` or manually.");
            process::exit(1);
        }
    };
    match toml::from_str::<Manifest>(&content) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Failed to parse harn.toml: {e}");
            process::exit(1);
        }
    }
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

fn collect_package_manifests(packages_dir: &Path) -> Vec<LocatedManifest> {
    let mut manifests = Vec::new();
    let Ok(entries) = fs::read_dir(packages_dir) else {
        return manifests;
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let manifest_path = dir.join(MANIFEST);
        let Ok(content) = fs::read_to_string(&manifest_path) else {
            continue;
        };
        match toml::from_str::<Manifest>(&content) {
            Ok(manifest) => manifests.push(LocatedManifest { manifest, dir }),
            Err(e) => eprintln!("warning: failed to parse {}: {e}", manifest_path.display()),
        }
    }
    manifests
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
        .map(|(table_index, trigger)| ResolvedTriggerConfig {
            id: trigger.id.clone(),
            kind: trigger.kind,
            provider: trigger.provider.clone(),
            match_: trigger.match_.clone(),
            when: trigger.when.clone(),
            handler: trigger.handler.clone(),
            dedupe_key: trigger.dedupe_key.clone(),
            retry: trigger.retry.clone(),
            priority: trigger.priority,
            budget: trigger.budget.clone(),
            secrets: trigger.secrets.clone(),
            filter: trigger.filter.clone(),
            kind_specific: trigger.kind_specific.clone(),
            manifest_dir: manifest_dir.to_path_buf(),
            manifest_path: manifest_path.clone(),
            package_name: package_name.clone(),
            exports: manifest.exports.clone(),
            table_index,
        })
        .collect()
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
        return Ok(TriggerHandlerUri::A2a {
            target: target.to_string(),
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

fn validate_static_trigger_config(trigger: &ResolvedTriggerConfig) -> Result<(), String> {
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
    if let Some(daily_cost_usd) = trigger.budget.daily_cost_usd {
        if daily_cost_usd.is_sign_negative() {
            return Err(trigger_error(
                trigger,
                "budget.daily_cost_usd must be greater than or equal to 0",
            ));
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

fn is_trigger_event_type(ty: &harn_parser::TypeExpr) -> bool {
    matches!(ty, harn_parser::TypeExpr::Named(name) if name == "TriggerEvent")
}

fn is_bool_type(ty: &harn_parser::TypeExpr) -> bool {
    matches!(ty, harn_parser::TypeExpr::Named(name) if name == "bool")
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
/// merge their runtime extensions. Installed packages load first; the root
/// project manifest wins on conflicts.
pub fn load_runtime_extensions(anchor: &Path) -> RuntimeExtensions {
    let Some((root_manifest, manifest_dir)) = find_nearest_manifest(anchor) else {
        return RuntimeExtensions::default();
    };

    let mut llm = harn_vm::llm_config::ProvidersConfig::default();
    let mut capabilities = harn_vm::llm::capabilities::CapabilitiesFile::default();
    let mut hooks = Vec::new();
    let mut triggers = Vec::new();

    for located in collect_package_manifests(&manifest_dir.join(PKG_DIR)) {
        llm.merge_from(&located.manifest.llm);
        if let Some(file) = manifest_capabilities(&located.manifest) {
            merge_capability_overrides(&mut capabilities, file);
        }
        hooks.extend(resolved_hooks_from_manifest(
            &located.manifest,
            &located.dir,
        ));
        triggers.extend(resolved_triggers_from_manifest(
            &located.manifest,
            &located.dir,
        ));
    }

    llm.merge_from(&root_manifest.llm);
    if let Some(file) = manifest_capabilities(&root_manifest) {
        merge_capability_overrides(&mut capabilities, file);
    }
    hooks.extend(resolved_hooks_from_manifest(&root_manifest, &manifest_dir));
    triggers.extend(resolved_triggers_from_manifest(
        &root_manifest,
        &manifest_dir,
    ));

    RuntimeExtensions {
        root_manifest: Some(root_manifest),
        llm: (!llm.is_empty()).then_some(llm),
        capabilities: (!is_empty_capabilities(&capabilities)).then_some(capabilities),
        hooks,
        triggers,
    }
}

/// Install merged runtime extensions on the current thread.
pub fn install_runtime_extensions(extensions: &RuntimeExtensions) {
    harn_vm::llm_config::set_user_overrides(extensions.llm.clone());
    harn_vm::llm::capabilities::set_user_overrides(extensions.capabilities.clone());
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
            TriggerHandlerUri::A2a { target } => CollectedTriggerHandler::A2a { target },
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
                .is_none_or(|return_type| !is_bool_type(return_type))
            {
                return Err(trigger_error(
                    trigger,
                    format!(
                        "when predicate '{}' must have signature fn(TriggerEvent) -> bool",
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

        collected.push(CollectedManifestTrigger {
            config: trigger.clone(),
            handler: collected_handler,
            when: collected_when,
        });
    }

    Ok(collected)
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

pub fn manifest_trigger_binding_spec(
    trigger: CollectedManifestTrigger,
) -> harn_vm::TriggerBindingSpec {
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
        CollectedTriggerHandler::A2a { target } => (
            harn_vm::TriggerHandlerSpec::A2a {
                target: target.clone(),
            },
            serde_json::json!({
                "kind": "a2a",
                "target": target,
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
    let id = config.id.clone();
    let kind = trigger_kind_label(config.kind).to_string();
    let provider = config.provider.clone();
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
    let max_concurrent = config.budget.max_concurrent;
    let manifest_path = Some(config.manifest_path.clone());
    let package_name = config.package_name.clone();

    let fingerprint = serde_json::to_string(&serde_json::json!({
        "id": &id,
        "kind": &kind,
        "provider": provider.as_str(),
        "match": config.match_,
        "when": when_raw,
        "handler": handler_descriptor,
        "dedupe_key": &dedupe_key,
        "retry": config.retry,
        "priority": config.priority,
        "budget": config.budget,
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
        handler,
        when,
        retry,
        match_events,
        dedupe_key,
        filter,
        dedupe_retention_days,
        daily_cost_usd,
        max_concurrent,
        manifest_path,
        package_name,
        definition_fingerprint: fingerprint,
    }
}

pub async fn install_manifest_triggers(
    vm: &mut harn_vm::Vm,
    extensions: &RuntimeExtensions,
) -> Result<(), String> {
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
            if let Ok(content) = fs::read_to_string(&candidate) {
                match toml::from_str::<Manifest>(&content) {
                    Ok(manifest) => return Some((manifest, dir)),
                    Err(e) => {
                        eprintln!("warning: failed to parse {}: {e}", candidate.display());
                        return None;
                    }
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

/// `harn install` — install all dependencies from harn.toml.
pub fn install_packages() {
    let manifest = read_manifest();

    if manifest.dependencies.is_empty() {
        println!("No dependencies to install.");
        return;
    }

    let has_git_deps = manifest
        .dependencies
        .values()
        .any(|d| d.git_url().is_some());
    if has_git_deps
        && process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
    {
        eprintln!("Error: git is required to install git dependencies but was not found.");
        eprintln!("Install git and ensure it's in your PATH.");
        process::exit(1);
    }

    let pkg_dir = PathBuf::from(PKG_DIR);
    if let Err(e) = fs::create_dir_all(&pkg_dir) {
        eprintln!("Failed to create {PKG_DIR}: {e}");
        process::exit(1);
    }

    let mut lock = LockFile::load(Path::new(LOCK_FILE));
    let mut installed = 0;
    let mut visiting = HashSet::new();

    for (name, dep) in &manifest.dependencies {
        install_one(
            name,
            dep,
            &pkg_dir,
            &mut lock,
            &mut visiting,
            &mut installed,
        );
    }

    lock.save(Path::new(LOCK_FILE));
    println!("\nInstalled {installed} package(s) to {PKG_DIR}/");
}

fn install_one(
    name: &str,
    dep: &Dependency,
    pkg_dir: &Path,
    lock: &mut LockFile,
    visiting: &mut HashSet<String>,
    installed: &mut usize,
) {
    if !visiting.insert(name.to_string()) {
        eprintln!("  warning: circular dependency detected for '{name}', skipping");
        return;
    }

    let dest = pkg_dir.join(name);

    if let Some(git_url) = dep.git_url() {
        install_git_dep(name, git_url, dep.tag(), &dest, lock);
        *installed += 1;
    } else if let Some(local_path) = dep.local_path() {
        install_local_dep(name, local_path, &dest);
        *installed += 1;
        lock.entries.insert(
            name.to_string(),
            LockEntry {
                git: None,
                tag: None,
                commit: None,
                path: Some(local_path.to_string()),
            },
        );
    } else {
        eprintln!("  {name}: no git or path specified, skipping");
        visiting.remove(name);
        return;
    }

    let sub_manifest_path = dest.join("harn.toml");
    if sub_manifest_path.exists() {
        if let Ok(content) = fs::read_to_string(&sub_manifest_path) {
            if let Ok(sub_manifest) = toml::from_str::<Manifest>(&content) {
                for (sub_name, sub_dep) in &sub_manifest.dependencies {
                    let sub_dest = pkg_dir.join(sub_name);
                    if !sub_dest.exists() {
                        install_one(sub_name, sub_dep, pkg_dir, lock, visiting, installed);
                    }
                }
            }
        }
    }

    visiting.remove(name);
}

fn install_git_dep(name: &str, git_url: &str, tag: Option<&str>, dest: &Path, lock: &mut LockFile) {
    if let Some(entry) = lock.entries.get(name) {
        if entry.git.as_deref() == Some(git_url)
            && entry.tag.as_deref() == tag
            && entry.commit.is_some()
            && dest.exists()
        {
            println!("  {name}: up to date (locked)");
            return;
        }
    }

    if dest.exists() {
        println!("  updating {name} from {git_url}");
        let _ = fs::remove_dir_all(dest);
    } else {
        println!("  installing {name} from {git_url}");
    }

    let mut cmd = process::Command::new("git");
    cmd.args(["clone", "--depth", "1"]);
    if let Some(t) = tag {
        cmd.args(["--branch", t]);
    }
    cmd.arg(git_url);
    cmd.arg(dest.as_os_str());
    cmd.stdout(process::Stdio::null());
    cmd.stderr(process::Stdio::piped());

    match cmd.output() {
        Ok(output) if output.status.success() => {
            let commit = get_git_commit(dest);
            // Drop .git to save disk space.
            let _ = fs::remove_dir_all(dest.join(".git"));
            lock.entries.insert(
                name.to_string(),
                LockEntry {
                    git: Some(git_url.to_string()),
                    tag: tag.map(|t| t.to_string()),
                    commit,
                    path: None,
                },
            );
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("  failed to clone {name}: {stderr}");
        }
        Err(e) => {
            eprintln!("  failed to run git for {name}: {e}");
            eprintln!("  hint: make sure git is installed and in PATH");
        }
    }
}

fn get_git_commit(repo_dir: &Path) -> Option<String> {
    let output = process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

fn install_local_dep(name: &str, source_path: &str, dest: &Path) {
    let source = Path::new(source_path);

    if source.is_dir() {
        if dest.exists() {
            println!("  updating {name} from {source_path}");
            let _ = fs::remove_dir_all(dest);
        } else {
            println!("  installing {name} from {source_path}");
        }
        if let Err(e) = copy_dir_recursive(source, dest) {
            eprintln!("  failed to install {name}: {e}");
        }
    } else if source.is_file() {
        let dest_file = dest.with_extension("harn");
        if dest_file.exists() {
            println!("  updating {name} from {source_path}");
        } else {
            println!("  installing {name} from {source_path}");
        }
        if let Some(parent) = dest_file.parent() {
            fs::create_dir_all(parent).ok();
        }
        if let Err(e) = fs::copy(source, &dest_file) {
            eprintln!("  failed to install {name}: {e}");
        }
    } else {
        let harn_source = PathBuf::from(format!("{source_path}.harn"));
        if harn_source.exists() {
            let dest_file = dest.with_extension("harn");
            println!("  installing {name} from {}", harn_source.display());
            if let Some(parent) = dest_file.parent() {
                fs::create_dir_all(parent).ok();
            }
            if let Err(e) = fs::copy(&harn_source, &dest_file) {
                eprintln!("  failed to install {name}: {e}");
            }
        } else {
            eprintln!("  package source not found: {source_path}");
        }
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

/// `harn add <name> --git <url> [--tag <tag>]` — add a dependency to harn.toml.
pub fn add_package(name: &str, git_url: Option<&str>, tag: Option<&str>, local_path: Option<&str>) {
    if git_url.is_none() && local_path.is_none() {
        eprintln!("Must specify --git <url> or --path <local-path>");
        process::exit(1);
    }

    let manifest_path = Path::new(MANIFEST);
    let mut content = if manifest_path.exists() {
        fs::read_to_string(manifest_path).unwrap_or_default()
    } else {
        "[package]\nname = \"my-project\"\nversion = \"0.1.0\"\n".to_string()
    };

    if !content.contains("[dependencies]") {
        content.push_str("\n[dependencies]\n");
    }

    let dep_line = if let Some(url) = git_url {
        if let Some(t) = tag {
            format!("{name} = {{ git = \"{url}\", tag = \"{t}\" }}")
        } else {
            format!("{name} = {{ git = \"{url}\" }}")
        }
    } else {
        let p = local_path.unwrap();
        format!("{name} = {{ path = \"{p}\" }}")
    };

    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let mut replaced = false;
    for line in &mut lines {
        if line.starts_with(name) && line.contains('=') {
            // Avoid prefix matches (e.g. `foo_bar` when looking for `foo`).
            let before_eq = line.split('=').next().unwrap_or("").trim();
            if before_eq == name {
                *line = dep_line.clone();
                replaced = true;
                break;
            }
        }
    }

    if !replaced {
        let dep_idx = lines
            .iter()
            .position(|l| l.trim() == "[dependencies]")
            .unwrap_or_else(|| {
                lines.push("[dependencies]".to_string());
                lines.len() - 1
            });
        lines.insert(dep_idx + 1, dep_line);
    }

    let new_content = lines.join("\n") + "\n";
    if let Err(e) = fs::write(manifest_path, &new_content) {
        eprintln!("Failed to write harn.toml: {e}");
        process::exit(1);
    }

    println!("Added {name} to harn.toml");
    println!("Run `harn install` to fetch the package.");
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

    Some(ResolvedSkillsConfig {
        config: manifest.skills,
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
    fn load_runtime_extensions_merges_package_and_root_llm_config() {
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
        assert!(llm.providers.contains_key("acme"));
        assert!(llm.providers.contains_key("project"));
        assert!(llm.aliases.contains_key("acme-fast"));
        assert!(llm.aliases.contains_key("project-fast"));
    }

    #[test]
    fn load_runtime_extensions_collects_manifest_hooks_in_order() {
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
        assert_eq!(extensions.hooks.len(), 2);
        assert_eq!(extensions.hooks[0].handler, "acme::audit_edit");
        assert_eq!(extensions.hooks[1].handler, "workspace::after_read");
    }

    #[test]
    fn trigger_manifest_entries_round_trip_through_toml() {
        let source = r#"
[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
handler = "handlers::on_new_issue"
dedupe_key = "event.dedupe_key"
retry = { max = 7, backoff = "svix", retention_days = 7 }
priority = "high"
budget = { daily_cost_usd = 5.0, max_concurrent = 10 }
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
    fn load_runtime_extensions_collects_manifest_triggers_in_order() {
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
        assert_eq!(extensions.triggers.len(), 2);
        assert_eq!(extensions.triggers[0].id, "acme-trigger");
        assert_eq!(extensions.triggers[1].id, "workspace-trigger");
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

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
handler = "handlers::on_new_issue"
dedupe_key = "event.dedupe_key"
retry = { max = 7, backoff = "svix", retention_days = 7 }
priority = "normal"
budget = { daily_cost_usd = 5.0, max_concurrent = 10 }
secrets = { signing_secret = "github/webhook-secret" }
filter = "event.kind"
"#,
            Some(
                r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) {
  log(event.kind)
}

pub fn should_handle(event: TriggerEvent) -> bool {
  return event.provider == "github"
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
        assert!(matches!(
            &collected[0].handler,
            CollectedTriggerHandler::Local { reference, .. } if reference.raw == "handlers::on_new_issue"
        ));
        assert_eq!(collected[0].config.priority, TriggerPriority::Normal);
        assert!(collected[0].when.is_some());
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
        assert!(error.contains("must have signature fn(TriggerEvent) -> bool"));
    }
}
