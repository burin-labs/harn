//! Offline provider tool-call quality scorecards.
//!
//! This module intentionally starts from saved `ToolConformanceReport`
//! envelopes. Live HTTP probing stays with `tool_conformance`; the scorecard is
//! the deterministic aggregation layer that downstream catalog reviews and
//! LoRA promotion receipts can cite without requiring provider credentials in
//! ordinary CI.

use std::collections::{BTreeMap, BTreeSet};

use crate::provider_catalog::{CatalogModel, CatalogProvider};

use super::tool_conformance::{
    ToolConformanceCase, ToolConformanceReport, ToolProbeCase, ToolProbeClassification,
    ToolProbeFallbackMode,
};

pub use super::tool_scorecard_types::*;

pub const TOOL_SCORECARD_SCHEMA_VERSION: u32 = 7;
pub const TOOL_SCORECARD_PLAN_SCHEMA_VERSION: u32 = 4;

#[derive(Debug, Default)]
struct RouteAccumulator {
    provider: String,
    model: String,
    report_count: usize,
    cases: Vec<ToolScorecardObservedCase>,
}

#[derive(Debug)]
struct ToolScorecardObservedCase {
    probe_case: ToolProbeCase,
    case: ToolConformanceCase,
}

#[derive(Debug, Default)]
struct CaseStats {
    case_count: usize,
    required_tool_call_cases: usize,
    successful_cases: usize,
    parseable_tool_call_cases: usize,
    native_tool_call_cases: usize,
    text_tool_call_cases: usize,
    actionless_cases: usize,
    empty_completion_cases: usize,
    malformed_argument_cases: usize,
    http_error_cases: usize,
    transport_error_cases: usize,
    rate_limited_cases: usize,
    latency_ms: Vec<u64>,
    observed_usage_case_count: usize,
    input_token_case_count: usize,
    input_tokens: i64,
    output_token_case_count: usize,
    output_tokens: i64,
    observed_cost_case_count: usize,
    cost_usd: f64,
    classification_counts: BTreeMap<&'static str, usize>,
    observed_wire_dialects: BTreeSet<&'static str>,
}

impl CaseStats {
    fn record(&mut self, observed: &ToolScorecardObservedCase) {
        let case = &observed.case;
        self.case_count += 1;
        if probe_case_requires_new_tool_call(observed.probe_case) {
            self.required_tool_call_cases += 1;
        }
        *self
            .classification_counts
            .entry(classification_key(&case.classification))
            .or_insert(0) += 1;
        self.observed_wire_dialects
            .insert(wire_dialect_key(&case.classification));
        if let Some(elapsed_ms) = case.elapsed_ms {
            self.latency_ms.push(elapsed_ms);
        }
        if case.http_status == Some(429) {
            self.rate_limited_cases += 1;
        }
        if let Some(usage) = &case.usage {
            self.observed_usage_case_count += 1;
            if let Some(input_tokens) = usage.input_tokens {
                self.input_token_case_count += 1;
                self.input_tokens = self.input_tokens.saturating_add(input_tokens.max(0));
            }
            if let Some(output_tokens) = usage.output_tokens {
                self.output_token_case_count += 1;
                self.output_tokens = self.output_tokens.saturating_add(output_tokens.max(0));
            }
            if let Some(cost_usd) = usage
                .cost_usd
                .filter(|cost| cost.is_finite() && *cost >= 0.0)
            {
                self.observed_cost_case_count += 1;
                self.cost_usd += cost_usd;
            }
        }
        if case.ok {
            self.successful_cases += 1;
        }
        match case.classification {
            ToolProbeClassification::StructuredNativeToolCall => {
                self.parseable_tool_call_cases += 1;
                self.native_tool_call_cases += 1;
            }
            ToolProbeClassification::ParseableHarnTextToolCall => {
                self.parseable_tool_call_cases += 1;
                self.text_tool_call_cases += 1;
            }
            ToolProbeClassification::DirectAnswerNoTool
            | ToolProbeClassification::UnavailableToolRepair
            | ToolProbeClassification::DoneSentinel => {}
            ToolProbeClassification::ProseOnlyNonTool => {
                if !case.ok {
                    self.actionless_cases += 1;
                }
            }
            ToolProbeClassification::EmptySilent => {
                self.actionless_cases += 1;
                self.empty_completion_cases += 1;
            }
            ToolProbeClassification::MalformedJsonArguments => {
                self.malformed_argument_cases += 1;
            }
            ToolProbeClassification::HttpError => {
                self.http_error_cases += 1;
            }
            ToolProbeClassification::TransportError => {
                self.transport_error_cases += 1;
            }
            ToolProbeClassification::RawModelToolTag => {}
        }
    }
}

pub fn scorecard_from_tool_reports(reports: Vec<ToolConformanceReport>) -> ToolScorecardReport {
    let catalog_claims = catalog_claims_by_route();
    let mut grouped: BTreeMap<(String, String), RouteAccumulator> = BTreeMap::new();
    for report in reports {
        let key = (report.provider.clone(), report.model.clone());
        let entry = grouped.entry(key).or_insert_with(|| RouteAccumulator {
            provider: report.provider,
            model: report.model,
            report_count: 0,
            cases: Vec::new(),
        });
        entry.report_count += 1;
        entry.cases.extend(
            report
                .cases
                .into_iter()
                .map(|case| ToolScorecardObservedCase {
                    probe_case: report.probe_case,
                    case,
                }),
        );
    }

    let mut routes = grouped
        .into_iter()
        .map(|(key, acc)| score_route(acc, catalog_claims.get(&key).cloned()))
        .collect::<Vec<ToolScorecardRoute>>();
    routes.sort_by(|left, right| {
        status_rank(right.status)
            .cmp(&status_rank(left.status))
            .then_with(|| right.quality_score.cmp(&left.quality_score))
            .then_with(|| left.provider.cmp(&right.provider))
            .then_with(|| left.model.cmp(&right.model))
    });

    let mut summary = ToolScorecardSummary {
        pass: 0,
        warn: 0,
        fail: 0,
        trusted: 0,
        needs_review: 0,
        quarantined: 0,
        best_route: routes.first().map(|route| ToolScorecardRouteKey {
            provider: route.provider.clone(),
            model: route.model.clone(),
            quality_score: route.quality_score,
        }),
    };
    for route in &routes {
        match route.status {
            "pass" => summary.pass += 1,
            "warn" => summary.warn += 1,
            _ => summary.fail += 1,
        }
        match route.trust_status {
            "trusted" => summary.trusted += 1,
            "quarantined" => summary.quarantined += 1,
            _ => summary.needs_review += 1,
        }
    }

    ToolScorecardReport {
        schema_version: TOOL_SCORECARD_SCHEMA_VERSION,
        route_count: routes.len(),
        summary,
        routes,
    }
}

