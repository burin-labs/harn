use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use harn_ir::{CallClassification, Capability, LiteralValue, NodeSemantics};
use harn_parser::{Node, SNode};
use serde::Serialize;

use crate::cli::RoutesArgs;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;
use crate::json_envelope::{to_string_pretty, JsonEnvelope};
use crate::package::{
    self, parse_duration_millis, parse_local_trigger_ref, parse_trigger_handler_uri,
    trigger_kind_label, ResolvedTriggerConfig, RuntimeExtensions, TriggerFunctionRef,
    TriggerHandlerUri, TriggerKind,
};

const ROUTES_SCHEMA_VERSION: u32 = 1;

/// Env var the embedded `cli/routes.harn` script reads to pick up the
/// pre-serialised inventory the Rust shim derived from the on-disk
/// manifest cache and IR analyser. Renaming requires updating both the
/// shim and the script in lockstep.
const ROUTES_VIEW_ENV: &str = "HARN_ROUTES_VIEW_JSON";

/// Serialises the dispatch path so concurrent in-process callers don't
/// race on the global env vars the shim sets. Matches the lock pattern
/// in `commands/models/list.rs`.
static DISPATCH_ROUTES_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RoutesReport {
    pub triggers: Vec<RouteTrigger>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RouteTrigger {
    pub id: String,
    pub kind: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub module: String,
    pub handler: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    pub requires_capabilities: Vec<String>,
    pub budgets: RouteBudgets,
    pub vendor_locked: bool,
    pub framework_overhead_tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct RouteBudgets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daily_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hourly_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<u32>,
}

#[derive(Debug)]
struct ParsedModule {
    path: PathBuf,
    report: harn_ir::AnalysisReport,
    vendor_locked: bool,
}

#[derive(Debug, Default)]
struct ModuleCache {
    modules: BTreeMap<PathBuf, ParsedModule>,
}

#[derive(Debug, Default)]
struct LocalRouteAnalysis {
    module: Option<String>,
    capabilities: BTreeSet<String>,
    vendor_locked: bool,
    framework_overhead_tokens: u64,
}

/// Run `harn routes`. Rust owns manifest-cache and IR extraction; the
/// embedded `cli/routes.harn` script owns rendering.
///
/// The inventory extraction itself (manifest cache + IR analyser) stays
/// in Rust — porting it would require a new `harness.modules.compile_view`
/// host capability. On extraction failure, the shim renders the error
/// envelope / stderr line directly because no view exists to pass into
/// the script.
pub(crate) async fn run(args: RoutesArgs) -> i32 {
    let report = match analyze_routes(&args.root).await {
        Ok(report) => report,
        Err(error) => {
            if args.json {
                let envelope: JsonEnvelope<RoutesReport> =
                    JsonEnvelope::err(ROUTES_SCHEMA_VERSION, "routes_error", error);
                println!("{}", to_string_pretty(&envelope));
            } else {
                eprintln!("error: {error}");
            }
            return 1;
        }
    };

    run_dispatch(&report, args.json).await
}

async fn run_dispatch(report: &RoutesReport, json: bool) -> i32 {
    let view_json = match serde_json::to_string(report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("internal error: failed to serialise routes view: {error}");
            return 1;
        }
    };
    let _guard = DISPATCH_ROUTES_LOCK.lock().await;
    let _view = ScopedEnvVar::set(ROUTES_VIEW_ENV, &view_json);
    let outcome = dispatch::run_embedded_script("routes", Vec::new(), json).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    outcome.exit_code
}

async fn analyze_routes(root: &Path) -> Result<RoutesReport, String> {
    let extensions =
        package::try_load_runtime_extensions(root).map_err(|error| error.to_string())?;
    let Some(manifest_dir) = extensions.root_manifest_dir.as_deref() else {
        return Err(format!("no harn.toml found from {}", root.display()));
    };
    let manifest_dir = manifest_dir
        .canonicalize()
        .unwrap_or_else(|_| manifest_dir.to_path_buf());

    validate_route_inputs(&extensions).await?;

    let mut cache = ModuleCache::default();
    let mut triggers = Vec::new();
    for trigger in &extensions.triggers {
        triggers.push(describe_trigger(trigger, &manifest_dir, &mut cache)?);
    }
    triggers.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(RoutesReport { triggers })
}

