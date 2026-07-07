//! Offline provider tool-call quality scorecards.
//!
//! This module intentionally starts from saved `ToolConformanceReport`
//! envelopes. Live HTTP probing stays with `tool_conformance`; the scorecard is
//! the deterministic aggregation layer that downstream catalog reviews and
//! LoRA promotion receipts can cite without requiring provider credentials in
//! ordinary CI.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::tool_conformance::{
    ToolConformanceCase, ToolConformanceReport, ToolProbeClassification, ToolProbeFallbackMode,
};

pub const TOOL_SCORECARD_SCHEMA_VERSION: u32 = 1;

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
}

#[derive(Debug, Default)]
struct RouteAccumulator {
    provider: String,
    model: String,
    report_count: usize,
    cases: Vec<ToolConformanceCase>,
}

pub fn scorecard_from_tool_reports(reports: Vec<ToolConformanceReport>) -> ToolScorecardReport {
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
        .into_values()
        .map(score_route)
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

fn score_route(acc: RouteAccumulator) -> ToolScorecardRoute {
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
}