pub fn tool_scorecard_plan_from_catalog(
    route_filters: &[String],
    include_batch_manifest: bool,
) -> Result<ToolScorecardPlan, String> {
    let artifact = crate::provider_catalog::artifact();
    let catalog_json = serde_json::to_vec(&artifact)
        .map_err(|error| format!("error: failed to serialize provider catalog: {error}"))?;
    let catalog_hash = format!("blake3:{}", blake3::hash(&catalog_json));
    let requested_routes = parse_route_filters(route_filters)?;

    let mut seen_routes = BTreeSet::new();
    let mut plan_routes = Vec::new();
    let mut batch_manifest_requests = Vec::new();

    let catalog_claims = catalog_claims_by_route();

    for route in &artifact.routing_routes {
        let route_key = (route.provider.clone(), route.model.clone());
        if !requested_routes.is_empty() && !requested_routes.contains(&route_key) {
            continue;
        }
        seen_routes.insert(route_key);
        let claim = catalog_claims
            .get(&(route.provider.clone(), route.model.clone()))
            .cloned()
            .expect(
                "routing routes are generated from catalog models indexed by id and wire_model",
            );
        let cases = fixed_micro_cases_for_route(&route.provider, &route.model, &claim);
        if include_batch_manifest && claim.batch_api {
            for case in &cases {
                if !case.batch_eligible {
                    continue;
                }
                batch_manifest_requests.push(ToolScorecardBatchManifestRequest {
                    request_id: format!(
                        "tool-scorecard:{}:{}:{}",
                        route.provider, route.model, case.id
                    ),
                    provider: route.provider.clone(),
                    model: route.model.clone(),
                    case_id: case.id,
                    batch_wire_format: claim.batch_wire_format.clone(),
                    batch_input_mode: claim.batch_input_mode.clone(),
                });
            }
        }
        let readiness = readiness_plan_for_route(&route.provider, &route.model);
        plan_routes.push(ToolScorecardPlanRoute {
            provider: route.provider.clone(),
            model: route.model.clone(),
            trust_status: "needs_review",
            trust_reasons: vec!["missing_live_evidence"],
            readiness,
            catalog_claim: claim,
            cases,
        });
    }

    if !requested_routes.is_empty() {
        let missing = requested_routes
            .difference(&seen_routes)
            .map(|(provider, model)| format!("{provider}:{model}"))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!(
                "error: route(s) not found in provider catalog: {}",
                missing.join(", ")
            ));
        }
    }

    let case_count = plan_routes.iter().map(|route| route.cases.len()).sum();
    let readiness_command_count = plan_routes
        .iter()
        .filter(|route| route.readiness.command.is_some())
        .count();
    let required_case_count = plan_routes
        .iter()
        .flat_map(|route| &route.cases)
        .filter(|case| case.requirement == "required")
        .count();
    let unscorecardable_providers = unscorecardable_providers(&artifact);

    Ok(ToolScorecardPlan {
        schema_version: TOOL_SCORECARD_PLAN_SCHEMA_VERSION,
        kind: "plan",
        catalog: ToolScorecardCatalogProvenance {
            schema_version: artifact.schema_version,
            generated_by: artifact.generated_by,
            hash_blake3: catalog_hash,
        },
        route_count: plan_routes.len(),
        readiness_command_count,
        unscorecardable_provider_count: unscorecardable_providers.len(),
        case_count,
        required_case_count,
        batch_manifest_request_count: batch_manifest_requests.len(),
        routes: plan_routes,
        unscorecardable_providers,
        batch_manifest_requests,
    })
}

fn readiness_plan_for_route(provider: &str, model: &str) -> ToolScorecardReadinessPlan {
    let Some(def) = crate::llm_config::provider_config(provider) else {
        return readiness_missing_runner("unknown_provider");
    };
    if !crate::llm::readiness::supports_model_readiness_probe(&def) {
        return readiness_missing_runner("provider_has_no_model_readiness_probe");
    }
    ToolScorecardReadinessPlan {
        status: "executable",
        runner: "provider_probe",
        reason: "route_model_inventory_gate",
        command: Some(vec![
            "harn".to_string(),
            "provider".to_string(),
            "probe".to_string(),
            provider.to_string(),
            "--model".to_string(),
            model.to_string(),
            "--json".to_string(),
        ]),
        artifact_hint: Some(format!(
            "provider-readiness-{}-{}.json",
            artifact_segment(provider),
            artifact_segment(model)
        )),
    }
}

fn readiness_missing_runner(reason: &'static str) -> ToolScorecardReadinessPlan {
    ToolScorecardReadinessPlan {
        status: "missing_runner",
        runner: "none",
        reason,
        command: None,
        artifact_hint: None,
    }
}