async fn validate_route_inputs(extensions: &RuntimeExtensions) -> Result<(), String> {
    let _guard = package::lock_manifest_provider_schemas().await;
    let provider_catalog = package::build_manifest_provider_catalog(extensions)
        .await
        .map_err(|error| error.to_string())?;
    package::validate_orchestrator_budget(extensions.root_manifest.as_ref())
        .map_err(|error| error.to_string())?;
    package::validate_static_trigger_configs(&extensions.triggers, &provider_catalog)
        .map_err(|error| error.to_string())
}

fn describe_trigger(
    trigger: &ResolvedTriggerConfig,
    manifest_dir: &Path,
    cache: &mut ModuleCache,
) -> Result<RouteTrigger, String> {
    let handler_uri = parse_trigger_handler_uri(trigger).map_err(|error| error.to_string())?;
    use TriggerHandlerUri::*;
    let mut capabilities = BTreeSet::new();
    let mut module = None;
    let mut vendor_locked = false;
    let mut framework_overhead_tokens = 0u64;
    let (handler, dispatch_capability) = match &handler_uri {
        Local(reference) => {
            let local = analyze_local_function(trigger, reference, manifest_dir, cache)?;
            module = local.module;
            capabilities.extend(local.capabilities);
            vendor_locked |= local.vendor_locked;
            framework_overhead_tokens =
                framework_overhead_tokens.saturating_add(local.framework_overhead_tokens);
            (reference.function_name.clone(), None)
        }
        A2a { target, .. } => (format!("a2a://{target}"), Some("worker.dispatch")),
        Worker { queue } => (format!("worker://{queue}"), Some("worker.dispatch")),
        Persona { name } => (format!("persona://{name}"), Some("persona.dispatch")),
        EvalPack { target } => (format!("eval_pack://{target}"), Some("eval.dispatch")),
    };
    if let Some(capability) = dispatch_capability {
        capabilities.insert(capability.to_string());
    }

    if let Some(when_raw) = trigger.when.as_deref() {
        let reference = parse_local_trigger_ref(when_raw, "when", trigger)
            .map_err(|error| error.to_string())?;
        let local = analyze_local_function(trigger, &reference, manifest_dir, cache)?;
        if module.is_none() {
            module = local.module;
        }
        capabilities.extend(local.capabilities);
        vendor_locked |= local.vendor_locked;
        framework_overhead_tokens =
            framework_overhead_tokens.saturating_add(local.framework_overhead_tokens);
    }

    Ok(RouteTrigger {
        id: trigger.id.clone(),
        kind: trigger_kind_label(trigger.kind).to_string(),
        provider: trigger.provider.as_str().to_string(),
        path: trigger_path(trigger)?,
        module: module.unwrap_or_else(|| "-".to_string()),
        handler,
        events: trigger.match_.events.clone(),
        requires_capabilities: capabilities.into_iter().collect(),
        budgets: route_budgets(trigger),
        vendor_locked,
        framework_overhead_tokens,
    })
}

