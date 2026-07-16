//! One-tool provider conformance probe for local/runtime tool calling.
//!
//! The probe is deliberately tiny: define one harmless `echo_marker` tool,
//! ask the model to call it with a fixed marker, and classify what came back.
//! The classification is the stable contract eval harnesses consume; the live
//! HTTP runner is a convenience around that classifier.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::llm_config::{self, ProviderDef};

#[path = "tool_conformance_helpers.rs"]
mod helpers;
#[path = "tool_conformance_request.rs"]
mod request;
#[path = "tool_conformance_types.rs"]
mod types;
use super::usage_normalization::extract_probe_usage;
pub use super::usage_normalization::ToolProbeUsage;
pub(super) use helpers::{aggregate_stream_text, probe_tool_registry};
use request::{probe_request_body, probe_request_body_with_warnings, validate_probe_request_body};
pub use types::{
    ToolConformanceRequestAuditFailure, ToolConformanceRequestAuditNotApplicable,
    ToolConformanceRequestAuditReport, ToolConformanceRequestAuditRoute,
    ToolConformanceRequestAuditWarning, ToolConformanceRequestCase, ToolConformanceRequestReport,
    ToolConformanceRequestValidation, ToolConformanceRequestValidationStatus,
    ToolConformanceRequestWarning,
};

pub const TOOL_CONFORMANCE_SCHEMA_VERSION: u32 = 1;
pub const TOOL_CONFORMANCE_REQUEST_SCHEMA_VERSION: u32 = 4;
pub const TOOL_CONFORMANCE_REQUEST_AUDIT_SCHEMA_VERSION: u32 = 5;
pub const TOOL_PROBE_TOOL_NAME: &str = "echo_marker";
pub const DEFAULT_TOOL_PROBE_MARKER: &str = "harn_tool_probe_marker";

#[derive(Debug, Clone)]
pub struct ToolConformanceProbeOptions {
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    pub modes: Vec<ToolProbeMode>,
    pub probe_case: ToolProbeCase,
    pub marker: String,
    pub repeat: usize,
    pub timeout_secs: u64,
}

impl ToolConformanceProbeOptions {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            base_url: None,
            modes: vec![ToolProbeMode::NonStreaming, ToolProbeMode::Streaming],
            probe_case: ToolProbeCase::SingleToolCall,
            marker: DEFAULT_TOOL_PROBE_MARKER.to_string(),
            repeat: 1,
            timeout_secs: 120,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProbeMode {
    NonStreaming,
    Streaming,
}

impl ToolProbeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NonStreaming => "non_streaming",
            Self::Streaming => "streaming",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProbeCase {
    #[default]
    SingleToolCall,
    ParallelToolCalls,
    LargeStringArgument,
    ToolResultFollowup,
    SignedThinkingToolResultFollowup,
    NoToolAnswerOrRefusal,
    UnavailableToolRepair,
    DoneSentinel,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProbeRequestProfile {
    #[default]
    CatalogDefault,
    ParameterEdges,
}

impl ToolProbeRequestProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CatalogDefault => "catalog_default",
            Self::ParameterEdges => "parameter_edges",
        }
    }

    pub fn catalog_request_audit_profiles() -> Vec<Self> {
        vec![Self::CatalogDefault, Self::ParameterEdges]
    }
}