fn unscorecardable_providers(
    artifact: &crate::provider_catalog::ProviderCatalogArtifact,
) -> Vec<ToolScorecardUnscorecardableProvider> {
    let mut route_counts = BTreeMap::<&str, usize>::new();
    for route in &artifact.routing_routes {
        *route_counts.entry(route.provider.as_str()).or_default() += 1;
    }
    let mut model_counts = BTreeMap::<&str, usize>::new();
    let mut active_model_counts = BTreeMap::<&str, usize>::new();
    for model in &artifact.models {
        *model_counts.entry(model.provider.as_str()).or_default() += 1;
        if model.deprecation.status == crate::provider_catalog::DeprecationStatus::Active {
            *active_model_counts
                .entry(model.provider.as_str())
                .or_default() += 1;
        }
    }
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
            Some(ToolScorecardUnscorecardableProvider {
                provider: provider.id.clone(),
                reason: unscorecardable_provider_reason(provider, model_count, active_model_count),
                model_count,
                active_model_count,
                route_count,
                local_runtime: provider.local_runtime.is_some(),
                auth_required: provider.auth.required,
                credential_env_names: provider.auth.env.clone(),
            })
        })
        .collect()
}

fn unscorecardable_provider_reason(
    provider: &CatalogProvider,
    model_count: usize,
    active_model_count: usize,
) -> &'static str {
    if model_count == 0 && provider.local_runtime.is_some() {
        return "requires_runtime_model";
    }
    if model_count == 0 {
        return "catalog_provider_has_no_models";
    }
    if active_model_count == 0 {
        return "catalog_provider_has_no_active_models";
    }
    "catalog_provider_has_no_routing_routes"
}

fn catalog_claims_by_route() -> BTreeMap<(String, String), ToolScorecardCatalogClaim> {
    let artifact = crate::provider_catalog::artifact();
    let provider_by_id = providers_by_id(&artifact.providers);
    let mut claims = BTreeMap::new();
    for model in &artifact.models {
        let claim = catalog_claim_for_model(model, &provider_by_id);
        claims.insert((model.provider.clone(), model.id.clone()), claim.clone());
        if let Some(wire_model) = model.wire_model.as_ref() {
            claims.insert((model.provider.clone(), wire_model.clone()), claim);
        }
    }
    claims
}

fn providers_by_id(providers: &[CatalogProvider]) -> BTreeMap<&str, &CatalogProvider> {
    providers
        .iter()
        .map(|provider| (provider.id.as_str(), provider))
        .collect()
}

fn catalog_claim_for_model(
    model: &CatalogModel,
    provider_by_id: &BTreeMap<&str, &CatalogProvider>,
) -> ToolScorecardCatalogClaim {
    let caps = crate::llm::capabilities::lookup(&model.provider, &model.id);
    let provider = provider_by_id.get(model.provider.as_str()).copied();
    ToolScorecardCatalogClaim {
        preferred_tool_format: model.tool_support.preferred_format.clone(),
        tool_mode_parity: model.tool_support.parity.clone(),
        native_tools: model.tool_support.native,
        text_tools: model.tool_support.text,
        text_tool_wire_format_supported: model.tool_support.text,
        max_tools: model.tool_support.max_tools,
        supports_parallel_tool_calls: caps.supports_parallel_tool_calls,
        server_parser: caps.server_parser,
        tool_search: model.tool_support.tool_search.clone(),
        batch_api: model.batch.is_some(),
        batch_wire_format: model.batch.as_ref().map(|batch| batch.wire_format.clone()),
        batch_input_mode: model.batch.as_ref().map(|batch| batch.input_mode.clone()),
        batch_discount_percent: model
            .batch
            .as_ref()
            .and_then(|batch| batch.discount_percent),
        provider_rate_limits: provider
            .and_then(|provider| provider.rate_limits.as_ref())
            .is_some(),
        model_rate_limits: model.rate_limits.is_some(),
        provider_rpm: provider.and_then(|provider| provider.rpm),
        pricing: model.pricing.is_some(),
        provider_latency_p50_ms: provider.and_then(|provider| provider.latency_p50_ms),
    }
}

fn parse_route_filters(filters: &[String]) -> Result<BTreeSet<(String, String)>, String> {
    let mut routes = BTreeSet::new();
    for filter in filters {
        let Some((provider, model)) = filter.split_once(':') else {
            return Err(format!(
                "error: route filter '{filter}' must use provider:model"
            ));
        };
        let provider = provider.trim();
        let model = model.trim();
        if provider.is_empty() || model.is_empty() {
            return Err(format!(
                "error: route filter '{filter}' must include both provider and model"
            ));
        }
        routes.insert((provider.to_string(), model.to_string()));
    }
    Ok(routes)
}

