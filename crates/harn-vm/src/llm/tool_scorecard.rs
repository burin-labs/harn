//! Offline provider tool-call quality scorecards.
//!
//! This module intentionally starts from saved `ToolConformanceReport`
//! envelopes. Live HTTP probing stays with `tool_conformance`; the scorecard is
//! the deterministic aggregation layer that downstream catalog reviews and
//! LoRA promotion receipts can cite without requiring provider credentials in
//! ordinary CI.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::provider_catalog::{CatalogModel, CatalogProvider};

use super::tool_conformance::{
    ToolConformanceCase, ToolConformanceReport, ToolProbeClassification, ToolProbeFallbackMode,
};

pub const TOOL_SCORECARD_SCHEMA_VERSION: u32 = 2;
pub const TOOL_SCORECARD_PLAN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardReport {
    pub schema_version: u32,
    pub route_count: usize,
    pub summary: ToolScorecardSummary,
    pub routes: Vec<ToolScorecardRoute>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardSummary {
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
    pub best_route: Option<ToolScorecardRouteKey>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardRouteKey {
    pub provider: String,
    pub model: String,
    pub quality_score: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardRoute {
    pub provider: String,
    pub model: String,
    pub catalog_claim: Option<ToolScorecardCatalogClaim>,
    pub report_count: usize,
    pub case_count: usize,
    pub successful_cases: usize,
    pub parseable_tool_call_cases: usize,
    pub native_tool_call_cases: usize,
    pub text_tool_call_cases: usize,
    pub actionless_cases: usize,
    pub empty_completion_cases: usize,
    pub malformed_argument_cases: usize,
    pub http_error_cases: usize,
    pub transport_error_cases: usize,
    pub pass_rate: f64,
    pub parseable_tool_call_rate: f64,
    pub empty_completion_rate: f64,
    pub actionless_rate: f64,
    pub quality_score: u16,
    pub status: &'static str,
    pub recommended_tool_mode: &'static str,
    pub observed_wire_dialects: Vec<&'static str>,
    pub classification_counts: BTreeMap<&'static str, usize>,
    pub issues: Vec<&'static str>,
    pub catalog_mismatches: Vec<ToolScorecardCatalogMismatch>,
    pub suggested_catalog_updates: Vec<ToolScorecardCatalogUpdate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardPlan {
    pub schema_version: u32,
    pub kind: &'static str,
    pub catalog: ToolScorecardCatalogProvenance,
    pub route_count: usize,
    pub case_count: usize,
    pub required_case_count: usize,
    pub batch_manifest_request_count: usize,
    pub routes: Vec<ToolScorecardPlanRoute>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub batch_manifest_requests: Vec<ToolScorecardBatchManifestRequest>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardCatalogProvenance {
    pub schema_version: u32,
    pub generated_by: String,
    pub hash_blake3: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardPlanRoute {
    pub provider: String,
    pub model: String,
    pub catalog_claim: ToolScorecardCatalogClaim,
    pub cases: Vec<ToolScorecardPlanCase>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardCatalogClaim {
    pub preferred_tool_format: Option<String>,
    pub tool_mode_parity: Option<String>,
    pub native_tools: bool,
    pub text_tools: bool,
    pub text_tool_wire_format_supported: bool,
    pub max_tools: Option<u32>,
    pub supports_parallel_tool_calls: bool,
    pub server_parser: String,
    pub tool_search: Vec<String>,
    pub batch_api: bool,
    pub batch_wire_format: Option<String>,
    pub batch_input_mode: Option<String>,
    pub batch_discount_percent: Option<u32>,
    pub provider_rate_limits: bool,
    pub model_rate_limits: bool,
    pub provider_rpm: Option<u32>,
    pub pricing: bool,
    pub provider_latency_p50_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardCatalogMismatch {
    pub code: &'static str,
    pub field: &'static str,
    pub observed: String,
    pub catalog: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardCatalogUpdate {
    pub field: &'static str,
    pub operation: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardPlanCase {
    pub id: &'static str,
    pub description: &'static str,
    pub requirement: &'static str,
    pub requirement_reason: &'static str,
    pub turn_count: u8,
    pub batch_eligible: bool,
    pub probe_focus: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardBatchManifestRequest {
    pub request_id: String,
    pub provider: String,
    pub model: String,
    pub case_id: &'static str,
    pub batch_wire_format: Option<String>,
    pub batch_input_mode: Option<String>,
}

#[derive(Debug, Default)]
struct RouteAccumulator {
    provider: String,
    model: String,
    report_count: usize,
    cases: Vec<ToolConformanceCase>,
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
        entry.cases.extend(report.cases);
    }

    let mut routes = grouped
        .into_iter()
        .map(|(key, acc)| score_route(acc, catalog_claims.get(&key).cloned()))
        .collect::<Vec<ToolScorecardRoute>>();
    routes.sort_by(|left, right| {
        right
            .quality_score
            .cmp(&left.quality_score)
            .then_with(|| left.provider.cmp(&right.provider))
            .then_with(|| left.model.cmp(&right.model))
    });

    let mut summary = ToolScorecardSummary {
        pass: 0,
        warn: 0,
        fail: 0,
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
    let provider_by_id = providers_by_id(&artifact.providers);

    let mut seen_routes = BTreeSet::new();
    let mut plan_routes = Vec::new();
    let mut batch_manifest_requests = Vec::new();

    for model in &artifact.models {
        let route_key = (model.provider.clone(), model.id.clone());
        if !requested_routes.is_empty() && !requested_routes.contains(&route_key) {
            continue;
        }
        seen_routes.insert(route_key);
        let claim = catalog_claim_for_model(model, &provider_by_id);
        let cases = fixed_micro_cases_for_claim(&claim);
        if include_batch_manifest && claim.batch_api {
            for case in &cases {
                if !case.batch_eligible {
                    continue;
                }
                batch_manifest_requests.push(ToolScorecardBatchManifestRequest {
                    request_id: format!(
                        "tool-scorecard:{}:{}:{}",
                        model.provider, model.id, case.id
                    ),
                    provider: model.provider.clone(),
                    model: model.id.clone(),
                    case_id: case.id,
                    batch_wire_format: claim.batch_wire_format.clone(),
                    batch_input_mode: claim.batch_input_mode.clone(),
                });
            }
        }
        plan_routes.push(ToolScorecardPlanRoute {
            provider: model.provider.clone(),
            model: model.id.clone(),
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
    let required_case_count = plan_routes
        .iter()
        .flat_map(|route| &route.cases)
        .filter(|case| case.requirement == "required")
        .count();

    Ok(ToolScorecardPlan {
        schema_version: TOOL_SCORECARD_PLAN_SCHEMA_VERSION,
        kind: "plan",
        catalog: ToolScorecardCatalogProvenance {
            schema_version: artifact.schema_version,
            generated_by: artifact.generated_by,
            hash_blake3: catalog_hash,
        },
        route_count: plan_routes.len(),
        case_count,
        required_case_count,
        batch_manifest_request_count: batch_manifest_requests.len(),
        routes: plan_routes,
        batch_manifest_requests,
    })
}

fn catalog_claims_by_route() -> BTreeMap<(String, String), ToolScorecardCatalogClaim> {
    let artifact = crate::provider_catalog::artifact();
    let provider_by_id = providers_by_id(&artifact.providers);
    artifact
        .models
        .iter()
        .map(|model| {
            (
                (model.provider.clone(), model.id.clone()),
                catalog_claim_for_model(model, &provider_by_id),
            )
        })
        .collect()
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

fn fixed_micro_cases_for_claim(claim: &ToolScorecardCatalogClaim) -> Vec<ToolScorecardPlanCase> {
    let parallel_requirement = if claim.supports_parallel_tool_calls {
        ("required", "route_claims_parallel_tool_calls")
    } else {
        ("not_applicable", "route_does_not_claim_parallel_tool_calls")
    };
    vec![
        ToolScorecardPlanCase {
            id: "single_tool_call",
            description: "single ordinary tool call with exact JSON arguments",
            requirement: "required",
            requirement_reason: "baseline_tool_call_quality",
            turn_count: 1,
            batch_eligible: true,
            probe_focus: vec!["tool_choice", "json_arguments", "wire_dialect"],
        },
        ToolScorecardPlanCase {
            id: "parallel_tool_calls",
            description: "multiple tool calls in one assistant response",
            requirement: parallel_requirement.0,
            requirement_reason: parallel_requirement.1,
            turn_count: 1,
            batch_eligible: true,
            probe_focus: vec!["parallel_dispatch", "tool_call_count", "argument_binding"],
        },
        ToolScorecardPlanCase {
            id: "large_string_argument",
            description: "large string/code argument with quotes, unicode, and heredoc-shaped text",
            requirement: "required",
            requirement_reason: "byte_fidelity_for_edit_payloads",
            turn_count: 1,
            batch_eligible: true,
            probe_focus: vec!["byte_fidelity", "escaping", "unicode"],
        },
        ToolScorecardPlanCase {
            id: "tool_result_followup",
            description: "assistant continuation after receiving a tool result",
            requirement: "required",
            requirement_reason: "multi_turn_agent_loop_quality",
            turn_count: 2,
            batch_eligible: false,
            probe_focus: vec!["tool_result_adjacency", "continuation", "action_vs_prose"],
        },
        ToolScorecardPlanCase {
            id: "no_tool_answer_or_refusal",
            description: "plain answer or refusal when no tool should be called",
            requirement: "required",
            requirement_reason: "spurious_tool_call_guard",
            turn_count: 1,
            batch_eligible: true,
            probe_focus: vec!["no_tool", "refusal", "answer_quality"],
        },
        ToolScorecardPlanCase {
            id: "unavailable_tool_repair",
            description: "repair or reject a request for an unavailable tool",
            requirement: "required",
            requirement_reason: "unsafe_or_unavailable_tool_recovery",
            turn_count: 1,
            batch_eligible: true,
            probe_focus: vec!["tool_name_repair", "no_unsafe_args", "recovery"],
        },
        ToolScorecardPlanCase {
            id: "done_sentinel",
            description: "completion-contract done sentinel emission",
            requirement: "required",
            requirement_reason: "agent_completion_contract",
            turn_count: 1,
            batch_eligible: true,
            probe_focus: vec!["done_sentinel", "completion_contract"],
        },
        ToolScorecardPlanCase {
            id: "parameter_edges",
            description: "provider-advertised parameter edge behavior",
            requirement: "required",
            requirement_reason: "catalog_parameter_claims",
            turn_count: 1,
            batch_eligible: true,
            probe_focus: vec!["temperature", "max_tokens", "tool_choice"],
        },
    ]
}

fn score_route(
    acc: RouteAccumulator,
    catalog_claim: Option<ToolScorecardCatalogClaim>,
) -> ToolScorecardRoute {
    let case_count = acc.cases.len();
    let mut successful_cases = 0;
    let mut parseable_tool_call_cases = 0;
    let mut native_tool_call_cases = 0;
    let mut text_tool_call_cases = 0;
    let mut actionless_cases = 0;
    let mut empty_completion_cases = 0;
    let mut malformed_argument_cases = 0;
    let mut http_error_cases = 0;
    let mut transport_error_cases = 0;
    let mut observed_wire_dialects = BTreeSet::new();
    let mut classification_counts = BTreeMap::new();

    for case in &acc.cases {
        *classification_counts
            .entry(classification_key(&case.classification))
            .or_insert(0) += 1;
        observed_wire_dialects.insert(wire_dialect_key(&case.classification));
        if case.ok {
            successful_cases += 1;
        }
        match case.classification {
            ToolProbeClassification::StructuredNativeToolCall => {
                parseable_tool_call_cases += 1;
                native_tool_call_cases += 1;
            }
            ToolProbeClassification::ParseableHarnTextToolCall => {
                parseable_tool_call_cases += 1;
                text_tool_call_cases += 1;
            }
            ToolProbeClassification::ProseOnlyNonTool => {
                actionless_cases += 1;
            }
            ToolProbeClassification::EmptySilent => {
                actionless_cases += 1;
                empty_completion_cases += 1;
            }
            ToolProbeClassification::MalformedJsonArguments => {
                malformed_argument_cases += 1;
            }
            ToolProbeClassification::HttpError => {
                http_error_cases += 1;
            }
            ToolProbeClassification::TransportError => {
                transport_error_cases += 1;
            }
            ToolProbeClassification::RawModelToolTag => {}
        }
    }

    let pass_rate = rate(successful_cases, case_count);
    let parseable_tool_call_rate = rate(parseable_tool_call_cases, case_count);
    let empty_completion_rate = rate(empty_completion_cases, case_count);
    let actionless_rate = rate(actionless_cases, case_count);
    let quality_score = ((pass_rate * 100.0).round() as u16).min(100);
    let recommended_tool_mode = recommended_tool_mode(native_tool_call_cases, text_tool_call_cases);
    let (catalog_mismatches, suggested_catalog_updates) =
        catalog_drift(&catalog_claim, recommended_tool_mode);
    let issues = route_issues(
        case_count,
        successful_cases,
        recommended_tool_mode,
        actionless_cases,
        malformed_argument_cases,
        http_error_cases,
        transport_error_cases,
    );
    let status = route_status(recommended_tool_mode, successful_cases, case_count, &issues);

    ToolScorecardRoute {
        provider: acc.provider,
        model: acc.model,
        catalog_claim,
        report_count: acc.report_count,
        case_count,
        successful_cases,
        parseable_tool_call_cases,
        native_tool_call_cases,
        text_tool_call_cases,
        actionless_cases,
        empty_completion_cases,
        malformed_argument_cases,
        http_error_cases,
        transport_error_cases,
        pass_rate,
        parseable_tool_call_rate,
        empty_completion_rate,
        actionless_rate,
        quality_score,
        status,
        recommended_tool_mode,
        observed_wire_dialects: observed_wire_dialects.into_iter().collect(),
        classification_counts,
        issues,
        catalog_mismatches,
        suggested_catalog_updates,
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
    successful_cases: usize,
    case_count: usize,
    issues: &[&'static str],
) -> &'static str {
    if recommended_tool_mode == "disabled" || case_count == 0 || successful_cases == 0 {
        "fail"
    } else if successful_cases < case_count || !issues.is_empty() {
        "warn"
    } else {
        "pass"
    }
}

fn route_issues(
    case_count: usize,
    successful_cases: usize,
    recommended_tool_mode: &str,
    actionless_cases: usize,
    malformed_argument_cases: usize,
    http_error_cases: usize,
    transport_error_cases: usize,
) -> Vec<&'static str> {
    let mut issues = Vec::new();
    if case_count == 0 {
        issues.push("no_cases");
    }
    if recommended_tool_mode == "disabled" {
        issues.push("tool_calling_disabled");
    }
    if successful_cases > 0 && successful_cases < case_count {
        issues.push("partial_tool_call_pass_rate");
    }
    if actionless_cases > 0 {
        issues.push("empty_or_actionless_completion");
    }
    if malformed_argument_cases > 0 {
        issues.push("malformed_tool_arguments");
    }
    if http_error_cases > 0 {
        issues.push("provider_http_errors");
    }
    if transport_error_cases > 0 {
        issues.push("transport_errors");
    }
    issues
}

fn classification_key(classification: &ToolProbeClassification) -> &'static str {
    match classification {
        ToolProbeClassification::StructuredNativeToolCall => "structured_native_tool_call",
        ToolProbeClassification::ParseableHarnTextToolCall => "parseable_harn_text_tool_call",
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
        ToolProbeClassification::RawModelToolTag => "raw_model_tool_tag",
        ToolProbeClassification::ProseOnlyNonTool => "prose",
        ToolProbeClassification::MalformedJsonArguments => "malformed_tool_args",
        ToolProbeClassification::EmptySilent => "empty",
        ToolProbeClassification::HttpError => "http_error",
        ToolProbeClassification::TransportError => "transport_error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::tool_conformance::{
        ToolCallingConformanceSummary, ToolProbeMode, ToolProbeStatus,
    };

    #[test]
    fn scorecard_ranks_successful_native_route_first() {
        let pass = report(
            "anthropic",
            "claude",
            vec![case(
                ToolProbeClassification::StructuredNativeToolCall,
                true,
            )],
        );
        let fail = report(
            "fireworks",
            "gpt-oss",
            vec![case(ToolProbeClassification::EmptySilent, false)],
        );

        let scorecard = scorecard_from_tool_reports(vec![fail, pass]);

        assert_eq!(scorecard.schema_version, TOOL_SCORECARD_SCHEMA_VERSION);
        assert_eq!(scorecard.route_count, 2);
        assert_eq!(scorecard.summary.pass, 1);
        assert_eq!(scorecard.summary.fail, 1);
        assert_eq!(scorecard.routes[0].provider, "anthropic");
        assert_eq!(scorecard.routes[0].status, "pass");
        assert_eq!(scorecard.routes[0].recommended_tool_mode, "native");
        assert_eq!(
            scorecard.routes[1].issues,
            vec!["tool_calling_disabled", "empty_or_actionless_completion"]
        );
    }

    #[test]
    fn scorecard_reports_catalog_drift_without_failing_route() {
        let scorecard = scorecard_from_tool_reports(vec![report(
            "anthropic",
            "claude-sonnet-4-6",
            vec![case(
                ToolProbeClassification::ParseableHarnTextToolCall,
                true,
            )],
        )]);

        let route = &scorecard.routes[0];
        assert_eq!(route.status, "pass");
        assert_eq!(route.recommended_tool_mode, "text");
        assert!(route.catalog_claim.is_some());
        assert!(route
            .catalog_mismatches
            .iter()
            .any(|mismatch| mismatch.code == "preferred_tool_format_disagrees"));
        assert!(route.suggested_catalog_updates.iter().any(|update| {
            update.field == "tool_support.preferred_format"
                && update.operation == "set"
                && update.value.as_deref() == Some("json")
        }));
    }

    #[test]
    fn scorecard_does_not_suggest_catalog_disable_without_positive_evidence() {
        let scorecard = scorecard_from_tool_reports(vec![report(
            "anthropic",
            "claude-sonnet-4-6",
            vec![case(ToolProbeClassification::HttpError, false)],
        )]);

        let route = &scorecard.routes[0];
        assert_eq!(route.status, "fail");
        assert_eq!(route.recommended_tool_mode, "disabled");
        assert!(route.catalog_mismatches.is_empty());
        assert!(route.suggested_catalog_updates.is_empty());
    }

    #[test]
    fn catalog_drift_treats_missing_preferred_format_as_no_preference() {
        let (mismatches, updates) =
            catalog_drift(&Some(catalog_claim(None, true, false)), "native");

        assert!(mismatches.is_empty());
        assert!(updates.is_empty());
    }

    #[test]
    fn catalog_drift_treats_json_preferred_format_as_text_channel_match() {
        let (mismatches, updates) =
            catalog_drift(&Some(catalog_claim(Some("json"), false, true)), "text");

        assert!(mismatches.is_empty());
        assert!(updates.is_empty());
    }

    #[test]
    fn catalog_drift_suggests_safe_text_channel_default_for_native_mismatch() {
        let (mismatches, updates) =
            catalog_drift(&Some(catalog_claim(Some("native"), true, true)), "text");

        assert_eq!(mismatches[0].code, "preferred_tool_format_disagrees");
        assert_eq!(updates[0].field, "tool_support.preferred_format");
        assert_eq!(updates[0].value.as_deref(), Some("json"));
    }

    #[test]
    fn scorecard_plan_filters_catalog_routes_and_names_required_cases() {
        let plan =
            tool_scorecard_plan_from_catalog(&[String::from("anthropic:claude-sonnet-5")], true)
                .expect("plan from catalog");

        assert_eq!(plan.schema_version, TOOL_SCORECARD_PLAN_SCHEMA_VERSION);
        assert_eq!(plan.kind, "plan");
        assert_eq!(plan.route_count, 1);
        assert_eq!(plan.routes[0].provider, "anthropic");
        assert_eq!(plan.routes[0].model, "claude-sonnet-5");
        assert!(plan.catalog.hash_blake3.starts_with("blake3:"));
        let case_ids = plan.routes[0]
            .cases
            .iter()
            .map(|case| case.id)
            .collect::<Vec<_>>();
        assert!(case_ids.contains(&"single_tool_call"));
        assert!(case_ids.contains(&"large_string_argument"));
        assert!(case_ids.contains(&"tool_result_followup"));
        assert!(case_ids.contains(&"done_sentinel"));
        assert_eq!(plan.case_count, plan.routes[0].cases.len());
        assert!(plan.required_case_count >= 7);
    }

    #[test]
    fn scorecard_plan_rejects_unknown_route_filters() {
        let err = tool_scorecard_plan_from_catalog(&[String::from("missing:nope")], false)
            .expect_err("unknown route should fail");

        assert!(err.contains("missing:nope"), "{err}");
    }

    fn report(
        provider: &str,
        model: &str,
        cases: Vec<ToolConformanceCase>,
    ) -> ToolConformanceReport {
        ToolConformanceReport {
            schema_version: 1,
            provider: provider.to_string(),
            model: model.to_string(),
            base_url: None,
            tool_name: "echo_marker".to_string(),
            marker: "marker".to_string(),
            cases,
            tool_calling: ToolCallingConformanceSummary {
                native: ToolProbeStatus::Unknown,
                text: ToolProbeStatus::Unknown,
                streaming_native: ToolProbeStatus::Unknown,
                fallback_mode: ToolProbeFallbackMode::Disabled,
                failure_reason: None,
            },
        }
    }

    fn case(classification: ToolProbeClassification, ok: bool) -> ToolConformanceCase {
        ToolConformanceCase {
            mode: ToolProbeMode::NonStreaming,
            ok,
            classification,
            fallback_mode: ToolProbeFallbackMode::Native,
            failure_reason: None,
            http_status: None,
            elapsed_ms: Some(1),
            native_tool_call_count: usize::from(ok),
            text_tool_call_count: 0,
            parser_errors: Vec::new(),
            protocol_violations: Vec::new(),
            content_sample: None,
        }
    }

    fn catalog_claim(
        preferred_tool_format: Option<&str>,
        native_tools: bool,
        text_tools: bool,
    ) -> ToolScorecardCatalogClaim {
        ToolScorecardCatalogClaim {
            preferred_tool_format: preferred_tool_format.map(str::to_string),
            tool_mode_parity: None,
            native_tools,
            text_tools,
            text_tool_wire_format_supported: text_tools,
            max_tools: None,
            supports_parallel_tool_calls: false,
            server_parser: "unknown".to_string(),
            tool_search: Vec::new(),
            batch_api: false,
            batch_wire_format: None,
            batch_input_mode: None,
            batch_discount_percent: None,
            provider_rate_limits: false,
            model_rate_limits: false,
            provider_rpm: None,
            pricing: false,
            provider_latency_p50_ms: None,
        }
    }
}