fn analyze_local_function(
    trigger: &ResolvedTriggerConfig,
    reference: &TriggerFunctionRef,
    manifest_dir: &Path,
    cache: &mut ModuleCache,
) -> Result<LocalRouteAnalysis, String> {
    let module_path = package::manifest_module_source_path(
        &trigger.manifest_dir,
        trigger.package_name.as_deref(),
        &trigger.exports,
        reference.module_name.as_deref(),
    )
    .map_err(|error| error.to_string())?;
    let module = cache.load(&module_path)?;
    let Some(handler) = module.report.handler(&reference.function_name) else {
        return Err(format!(
            "{}: handler '{}' was not found in {}",
            package::manifest_trigger_location(trigger),
            reference.function_name,
            module.path.display()
        ));
    };

    let mut capabilities = capabilities_for_handler(handler);
    let framework_overhead_tokens =
        template_overhead_tokens_for_handler(handler, &module.path, manifest_dir);
    if framework_overhead_tokens > 0 || handler_uses_template_rendering(handler) {
        capabilities.insert("template.render".to_string());
    }

    Ok(LocalRouteAnalysis {
        module: Some(display_module_path(&module.path, manifest_dir)),
        capabilities,
        vendor_locked: module.vendor_locked,
        framework_overhead_tokens,
    })
}

impl ModuleCache {
    fn load(&mut self, path: &Path) -> Result<&ParsedModule, String> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if !self.modules.contains_key(&canonical) {
            let source = fs::read_to_string(&canonical)
                .map_err(|error| format!("failed to read {}: {error}", canonical.display()))?;
            let program = harn_parser::parse_source(&source)
                .map_err(|error| format!("failed to parse {}: {error}", canonical.display()))?;
            let vendor_locked = module_vendor_locked(&program);
            let report = harn_ir::analyze_program(&program);
            self.modules.insert(
                canonical.clone(),
                ParsedModule {
                    path: canonical.clone(),
                    report,
                    vendor_locked,
                },
            );
        }
        Ok(self
            .modules
            .get(&canonical)
            .expect("parsed module inserted before lookup"))
    }
}

fn capabilities_for_handler(handler: &harn_ir::HandlerIr) -> BTreeSet<String> {
    let mut capabilities = BTreeSet::new();
    for node in &handler.nodes {
        let NodeSemantics::Call(call) = &node.semantics else {
            continue;
        };
        let direct = direct_call_capabilities(call);
        let use_ir_classification = call.name != "host_call" || direct.is_empty();
        capabilities.extend(direct);
        if use_ir_classification {
            if let CallClassification::Capabilities(effects) = &call.classification {
                for effect in effects {
                    capabilities.insert(capability_label(effect.capability).to_string());
                }
            }
        }
    }
    capabilities
}

fn direct_call_capabilities(call: &harn_ir::CallSemantics) -> BTreeSet<String> {
    let mut capabilities = BTreeSet::new();
    match call.name.as_str() {
        "read_file" | "read_file_bytes" | "read_file_result" | "read_lines" | "read_stdin" => {
            capabilities.insert("workspace.read_text".to_string());
        }
        "list_dir" | "walk_dir" | "glob" => {
            capabilities.insert("workspace.list".to_string());
        }
        "file_exists" | "path_status" | "stat" => {
            capabilities.insert("workspace.exists".to_string());
        }
        "write_file"
        | "write_file_bytes"
        | "replace_file"
        | "replace_file_result"
        | "replace_file_bytes"
        | "replace_file_bytes_result"
        | "append_file"
        | "mkdir"
        | "mkdtemp"
        | "copy_file"
        | "move_file" => {
            capabilities.insert("workspace.write_text".to_string());
        }
        "delete_file" => {
            capabilities.insert("workspace.delete".to_string());
        }
        "apply_edit" => {
            capabilities.insert("workspace.apply_edit".to_string());
        }
        "http_get"
        | "http_post"
        | "http_put"
        | "http_patch"
        | "http_delete"
        | "http_request"
        | "http_download"
        | "http_session"
        | "http_session_request"
        | "http_stream_open"
        | "sse_connect"
        | "websocket_connect"
        | "websocket_server" => {
            capabilities.insert("network.http".to_string());
        }
        "exec" | "exec_at" | "shell" | "shell_at" | "spawn_captured" => {
            capabilities.insert("process.exec".to_string());
        }
        "llm_call"
        | "llm_call_safe"
        | "llm_stream_call"
        | "llm_call_structured"
        | "llm_call_structured_safe"
        | "llm_call_structured_result"
        | "llm_completion"
        | "agent_llm_turn"
        | "agent_turn"
        | "agent_loop" => {
            capabilities.insert("llm.call".to_string());
        }
        "llm_catalog" | "llm_catalog_refresh" | "llm_provider_status" => {
            capabilities.insert("llm.catalog".to_string());
        }
        "spawn_agent" | "send_input" | "resume_agent" | "wait_agent" | "close_agent"
        | "worker_trigger" => {
            capabilities.insert("worker.dispatch".to_string());
        }
        "request_approval" => {
            capabilities.insert("human.approval".to_string());
        }
        "connector_call" | "mcp_call" | "host_tool_call" => {
            capabilities.insert("connector.call".to_string());
        }
        "secret_get" => {
            capabilities.insert("connector.secret_get".to_string());
        }
        "event_log_emit" => {
            capabilities.insert("connector.event_log_emit".to_string());
        }
        "metrics_inc" => {
            capabilities.insert("connector.metrics_inc".to_string());
        }
        "host_call" => insert_host_call_capability(call, &mut capabilities),
        _ if call.name.starts_with("mcp_") => {
            capabilities.insert("connector.call".to_string());
        }
        _ => {}
    }
    capabilities
}