fn fixed_micro_cases_for_route(
    provider: &str,
    model: &str,
    claim: &ToolScorecardCatalogClaim,
) -> Vec<ToolScorecardPlanCase> {
    let has_tool_surface = claim.native_tools || claim.text_tools;
    let single_tool_requirement =
        tool_surface_requirement(has_tool_surface, "baseline_tool_call_quality");
    let large_string_requirement =
        tool_surface_requirement(has_tool_surface, "byte_fidelity_for_edit_payloads");
    let tool_result_requirement =
        tool_surface_requirement(has_tool_surface, "multi_turn_agent_loop_quality");
    let signed_thinking_requirement = if has_tool_surface {
        (
            "conditional",
            "thinking_signature_replay_for_reasoning_tool_models",
        )
    } else {
        ("not_applicable", "route_declares_no_tool_surface")
    };
    let parallel_requirement = if claim.native_tools && claim.supports_parallel_tool_calls {
        ("required", "route_claims_parallel_tool_calls")
    } else {
        ("not_applicable", "route_does_not_claim_parallel_tool_calls")
    };
    vec![
        ToolScorecardPlanCase {
            id: "single_tool_call",
            description: "single ordinary tool call with exact JSON arguments",
            requirement: single_tool_requirement.0,
            requirement_reason: single_tool_requirement.1,
            turn_count: 1,
            batch_eligible: true,
            probe_focus: vec!["tool_choice", "json_arguments", "wire_dialect"],
            execution: if has_tool_surface {
                executable_tool_probe_case(provider, model, ToolProbeCase::SingleToolCall)
            } else {
                not_applicable_case("route_declares_no_tool_surface")
            },
        },
        ToolScorecardPlanCase {
            id: "parallel_tool_calls",
            description: "multiple tool calls in one assistant response",
            requirement: parallel_requirement.0,
            requirement_reason: parallel_requirement.1,
            turn_count: 1,
            batch_eligible: true,
            probe_focus: vec!["parallel_dispatch", "tool_call_count", "argument_binding"],
            execution: if claim.native_tools && claim.supports_parallel_tool_calls {
                executable_tool_probe_case(provider, model, ToolProbeCase::ParallelToolCalls)
            } else {
                not_applicable_case(parallel_requirement.1)
            },
        },
        ToolScorecardPlanCase {
            id: "large_string_argument",
            description: "large string/code argument with quotes, unicode, and heredoc-shaped text",
            requirement: large_string_requirement.0,
            requirement_reason: large_string_requirement.1,
            turn_count: 1,
            batch_eligible: true,
            probe_focus: vec!["byte_fidelity", "escaping", "unicode"],
            execution: if has_tool_surface {
                executable_tool_probe_case(provider, model, ToolProbeCase::LargeStringArgument)
            } else {
                not_applicable_case("route_declares_no_tool_surface")
            },
        },
        ToolScorecardPlanCase {
            id: "tool_result_followup",
            description: "assistant continuation after receiving a tool result",
            requirement: tool_result_requirement.0,
            requirement_reason: tool_result_requirement.1,
            turn_count: 2,
            batch_eligible: false,
            probe_focus: vec!["tool_result_adjacency", "continuation", "action_vs_prose"],
            execution: if has_tool_surface {
                executable_tool_probe_case(provider, model, ToolProbeCase::ToolResultFollowup)
            } else {
                not_applicable_case("route_declares_no_tool_surface")
            },
        },
        ToolScorecardPlanCase {
            id: "signed_thinking_tool_result_followup",
            description: "provider-native signed thinking replay adjacent to tool-use history",
            requirement: signed_thinking_requirement.0,
            requirement_reason: signed_thinking_requirement.1,
            turn_count: 2,
            batch_eligible: false,
            probe_focus: vec![
                "signed_thinking_replay",
                "tool_result_adjacency",
                "history_preservation",
            ],
            execution: if has_tool_surface
                && signed_thinking_tool_history_supported(provider, model)
            {
                executable_signed_thinking_request_case(provider, model)
            } else if !has_tool_surface {
                not_applicable_case("route_declares_no_tool_surface")
            } else {
                not_applicable_case("route_has_no_signed_thinking_tool_history_surface")
            },
        },
        ToolScorecardPlanCase {
            id: "no_tool_answer_or_refusal",
            description: "plain answer or refusal when no tool should be called",
            requirement: "required",
            requirement_reason: "spurious_tool_call_guard",
            turn_count: 1,
            batch_eligible: true,
            probe_focus: vec!["no_tool", "refusal", "answer_quality"],
            execution: executable_tool_probe_case(
                provider,
                model,
                ToolProbeCase::NoToolAnswerOrRefusal,
            ),
        },
        ToolScorecardPlanCase {
            id: "unavailable_tool_repair",
            description: "repair or reject a request for an unavailable tool",
            requirement: "required",
            requirement_reason: "unsafe_or_unavailable_tool_recovery",
            turn_count: 1,
            batch_eligible: true,
            probe_focus: vec!["tool_name_repair", "no_unsafe_args", "recovery"],
            execution: executable_tool_probe_case(
                provider,
                model,
                ToolProbeCase::UnavailableToolRepair,
            ),
        },
        ToolScorecardPlanCase {
            id: "done_sentinel",
            description: "completion-contract done sentinel emission",
            requirement: "required",
            requirement_reason: "agent_completion_contract",
            turn_count: 1,
            batch_eligible: true,
            probe_focus: vec!["done_sentinel", "completion_contract"],
            execution: executable_tool_probe_case(provider, model, ToolProbeCase::DoneSentinel),
        },
        ToolScorecardPlanCase {
            id: "parameter_edges",
            description: "provider-advertised parameter edge behavior",
            requirement: "required",
            requirement_reason: "catalog_parameter_claims",
            turn_count: 1,
            batch_eligible: true,
            probe_focus: vec!["temperature", "max_tokens", "tool_choice"],
            execution: executable_parameter_edges_request_case(provider, model, has_tool_surface),
        },
    ]
}

fn tool_surface_requirement(
    has_tool_surface: bool,
    required_reason: &'static str,
) -> (&'static str, &'static str) {
    if has_tool_surface {
        ("required", required_reason)
    } else {
        ("not_applicable", "route_declares_no_tool_surface")
    }
}

pub(crate) fn signed_thinking_tool_history_supported(provider: &str, model: &str) -> bool {
    let caps = crate::llm::capabilities::lookup(provider, model);
    let thinking_capable = !caps.thinking_modes.is_empty()
        || caps.interleaved_thinking_supported
        || caps.reasoning_effort_supported;
    thinking_capable
        && (matches!(
            caps.message_wire_format,
            crate::llm::capabilities::WireDialect::Anthropic
                | crate::llm::capabilities::WireDialect::Gemini
        ) || provider == "vertex")
}

