//! `harn provider dispatch-explain <provider> <model>` — deterministically
//! explain how a (provider, model) pair would dispatch, from the capability
//! registry alone. No network, no LLM call.
//!
//! This is the static counterpart to the runtime `resolved_dispatch` transcript
//! record: instead of asking "what did this call dispatch", it answers "what
//! WOULD this pair dispatch" — so anyone can confirm "does anthropic
//! claude-sonnet route native?" without running an eval and grepping a
//! transcript.

use std::collections::{BTreeMap, BTreeSet};
use std::process;

use serde::Serialize;

use crate::cli::{
    ProviderDispatchAuditArgs, ProviderDispatchAuditVariantArg, ProviderDispatchExplainArgs,
    ProviderToolProbeCaseArg,
};

/// Schema version stamped on every `provider dispatch-audit` envelope (the
/// report and its embedded tool-probe plan). Re-exported from the crate root as
/// the single source of truth so integration tests reference it instead of
/// duplicating the number — the duplication that let this rot silently while the
/// slow-E2E tier was unwatched. The in-crate `audit` unit tests still pin the
/// literal value, so an intentional bump remains a deliberate, reviewed change.
pub const DISPATCH_AUDIT_SCHEMA_VERSION: u8 = 4;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct DispatchExplanation {
    pub provider: String,
    pub model: String,
    pub wire_format: String,
    pub message_wire_format: String,
    pub native_tool_wire_format: String,
    pub base_url_host: String,
    pub tool_format: String,
    pub native_tools: bool,
    pub structured_output: Option<String>,
    pub structured_output_mode: String,
    pub advertises_thinking: bool,
    pub thinking_modes: Vec<String>,
    pub requested_thinking: bool,
    pub thinking_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DispatchAuditRow {
    id: String,
    variant: String,
    #[serde(flatten)]
    explanation: DispatchExplanation,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DispatchAuditFailure {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    route: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filter_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filter_value: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DispatchAuditCatalogProvenance {
    hash_blake3: String,
    provider_count: usize,
    model_count: usize,
    routing_route_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DispatchAuditUnroutedProvider {
    provider: String,
    model_count: usize,
    active_model_count: usize,
    route_count: usize,
    reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DispatchAuditReport {
    schema_version: u8,
    catalog: DispatchAuditCatalogProvenance,
    route_count: usize,
    variant_count: usize,
    row_count: usize,
    pass_count: usize,
    fail_count: usize,
    unrouted_provider_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unrouted_providers: Vec<DispatchAuditUnroutedProvider>,
    providers: Vec<String>,
    variants: Vec<String>,
    rows: Vec<DispatchAuditRow>,
    failures: Vec<DispatchAuditFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_probe_plan: Option<DispatchAuditToolProbePlan>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DispatchAuditToolProbePlan {
    schema_version: u8,
    plan_id: String,
    catalog_hash_blake3: String,
    matrix: DispatchAuditToolProbeMatrix,
    readiness_command_count: usize,
    command_count: usize,
    request_audit_command_count: usize,
    route_count: usize,
    cases: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    excluded_cases: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    not_applicable_commands: Vec<DispatchAuditToolProbeNotApplicable>,
    live_request_profiles: Vec<String>,
    request_audit_profiles: Vec<String>,
    modes: Vec<String>,
    repeat: u16,
    timeout_secs: u64,
    output_dir: String,
    readiness_commands: Vec<DispatchAuditReadinessCommand>,
    commands: Vec<DispatchAuditToolProbeCommand>,
    request_audit_commands: Vec<DispatchAuditToolProbeRequestAuditCommand>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DispatchAuditToolProbeMatrix {
    provider_count: usize,
    model_count: usize,
    provider_model_count: usize,
    route_count: usize,
    case_count: usize,
    mode_count: usize,
    live_request_profile_count: usize,
    request_audit_profile_count: usize,
    readiness_command_count: usize,
    command_count: usize,
    request_audit_command_count: usize,
    not_applicable_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DispatchAuditReadinessCommand {
    id: String,
    route: String,
    provider: String,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    structured_output: Option<String>,
    structured_output_mode: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    secret_envs: Vec<String>,
    argv: Vec<String>,
    output_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DispatchAuditToolProbeNotApplicable {
    route: String,
    provider: String,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    structured_output: Option<String>,
    structured_output_mode: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    secret_envs: Vec<String>,
    case: String,
    request_profile: String,
    mode: String,
    reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DispatchAuditToolProbeCommand {
    id: String,
    route: String,
    provider: String,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    structured_output: Option<String>,
    structured_output_mode: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    secret_envs: Vec<String>,
    case: String,
    request_profile: String,
    mode: String,
    repeat: u16,
    timeout_secs: u64,
    argv: Vec<String>,
    output_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DispatchAuditToolProbeRequestAuditCommand {
    id: String,
    route: String,
    provider: String,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    structured_output: Option<String>,
    structured_output_mode: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    secret_envs: Vec<String>,
    case: String,
    request_profile: String,
    mode: String,
    argv: Vec<String>,
    output_path: String,
}

struct DispatchAuditToolProbeCommandContext<'a> {
    catalog_hash: &'a str,
    output_dir: &'a str,
    secret_envs_by_provider: &'a BTreeMap<String, Vec<String>>,
    repeat: u16,
    timeout_secs: u64,
}

pub(crate) fn run(args: &ProviderDispatchExplainArgs) {
    let resolved = harn_vm::llm_config::resolve_model_info(&args.model);
    if resolved.alias.is_some() && resolved.provider != args.provider {
        eprintln!(
            "error: model selector `{}` resolves to provider `{}`, not requested provider `{}`",
            args.model, resolved.provider, args.provider
        );
        std::process::exit(1);
    }
    let model = resolved
        .alias
        .as_ref()
        .map_or(args.model.as_str(), |_| resolved.id.as_str());
    let report = explain(
        &args.provider,
        model,
        args.thinking,
        args.tool_format.as_deref(),
    );
    if args.json {
        print_json_or_exit(&report, "dispatch explanation");
        return;
    }

    print_human_explanation(&report);
}

pub(crate) fn run_audit(args: &ProviderDispatchAuditArgs) {
    let report = audit(args);
    if args.json {
        print_json_or_exit(&report, "dispatch audit report");
    } else {
        println!(
            "provider dispatch audit: {}/{} dispatch rows passed across {} catalog routes and {} variants",
            report.pass_count, report.row_count, report.route_count, report.variant_count
        );
        if let Some(plan) = &report.tool_probe_plan {
            println!(
                "tool-probe plan: {} live commands and {} request-audit commands across {} routes, {} cases, {} modes",
                plan.command_count,
                plan.request_audit_command_count,
                plan.route_count,
                plan.cases.len(),
                plan.modes.len()
            );
        }
        for failure in report.failures.iter().take(10) {
            let target = failure
                .route
                .as_ref()
                .or(failure.provider.as_ref())
                .map(|value| format!(" {value}"))
                .unwrap_or_default();
            println!("- {}{}", failure.code, target);
            println!("  {}", failure.message);
        }
    }
    if report.fail_count > 0 {
        process::exit(1);
    }
}

pub(crate) fn explain(
    provider: &str,
    model: &str,
    requested_thinking: bool,
    tool_format_override: Option<&str>,
) -> DispatchExplanation {
    let caps = harn_vm::llm::capabilities::lookup(provider, model);
    let wire_format = harn_vm::llm::resolved_dispatch::wire_format_for(provider, model);
    let message_wire_format = caps.message_wire_format.as_str().to_string();
    let base_url = harn_vm::llm_config::provider_config(provider)
        .map(|def| harn_vm::llm_config::resolve_base_url(&def))
        .unwrap_or_else(|| default_base_url_for(wire_format));
    let base_url_host = host_of(&base_url);

    let tool_format = tool_format_override
        .map(str::to_string)
        .or_else(|| caps.preferred_tool_format.clone())
        .unwrap_or_else(|| {
            if caps.native_tools {
                "native".to_string()
            } else {
                "text".to_string()
            }
        });

    // A model advertises thinking when the capability row lists any thinking
    // mode; requesting thinking against a model that advertises none is a
    // footgun (the operator asked for reasoning the route will silently drop).
    let advertises_thinking = !caps.thinking_modes.is_empty();
    let thinking_note = if requested_thinking && !advertises_thinking {
        Some(format!(
            "requested --thinking but {provider}:{model} advertises no thinking modes; the route will not return reasoning content"
        ))
    } else {
        None
    };

    DispatchExplanation {
        provider: provider.to_string(),
        model: model.to_string(),
        wire_format: wire_format.to_string(),
        message_wire_format,
        native_tool_wire_format: caps.native_tool_wire_format,
        base_url_host,
        tool_format,
        native_tools: caps.native_tools,
        structured_output: caps.structured_output,
        structured_output_mode: caps.structured_output_mode,
        advertises_thinking,
        thinking_modes: caps.thinking_modes,
        requested_thinking,
        thinking_note,
    }
}

fn audit(args: &ProviderDispatchAuditArgs) -> DispatchAuditReport {
    let variants = dispatch_audit_variants(&args.variants);
    let mut failures = route_filter_failures(&args.routes);
    let artifact = harn_vm::provider_catalog::artifact();
    let catalog = catalog_provenance(&artifact);
    let unrouted_providers = unrouted_providers(&artifact);
    let selected_routes = selected_routes(args, &artifact.routing_routes);
    failures.extend(provider_filter_failures(
        args,
        &artifact,
        &unrouted_providers,
    ));
    failures.extend(model_filter_failures(args, &artifact, &selected_routes));
    failures.extend(capability_filter_failures(args, &selected_routes));
    failures.extend(missing_route_filter_failures(args, &selected_routes));
    let providers: Vec<String> = selected_routes
        .iter()
        .map(|route| route.provider.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut rows = Vec::new();
    for route in &selected_routes {
        for variant in &variants {
            let explanation = explain(
                &route.provider,
                &route.model,
                variant.requested_thinking(),
                variant.tool_format_override(),
            );
            rows.push(DispatchAuditRow {
                id: stable_id(&["dispatch", &route.provider, &route.model, variant.name()]),
                variant: variant.name().to_string(),
                explanation,
            });
        }
    }
    if rows.is_empty() && failures.is_empty() {
        failures.push(DispatchAuditFailure {
            code: "no_routes_selected".to_string(),
            message: "filters selected zero catalog routing routes".to_string(),
            provider: None,
            route: None,
            filter_kind: None,
            filter_value: None,
        });
    }
    let fail_count = failures.len();
    let tool_probe_plan = if args.include_tool_probe_plan {
        Some(tool_probe_plan(
            args,
            &selected_routes,
            &catalog.hash_blake3,
            &artifact.providers,
        ))
    } else {
        None
    };
    DispatchAuditReport {
        schema_version: DISPATCH_AUDIT_SCHEMA_VERSION,
        catalog,
        route_count: selected_routes.len(),
        variant_count: variants.len(),
        row_count: rows.len(),
        pass_count: rows.len(),
        fail_count,
        unrouted_provider_count: unrouted_providers.len(),
        unrouted_providers,
        providers,
        variants: variants
            .into_iter()
            .map(|variant| variant.name().to_string())
            .collect(),
        rows,
        failures,
        tool_probe_plan,
    }
}

fn tool_probe_plan(
    args: &ProviderDispatchAuditArgs,
    selected_routes: &[harn_vm::provider_catalog::CatalogRoutingRoute],
    catalog_hash: &str,
    providers: &[harn_vm::provider_catalog::CatalogProvider],
) -> DispatchAuditToolProbePlan {
    let case_selection = tool_probe_plan_cases(&args.tool_probe_cases);
    let modes = args.tool_probe_mode.tool_probe_modes();
    let live_request_profiles = vec!["catalog_default".to_string()];
    let request_audit_profiles = vec!["parameter_edges".to_string()];
    let output_dir = args
        .tool_probe_output_dir
        .clone()
        .unwrap_or_else(|| default_tool_probe_output_dir(catalog_hash));
    let route_fingerprint = selected_routes
        .iter()
        .map(route_key)
        .collect::<Vec<_>>()
        .join("\0");
    let case_fingerprint = case_selection
        .included
        .iter()
        .map(|case| case.as_str())
        .collect::<Vec<_>>()
        .join("\0");
    let mode_fingerprint = modes
        .iter()
        .map(|mode| mode.as_str())
        .collect::<Vec<_>>()
        .join("\0");
    let live_profile_fingerprint = live_request_profiles.join("\0");
    let request_audit_profile_fingerprint = request_audit_profiles.join("\0");
    let repeat_fingerprint = args.tool_probe_repeat.to_string();
    let timeout_fingerprint = args.tool_probe_timeout_secs.to_string();
    let readiness_fingerprint = "provider_ready";
    let plan_id = stable_id(&[
        "tool_probe_plan",
        catalog_hash,
        &route_fingerprint,
        &case_fingerprint,
        &mode_fingerprint,
        &live_profile_fingerprint,
        &request_audit_profile_fingerprint,
        &repeat_fingerprint,
        &timeout_fingerprint,
        readiness_fingerprint,
        &output_dir,
    ]);
    let secret_envs_by_provider = catalog_secret_envs_by_provider(providers);
    let readiness_commands = tool_probe_readiness_commands(
        catalog_hash,
        &output_dir,
        selected_routes,
        &secret_envs_by_provider,
    );
    let mut commands = Vec::new();
    let mut request_audit_commands = Vec::new();
    let mut not_applicable_commands = Vec::new();
    let command_context = DispatchAuditToolProbeCommandContext {
        catalog_hash,
        output_dir: &output_dir,
        secret_envs_by_provider: &secret_envs_by_provider,
        repeat: args.tool_probe_repeat,
        timeout_secs: args.tool_probe_timeout_secs,
    };
    for route in selected_routes {
        for case in &case_selection.included {
            for mode in &modes {
                let case_name = case.as_str();
                let mode_name = mode.as_str();
                if !case.is_live_applicable(&route.provider, &route.model) {
                    let (structured_output, structured_output_mode) =
                        route_structured_output_contract(route);
                    for request_profile in live_request_profiles
                        .iter()
                        .chain(request_audit_profiles.iter())
                    {
                        not_applicable_commands.push(DispatchAuditToolProbeNotApplicable {
                            route: route_key(route),
                            provider: route.provider.clone(),
                            model: route.model.clone(),
                            structured_output: structured_output.clone(),
                            structured_output_mode: structured_output_mode.clone(),
                            secret_envs: secret_envs_for_route(route, &secret_envs_by_provider),
                            case: case_name.to_string(),
                            request_profile: request_profile.clone(),
                            mode: mode_name.to_string(),
                            reason: "route_has_no_signed_thinking_tool_history_surface".to_string(),
                        });
                    }
                    continue;
                }
                for request_profile in &live_request_profiles {
                    commands.push(live_tool_probe_command(
                        &command_context,
                        route,
                        case_name,
                        request_profile,
                        mode_name,
                        *mode,
                    ));
                }
                for request_profile in &request_audit_profiles {
                    request_audit_commands.push(request_audit_tool_probe_command(
                        &command_context,
                        route,
                        case_name,
                        request_profile,
                        mode_name,
                        *mode,
                    ));
                }
            }
        }
    }
    let provider_count = selected_routes
        .iter()
        .map(|route| route.provider.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let model_count = selected_routes
        .iter()
        .map(|route| route.model.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let matrix = DispatchAuditToolProbeMatrix {
        provider_count,
        model_count,
        provider_model_count: selected_routes.len(),
        route_count: selected_routes.len(),
        case_count: case_selection.included.len(),
        mode_count: modes.len(),
        live_request_profile_count: live_request_profiles.len(),
        request_audit_profile_count: request_audit_profiles.len(),
        readiness_command_count: readiness_commands.len(),
        command_count: commands.len(),
        request_audit_command_count: request_audit_commands.len(),
        not_applicable_count: not_applicable_commands.len(),
    };
    DispatchAuditToolProbePlan {
        schema_version: DISPATCH_AUDIT_SCHEMA_VERSION,
        plan_id,
        catalog_hash_blake3: catalog_hash.to_string(),
        matrix,
        readiness_command_count: readiness_commands.len(),
        command_count: commands.len(),
        request_audit_command_count: request_audit_commands.len(),
        route_count: selected_routes.len(),
        cases: case_selection
            .included
            .iter()
            .map(|case| case.as_str().to_string())
            .collect(),
        excluded_cases: case_selection.excluded,
        not_applicable_commands,
        live_request_profiles,
        request_audit_profiles,
        modes: modes.iter().map(|mode| mode.as_str().to_string()).collect(),
        repeat: args.tool_probe_repeat,
        timeout_secs: args.tool_probe_timeout_secs,
        output_dir,
        readiness_commands,
        commands,
        request_audit_commands,
    }
}

fn live_tool_probe_command(
    context: &DispatchAuditToolProbeCommandContext<'_>,
    route: &harn_vm::provider_catalog::CatalogRoutingRoute,
    case_name: &str,
    request_profile: &str,
    mode_name: &str,
    mode: harn_vm::llm::tool_conformance::ToolProbeMode,
) -> DispatchAuditToolProbeCommand {
    let id = stable_id(&[
        "tool_probe",
        context.catalog_hash,
        &route.provider,
        &route.model,
        case_name,
        request_profile,
        mode_name,
        &context.repeat.to_string(),
        &context.timeout_secs.to_string(),
    ]);
    let mut argv = vec![
        "harn".to_string(),
        "provider".to_string(),
        "tool-probe".to_string(),
        route.provider.clone(),
        "--model".to_string(),
        route.model.clone(),
        "--case".to_string(),
        case_name.to_string(),
        "--request-profile".to_string(),
        request_profile.to_string(),
        "--mode".to_string(),
        mode_cli_value(mode).to_string(),
        "--repeat".to_string(),
        context.repeat.to_string(),
        "--timeout-secs".to_string(),
        context.timeout_secs.to_string(),
        "--json".to_string(),
    ];
    argv.shrink_to_fit();
    let (structured_output, structured_output_mode) = route_structured_output_contract(route);
    DispatchAuditToolProbeCommand {
        output_path: tool_probe_output_path(
            context.output_dir,
            &id,
            &route.provider,
            &route.model,
            case_name,
            request_profile,
            mode_name,
        ),
        id,
        route: route_key(route),
        provider: route.provider.clone(),
        model: route.model.clone(),
        structured_output,
        structured_output_mode,
        secret_envs: secret_envs_for_route(route, context.secret_envs_by_provider),
        case: case_name.to_string(),
        request_profile: request_profile.to_string(),
        mode: mode_name.to_string(),
        repeat: context.repeat,
        timeout_secs: context.timeout_secs,
        argv,
    }
}

fn request_audit_tool_probe_command(
    context: &DispatchAuditToolProbeCommandContext<'_>,
    route: &harn_vm::provider_catalog::CatalogRoutingRoute,
    case_name: &str,
    request_profile: &str,
    mode_name: &str,
    mode: harn_vm::llm::tool_conformance::ToolProbeMode,
) -> DispatchAuditToolProbeRequestAuditCommand {
    let id = stable_id(&[
        "tool_probe_request_audit",
        context.catalog_hash,
        &route.provider,
        &route.model,
        case_name,
        request_profile,
        mode_name,
    ]);
    let mut argv = vec![
        "harn".to_string(),
        "provider".to_string(),
        "tool-probe".to_string(),
        route.provider.clone(),
        "--model".to_string(),
        route.model.clone(),
        "--case".to_string(),
        case_name.to_string(),
        "--request-profile".to_string(),
        request_profile.to_string(),
        "--mode".to_string(),
        mode_cli_value(mode).to_string(),
        "--dry-run-request".to_string(),
        "--json".to_string(),
    ];
    argv.shrink_to_fit();
    let (structured_output, structured_output_mode) = route_structured_output_contract(route);
    DispatchAuditToolProbeRequestAuditCommand {
        output_path: tool_probe_output_path(
            context.output_dir,
            &id,
            &route.provider,
            &route.model,
            case_name,
            request_profile,
            mode_name,
        ),
        id,
        route: route_key(route),
        provider: route.provider.clone(),
        model: route.model.clone(),
        structured_output,
        structured_output_mode,
        secret_envs: secret_envs_for_route(route, context.secret_envs_by_provider),
        case: case_name.to_string(),
        request_profile: request_profile.to_string(),
        mode: mode_name.to_string(),
        argv,
    }
}

fn tool_probe_readiness_commands(
    catalog_hash: &str,
    output_dir: &str,
    selected_routes: &[harn_vm::provider_catalog::CatalogRoutingRoute],
    secret_envs_by_provider: &BTreeMap<String, Vec<String>>,
) -> Vec<DispatchAuditReadinessCommand> {
    selected_routes
        .iter()
        .map(|route| {
            let id = stable_id(&[
                "provider_ready",
                catalog_hash,
                &route.provider,
                &route.model,
            ]);
            let (structured_output, structured_output_mode) =
                route_structured_output_contract(route);
            DispatchAuditReadinessCommand {
                output_path: readiness_output_path(output_dir, &id, &route.provider, &route.model),
                id,
                route: route_key(route),
                provider: route.provider.clone(),
                model: route.model.clone(),
                structured_output,
                structured_output_mode,
                secret_envs: secret_envs_for_route(route, secret_envs_by_provider),
                argv: vec![
                    "harn".to_string(),
                    "provider".to_string(),
                    "ready".to_string(),
                    route.provider.clone(),
                    "--model".to_string(),
                    route.model.clone(),
                    "--json".to_string(),
                ],
            }
        })
        .collect()
}

fn route_structured_output_contract(
    route: &harn_vm::provider_catalog::CatalogRoutingRoute,
) -> (Option<String>, String) {
    let caps = harn_vm::llm::capabilities::lookup(&route.provider, &route.model);
    (caps.structured_output, caps.structured_output_mode)
}

fn catalog_secret_envs_by_provider(
    providers: &[harn_vm::provider_catalog::CatalogProvider],
) -> BTreeMap<String, Vec<String>> {
    providers
        .iter()
        .map(|provider| (provider.id.clone(), provider.auth.env.clone()))
        .collect()
}

fn secret_envs_for_route(
    route: &harn_vm::provider_catalog::CatalogRoutingRoute,
    secret_envs_by_provider: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    secret_envs_by_provider
        .get(&route.provider)
        .cloned()
        .filter(|envs| !envs.is_empty())
        .or_else(|| route.secret_env.clone().map(|env| vec![env]))
        .unwrap_or_default()
}

struct ToolProbePlanCases {
    included: Vec<harn_vm::llm::tool_conformance::ToolProbeCase>,
    excluded: Vec<String>,
}

fn tool_probe_plan_cases(cases: &[ProviderToolProbeCaseArg]) -> ToolProbePlanCases {
    if cases.is_empty() {
        use harn_vm::llm::tool_conformance::ToolProbeCase;
        return ToolProbePlanCases {
            included: vec![
                ToolProbeCase::SingleToolCall,
                ToolProbeCase::ParallelToolCalls,
                ToolProbeCase::LargeStringArgument,
                ToolProbeCase::ToolResultFollowup,
                ToolProbeCase::NoToolAnswerOrRefusal,
                ToolProbeCase::UnavailableToolRepair,
                ToolProbeCase::DoneSentinel,
            ],
            excluded: vec!["signed_thinking_tool_result_followup".to_string()],
        };
    }
    let mut out = Vec::new();
    for case in cases {
        let probe_case = case.tool_probe_case();
        if !out.contains(&probe_case) {
            out.push(probe_case);
        }
    }
    ToolProbePlanCases {
        included: out,
        excluded: Vec::new(),
    }
}

fn mode_cli_value(mode: harn_vm::llm::tool_conformance::ToolProbeMode) -> &'static str {
    match mode {
        harn_vm::llm::tool_conformance::ToolProbeMode::NonStreaming => "non-streaming",
        harn_vm::llm::tool_conformance::ToolProbeMode::Streaming => "streaming",
    }
}

fn route_key(route: &harn_vm::provider_catalog::CatalogRoutingRoute) -> String {
    format!("{}:{}", route.provider, route.model)
}

fn default_tool_probe_output_dir(catalog_hash: &str) -> String {
    format!(
        ".harn-runs/provider-live-probes/{}",
        catalog_hash_slug(catalog_hash)
    )
}

fn catalog_hash_slug(catalog_hash: &str) -> String {
    catalog_hash
        .strip_prefix("blake3:")
        .unwrap_or(catalog_hash)
        .chars()
        .take(16)
        .collect()
}

fn tool_probe_output_path(
    output_dir: &str,
    command_id: &str,
    provider: &str,
    model: &str,
    case_name: &str,
    request_profile: &str,
    mode_name: &str,
) -> String {
    format!(
        "{}/{}-{}-{}-{}-{}-{}.json",
        output_dir.trim_end_matches('/'),
        path_slug(&command_id.chars().take(12).collect::<String>()),
        path_slug(provider),
        path_slug(model),
        path_slug(case_name),
        path_slug(request_profile),
        path_slug(mode_name),
    )
}

fn readiness_output_path(
    output_dir: &str,
    command_id: &str,
    provider: &str,
    model: &str,
) -> String {
    format!(
        "{}/{}-{}-{}-readiness.json",
        output_dir.trim_end_matches('/'),
        path_slug(&command_id.chars().take(12).collect::<String>()),
        path_slug(provider),
        path_slug(model),
    )
}

fn path_slug(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn stable_id(parts: &[&str]) -> String {
    let joined = parts.join("\0");
    let hash = blake3::hash(joined.as_bytes());
    format!("{}", hash.to_hex())
}

fn selected_routes(
    args: &ProviderDispatchAuditArgs,
    routes: &[harn_vm::provider_catalog::CatalogRoutingRoute],
) -> Vec<harn_vm::provider_catalog::CatalogRoutingRoute> {
    let provider_filter: BTreeSet<&str> = args.providers.iter().map(String::as_str).collect();
    let model_filter: BTreeSet<&str> = args.models.iter().map(String::as_str).collect();
    let capability_filter: BTreeSet<&str> = args.capabilities.iter().map(String::as_str).collect();
    let route_filter: BTreeSet<(&str, &str)> = args
        .routes
        .iter()
        .filter_map(|route| parse_route_filter(route))
        .collect();
    let route_filter_requested = !args.routes.is_empty();
    routes
        .iter()
        .filter(|route| {
            provider_filter.is_empty() || provider_filter.contains(route.provider.as_str())
        })
        .filter(|route| model_filter.is_empty() || model_filter.contains(route.model.as_str()))
        .filter(|route| {
            capability_filter.is_empty()
                || capability_filter
                    .iter()
                    .all(|capability| route.capabilities.iter().any(|value| value == capability))
        })
        .filter(|route| {
            !route_filter_requested
                || route_filter.contains(&(route.provider.as_str(), route.model.as_str()))
        })
        .cloned()
        .collect()
}

fn catalog_provenance(
    artifact: &harn_vm::provider_catalog::ProviderCatalogArtifact,
) -> DispatchAuditCatalogProvenance {
    let catalog_json =
        serde_json::to_vec(artifact).expect("provider catalog serializes for audit provenance");
    DispatchAuditCatalogProvenance {
        hash_blake3: format!("blake3:{}", blake3::hash(&catalog_json)),
        provider_count: artifact.providers.len(),
        model_count: artifact.models.len(),
        routing_route_count: artifact.routing_routes.len(),
    }
}

fn route_filter_failures(routes: &[String]) -> Vec<DispatchAuditFailure> {
    routes
        .iter()
        .filter(|route| parse_route_filter(route).is_none())
        .map(|route| DispatchAuditFailure {
            code: "invalid_route_filter".to_string(),
            message: "route filters must use provider:model".to_string(),
            provider: None,
            route: Some(route.clone()),
            filter_kind: Some("route".to_string()),
            filter_value: Some(route.clone()),
        })
        .collect()
}

fn provider_filter_failures(
    args: &ProviderDispatchAuditArgs,
    artifact: &harn_vm::provider_catalog::ProviderCatalogArtifact,
    unrouted_providers: &[DispatchAuditUnroutedProvider],
) -> Vec<DispatchAuditFailure> {
    if args.providers.is_empty() {
        return Vec::new();
    }
    let catalog_providers: BTreeSet<&str> = artifact
        .providers
        .iter()
        .map(|provider| provider.id.as_str())
        .collect();
    let unrouted_by_provider: BTreeMap<&str, &DispatchAuditUnroutedProvider> = unrouted_providers
        .iter()
        .map(|provider| (provider.provider.as_str(), provider))
        .collect();
    let mut failures = Vec::new();
    for provider in &args.providers {
        if !catalog_providers.contains(provider.as_str()) {
            failures.push(DispatchAuditFailure {
                code: "missing_provider_filter".to_string(),
                message: "provider filter did not match any catalog provider".to_string(),
                provider: Some(provider.clone()),
                route: None,
                filter_kind: Some("provider".to_string()),
                filter_value: Some(provider.clone()),
            });
            continue;
        }
        if let Some(unrouted) = unrouted_by_provider.get(provider.as_str()) {
            failures.push(DispatchAuditFailure {
                code: "provider_has_no_routing_routes".to_string(),
                message: format!(
                    "provider has zero catalog routing routes: {}",
                    unrouted.reason
                ),
                provider: Some(provider.clone()),
                route: None,
                filter_kind: Some("provider".to_string()),
                filter_value: Some(provider.clone()),
            });
        }
    }
    failures
}

fn model_filter_failures(
    args: &ProviderDispatchAuditArgs,
    artifact: &harn_vm::provider_catalog::ProviderCatalogArtifact,
    selected_routes: &[harn_vm::provider_catalog::CatalogRoutingRoute],
) -> Vec<DispatchAuditFailure> {
    if args.models.is_empty() {
        return Vec::new();
    }
    let catalog_route_models: BTreeSet<&str> = artifact
        .routing_routes
        .iter()
        .map(|route| route.model.as_str())
        .collect();
    let selected_models: BTreeSet<&str> = selected_routes
        .iter()
        .map(|route| route.model.as_str())
        .collect();
    args.models
        .iter()
        .filter(|model| {
            !catalog_route_models.contains(model.as_str())
                || !selected_models.contains(model.as_str())
        })
        .map(|model| DispatchAuditFailure {
            code: "missing_model_filter".to_string(),
            message: "model filter did not match any selected catalog routing route".to_string(),
            provider: None,
            route: None,
            filter_kind: Some("model".to_string()),
            filter_value: Some(model.clone()),
        })
        .collect()
}

fn capability_filter_failures(
    args: &ProviderDispatchAuditArgs,
    selected_routes: &[harn_vm::provider_catalog::CatalogRoutingRoute],
) -> Vec<DispatchAuditFailure> {
    if args.capabilities.is_empty() {
        return Vec::new();
    }
    let selected_capabilities: BTreeSet<&str> = selected_routes
        .iter()
        .flat_map(|route| route.capabilities.iter().map(String::as_str))
        .collect();
    args.capabilities
        .iter()
        .filter(|capability| !selected_capabilities.contains(capability.as_str()))
        .map(|capability| DispatchAuditFailure {
            code: "missing_capability_filter".to_string(),
            message: "capability filter did not match any selected catalog routing route"
                .to_string(),
            provider: None,
            route: None,
            filter_kind: Some("capability".to_string()),
            filter_value: Some(capability.clone()),
        })
        .collect()
}

fn missing_route_filter_failures(
    args: &ProviderDispatchAuditArgs,
    selected_routes: &[harn_vm::provider_catalog::CatalogRoutingRoute],
) -> Vec<DispatchAuditFailure> {
    if args.routes.is_empty() {
        return Vec::new();
    }
    let selected: BTreeSet<String> = selected_routes.iter().map(route_key).collect();
    args.routes
        .iter()
        .filter(|route| parse_route_filter(route).is_some())
        .filter(|route| !selected.contains(route.as_str()))
        .map(|route| DispatchAuditFailure {
            code: "missing_route_filter".to_string(),
            message: "route filter did not match any catalog routing route".to_string(),
            provider: None,
            route: Some(route.clone()),
            filter_kind: Some("route".to_string()),
            filter_value: Some(route.clone()),
        })
        .collect()
}

fn unrouted_providers(
    artifact: &harn_vm::provider_catalog::ProviderCatalogArtifact,
) -> Vec<DispatchAuditUnroutedProvider> {
    let route_counts =
        artifact
            .routing_routes
            .iter()
            .fold(BTreeMap::<&str, usize>::new(), |mut counts, route| {
                *counts.entry(route.provider.as_str()).or_insert(0) += 1;
                counts
            });
    let (model_counts, active_model_counts) = artifact.models.iter().fold(
        (
            BTreeMap::<&str, usize>::new(),
            BTreeMap::<&str, usize>::new(),
        ),
        |(mut model_counts, mut active_counts), model| {
            *model_counts.entry(model.provider.as_str()).or_insert(0) += 1;
            if model.deprecation.status == harn_vm::provider_catalog::DeprecationStatus::Active {
                *active_counts.entry(model.provider.as_str()).or_insert(0) += 1;
            }
            (model_counts, active_counts)
        },
    );
    artifact
        .providers
        .iter()
        .filter_map(|provider| {
            let route_count = route_counts
                .get(provider.id.as_str())
                .copied()
                .unwrap_or_default();
            if route_count > 0 {
                return None;
            }
            let model_count = model_counts
                .get(provider.id.as_str())
                .copied()
                .unwrap_or_default();
            let active_model_count = active_model_counts
                .get(provider.id.as_str())
                .copied()
                .unwrap_or_default();
            let reason = if model_count == 0 {
                "catalog_provider_has_no_models"
            } else if active_model_count == 0 {
                "catalog_provider_has_no_active_models"
            } else {
                "catalog_provider_has_no_routing_routes"
            };
            Some(DispatchAuditUnroutedProvider {
                provider: provider.id.clone(),
                model_count,
                active_model_count,
                route_count,
                reason: reason.to_string(),
            })
        })
        .collect()
}

fn parse_route_filter(route: &str) -> Option<(&str, &str)> {
    let (provider, model) = route.split_once(':')?;
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some((provider, model))
}

fn dispatch_audit_variants(
    variants: &[ProviderDispatchAuditVariantArg],
) -> Vec<ProviderDispatchAuditVariantArg> {
    if variants.is_empty() {
        vec![
            ProviderDispatchAuditVariantArg::Default,
            ProviderDispatchAuditVariantArg::Thinking,
            ProviderDispatchAuditVariantArg::Native,
            ProviderDispatchAuditVariantArg::Text,
            ProviderDispatchAuditVariantArg::Json,
        ]
    } else {
        let mut out = Vec::new();
        for variant in variants {
            if !out.contains(variant) {
                out.push(*variant);
            }
        }
        out
    }
}

fn print_human_explanation(report: &DispatchExplanation) {
    println!("dispatch-explain {}:{}", report.provider, report.model);
    println!("  wire_format:      {}", report.wire_format);
    println!("  message_wire:     {}", report.message_wire_format);
    println!("  tool_wire:        {}", report.native_tool_wire_format);
    println!("  base_url_host:    {}", report.base_url_host);
    println!("  tool_format:      {}", report.tool_format);
    println!("  native_tools:     {}", report.native_tools);
    println!(
        "  thinking:         advertised={advertises_thinking}{}",
        if report.requested_thinking {
            " (requested)"
        } else {
            ""
        },
        advertises_thinking = report.advertises_thinking
    );
    if let Some(note) = &report.thinking_note {
        println!("  NOTE: {note}");
    }
}

fn print_json_or_exit(value: &impl Serialize, label: &str) {
    match serde_json::to_string_pretty(value) {
        Ok(json) => println!("{json}"),
        Err(error) => {
            eprintln!("internal error: failed to render {label}: {error}");
            process::exit(1);
        }
    }
}

fn default_base_url_for(wire_format: &str) -> String {
    match wire_format {
        "anthropic_native" => "https://api.anthropic.com/v1".to_string(),
        "gemini" => "https://generativelanguage.googleapis.com/v1beta".to_string(),
        "ollama" => "http://localhost:11434".to_string(),
        _ => "https://api.openai.com/v1".to_string(),
    }
}

fn host_of(base_url: &str) -> String {
    base_url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .map(str::to_string)
        .unwrap_or_else(|| base_url.to_string())
}

#[cfg(test)]
mod tests {
    use crate::cli::ProviderToolProbeModeArg;

    use super::*;

    #[test]
    fn dispatch_audit_tool_probe_plan_separates_live_and_request_audit_commands() {
        let artifact = harn_vm::provider_catalog::artifact();
        let route = artifact
            .routing_routes
            .first()
            .expect("provider catalog should contain at least one routing route");
        let args = ProviderDispatchAuditArgs {
            providers: Vec::new(),
            models: Vec::new(),
            routes: vec![route_key(route)],
            capabilities: Vec::new(),
            variants: Vec::new(),
            include_tool_probe_plan: true,
            tool_probe_cases: vec![ProviderToolProbeCaseArg::SingleToolCall],
            tool_probe_mode: ProviderToolProbeModeArg::NonStreaming,
            tool_probe_repeat: 3,
            tool_probe_timeout_secs: 45,
            tool_probe_output_dir: Some(".harn-runs/provider-live-probes/test".to_string()),
            json: true,
        };

        let report = audit(&args);

        assert_eq!(report.schema_version, 4);
        assert_eq!(report.fail_count, 0, "{:?}", report.failures);
        let plan = report.tool_probe_plan.expect("tool probe plan emitted");
        assert_eq!(plan.schema_version, 4);
        assert_eq!(plan.live_request_profiles, vec!["catalog_default"]);
        assert_eq!(plan.request_audit_profiles, vec!["parameter_edges"]);
        assert_eq!(plan.command_count, 1);
        assert_eq!(plan.request_audit_command_count, 1);
        assert_eq!(plan.matrix.command_count, 1);
        assert_eq!(plan.matrix.request_audit_command_count, 1);

        let live = &plan.commands[0];
        assert_eq!(live.request_profile, "catalog_default");
        assert_eq!(live.repeat, 3);
        assert_eq!(live.timeout_secs, 45);
        assert!(live.argv.contains(&"--repeat".to_string()));
        assert!(!live.argv.contains(&"--dry-run-request".to_string()));

        let request_audit = &plan.request_audit_commands[0];
        assert_eq!(request_audit.request_profile, "parameter_edges");
        assert!(request_audit
            .argv
            .contains(&"--dry-run-request".to_string()));
        assert!(request_audit.argv.contains(&"--json".to_string()));
        assert!(!request_audit.argv.contains(&"--repeat".to_string()));
        assert!(!request_audit.argv.contains(&"--timeout-secs".to_string()));
    }
}