fn insert_host_call_capability(call: &harn_ir::CallSemantics, capabilities: &mut BTreeSet<String>) {
    let Some(operation) = call.literal_args.first().and_then(literal_as_str) else {
        capabilities.insert("connector.call".to_string());
        return;
    };
    if operation == "template.render" {
        capabilities.insert("template.render".to_string());
    } else if operation.starts_with("process.") {
        capabilities.insert("process.exec".to_string());
    } else if operation.starts_with("workspace.") || operation.starts_with("connector.") {
        capabilities.insert(operation.to_string());
    } else {
        capabilities.insert("connector.call".to_string());
    }
}

fn capability_label(capability: Capability) -> &'static str {
    match capability {
        Capability::WorkspaceMutation => "workspace.write_text",
        Capability::CommandExecution => "process.exec",
        Capability::NetworkAccess => "network.http",
        Capability::ConnectorAccess => "connector.call",
        Capability::ModelCall => "llm.call",
        Capability::WorkerDispatch => "worker.dispatch",
        Capability::HumanApproval => "human.approval",
        Capability::AutonomyPolicy => "autonomy.policy",
    }
}

fn template_overhead_tokens_for_handler(
    handler: &harn_ir::HandlerIr,
    module_path: &Path,
    manifest_dir: &Path,
) -> u64 {
    let mut total = 0u64;
    for node in &handler.nodes {
        let NodeSemantics::Call(call) = &node.semantics else {
            continue;
        };
        match call.name.as_str() {
            "render" | "render_prompt" => {
                if let Some(path) = call.literal_args.first().and_then(literal_as_str) {
                    total = total.saturating_add(template_file_overhead_tokens(
                        path,
                        module_path,
                        manifest_dir,
                    ));
                }
            }
            "render_string" => {
                if let Some(body) = call.literal_args.first().and_then(literal_as_str) {
                    total = total.saturating_add(estimate_template_markup_tokens(body));
                }
            }
            "host_call" => {
                let Some("template.render") = call.literal_args.first().and_then(literal_as_str)
                else {
                    continue;
                };
                if let Some(body) = call
                    .literal_args
                    .get(1)
                    .and_then(host_render_inline_template_arg)
                {
                    total = total.saturating_add(estimate_template_markup_tokens(body));
                } else if let Some(path) = call.literal_args.get(1).and_then(host_render_path_arg) {
                    total = total.saturating_add(template_file_overhead_tokens(
                        path,
                        module_path,
                        manifest_dir,
                    ));
                }
            }
            _ => {}
        }
    }
    total
}