fn executable_tool_probe_case(
    provider: &str,
    model: &str,
    probe_case: ToolProbeCase,
) -> ToolScorecardPlanCaseExecution {
    let probe_case_id = probe_case.as_str();
    ToolScorecardPlanCaseExecution {
        status: "executable",
        runner: "provider_tool_probe",
        reason: match probe_case {
            ToolProbeCase::SingleToolCall => {
                "harn provider tool-probe executes the single echo_marker tool-call transport probe"
            }
            ToolProbeCase::ParallelToolCalls => {
                "harn provider tool-probe executes the parallel echo_marker tool-call transport probe"
            }
            ToolProbeCase::LargeStringArgument => {
                "harn provider tool-probe executes the large string argument byte-fidelity probe"
            }
            ToolProbeCase::ToolResultFollowup => {
                "harn provider tool-probe executes the tool-result follow-up continuation probe"
            }
            ToolProbeCase::SignedThinkingToolResultFollowup => {
                "harn provider tool-probe renders the signed-thinking tool-result follow-up probe"
            }
            ToolProbeCase::NoToolAnswerOrRefusal => {
                "harn provider tool-probe executes the no-tool direct-answer fixture"
            }
            ToolProbeCase::UnavailableToolRepair => {
                "harn provider tool-probe executes the unavailable-tool repair fixture"
            }
            ToolProbeCase::DoneSentinel => {
                "harn provider tool-probe executes the completion-sentinel fixture"
            }
        },
        command: Some(vec![
            "harn".to_string(),
            "provider".to_string(),
            "tool-probe".to_string(),
            provider.to_string(),
            "--model".to_string(),
            model.to_string(),
            "--mode".to_string(),
            "both".to_string(),
            "--case".to_string(),
            probe_case_id.to_string(),
            "--repeat".to_string(),
            "1".to_string(),
            "--timeout-secs".to_string(),
            "120".to_string(),
            "--json".to_string(),
        ]),
        artifact_hint: Some(format!(
            "tool-probe-{}-{}-{}.json",
            artifact_segment(provider),
            artifact_segment(model),
            probe_case_id
        )),
    }
}

fn executable_signed_thinking_request_case(
    provider: &str,
    model: &str,
) -> ToolScorecardPlanCaseExecution {
    let probe_case = ToolProbeCase::SignedThinkingToolResultFollowup;
    ToolScorecardPlanCaseExecution {
        status: "executable",
        runner: "provider_tool_probe_request",
        reason: "harn provider tool-probe --dry-run-request renders and validates provider-native signed-thinking tool-history replay without provider calls",
        command: Some(vec![
            "harn".to_string(),
            "provider".to_string(),
            "tool-probe".to_string(),
            provider.to_string(),
            "--model".to_string(),
            model.to_string(),
            "--mode".to_string(),
            "both".to_string(),
            "--case".to_string(),
            probe_case.as_str().to_string(),
            "--dry-run-request".to_string(),
            "--json".to_string(),
        ]),
        artifact_hint: Some(format!(
            "tool-probe-request-{}-{}-{}.json",
            artifact_segment(provider),
            artifact_segment(model),
            probe_case.as_str()
        )),
    }
}

fn executable_parameter_edges_request_case(
    provider: &str,
    model: &str,
    has_tool_surface: bool,
) -> ToolScorecardPlanCaseExecution {
    let probe_case = if has_tool_surface {
        ToolProbeCase::SingleToolCall
    } else {
        ToolProbeCase::NoToolAnswerOrRefusal
    };
    ToolScorecardPlanCaseExecution {
        status: "executable",
        runner: "provider_tool_probe_request",
        reason: "harn provider tool-probe --dry-run-request renders and validates provider-specific parameter-edge request bodies without provider calls",
        command: Some(vec![
            "harn".to_string(),
            "provider".to_string(),
            "tool-probe".to_string(),
            provider.to_string(),
            "--model".to_string(),
            model.to_string(),
            "--mode".to_string(),
            "both".to_string(),
            "--case".to_string(),
            probe_case.as_str().to_string(),
            "--request-profile".to_string(),
            "parameter_edges".to_string(),
            "--dry-run-request".to_string(),
            "--json".to_string(),
        ]),
        artifact_hint: Some(format!(
            "tool-probe-request-{}-{}-parameter_edges.json",
            artifact_segment(provider),
            artifact_segment(model)
        )),
    }
}

fn not_applicable_case(reason: &'static str) -> ToolScorecardPlanCaseExecution {
    ToolScorecardPlanCaseExecution {
        status: "not_applicable",
        runner: "none",
        reason,
        command: None,
        artifact_hint: None,
    }
}

fn artifact_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '-',
        })
        .collect()
}