impl ToolProbeCase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SingleToolCall => "single_tool_call",
            Self::ParallelToolCalls => "parallel_tool_calls",
            Self::LargeStringArgument => "large_string_argument",
            Self::ToolResultFollowup => "tool_result_followup",
            Self::SignedThinkingToolResultFollowup => "signed_thinking_tool_result_followup",
            Self::NoToolAnswerOrRefusal => "no_tool_answer_or_refusal",
            Self::UnavailableToolRepair => "unavailable_tool_repair",
            Self::DoneSentinel => "done_sentinel",
        }
    }

    pub fn catalog_request_audit_cases() -> Vec<Self> {
        vec![
            Self::SingleToolCall,
            Self::ParallelToolCalls,
            Self::LargeStringArgument,
            Self::ToolResultFollowup,
            Self::SignedThinkingToolResultFollowup,
            Self::NoToolAnswerOrRefusal,
            Self::UnavailableToolRepair,
            Self::DoneSentinel,
        ]
    }

    pub fn is_live_applicable(self, provider: &str, model: &str) -> bool {
        match self {
            Self::SignedThinkingToolResultFollowup => {
                crate::llm::tool_scorecard::signed_thinking_tool_history_supported(provider, model)
            }
            _ => true,
        }
    }

    fn expected_value(self, marker: &str) -> String {
        match self {
            Self::SingleToolCall => marker.to_string(),
            Self::ParallelToolCalls => marker.to_string(),
            Self::LargeStringArgument => format!(
                "marker={marker}\nquoted=\"value\"\njson={{\"marker\":{marker:?},\"nested\":[1,true]}}\nheredoc=<<EOF\n{marker}\nEOF\nunicode=\\u{{2603}}\\u{{1F680}}\nend"
            ),
            Self::ToolResultFollowup => format!("tool_result_followup:{marker}"),
            Self::SignedThinkingToolResultFollowup => {
                format!("signed_thinking_tool_result_followup:{marker}")
            }
            Self::NoToolAnswerOrRefusal => format!("direct_answer:{marker}"),
            Self::UnavailableToolRepair => format!("unavailable_tool:{marker}"),
            Self::DoneSentinel => format!("<done>{marker}</done>"),
        }
    }

    fn requires_probe_tool(self) -> bool {
        matches!(
            self,
            Self::SingleToolCall | Self::ParallelToolCalls | Self::LargeStringArgument
        )
    }

    fn request_uses_probe_tool(self) -> bool {
        self.requires_probe_tool()
            || matches!(
                self,
                Self::ToolResultFollowup | Self::SignedThinkingToolResultFollowup
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProbeClassification {
    StructuredNativeToolCall,
    ParseableHarnTextToolCall,
    DirectAnswerNoTool,
    UnavailableToolRepair,
    DoneSentinel,
    RawModelToolTag,
    ProseOnlyNonTool,
    MalformedJsonArguments,
    EmptySilent,
    HttpError,
    TransportError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProbeStatus {
    Pass,
    Fail,
    Unknown,
}

impl ToolProbeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProbeFallbackMode {
    Native,
    Text,
    Disabled,
}

impl ToolProbeFallbackMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Text => "text",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceReport {
    pub schema_version: u32,
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default)]
    pub probe_case: ToolProbeCase,
    pub tool_name: String,
    pub marker: String,
    #[serde(default)]
    pub expected_value: String,
    pub cases: Vec<ToolConformanceCase>,
    pub tool_calling: ToolCallingConformanceSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallingConformanceSummary {
    pub native: ToolProbeStatus,
    pub text: ToolProbeStatus,
    pub streaming_native: ToolProbeStatus,
    pub fallback_mode: ToolProbeFallbackMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceCase {
    pub mode: ToolProbeMode,
    pub ok: bool,
    pub classification: ToolProbeClassification,
    pub fallback_mode: ToolProbeFallbackMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    pub native_tool_call_count: usize,
    pub text_tool_call_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<ToolProbeUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parser_errors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocol_violations: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_sample: Option<String>,
}

impl ToolConformanceCase {
    fn transport_error(mode: ToolProbeMode, message: String, elapsed_ms: Option<u64>) -> Self {
        Self {
            mode,
            ok: false,
            classification: ToolProbeClassification::TransportError,
            fallback_mode: ToolProbeFallbackMode::Disabled,
            failure_reason: Some(message),
            http_status: None,
            elapsed_ms,
            native_tool_call_count: 0,
            text_tool_call_count: 0,
            usage: None,
            parser_errors: Vec::new(),
            protocol_violations: Vec::new(),
            content_sample: None,
        }
    }

    fn http_error(
        mode: ToolProbeMode,
        status: u16,
        message: String,
        elapsed_ms: Option<u64>,
    ) -> Self {
        Self {
            mode,
            ok: false,
            classification: ToolProbeClassification::HttpError,
            fallback_mode: ToolProbeFallbackMode::Disabled,
            failure_reason: Some(message),
            http_status: Some(status),
            elapsed_ms,
            native_tool_call_count: 0,
            text_tool_call_count: 0,
            usage: None,
            parser_errors: Vec::new(),
            protocol_violations: Vec::new(),
            content_sample: None,
        }
    }
}

pub async fn run_tool_conformance_probe(
    options: ToolConformanceProbeOptions,
) -> ToolConformanceReport {
    let model = llm_config::resolve_model_info(&options.model);
    let provider = if options.provider.trim().is_empty() {
        model.provider.clone()
    } else {
        options.provider.clone()
    };
    let model_id = resolved_probe_model_id(&model.id);
    let base_url = options.base_url.clone().or_else(|| {
        llm_config::provider_config(&provider).map(|def| llm_config::resolve_base_url(&def))
    });
    let mut cases = Vec::new();
    let modes = normalized_modes(&options.modes);
    let expected_value = options.probe_case.expected_value(&options.marker);
    for _ in 0..options.repeat.max(1) {
        for mode in &modes {
            cases.push(
                execute_live_probe_case(
                    &provider,
                    &model_id,
                    base_url.as_deref(),
                    *mode,
                    options.probe_case,
                    &expected_value,
                    options.timeout_secs,
                )
                .await,
            );
        }
    }
    report_from_cases(
        provider,
        model_id,
        base_url,
        options.probe_case,
        options.marker,
        expected_value,
        cases,
    )
}

fn resolved_probe_model_id(selector: &str) -> String {
    llm_config::wire_model_id(selector)
}

pub fn classify_tool_conformance_fixture(
    provider: impl Into<String>,
    model: impl Into<String>,
    mode: ToolProbeMode,
    marker: impl Into<String>,
    raw: &str,
) -> ToolConformanceReport {
    classify_tool_conformance_fixture_for_case(
        provider,
        model,
        mode,
        ToolProbeCase::SingleToolCall,
        marker,
        raw,
    )
}

pub fn classify_tool_conformance_fixture_for_case(
    provider: impl Into<String>,
    model: impl Into<String>,
    mode: ToolProbeMode,
    probe_case: ToolProbeCase,
    marker: impl Into<String>,
    raw: &str,
) -> ToolConformanceReport {
    let marker = marker.into();
    let expected_value = probe_case.expected_value(&marker);
    let provider = provider.into();
    let model = model.into();
    let response = serde_json::from_str::<Value>(raw).unwrap_or_else(|_| json!({ "content": raw }));
    let usage = extract_probe_usage(&provider, &model, &response);
    let case = classify_tool_probe_response(
        mode,
        &response,
        probe_case,
        &expected_value,
        None,
        None,
        usage,
    );
    report_from_cases(
        provider,
        model,
        None,
        probe_case,
        marker,
        expected_value,
        vec![case],
    )
}

pub fn tool_conformance_request_report(
    provider: impl Into<String>,
    model: impl Into<String>,
    base_url: Option<String>,
    modes: Vec<ToolProbeMode>,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    marker: impl Into<String>,
) -> Result<ToolConformanceRequestReport, String> {
    let provider = provider.into();
    let model = model.into();
    let marker = marker.into();
    let expected_value = probe_case.expected_value(&marker);
    let mut requests = Vec::new();
    for mode in normalized_modes(&modes) {
        let (request_body, warnings) = probe_request_body_with_warnings(
            &provider,
            &model,
            mode,
            probe_case,
            request_profile,
            &expected_value,
        )?;
        let mut validation = validate_probe_request_body(
            &provider,
            &model,
            probe_case,
            request_profile,
            &request_body,
        );
        validation.warnings = warnings;
        requests.push(ToolConformanceRequestCase {
            mode,
            validation,
            request_body,
        });
    }
    Ok(ToolConformanceRequestReport {
        schema_version: TOOL_CONFORMANCE_REQUEST_SCHEMA_VERSION,
        provider,
        model,
        base_url,
        probe_case,
        request_profile,
        tool_name: TOOL_PROBE_TOOL_NAME.to_string(),
        marker,
        expected_value,
        requests,
    })
}

pub fn tool_conformance_request_report_json(
    provider: impl Into<String>,
    model: impl Into<String>,
    base_url: Option<String>,
    modes: Vec<ToolProbeMode>,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    marker: impl Into<String>,
) -> Result<String, String> {
    let report = tool_conformance_request_report(
        provider,
        model,
        base_url,
        modes,
        probe_case,
        request_profile,
        marker,
    )?;
    serde_json::to_string_pretty(&report).map_err(|error| {
        format!("internal error: failed to render tool-probe request report: {error}")
    })
}

pub fn tool_conformance_request_catalog_audit(
    probe_cases: Vec<ToolProbeCase>,
    request_profiles: Vec<ToolProbeRequestProfile>,
    modes: Vec<ToolProbeMode>,
) -> ToolConformanceRequestAuditReport {
    let probe_cases = if probe_cases.is_empty() {
        ToolProbeCase::catalog_request_audit_cases()
    } else {
        probe_cases
    };
    let modes = normalized_modes(&modes);
    let request_profiles = if request_profiles.is_empty() {
        ToolProbeRequestProfile::catalog_request_audit_profiles()
    } else {
        normalized_request_profiles(&request_profiles)
    };
    let catalog_model_count = llm_config::model_catalog_entries().len();
    let routing_routes = crate::provider_catalog::artifact().routing_routes;
    let mut request_count = 0usize;
    let mut validation_pass_count = 0usize;
    let mut validation_fail_count = 0usize;
    let mut warning_count = 0usize;
    let mut not_applicable_count = 0usize;
    let mut dialect_counts = BTreeMap::new();
    let mut provider_counts = BTreeMap::new();
    let mut routes = Vec::new();
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let mut not_applicable = Vec::new();

    for catalog_route in &routing_routes {
        let mut route = ToolConformanceRequestAuditRoute {
            provider: catalog_route.provider.clone(),
            model: catalog_route.model.clone(),
            request_count: 0,
            validation_pass_count: 0,
            validation_fail_count: 0,
            not_applicable_count: 0,
            dialect_counts: BTreeMap::new(),
        };
        for probe_case in &probe_cases {
            for request_profile in &request_profiles {
                for mode in &modes {
                    route.request_count += 1;
                    request_count += 1;
                    *provider_counts
                        .entry(catalog_route.provider.clone())
                        .or_insert(0) += 1;
                    match tool_conformance_request_report(
                        catalog_route.provider.clone(),
                        catalog_route.model.clone(),
                        None,
                        vec![*mode],
                        *probe_case,
                        *request_profile,
                        DEFAULT_TOOL_PROBE_MARKER,
                    ) {
                        Ok(report) => {
                            for request in report.requests {
                                let dialect = request.validation.dialect.clone();
                                *dialect_counts.entry(dialect.clone()).or_insert(0) += 1;
                                *route.dialect_counts.entry(dialect.clone()).or_insert(0) += 1;
                                if !request.validation.warnings.is_empty() {
                                    warning_count += request.validation.warnings.len();
                                    warnings.push(ToolConformanceRequestAuditWarning {
                                        provider: catalog_route.provider.clone(),
                                        model: catalog_route.model.clone(),
                                        probe_case: probe_case.as_str().to_string(),
                                        request_profile: request_profile.as_str().to_string(),
                                        mode: mode.as_str().to_string(),
                                        dialect: dialect.clone(),
                                        warnings: request.validation.warnings.clone(),
                                    });
                                }
                                match request.validation.status {
                                    ToolConformanceRequestValidationStatus::Pass => {
                                        route.validation_pass_count += 1;
                                        validation_pass_count += 1;
                                    }
                                    ToolConformanceRequestValidationStatus::Fail => {
                                        route.validation_fail_count += 1;
                                        validation_fail_count += 1;
                                        failures.push(ToolConformanceRequestAuditFailure {
                                            provider: catalog_route.provider.clone(),
                                            model: catalog_route.model.clone(),
                                            probe_case: probe_case.as_str().to_string(),
                                            request_profile: request_profile.as_str().to_string(),
                                            mode: mode.as_str().to_string(),
                                            dialect,
                                            issues: request.validation.issues,
                                        });
                                    }
                                    ToolConformanceRequestValidationStatus::NotApplicable => {
                                        route.not_applicable_count += 1;
                                        not_applicable_count += 1;
                                        not_applicable.push(
                                            ToolConformanceRequestAuditNotApplicable {
                                                provider: catalog_route.provider.clone(),
                                                model: catalog_route.model.clone(),
                                                probe_case: probe_case.as_str().to_string(),
                                                request_profile: request_profile
                                                    .as_str()
                                                    .to_string(),
                                                mode: mode.as_str().to_string(),
                                                dialect,
                                                reason: request
                                                    .validation
                                                    .issues
                                                    .first()
                                                    .cloned()
                                                    .unwrap_or_else(|| {
                                                        "probe case is not applicable to this route"
                                                            .to_string()
                                                    }),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            route.validation_fail_count += 1;
                            validation_fail_count += 1;
                            failures.push(ToolConformanceRequestAuditFailure {
                                provider: catalog_route.provider.clone(),
                                model: catalog_route.model.clone(),
                                probe_case: probe_case.as_str().to_string(),
                                request_profile: request_profile.as_str().to_string(),
                                mode: mode.as_str().to_string(),
                                dialect: "request_build".to_string(),
                                issues: vec![error],
                            });
                        }
                    }
                }
            }
        }
        routes.push(route);
    }

    ToolConformanceRequestAuditReport {
        schema_version: TOOL_CONFORMANCE_REQUEST_AUDIT_SCHEMA_VERSION,
        catalog_model_count,
        route_count: routes.len(),
        probe_cases: probe_cases
            .into_iter()
            .map(|probe_case| probe_case.as_str().to_string())
            .collect(),
        request_profiles: request_profiles
            .into_iter()
            .map(|request_profile| request_profile.as_str().to_string())
            .collect(),
        modes: modes
            .into_iter()
            .map(|mode| mode.as_str().to_string())
            .collect(),
        request_count,
        validation_pass_count,
        validation_fail_count,
        warning_count,
        not_applicable_count,
        dialect_counts,
        provider_counts,
        routes,
        failures,
        warnings,
        not_applicable,
    }
}

pub fn report_satisfies_required_probe(report: &ToolConformanceReport, requirement: &str) -> bool {
    match requirement {
        "tool_probe" | "tool_call_probe" => {
            report.tool_calling.fallback_mode != ToolProbeFallbackMode::Disabled
                && report.cases.iter().any(|case| case.ok)
        }
        "native_tool_probe" => report.tool_calling.native == ToolProbeStatus::Pass,
        "streaming_tool_probe" => report.tool_calling.streaming_native == ToolProbeStatus::Pass,
        _ => false,
    }
}

fn normalized_modes(modes: &[ToolProbeMode]) -> Vec<ToolProbeMode> {
    if modes.is_empty() {
        return vec![ToolProbeMode::NonStreaming, ToolProbeMode::Streaming];
    }
    let mut out = Vec::new();
    for mode in modes {
        if !out.contains(mode) {
            out.push(*mode);
        }
    }
    out
}

fn normalized_request_profiles(
    profiles: &[ToolProbeRequestProfile],
) -> Vec<ToolProbeRequestProfile> {
    if profiles.is_empty() {
        return ToolProbeRequestProfile::catalog_request_audit_profiles();
    }
    let mut out = Vec::new();
    for profile in profiles {
        if !out.contains(profile) {
            out.push(*profile);
        }
    }
    out
}

fn report_from_cases(
    provider: String,
    model: String,
    base_url: Option<String>,
    probe_case: ToolProbeCase,
    marker: String,
    expected_value: String,
    cases: Vec<ToolConformanceCase>,
) -> ToolConformanceReport {
    let summary = summarize_cases(&cases);
    ToolConformanceReport {
        schema_version: TOOL_CONFORMANCE_SCHEMA_VERSION,
        provider,
        model,
        base_url,
        probe_case,
        tool_name: TOOL_PROBE_TOOL_NAME.to_string(),
        marker,
        expected_value,
        cases,
        tool_calling: summary,
    }
}

fn summarize_cases(cases: &[ToolConformanceCase]) -> ToolCallingConformanceSummary {
    let native = summarize_native_mode(cases, ToolProbeMode::NonStreaming);
    let streaming_native = summarize_native_mode(cases, ToolProbeMode::Streaming);
    let text = summarize_text_mode(cases);

    let fallback_mode =
        if native == ToolProbeStatus::Pass || streaming_native == ToolProbeStatus::Pass {
            ToolProbeFallbackMode::Native
        } else if text == ToolProbeStatus::Pass {
            ToolProbeFallbackMode::Text
        } else {
            ToolProbeFallbackMode::Disabled
        };

    let failure_reason = if fallback_mode == ToolProbeFallbackMode::Disabled {
        cases.iter().find_map(|case| case.failure_reason.clone())
    } else {
        None
    };

    ToolCallingConformanceSummary {
        native,
        text,
        streaming_native,
        fallback_mode,
        failure_reason,
    }
}

fn summarize_native_mode(cases: &[ToolConformanceCase], mode: ToolProbeMode) -> ToolProbeStatus {
    let mut saw_mode = false;
    let mut all_passed = true;
    for case in cases.iter().filter(|case| case.mode == mode) {
        saw_mode = true;
        if !(case.ok && case.classification == ToolProbeClassification::StructuredNativeToolCall) {
            all_passed = false;
        }
    }
    match (saw_mode, all_passed) {
        (false, _) => ToolProbeStatus::Unknown,
        (true, true) => ToolProbeStatus::Pass,
        (true, false) => ToolProbeStatus::Fail,
    }
}

fn summarize_text_mode(cases: &[ToolConformanceCase]) -> ToolProbeStatus {
    let mut saw_text = false;
    let mut saw_passing_mode = false;
    for mode in [ToolProbeMode::NonStreaming, ToolProbeMode::Streaming] {
        let mut saw_mode = false;
        let mut saw_text_in_mode = false;
        let mut all_mode_cases_passed = true;
        for case in cases.iter().filter(|case| case.mode == mode) {
            saw_mode = true;
            saw_text_in_mode |= case.classification
                == ToolProbeClassification::ParseableHarnTextToolCall
                || case.text_tool_call_count > 0;
            if !(case.ok
                && case.classification == ToolProbeClassification::ParseableHarnTextToolCall)
            {
                all_mode_cases_passed = false;
            }
        }
        saw_text |= saw_text_in_mode;
        if saw_mode && saw_text_in_mode && all_mode_cases_passed {
            saw_passing_mode = true;
        }
    }
    if !saw_text {
        return ToolProbeStatus::Unknown;
    }
    if saw_passing_mode {
        ToolProbeStatus::Pass
    } else {
        ToolProbeStatus::Fail
    }
}

async fn execute_live_probe_case(
    provider: &str,
    model: &str,
    base_url: Option<&str>,
    mode: ToolProbeMode,
    probe_case: ToolProbeCase,
    marker: &str,
    timeout_secs: u64,
) -> ToolConformanceCase {
    let clock = harn_clock::RealClock::arc();
    let started_ms = clock.monotonic_ms();
    let Some(def) = llm_config::provider_config(provider) else {
        return ToolConformanceCase::transport_error(
            mode,
            format!("unknown provider: {provider}"),
            Some(elapsed_ms(&*clock, started_ms)),
        );
    };
    let base_url = base_url
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| llm_config::resolve_base_url(&def));
    let url = match chat_url(&def, &base_url) {
        Ok(url) => url,
        Err(message) => {
            return ToolConformanceCase::transport_error(
                mode,
                message,
                Some(elapsed_ms(&*clock, started_ms)),
            );
        }
    };
    let body = match probe_request_body(
        provider,
        model,
        mode,
        probe_case,
        ToolProbeRequestProfile::CatalogDefault,
        marker,
    ) {
        Ok(body) => body,
        Err(message) => {
            return ToolConformanceCase::transport_error(
                mode,
                message,
                Some(elapsed_ms(&*clock, started_ms)),
            );
        }
    };
    let client = if mode == ToolProbeMode::Streaming {
        crate::llm::streaming_client_for_base_url(&base_url)
    } else {
        crate::llm::blocking_client_for_base_url(&base_url)
    };
    let api_key = crate::llm::resolve_api_key(provider).unwrap_or_default();
    let request = client
        .post(&url)
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .json(&body);
    let mut request = crate::llm::api::apply_auth_headers(request, &api_key, Some(&def));
    for (name, value) in &def.extra_headers {
        request = request.header(name.as_str(), value.as_str());
    }

    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            return ToolConformanceCase::transport_error(
                mode,
                format!("provider request failed: {error}"),
                Some(elapsed_ms(&*clock, started_ms)),
            );
        }
    };
    let status = response.status();
    let text = match response.text().await {
        Ok(text) => text,
        Err(error) => {
            return ToolConformanceCase::transport_error(
                mode,
                format!("provider response was unreadable: {error}"),
                Some(elapsed_ms(&*clock, started_ms)),
            );
        }
    };
    let elapsed = Some(elapsed_ms(&*clock, started_ms));
    if !status.is_success() {
        return ToolConformanceCase::http_error(
            mode,
            status.as_u16(),
            sample_failure(&text, "provider returned non-success HTTP status"),
            elapsed,
        );
    }
    let response_value = if mode == ToolProbeMode::Streaming {
        aggregate_stream_text(&text, provider)
    } else {
        serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({ "content": text }))
    };
    let usage = extract_probe_usage(provider, model, &response_value);
    classify_tool_probe_response(
        mode,
        &response_value,
        probe_case,
        marker,
        Some(status.as_u16()),
        elapsed,
        usage,
    )
}

/// True when `calls` contains the probe's echo_marker call (the
/// `TOOL_PROBE_TOOL_NAME` tool with `args.value == marker`). Shared by the
/// tagged and fenced-JSON text-channel parse attempts.
fn probe_marker_present(calls: &[Value], marker: &str) -> bool {
    probe_values_present(calls, &[marker.to_string()])
}

fn probe_values_present(calls: &[Value], expected_values: &[String]) -> bool {
    expected_values
        .iter()
        .all(|expected| probe_value_count(calls, expected) > 0)
}

fn probe_value_count(calls: &[Value], marker: &str) -> usize {
    calls.iter().any(|call| {
        call.get("name").and_then(Value::as_str) == Some(TOOL_PROBE_TOOL_NAME)
            && call
                .get("arguments")
                .and_then(|args| args.get("value"))
                .and_then(Value::as_str)
                == Some(marker)
    }) as usize
}

fn expected_tool_values(probe_case: ToolProbeCase, expected_value: &str) -> Vec<String> {
    match probe_case {
        ToolProbeCase::ParallelToolCalls => vec![
            format!("{expected_value}:first"),
            format!("{expected_value}:second"),
        ],
        _ => vec![expected_value.to_string()],
    }
}

fn classify_tool_probe_response(
    mode: ToolProbeMode,
    response: &Value,
    probe_case: ToolProbeCase,
    expected_value: &str,
    http_status: Option<u16>,
    elapsed_ms: Option<u64>,
    usage: Option<ToolProbeUsage>,
) -> ToolConformanceCase {
    let native = extract_native_tool_calls(response);
    let native_count = native.len();
    let mut malformed_native = false;
    let expected_tool_values = expected_tool_values(probe_case, expected_value);
    let mut native_probe_calls = Vec::new();
    for call in &native {
        if call.name == TOOL_PROBE_TOOL_NAME {
            match &call.arguments {
                Some(Value::Object(map)) => {
                    if let Some(value) = map.get("value").and_then(Value::as_str) {
                        native_probe_calls.push(value.to_string());
                    }
                }
                _ => malformed_native = true,
            }
        }
    }
    let native_pass = probe_case.requires_probe_tool()
        && expected_tool_values
            .iter()
            .all(|expected| native_probe_calls.contains(expected))
        && (probe_case != ToolProbeCase::ParallelToolCalls
            || (native_probe_calls.len() == expected_tool_values.len() && !malformed_native));
    if native_pass {
        return ToolConformanceCase {
            mode,
            ok: true,
            classification: ToolProbeClassification::StructuredNativeToolCall,
            fallback_mode: ToolProbeFallbackMode::Native,
            failure_reason: None,
            http_status,
            elapsed_ms,
            native_tool_call_count: native_count,
            text_tool_call_count: 0,
            usage,
            parser_errors: Vec::new(),
            protocol_violations: Vec::new(),
            content_sample: content_sample(response),
        };
    }

    let content = extract_content(response);
    let tools = probe_tool_registry();
    let tagged = crate::llm::tools::parse_text_tool_calls_with_tools(&content, Some(&tools));
    let parsed = if probe_marker_present(&tagged.calls, expected_value) {
        tagged
    } else {
        let fenced = crate::llm::tools::parse_fenced_json_tool_calls(&content);
        if probe_marker_present(&fenced.calls, expected_value) {
            fenced
        } else {
            tagged
        }
    };
    let text_count = parsed.calls.len();
    if !probe_case.requires_probe_tool() {
        return classify_no_tool_probe_response(
            mode,
            probe_case,
            expected_value,
            &content,
            (native_count, text_count),
            parsed,
            (http_status, elapsed_ms),
            usage,
        );
    }
    let text_pass = if probe_case == ToolProbeCase::ParallelToolCalls {
        probe_values_present(&parsed.calls, &expected_tool_values)
            && parsed.calls.len() == expected_tool_values.len()
    } else {
        probe_marker_present(&parsed.calls, expected_value)
    };
    if text_pass {
        return ToolConformanceCase {
            mode,
            ok: true,
            classification: ToolProbeClassification::ParseableHarnTextToolCall,
            fallback_mode: ToolProbeFallbackMode::Text,
            failure_reason: None,
            http_status,
            elapsed_ms,
            native_tool_call_count: native_count,
            text_tool_call_count: text_count,
            usage,
            parser_errors: parsed.errors,
            protocol_violations: parsed.violations,
            content_sample: sample_content(&content),
        };
    }

    let (classification, failure_reason) = if malformed_native || !parsed.errors.is_empty() {
        (
            ToolProbeClassification::MalformedJsonArguments,
            Some(first_non_empty(
                parsed.errors.first().cloned(),
                "malformed_tool_arguments",
            )),
        )
    } else if content.trim().is_empty() && native_count == 0 {
        (
            ToolProbeClassification::EmptySilent,
            Some("empty_silent_response".to_string()),
        )
    } else if has_raw_model_tool_tag(&content) {
        (
            ToolProbeClassification::RawModelToolTag,
            Some("raw_tool_tag_no_structured_calls".to_string()),
        )
    } else {
        (
            ToolProbeClassification::ProseOnlyNonTool,
            Some("no_executable_tool_call".to_string()),
        )
    };

    ToolConformanceCase {
        mode,
        ok: false,
        classification,
        fallback_mode: ToolProbeFallbackMode::Disabled,
        failure_reason,
        http_status,
        elapsed_ms,
        native_tool_call_count: native_count,
        text_tool_call_count: text_count,
        usage,
        parser_errors: parsed.errors,
        protocol_violations: parsed.violations,
        content_sample: sample_content(&content),
    }
}

fn classify_no_tool_probe_response(
    mode: ToolProbeMode,
    probe_case: ToolProbeCase,
    expected_value: &str,
    content: &str,
    counts: (usize, usize),
    parsed: crate::llm::tools::TextToolParseResult,
    timing: (Option<u16>, Option<u64>),
    usage: Option<ToolProbeUsage>,
) -> ToolConformanceCase {
    let (native_count, text_count) = counts;
    let (http_status, elapsed_ms) = timing;
    let unexpected_tool = native_count > 0 || text_count > 0;
    let empty = content.trim().is_empty() && !unexpected_tool;
    let ok = !unexpected_tool && !empty && content.contains(expected_value);
    let classification = if unexpected_tool {
        ToolProbeClassification::RawModelToolTag
    } else if empty {
        ToolProbeClassification::EmptySilent
    } else if ok {
        match probe_case {
            ToolProbeCase::NoToolAnswerOrRefusal => ToolProbeClassification::DirectAnswerNoTool,
            ToolProbeCase::UnavailableToolRepair => ToolProbeClassification::UnavailableToolRepair,
            ToolProbeCase::DoneSentinel => ToolProbeClassification::DoneSentinel,
            _ => ToolProbeClassification::ProseOnlyNonTool,
        }
    } else if has_raw_model_tool_tag(content) {
        ToolProbeClassification::RawModelToolTag
    } else {
        ToolProbeClassification::ProseOnlyNonTool
    };
    let failure_reason = (!ok).then(|| {
        if unexpected_tool {
            "unexpected_tool_call".to_string()
        } else if empty {
            "empty_silent_response".to_string()
        } else {
            format!("missing_expected_text:{expected_value}")
        }
    });
    ToolConformanceCase {
        mode,
        ok,
        classification,
        fallback_mode: ToolProbeFallbackMode::Disabled,
        failure_reason,
        http_status,
        elapsed_ms,
        native_tool_call_count: native_count,
        text_tool_call_count: text_count,
        usage,
        parser_errors: parsed.errors,
        protocol_violations: parsed.violations,
        content_sample: sample_content(content),
    }
}

fn chat_url(def: &ProviderDef, base_url: &str) -> Result<String, String> {
    let endpoint = if def.chat_endpoint.trim().is_empty() {
        "/v1/chat/completions"
    } else {
        def.chat_endpoint.as_str()
    };
    let url = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        endpoint.to_string()
    } else if endpoint.starts_with('/') {
        format!("{}{}", base_url.trim_end_matches('/'), endpoint)
    } else {
        format!("{}/{}", base_url.trim_end_matches('/'), endpoint)
    };
    reqwest::Url::parse(&url)
        .map(|_| url.clone())
        .map_err(|error| format!("invalid provider chat URL '{url}': {error}"))
}

#[derive(Debug)]
struct NativeToolCall {
    name: String,
    arguments: Option<Value>,
}

fn extract_native_tool_calls(response: &Value) -> Vec<NativeToolCall> {
    let mut calls = Vec::new();
    visit_native_tool_call_arrays(response, &mut calls);
    calls
}

fn visit_native_tool_call_arrays(value: &Value, calls: &mut Vec<NativeToolCall>) {
    match value {
        Value::Object(map) => {
            if let Some(call) = parse_anthropic_tool_use_object(map) {
                calls.push(call);
            }
            if let Some(tool_calls) = map.get("tool_calls").and_then(Value::as_array) {
                for item in tool_calls {
                    if let Some(call) = parse_native_tool_call(item) {
                        calls.push(call);
                    }
                }
            }
            for child in map.values() {
                visit_native_tool_call_arrays(child, calls);
            }
        }
        Value::Array(items) => {
            for item in items {
                visit_native_tool_call_arrays(item, calls);
            }
        }
        _ => {}
    }
}

fn parse_anthropic_tool_use_object(
    object: &serde_json::Map<String, Value>,
) -> Option<NativeToolCall> {
    if object.get("type").and_then(Value::as_str) != Some("tool_use") {
        return None;
    }
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())?
        .to_string();
    let arguments = object
        .get("input")
        .or_else(|| object.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    Some(NativeToolCall {
        name,
        arguments: Some(arguments),
    })
}

fn parse_native_tool_call(item: &Value) -> Option<NativeToolCall> {
    let obj = item.as_object()?;
    let function = obj.get("function").and_then(Value::as_object);
    let name = function
        .and_then(|function| function.get("name"))
        .or_else(|| obj.get("name"))
        .and_then(Value::as_str)?
        .to_string();
    match crate::llm::tools::parse_text_tool_call_from_native_name(&name) {
        crate::llm::tools::NativeToolNameTextCall::Parsed { name, arguments } => {
            return Some(NativeToolCall {
                name,
                arguments: Some(arguments),
            });
        }
        crate::llm::tools::NativeToolNameTextCall::Malformed { name, .. } => {
            return Some(NativeToolCall {
                name,
                arguments: None,
            });
        }
        crate::llm::tools::NativeToolNameTextCall::NotCall => {}
    }
    let raw_args = function
        .and_then(|function| function.get("arguments"))
        .or_else(|| obj.get("arguments"));
    let arguments = match raw_args {
        Some(Value::String(raw)) => serde_json::from_str::<Value>(raw).ok(),
        Some(value @ Value::Object(_)) => Some(value.clone()),
        Some(_) => None,
        None => Some(json!({})),
    };
    Some(NativeToolCall { name, arguments })
}

fn extract_content(response: &Value) -> String {
    let mut parts = Vec::new();
    visit_content(response, &mut parts);
    parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn visit_content(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if object_is_private_reasoning_content(map) {
                return;
            }
            for key in ["content", "response", "text"] {
                if let Some(text) = map.get(key).and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
            for (key, child) in map {
                if field_is_private_reasoning(key) {
                    continue;
                }
                visit_content(child, parts);
            }
        }
        Value::Array(items) => {
            for item in items {
                visit_content(item, parts);
            }
        }
        _ => {}
    }
}

fn object_is_private_reasoning_content(map: &serde_json::Map<String, Value>) -> bool {
    let block_type = map.get("type").and_then(Value::as_str).unwrap_or("");
    if matches!(block_type, "reasoning" | "thinking" | "reasoning_summary") {
        return true;
    }
    matches!(
        map.get("visibility").and_then(Value::as_str),
        Some("private" | "internal")
    )
}

fn field_is_private_reasoning(field: &str) -> bool {
    matches!(
        field,
        "analysis"
            | "reasoning"
            | "reasoning_content"
            | "reasoning_details"
            | "reasoning_summary"
            | "thinking"
            | "thinking_summary"
    )
}

fn has_raw_model_tool_tag(content: &str) -> bool {
    let lowered = content.to_ascii_lowercase();
    lowered.contains("<tool_call")
        || lowered.contains("<toolcall")
        || lowered.contains("tool_code:")
        || lowered.contains("tool_call:")
        || lowered.contains("call:")
        || lowered.contains("<function")
}

fn content_sample(response: &Value) -> Option<String> {
    sample_content(&extract_content(response))
}

fn sample_content(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(240).collect())
    }
}

fn sample_failure(text: &str, fallback: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        format!(
            "{fallback}: {}",
            trimmed.chars().take(240).collect::<String>()
        )
    }
}

fn first_non_empty(value: Option<String>, fallback: &str) -> String {
    value
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn elapsed_ms(clock: &dyn harn_clock::Clock, started_ms: i64) -> u64 {
    clock.monotonic_ms().saturating_sub(started_ms).max(0) as u64
}

#[cfg(test)]
#[path = "tool_conformance_tests.rs"]
mod tests;