fn handler_uses_template_rendering(handler: &harn_ir::HandlerIr) -> bool {
    handler.nodes.iter().any(|node| {
        let NodeSemantics::Call(call) = &node.semantics else {
            return false;
        };
        matches!(
            call.name.as_str(),
            "render" | "render_prompt" | "render_string"
        ) || (call.name == "host_call"
            && matches!(
                call.literal_args.first().and_then(literal_as_str),
                Some("template.render")
            ))
    })
}

fn host_render_path_arg(value: &LiteralValue) -> Option<&str> {
    let LiteralValue::Dict(entries) = value else {
        return None;
    };
    entries
        .get("path")
        .or_else(|| entries.get("template_path"))
        .and_then(literal_as_str)
}

fn host_render_inline_template_arg(value: &LiteralValue) -> Option<&str> {
    let LiteralValue::Dict(entries) = value else {
        return None;
    };
    entries
        .get("template")
        .or_else(|| entries.get("source"))
        .or_else(|| entries.get("body"))
        .and_then(literal_as_str)
}

fn literal_as_str(value: &LiteralValue) -> Option<&str> {
    match value {
        LiteralValue::String(value) | LiteralValue::Identifier(value) => Some(value.as_str()),
        _ => None,
    }
}

fn module_vendor_locked(program: &[SNode]) -> bool {
    program.iter().any(import_is_vendor_locked)
}

fn import_is_vendor_locked(node: &SNode) -> bool {
    let (_, inner) = harn_parser::peel_attributes(node);
    match &inner.node {
        Node::ImportDecl { path, .. } => import_path_or_names_vendor_locked(path, &[]),
        Node::SelectiveImport { names, path, .. } => {
            import_path_or_names_vendor_locked(path, names)
        }
        _ => false,
    }
}

fn import_path_or_names_vendor_locked(path: &str, names: &[String]) -> bool {
    direct_vendor_token(path) || names.iter().any(|name| direct_vendor_token(name))
}

fn direct_vendor_token(value: &str) -> bool {
    const VENDORS: &[&str] = &[
        "anthropic",
        "openai",
        "gemini",
        "bedrock",
        "openrouter",
        "together",
        "huggingface",
        "ollama",
        "github",
        "gitlab",
        "slack",
        "linear",
        "notion",
        "kafka",
        "nats",
        "pulsar",
        "postgres",
    ];
    let normalized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    normalized
        .split('-')
        .filter(|token| !token.is_empty())
        .any(|token| VENDORS.contains(&token))
}

fn template_file_overhead_tokens(
    template_ref: &str,
    module_path: &Path,
    manifest_dir: &Path,
) -> u64 {
    let Some(body) = resolve_template_body(template_ref, module_path, manifest_dir) else {
        return 0;
    };
    estimate_template_markup_tokens(&body)
}

fn resolve_template_body(
    template_ref: &str,
    module_path: &Path,
    manifest_dir: &Path,
) -> Option<String> {
    if harn_modules::asset_paths::stdlib_prompt_asset_path(template_ref).is_some() {
        return harn_vm::stdlib_modules::get_stdlib_prompt_asset(template_ref).map(str::to_string);
    }
    let module_dir = module_path.parent().unwrap_or(manifest_dir);
    if let Some(asset_ref) = harn_modules::asset_paths::parse(template_ref) {
        return harn_modules::asset_paths::resolve(&asset_ref, module_dir)
            .ok()
            .and_then(|path| fs::read_to_string(path).ok());
    }
    let candidate = if Path::new(template_ref).is_absolute() {
        PathBuf::from(template_ref)
    } else {
        module_dir.join(template_ref)
    };
    fs::read_to_string(candidate).ok()
}

fn estimate_template_markup_tokens(body: &str) -> u64 {
    let mut markup_chars = 0usize;
    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("}}") else {
            break;
        };
        markup_chars = markup_chars.saturating_add(end + 4);
        rest = &after_start[end + 2..];
    }
    if markup_chars == 0 {
        0
    } else {
        ((markup_chars as u64).saturating_add(3)) / 4
    }
}