fn score_route(
    acc: RouteAccumulator,
    catalog_claim: Option<ToolScorecardCatalogClaim>,
) -> ToolScorecardRoute {
    let mut stats = CaseStats::default();
    let mut stats_by_mode: BTreeMap<&'static str, CaseStats> = BTreeMap::new();

    for case in &acc.cases {
        stats.record(case);
        stats_by_mode
            .entry(case.case.mode.as_str())
            .or_default()
            .record(case);
    }

    let observed_probe_cases = observed_probe_cases(&acc.cases);
    let observed_probe_evidence = observed_probe_evidence(&acc.cases);
    let missing_required_probe_evidence = missing_required_probe_evidence(
        &acc.provider,
        &acc.model,
        catalog_claim.as_ref(),
        &observed_probe_evidence,
    );
    let missing_required_request_cases =
        missing_required_request_cases(&acc.provider, &acc.model, catalog_claim.as_ref());
    let probe_evidence_status = if missing_required_probe_evidence.is_empty() {
        "complete"
    } else {
        "partial"
    };
    let request_evidence_status = if missing_required_request_cases.is_empty() {
        "complete"
    } else {
        "partial"
    };
    let evidence_status =
        if probe_evidence_status == "complete" && request_evidence_status == "complete" {
            "complete"
        } else {
            "partial"
        };
    let missing_required_cases = missing_required_case_ids(
        &missing_required_probe_evidence,
        &missing_required_request_cases,
    );
    let case_count = stats.case_count;
    let pass_rate = rate(stats.successful_cases, case_count);
    let parseable_tool_call_rate = rate(stats.parseable_tool_call_cases, case_count);
    let empty_completion_rate = rate(stats.empty_completion_cases, case_count);
    let actionless_rate = rate(stats.actionless_cases, case_count);
    let quality_score = ((pass_rate * 100.0).round() as u16).min(100);
    let recommended_tool_mode =
        recommended_tool_mode(stats.native_tool_call_cases, stats.text_tool_call_cases);
    let (catalog_mismatches, suggested_catalog_updates) =
        catalog_drift(&catalog_claim, recommended_tool_mode);
    let issues = route_issues(
        &stats,
        recommended_tool_mode,
        !missing_required_probe_evidence.is_empty(),
        !missing_required_request_cases.is_empty(),
    );
    let status = route_status(
        recommended_tool_mode,
        stats.required_tool_call_cases,
        stats.successful_cases,
        case_count,
        evidence_status,
        &issues,
    );
    let trust_status = route_trust_status(status, recommended_tool_mode, evidence_status, &issues);
    let trust_reasons = route_trust_reasons(status, evidence_status, &issues, &catalog_mismatches);
    let mode_evidence = stats_by_mode
        .into_iter()
        .map(|(mode, mode_stats)| mode_evidence(mode, mode_stats))
        .collect();

    ToolScorecardRoute {
        provider: acc.provider,
        model: acc.model,
        catalog_claim,
        report_count: acc.report_count,
        case_count,
        successful_cases: stats.successful_cases,
        parseable_tool_call_cases: stats.parseable_tool_call_cases,
        native_tool_call_cases: stats.native_tool_call_cases,
        text_tool_call_cases: stats.text_tool_call_cases,
        actionless_cases: stats.actionless_cases,
        empty_completion_cases: stats.empty_completion_cases,
        malformed_argument_cases: stats.malformed_argument_cases,
        http_error_cases: stats.http_error_cases,
        transport_error_cases: stats.transport_error_cases,
        rate_limited_cases: stats.rate_limited_cases,
        observed_latency_case_count: stats.latency_ms.len(),
        latency_p50_ms: upper_percentile_ms(&stats.latency_ms, 50),
        latency_p95_ms: upper_percentile_ms(&stats.latency_ms, 95),
        observed_usage_case_count: stats.observed_usage_case_count,
        input_tokens: observed_i64_sum(stats.input_token_case_count, stats.input_tokens),
        output_tokens: observed_i64_sum(stats.output_token_case_count, stats.output_tokens),
        observed_cost_case_count: stats.observed_cost_case_count,
        cost_usd: observed_cost_sum(stats.observed_cost_case_count, stats.cost_usd),
        pass_rate,
        parseable_tool_call_rate,
        empty_completion_rate,
        actionless_rate,
        quality_score,
        status,
        trust_status,
        trust_reasons,
        evidence_status,
        probe_evidence_status,
        request_evidence_status,
        recommended_tool_mode,
        observed_probe_cases,
        missing_required_cases,
        observed_probe_evidence,
        missing_required_probe_evidence,
        missing_required_request_cases,
        observed_wire_dialects: stats.observed_wire_dialects.into_iter().collect(),
        classification_counts: stats.classification_counts,
        mode_evidence,
        issues,
        catalog_mismatches,
        suggested_catalog_updates,
    }
}

fn route_trust_status(
    status: &str,
    recommended_tool_mode: &str,
    evidence_status: &str,
    issues: &[&'static str],
) -> &'static str {
    if status == "pass" && evidence_status == "complete" && issues.is_empty() {
        return "trusted";
    }
    if status == "fail" || recommended_tool_mode == "disabled" {
        return "quarantined";
    }
    "needs_review"
}

fn route_trust_reasons(
    status: &str,
    evidence_status: &str,
    issues: &[&'static str],
    catalog_mismatches: &[ToolScorecardCatalogMismatch],
) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if status == "fail" {
        reasons.push("scorecard_failed");
    }
    if evidence_status != "complete" {
        reasons.push("incomplete_evidence");
    }
    reasons.extend(issues.iter().copied());
    if !catalog_mismatches.is_empty() {
        reasons.push("catalog_drift");
    }
    if reasons.is_empty() {
        reasons.push("scorecard_passed");
    }
    reasons.sort();
    reasons.dedup();
    reasons
}

fn mode_evidence(mode: &'static str, stats: CaseStats) -> ToolScorecardModeEvidence {
    let pass_rate = rate(stats.successful_cases, stats.case_count);
    let recommended_tool_mode =
        recommended_tool_mode(stats.native_tool_call_cases, stats.text_tool_call_cases);
    let issues = route_issues(&stats, recommended_tool_mode, false, false);
    let status = route_status(
        recommended_tool_mode,
        stats.required_tool_call_cases,
        stats.successful_cases,
        stats.case_count,
        "complete",
        &issues,
    );
    ToolScorecardModeEvidence {
        mode,
        case_count: stats.case_count,
        successful_cases: stats.successful_cases,
        parseable_tool_call_cases: stats.parseable_tool_call_cases,
        native_tool_call_cases: stats.native_tool_call_cases,
        text_tool_call_cases: stats.text_tool_call_cases,
        actionless_cases: stats.actionless_cases,
        empty_completion_cases: stats.empty_completion_cases,
        malformed_argument_cases: stats.malformed_argument_cases,
        http_error_cases: stats.http_error_cases,
        transport_error_cases: stats.transport_error_cases,
        rate_limited_cases: stats.rate_limited_cases,
        observed_latency_case_count: stats.latency_ms.len(),
        latency_p50_ms: upper_percentile_ms(&stats.latency_ms, 50),
        latency_p95_ms: upper_percentile_ms(&stats.latency_ms, 95),
        observed_usage_case_count: stats.observed_usage_case_count,
        input_tokens: observed_i64_sum(stats.input_token_case_count, stats.input_tokens),
        output_tokens: observed_i64_sum(stats.output_token_case_count, stats.output_tokens),
        observed_cost_case_count: stats.observed_cost_case_count,
        cost_usd: observed_cost_sum(stats.observed_cost_case_count, stats.cost_usd),
        pass_rate,
        recommended_tool_mode,
        status,
        classification_counts: stats.classification_counts,
        issues,
    }
}

