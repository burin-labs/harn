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
use crate::value::VmValue;

#[path = "tool_conformance_request.rs"]
mod request;
use request::probe_request_body;

pub const TOOL_CONFORMANCE_SCHEMA_VERSION: u32 = 1;
pub const TOOL_CONFORMANCE_REQUEST_SCHEMA_VERSION: u32 = 2;
pub const TOOL_CONFORMANCE_REQUEST_AUDIT_SCHEMA_VERSION: u32 = 1;
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
    LargeStringArgument,
}

impl ToolProbeCase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SingleToolCall => "single_tool_call",
            Self::LargeStringArgument => "large_string_argument",
        }
    }

    pub fn catalog_request_audit_cases() -> Vec<Self> {
        vec![Self::SingleToolCall, Self::LargeStringArgument]
    }

    fn expected_value(self, marker: &str) -> String {
        match self {
            Self::SingleToolCall => marker.to_string(),
            Self::LargeStringArgument => format!(
                "marker={marker}\nquoted=\"value\"\njson={{\"marker\":{marker:?},\"nested\":[1,true]}}\nheredoc=<<EOF\n{marker}\nEOF\nunicode=\\u{{2603}}\\u{{1F680}}\nend"
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceRequestReport {
    pub schema_version: u32,
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default)]
    pub probe_case: ToolProbeCase,
    pub tool_name: String,
    pub marker: String,
    pub expected_value: String,
    pub requests: Vec<ToolConformanceRequestCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceRequestCase {
    pub mode: ToolProbeMode,
    pub request_body: Value,
    pub validation: ToolConformanceRequestValidation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceRequestValidation {
    pub dialect: String,
    pub status: ToolConformanceRequestValidationStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceRequestAuditReport {
    pub schema_version: u32,
    pub catalog_model_count: usize,
    pub route_count: usize,
    pub probe_cases: Vec<String>,
    pub modes: Vec<String>,
    pub request_count: usize,
    pub validation_pass_count: usize,
    pub validation_fail_count: usize,
    pub dialect_counts: BTreeMap<String, usize>,
    pub provider_counts: BTreeMap<String, usize>,
    pub routes: Vec<ToolConformanceRequestAuditRoute>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<ToolConformanceRequestAuditFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceRequestAuditRoute {
    pub provider: String,
    pub model: String,
    pub request_count: usize,
    pub validation_pass_count: usize,
    pub validation_fail_count: usize,
    pub dialect_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceRequestAuditFailure {
    pub provider: String,
    pub model: String,
    pub probe_case: String,
    pub mode: String,
    pub dialect: String,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolConformanceRequestValidationStatus {
    Pass,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProbeClassification {
    StructuredNativeToolCall,
    ParseableHarnTextToolCall,
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
    let response = serde_json::from_str::<Value>(raw).unwrap_or_else(|_| json!({ "content": raw }));
    let case = classify_tool_probe_response(mode, &response, &expected_value, None, None);
    report_from_cases(
        provider.into(),
        model.into(),
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
    marker: impl Into<String>,
) -> Result<ToolConformanceRequestReport, String> {
    let provider = provider.into();
    let model = model.into();
    let marker = marker.into();
    let expected_value = probe_case.expected_value(&marker);
    let mut requests = Vec::new();
    for mode in normalized_modes(&modes) {
        let request_body =
            probe_request_body(&provider, &model, mode, probe_case, &expected_value)?;
        requests.push(ToolConformanceRequestCase {
            mode,
            validation: validate_probe_request_body(&provider, &model, &request_body),
            request_body,
        });
    }
    Ok(ToolConformanceRequestReport {
        schema_version: TOOL_CONFORMANCE_REQUEST_SCHEMA_VERSION,
        provider,
        model,
        base_url,
        probe_case,
        tool_name: TOOL_PROBE_TOOL_NAME.to_string(),
        marker,
        expected_value,
        requests,
    })
}

fn validate_probe_request_body(
    provider: &str,
    model: &str,
    body: &Value,
) -> ToolConformanceRequestValidation {
    let caps = crate::llm::capabilities::lookup(provider, model);
    let dialect = request_validation_dialect(provider, &caps);
    let mut issues = Vec::new();
    match dialect.as_str() {
        "anthropic" => validate_anthropic_probe_request(body, &mut issues),
        "bedrock" => validate_bedrock_probe_request(body, &mut issues),
        "gemini" | "vertex" => validate_gemini_probe_request(body, &mut issues),
        "ollama" => validate_ollama_probe_request(body, &mut issues),
        "openai_compat" => validate_openai_compat_probe_request(body, &caps, &mut issues),
        _ => issues.push(format!("unsupported validation dialect `{dialect}`")),
    }
    ToolConformanceRequestValidation {
        dialect,
        status: if issues.is_empty() {
            ToolConformanceRequestValidationStatus::Pass
        } else {
            ToolConformanceRequestValidationStatus::Fail
        },
        issues,
    }
}

fn request_validation_dialect(
    provider: &str,
    caps: &crate::llm::capabilities::Capabilities,
) -> String {
    if provider == "bedrock" {
        return "bedrock".to_string();
    }
    if provider == "vertex" {
        return "vertex".to_string();
    }
    match caps.message_wire_format {
        crate::llm::capabilities::WireDialect::Anthropic => "anthropic".to_string(),
        crate::llm::capabilities::WireDialect::Gemini => "gemini".to_string(),
        crate::llm::capabilities::WireDialect::Ollama => "ollama".to_string(),
        crate::llm::capabilities::WireDialect::OpenAiCompat => "openai_compat".to_string(),
    }
}

fn validate_openai_compat_probe_request(
    body: &Value,
    caps: &crate::llm::capabilities::Capabilities,
    issues: &mut Vec<String>,
) {
    require_array(body, "/messages", issues);
    require_openai_function_tool(body, "/tools/0", issues);
    validate_openai_compat_tool_choice(body, caps, issues);
    reject_present(body, "/toolConfig", "OpenAI-compatible request", issues);
}

fn validate_openai_compat_tool_choice(
    body: &Value,
    caps: &crate::llm::capabilities::Capabilities,
    issues: &mut Vec<String>,
) {
    let Some(tool_choice) = body.get("tool_choice") else {
        issues.push("OpenAI-compatible request missing /tool_choice".to_string());
        return;
    };
    if tool_choice.pointer("/type").and_then(Value::as_str) == Some("function") {
        require_string_eq(
            body,
            "/tool_choice/function/name",
            TOOL_PROBE_TOOL_NAME,
            "OpenAI-compatible tool_choice.function.name",
            issues,
        );
        return;
    }
    if let Some(mode) = tool_choice.as_str() {
        if caps
            .allowed_tool_choice_modes
            .iter()
            .any(|allowed| allowed == mode)
        {
            return;
        }
        issues.push(format!(
            "OpenAI-compatible tool_choice mode `{mode}` is not allowed by catalog capabilities"
        ));
        return;
    }
    require_string_eq(
        body,
        "/tool_choice/type",
        "function",
        "OpenAI-compatible tool_choice.type",
        issues,
    );
}

fn validate_anthropic_probe_request(body: &Value, issues: &mut Vec<String>) {
    require_array(body, "/messages", issues);
    require_string_eq(
        body,
        "/tools/0/name",
        TOOL_PROBE_TOOL_NAME,
        "Anthropic tool name",
        issues,
    );
    require_string_eq(
        body,
        "/tools/0/input_schema/properties/value/type",
        "string",
        "Anthropic input_schema value type",
        issues,
    );
    reject_present(
        body,
        "/tools/0/function",
        "Anthropic tool declaration",
        issues,
    );
    require_string_eq(
        body,
        "/tool_choice/type",
        "tool",
        "Anthropic tool_choice.type",
        issues,
    );
    require_string_eq(
        body,
        "/tool_choice/name",
        TOOL_PROBE_TOOL_NAME,
        "Anthropic tool_choice.name",
        issues,
    );
}

fn validate_gemini_probe_request(body: &Value, issues: &mut Vec<String>) {
    require_array(body, "/contents", issues);
    require_string_eq(
        body,
        "/tools/0/functionDeclarations/0/name",
        TOOL_PROBE_TOOL_NAME,
        "Gemini function declaration name",
        issues,
    );
    require_string_eq(
        body,
        "/tools/0/functionDeclarations/0/parameters/properties/value/type",
        "string",
        "Gemini function declaration value type",
        issues,
    );
    require_string_eq(
        body,
        "/toolConfig/functionCallingConfig/mode",
        "ANY",
        "Gemini toolConfig mode",
        issues,
    );
    require_array_contains_string(
        body,
        "/toolConfig/functionCallingConfig/allowedFunctionNames",
        TOOL_PROBE_TOOL_NAME,
        "Gemini allowedFunctionNames",
        issues,
    );
    reject_present(body, "/tool_choice", "Gemini request", issues);
}

fn validate_bedrock_probe_request(body: &Value, issues: &mut Vec<String>) {
    require_array(body, "/messages", issues);
    require_string_eq(
        body,
        "/toolConfig/tools/0/toolSpec/name",
        TOOL_PROBE_TOOL_NAME,
        "Bedrock toolSpec name",
        issues,
    );
    require_string_eq(
        body,
        "/toolConfig/tools/0/toolSpec/inputSchema/json/properties/value/type",
        "string",
        "Bedrock toolSpec value type",
        issues,
    );
    reject_present(body, "/tool_choice", "Bedrock request", issues);
}

fn validate_ollama_probe_request(body: &Value, issues: &mut Vec<String>) {
    require_array(body, "/messages", issues);
    require_openai_function_tool(body, "/tools/0", issues);
    reject_present(body, "/tool_choice", "Ollama request", issues);
}

fn require_openai_function_tool(body: &Value, base: &str, issues: &mut Vec<String>) {
    require_string_eq(
        body,
        &format!("{base}/type"),
        "function",
        "tool type",
        issues,
    );
    require_string_eq(
        body,
        &format!("{base}/function/name"),
        TOOL_PROBE_TOOL_NAME,
        "function tool name",
        issues,
    );
    require_string_eq(
        body,
        &format!("{base}/function/parameters/properties/value/type"),
        "string",
        "function tool value type",
        issues,
    );
}

fn require_array(body: &Value, pointer: &str, issues: &mut Vec<String>) {
    if !body.pointer(pointer).is_some_and(Value::is_array) {
        issues.push(format!("{pointer} must be an array"));
    }
}

fn require_string_eq(
    body: &Value,
    pointer: &str,
    expected: &str,
    label: &str,
    issues: &mut Vec<String>,
) {
    match body.pointer(pointer).and_then(Value::as_str) {
        Some(actual) if actual == expected => {}
        Some(actual) => issues.push(format!("{label} must be `{expected}`, got `{actual}`")),
        None => issues.push(format!("{label} missing at {pointer}")),
    }
}

fn require_array_contains_string(
    body: &Value,
    pointer: &str,
    expected: &str,
    label: &str,
    issues: &mut Vec<String>,
) {
    let Some(values) = body.pointer(pointer).and_then(Value::as_array) else {
        issues.push(format!("{label} missing array at {pointer}"));
        return;
    };
    if !values.iter().any(|value| value.as_str() == Some(expected)) {
        issues.push(format!("{label} must contain `{expected}`"));
    }
}

fn reject_present(body: &Value, pointer: &str, label: &str, issues: &mut Vec<String>) {
    if body.pointer(pointer).is_some() {
        issues.push(format!("{label} must not include {pointer}"));
    }
}

pub fn tool_conformance_request_report_json(
    provider: impl Into<String>,
    model: impl Into<String>,
    base_url: Option<String>,
    modes: Vec<ToolProbeMode>,
    probe_case: ToolProbeCase,
    marker: impl Into<String>,
) -> Result<String, String> {
    let report =
        tool_conformance_request_report(provider, model, base_url, modes, probe_case, marker)?;
    serde_json::to_string_pretty(&report).map_err(|error| {
        format!("internal error: failed to render tool-probe request report: {error}")
    })
}

pub fn tool_conformance_request_catalog_audit(
    probe_cases: Vec<ToolProbeCase>,
    modes: Vec<ToolProbeMode>,
) -> ToolConformanceRequestAuditReport {
    let probe_cases = if probe_cases.is_empty() {
        ToolProbeCase::catalog_request_audit_cases()
    } else {
        probe_cases
    };
    let modes = normalized_modes(&modes);
    let entries = llm_config::model_catalog_entries();
    let mut request_count = 0usize;
    let mut validation_pass_count = 0usize;
    let mut validation_fail_count = 0usize;
    let mut dialect_counts = BTreeMap::new();
    let mut provider_counts = BTreeMap::new();
    let mut routes = Vec::new();
    let mut failures = Vec::new();

    for (model_id, model) in &entries {
        let mut route = ToolConformanceRequestAuditRoute {
            provider: model.provider.clone(),
            model: model_id.clone(),
            request_count: 0,
            validation_pass_count: 0,
            validation_fail_count: 0,
            dialect_counts: BTreeMap::new(),
        };
        for probe_case in &probe_cases {
            for mode in &modes {
                route.request_count += 1;
                request_count += 1;
                *provider_counts.entry(model.provider.clone()).or_insert(0) += 1;
                match tool_conformance_request_report(
                    model.provider.clone(),
                    model_id.clone(),
                    None,
                    vec![*mode],
                    *probe_case,
                    DEFAULT_TOOL_PROBE_MARKER,
                ) {
                    Ok(report) => {
                        for request in report.requests {
                            let dialect = request.validation.dialect.clone();
                            *dialect_counts.entry(dialect.clone()).or_insert(0) += 1;
                            *route.dialect_counts.entry(dialect.clone()).or_insert(0) += 1;
                            match request.validation.status {
                                ToolConformanceRequestValidationStatus::Pass => {
                                    route.validation_pass_count += 1;
                                    validation_pass_count += 1;
                                }
                                ToolConformanceRequestValidationStatus::Fail => {
                                    route.validation_fail_count += 1;
                                    validation_fail_count += 1;
                                    failures.push(ToolConformanceRequestAuditFailure {
                                        provider: model.provider.clone(),
                                        model: model_id.clone(),
                                        probe_case: probe_case.as_str().to_string(),
                                        mode: mode.as_str().to_string(),
                                        dialect,
                                        issues: request.validation.issues,
                                    });
                                }
                            }
                        }
                    }
                    Err(error) => {
                        route.validation_fail_count += 1;
                        validation_fail_count += 1;
                        failures.push(ToolConformanceRequestAuditFailure {
                            provider: model.provider.clone(),
                            model: model_id.clone(),
                            probe_case: probe_case.as_str().to_string(),
                            mode: mode.as_str().to_string(),
                            dialect: "request_build".to_string(),
                            issues: vec![error],
                        });
                    }
                }
            }
        }
        routes.push(route);
    }

    ToolConformanceRequestAuditReport {
        schema_version: TOOL_CONFORMANCE_REQUEST_AUDIT_SCHEMA_VERSION,
        catalog_model_count: entries.len(),
        route_count: routes.len(),
        probe_cases: probe_cases
            .into_iter()
            .map(|probe_case| probe_case.as_str().to_string())
            .collect(),
        modes: modes
            .into_iter()
            .map(|mode| mode.as_str().to_string())
            .collect(),
        request_count,
        validation_pass_count,
        validation_fail_count,
        dialect_counts,
        provider_counts,
        routes,
        failures,
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
    let body = match probe_request_body(provider, model, mode, probe_case, marker) {
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
    classify_tool_probe_response(
        mode,
        &response_value,
        marker,
        Some(status.as_u16()),
        elapsed,
    )
}

/// True when `calls` contains the probe's echo_marker call (the
/// `TOOL_PROBE_TOOL_NAME` tool with `args.value == marker`). Shared by the
/// tagged and fenced-JSON text-channel parse attempts.
fn probe_marker_present(calls: &[Value], marker: &str) -> bool {
    calls.iter().any(|call| {
        call.get("name").and_then(Value::as_str) == Some(TOOL_PROBE_TOOL_NAME)
            && call
                .get("arguments")
                .and_then(|args| args.get("value"))
                .and_then(Value::as_str)
                == Some(marker)
    })
}

fn classify_tool_probe_response(
    mode: ToolProbeMode,
    response: &Value,
    marker: &str,
    http_status: Option<u16>,
    elapsed_ms: Option<u64>,
) -> ToolConformanceCase {
    let native = extract_native_tool_calls(response);
    let native_count = native.len();
    let mut malformed_native = false;
    for call in &native {
        if call.name == TOOL_PROBE_TOOL_NAME {
            match &call.arguments {
                Some(Value::Object(map))
                    if map.get("value").and_then(Value::as_str) == Some(marker) =>
                {
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
                        parser_errors: Vec::new(),
                        protocol_violations: Vec::new(),
                        content_sample: content_sample(response),
                    };
                }
                Some(Value::Object(_)) => {}
                _ => malformed_native = true,
            }
        }
    }

    let content = extract_content(response);
    let tools = probe_tool_registry();
    // Try the canonical tagged/heredoc grammar first; if it does not yield the
    // echo_marker call, also try the fenced-JSON grammar. A fenced-JSON
    // emission that parses to the probe call still classifies as
    // ParseableHarnTextToolCall (it is a text-channel format) — the taxonomy is
    // unchanged, only the body grammar the text path accepts is extended.
    let tagged = crate::llm::tools::parse_text_tool_calls_with_tools(&content, Some(&tools));
    let parsed = if probe_marker_present(&tagged.calls, marker) {
        tagged
    } else {
        let fenced = crate::llm::tools::parse_fenced_json_tool_calls(&content);
        if probe_marker_present(&fenced.calls, marker) {
            fenced
        } else {
            tagged
        }
    };
    let text_count = parsed.calls.len();
    let text_pass = probe_marker_present(&parsed.calls, marker);
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
        parser_errors: parsed.errors,
        protocol_violations: parsed.violations,
        content_sample: sample_content(&content),
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

fn aggregate_stream_text(text: &str, _provider: &str) -> Value {
    let mut content = String::new();
    let mut calls: BTreeMap<String, PartialStreamCall> = BTreeMap::new();
    let mut frames = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let payload = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
        if payload == "[DONE]" {
            continue;
        }
        let Ok(frame) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        collect_stream_content_and_calls(&frame, &mut content, &mut calls);
        frames.push(frame);
    }
    let tool_calls: Vec<Value> = calls
        .into_values()
        .map(|call| {
            json!({
                "id": call.id.unwrap_or_else(|| "stream_tool".to_string()),
                "type": "function",
                "function": {
                    "name": call.name.unwrap_or_default(),
                    "arguments": call.arguments,
                }
            })
        })
        .collect();
    json!({
        "content": content,
        "tool_calls": tool_calls,
        "frames": frames,
    })
}

#[derive(Debug, Default)]
struct PartialStreamCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

fn collect_stream_content_and_calls(
    frame: &Value,
    content: &mut String,
    calls: &mut BTreeMap<String, PartialStreamCall>,
) {
    collect_anthropic_stream_content_and_calls(frame, content, calls);
    if let Some(text) = frame
        .pointer("/message/content")
        .or_else(|| frame.pointer("/choices/0/delta/content"))
        .or_else(|| frame.pointer("/choices/0/message/content"))
        .or_else(|| frame.get("response"))
        .and_then(Value::as_str)
    {
        content.push_str(text);
    }
    for item in frame
        .pointer("/message/tool_calls")
        .or_else(|| frame.pointer("/choices/0/delta/tool_calls"))
        .or_else(|| frame.pointer("/choices/0/message/tool_calls"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let key = item
            .get("index")
            .and_then(Value::as_u64)
            .map(|index| index.to_string())
            .or_else(|| item.get("id").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| calls.len().to_string());
        let slot = calls.entry(key).or_default();
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            slot.id = Some(id.to_string());
        }
        if let Some(name) = item
            .pointer("/function/name")
            .or_else(|| item.get("name"))
            .and_then(Value::as_str)
        {
            slot.name = Some(name.to_string());
        }
        if let Some(arguments) = item
            .pointer("/function/arguments")
            .or_else(|| item.get("arguments"))
        {
            match arguments {
                Value::String(delta) => slot.arguments.push_str(delta),
                Value::Object(_) => slot.arguments = arguments.to_string(),
                _ => {}
            }
        }
    }
}

fn collect_anthropic_stream_content_and_calls(
    frame: &Value,
    content: &mut String,
    calls: &mut BTreeMap<String, PartialStreamCall>,
) {
    match frame.get("type").and_then(Value::as_str) {
        Some("content_block_start") => {
            let Some(block) = frame.get("content_block") else {
                return;
            };
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    content.push_str(text);
                }
                return;
            }
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                return;
            }
            let key = frame
                .get("index")
                .and_then(Value::as_u64)
                .map(|index| index.to_string())
                .unwrap_or_else(|| calls.len().to_string());
            let slot = calls.entry(key).or_default();
            if let Some(id) = block.get("id").and_then(Value::as_str) {
                slot.id = Some(id.to_string());
            }
            if let Some(name) = block.get("name").and_then(Value::as_str) {
                slot.name = Some(name.to_string());
            }
            if let Some(input) = block.get("input").filter(|input| !input.is_null()) {
                if !(input.is_object() && input.as_object().is_some_and(|object| object.is_empty()))
                {
                    slot.arguments = input.to_string();
                }
            }
        }
        Some("content_block_delta") => {
            let key = frame
                .get("index")
                .and_then(Value::as_u64)
                .map(|index| index.to_string())
                .unwrap_or_else(|| calls.len().to_string());
            let Some(delta) = frame.get("delta") else {
                return;
            };
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => {
                    if let Some(text) = delta.get("text").and_then(Value::as_str) {
                        content.push_str(text);
                    }
                }
                Some("input_json_delta") => {
                    if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                        calls.entry(key).or_default().arguments.push_str(partial);
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

fn probe_tool_registry() -> VmValue {
    let mut value_param = BTreeMap::new();
    value_param.insert("type".to_string(), vm_str("string"));
    value_param.insert(
        "description".to_string(),
        vm_str("The marker value to echo."),
    );
    let mut params = BTreeMap::new();
    params.insert("value".to_string(), VmValue::dict(value_param));
    let tool = vm_dict(&[
        ("name", vm_str(TOOL_PROBE_TOOL_NAME)),
        ("description", vm_str("Echo the probe marker exactly.")),
        ("parameters", VmValue::dict(params)),
    ]);
    vm_dict(&[("tools", VmValue::List(std::sync::Arc::new(vec![tool])))])
}

fn vm_str(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn vm_dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map = BTreeMap::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    VmValue::dict(map)
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