fn trigger_path(trigger: &ResolvedTriggerConfig) -> Result<Option<String>, String> {
    let configured = toml_table_string(&trigger.kind_specific, "path")
        .or_else(|| toml_table_string(&trigger.match_.extra, "path"));
    let path = match trigger.kind {
        TriggerKind::Webhook | TriggerKind::A2aPush => {
            Some(configured.unwrap_or_else(|| format!("/triggers/{}", trigger.id)))
        }
        TriggerKind::Stream => configured,
        TriggerKind::Cron | TriggerKind::Poll | TriggerKind::Predicate => configured,
    };
    let path_requires_slash = matches!(
        trigger.kind,
        TriggerKind::Webhook | TriggerKind::A2aPush | TriggerKind::Stream
    );
    if path_requires_slash && path.as_deref().is_some_and(|path| !path.starts_with('/')) {
        return Err(format!("trigger '{}' path must start with '/'", trigger.id));
    }
    Ok(path)
}

fn route_budgets(trigger: &ResolvedTriggerConfig) -> RouteBudgets {
    RouteBudgets {
        max_latency_ms: latency_budget_ms(trigger),
        max_cost_usd: trigger.budget.max_cost_usd.or_else(|| {
            trigger
                .when_budget
                .as_ref()
                .and_then(|budget| budget.max_cost_usd)
        }),
        max_tokens: trigger.budget.max_tokens.or_else(|| {
            trigger
                .when_budget
                .as_ref()
                .and_then(|budget| budget.tokens_max)
        }),
        daily_cost_usd: trigger.budget.daily_cost_usd,
        hourly_cost_usd: trigger.budget.hourly_cost_usd,
        max_concurrent: trigger
            .concurrency
            .as_ref()
            .map(|concurrency| concurrency.max)
            .or(trigger.budget.max_concurrent),
    }
}

fn latency_budget_ms(trigger: &ResolvedTriggerConfig) -> Option<u64> {
    for field in [
        "max_latency_ms",
        "max-latency-ms",
        "latency_ms",
        "timeout_ms",
        "timeout-ms",
        "timeout",
    ] {
        if let Some(value) = trigger.kind_specific.get(field).and_then(toml_duration_ms) {
            return Some(value);
        }
    }
    trigger
        .when_budget
        .as_ref()
        .and_then(|budget| budget.timeout.as_deref())
        .and_then(|raw| parse_duration_millis(raw).ok())
}

fn toml_table_string(table: &BTreeMap<String, toml::Value>, key: &str) -> Option<String> {
    table
        .get(key)
        .and_then(toml_string)
        .filter(|value| !value.trim().is_empty())
}

fn toml_string(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(value) => Some(value.clone()),
        toml::Value::Integer(value) => Some(value.to_string()),
        _ => None,
    }
}

fn toml_duration_ms(value: &toml::Value) -> Option<u64> {
    match value {
        toml::Value::Integer(value) if *value >= 0 => Some(*value as u64),
        toml::Value::String(value) => parse_duration_millis(value).ok(),
        _ => None,
    }
}

fn display_module_path(path: &Path, manifest_dir: &Path) -> String {
    path.strip_prefix(manifest_dir)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_template_markup_only() {
        assert_eq!(estimate_template_markup_tokens("plain text"), 0);
        assert_eq!(estimate_template_markup_tokens("hello {{ name }}"), 3);
        assert!(estimate_template_markup_tokens("{{ if ok }}yes{{ endif }}") > 0);
    }

    #[test]
    fn detects_vendor_specific_imports() {
        let portable = harn_parser::parse_source(r#"import "std/triggers""#).unwrap();
        assert!(!module_vendor_locked(&portable));

        let vendor_path =
            harn_parser::parse_source(r#"import { post_message } from "std/connectors/slack""#)
                .unwrap();
        assert!(module_vendor_locked(&vendor_path));

        let vendor_name =
            harn_parser::parse_source(r#"import { github_call } from "std/connectors""#).unwrap();
        assert!(module_vendor_locked(&vendor_name));
    }
}