fn catalog_drift(
    claim: &Option<ToolScorecardCatalogClaim>,
    recommended_tool_mode: &str,
) -> (
    Vec<ToolScorecardCatalogMismatch>,
    Vec<ToolScorecardCatalogUpdate>,
) {
    let Some(claim) = claim else {
        return (
            vec![ToolScorecardCatalogMismatch {
                code: "route_missing_from_catalog",
                field: "provider_catalog.models",
                observed: recommended_tool_mode.to_string(),
                catalog: None,
            }],
            Vec::new(),
        );
    };

    let mut mismatches = Vec::new();
    let mut updates = Vec::new();
    let preferred = claim.preferred_tool_format.as_deref();
    let recommended_preferred_format = recommended_preferred_format(recommended_tool_mode);

    if let (Some(preferred), Some(recommended_preferred_format)) =
        (preferred, recommended_preferred_format)
    {
        if !tool_format_matches_mode(preferred, recommended_tool_mode) {
            mismatches.push(ToolScorecardCatalogMismatch {
                code: "preferred_tool_format_disagrees",
                field: "tool_support.preferred_format",
                observed: recommended_tool_mode.to_string(),
                catalog: Some(preferred.to_string()),
            });
            updates.push(set_catalog_update(
                "tool_support.preferred_format",
                recommended_preferred_format.to_string(),
                "scorecard_recommended_tool_mode",
            ));
        }
    }

    match recommended_tool_mode {
        "native" if !claim.native_tools => {
            mismatches.push(ToolScorecardCatalogMismatch {
                code: "observed_native_not_cataloged",
                field: "tool_support.native",
                observed: "true".to_string(),
                catalog: Some("false".to_string()),
            });
            updates.push(set_catalog_update(
                "tool_support.native",
                "true".to_string(),
                "scorecard_observed_native_tool_calls",
            ));
        }
        "text" if !claim.text_tools => {
            mismatches.push(ToolScorecardCatalogMismatch {
                code: "observed_text_not_cataloged",
                field: "tool_support.text",
                observed: "true".to_string(),
                catalog: Some("false".to_string()),
            });
            updates.push(set_catalog_update(
                "tool_support.text",
                "true".to_string(),
                "scorecard_observed_text_tool_calls",
            ));
        }
        _ => {}
    }

    (mismatches, updates)
}

fn tool_format_matches_mode(format: &str, recommended_tool_mode: &str) -> bool {
    let Some(format_channel) = crate::llm_config::tool_format_channel(format) else {
        return false;
    };
    matches!(
        (format_channel, recommended_tool_mode),
        (crate::llm_config::ToolFormatChannel::Native, "native")
            | (crate::llm_config::ToolFormatChannel::Text, "text")
    )
}

fn recommended_preferred_format(recommended_tool_mode: &str) -> Option<&'static str> {
    match recommended_tool_mode {
        "native" => Some("native"),
        // A scorecard text-channel observation does not distinguish heredoc
        // from fenced JSON; prefer Harn's safer global text-channel default.
        "text" => Some("json"),
        _ => None,
    }
}

fn set_catalog_update(
    field: &'static str,
    value: String,
    reason: &'static str,
) -> ToolScorecardCatalogUpdate {
    ToolScorecardCatalogUpdate {
        field,
        operation: "set",
        value: Some(value),
        reason,
    }
}

fn rate(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn recommended_tool_mode(native_cases: usize, text_cases: usize) -> &'static str {
    if native_cases > 0 {
        ToolProbeFallbackMode::Native.as_str()
    } else if text_cases > 0 {
        ToolProbeFallbackMode::Text.as_str()
    } else {
        ToolProbeFallbackMode::Disabled.as_str()
    }
}

fn route_status(
    recommended_tool_mode: &str,
    required_tool_call_cases: usize,
    successful_cases: usize,
    case_count: usize,
    evidence_status: &str,
    issues: &[&'static str],
) -> &'static str {
    if case_count == 0
        || successful_cases == 0
        || (required_tool_call_cases > 0 && recommended_tool_mode == "disabled")
    {
        "fail"
    } else if evidence_status != "complete" || successful_cases < case_count || !issues.is_empty() {
        "warn"
    } else {
        "pass"
    }
}

fn route_issues(
    stats: &CaseStats,
    recommended_tool_mode: &str,
    missing_required_probe_evidence: bool,
    missing_required_request_evidence: bool,
) -> Vec<&'static str> {
    let mut issues = Vec::new();
    if stats.case_count == 0 {
        issues.push("no_cases");
    }
    if stats.required_tool_call_cases > 0 && recommended_tool_mode == "disabled" {
        issues.push("tool_calling_disabled");
    }
    if stats.successful_cases > 0 && stats.successful_cases < stats.case_count {
        issues.push("partial_tool_call_pass_rate");
    }
    if missing_required_probe_evidence {
        issues.push("incomplete_required_probe_evidence");
    }
    if missing_required_request_evidence {
        issues.push("incomplete_required_request_evidence");
    }
    if stats.actionless_cases > 0 {
        issues.push("empty_or_actionless_completion");
    }
    if stats.malformed_argument_cases > 0 {
        issues.push("malformed_tool_arguments");
    }
    if stats.http_error_cases > 0 {
        issues.push("provider_http_errors");
    }
    if stats.rate_limited_cases > 0 {
        issues.push("provider_rate_limited");
    }
    if stats.transport_error_cases > 0 {
        issues.push("transport_errors");
    }
    issues
}

fn upper_percentile_ms(samples: &[u64], percentile: usize) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let index = (sorted.len() * percentile / 100).min(sorted.len() - 1);
    Some(sorted[index])
}

fn observed_i64_sum(observed_count: usize, total: i64) -> Option<i64> {
    (observed_count > 0).then_some(total)
}

fn observed_cost_sum(observed_count: usize, total: f64) -> Option<f64> {
    (observed_count > 0).then_some(total)
}

fn observed_probe_cases(cases: &[ToolScorecardObservedCase]) -> Vec<&'static str> {
    cases
        .iter()
        .map(|case| case.probe_case.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn observed_probe_evidence(cases: &[ToolScorecardObservedCase]) -> Vec<ToolScorecardProbeEvidence> {
    cases
        .iter()
        .filter_map(|case| {
            scorecard_probe_case_id(case.probe_case.as_str()).map(|case_id| {
                ToolScorecardProbeEvidence {
                    case_id,
                    mode: case.case.mode.as_str(),
                }
            })
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn missing_required_probe_evidence(
    provider: &str,
    model: &str,
    catalog_claim: Option<&ToolScorecardCatalogClaim>,
    observed_probe_evidence: &[ToolScorecardProbeEvidence],
) -> Vec<ToolScorecardProbeEvidence> {
    let observed = observed_probe_evidence
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    required_scorecard_probe_evidence(provider, model, catalog_claim)
        .into_iter()
        .filter(|case| !observed.contains(case))
        .collect()
}

fn required_scorecard_probe_evidence(
    provider: &str,
    model: &str,
    catalog_claim: Option<&ToolScorecardCatalogClaim>,
) -> Vec<ToolScorecardProbeEvidence> {
    if let Some(claim) = catalog_claim {
        return fixed_micro_cases_for_route(provider, model, claim)
            .into_iter()
            .filter(|case| case.requirement == "required")
            .filter(|case| case.execution.runner == "provider_tool_probe")
            .filter_map(|case| scorecard_probe_case_id(case.id).map(|case_id| (case_id, case)))
            .flat_map(|(case_id, case)| {
                scorecard_probe_modes(&case.execution)
                    .into_iter()
                    .map(move |mode| ToolScorecardProbeEvidence { case_id, mode })
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    [
        "single_tool_call",
        "large_string_argument",
        "tool_result_followup",
        "no_tool_answer_or_refusal",
        "unavailable_tool_repair",
        "done_sentinel",
    ]
    .into_iter()
    .flat_map(|case_id| {
        ["non_streaming", "streaming"]
            .into_iter()
            .map(move |mode| ToolScorecardProbeEvidence { case_id, mode })
    })
    .collect()
}

fn missing_required_request_cases(
    provider: &str,
    model: &str,
    catalog_claim: Option<&ToolScorecardCatalogClaim>,
) -> Vec<&'static str> {
    let Some(claim) = catalog_claim else {
        return Vec::new();
    };
    fixed_micro_cases_for_route(provider, model, claim)
        .into_iter()
        .filter(|case| case.requirement == "required")
        .filter(|case| case.execution.runner == "provider_tool_probe_request")
        .map(|case| case.id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn missing_required_case_ids(
    missing_probe_evidence: &[ToolScorecardProbeEvidence],
    missing_request_cases: &[&'static str],
) -> Vec<&'static str> {
    missing_probe_evidence
        .iter()
        .map(|evidence| evidence.case_id)
        .chain(missing_request_cases.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn scorecard_probe_modes(execution: &ToolScorecardPlanCaseExecution) -> Vec<&'static str> {
    let Some(command) = execution.command.as_ref() else {
        return Vec::new();
    };
    let mode = command
        .windows(2)
        .find_map(|pair| (pair[0] == "--mode").then_some(pair[1].as_str()))
        .unwrap_or("both");
    match mode {
        "non-streaming" | "non_streaming" => vec!["non_streaming"],
        "streaming" => vec!["streaming"],
        "both" => vec!["non_streaming", "streaming"],
        _ => vec!["non_streaming", "streaming"],
    }
}

fn scorecard_probe_case_id(case_id: &'static str) -> Option<&'static str> {
    match case_id {
        "single_tool_call"
        | "parallel_tool_calls"
        | "large_string_argument"
        | "tool_result_followup"
        | "signed_thinking_tool_result_followup"
        | "no_tool_answer_or_refusal"
        | "unavailable_tool_repair"
        | "done_sentinel" => Some(case_id),
        _ => None,
    }
}

fn status_rank(status: &str) -> u8 {
    match status {
        "pass" => 2,
        "warn" => 1,
        _ => 0,
    }
}

fn probe_case_requires_new_tool_call(probe_case: ToolProbeCase) -> bool {
    matches!(
        probe_case,
        ToolProbeCase::SingleToolCall
            | ToolProbeCase::ParallelToolCalls
            | ToolProbeCase::LargeStringArgument
    )
}

fn classification_key(classification: &ToolProbeClassification) -> &'static str {
    match classification {
        ToolProbeClassification::StructuredNativeToolCall => "structured_native_tool_call",
        ToolProbeClassification::ParseableHarnTextToolCall => "parseable_harn_text_tool_call",
        ToolProbeClassification::DirectAnswerNoTool => "direct_answer_no_tool",
        ToolProbeClassification::UnavailableToolRepair => "unavailable_tool_repair",
        ToolProbeClassification::DoneSentinel => "done_sentinel",
        ToolProbeClassification::RawModelToolTag => "raw_model_tool_tag",
        ToolProbeClassification::ProseOnlyNonTool => "prose_only_non_tool",
        ToolProbeClassification::MalformedJsonArguments => "malformed_json_arguments",
        ToolProbeClassification::EmptySilent => "empty_silent",
        ToolProbeClassification::HttpError => "http_error",
        ToolProbeClassification::TransportError => "transport_error",
    }
}

fn wire_dialect_key(classification: &ToolProbeClassification) -> &'static str {
    match classification {
        ToolProbeClassification::StructuredNativeToolCall => "native_tool_calls",
        ToolProbeClassification::ParseableHarnTextToolCall => "harn_text_tool_calls",
        ToolProbeClassification::DirectAnswerNoTool
        | ToolProbeClassification::UnavailableToolRepair
        | ToolProbeClassification::DoneSentinel => "prose",
        ToolProbeClassification::RawModelToolTag => "raw_model_tool_tag",
        ToolProbeClassification::ProseOnlyNonTool => "prose",
        ToolProbeClassification::MalformedJsonArguments => "malformed_tool_args",
        ToolProbeClassification::EmptySilent => "empty",
        ToolProbeClassification::HttpError => "http_error",
        ToolProbeClassification::TransportError => "transport_error",
    }
}

#[cfg(test)]
mod tests;
