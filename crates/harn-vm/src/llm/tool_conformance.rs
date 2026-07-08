//! One-tool provider conformance probe for local/runtime tool calling.
//!
//! The probe is deliberately tiny: define one harmless `echo_marker` tool,
//! ask the model to call it with a fixed marker, and classify what came back.
//! The classification is the stable contract eval harnesses consume; the live
//! HTTP runner is a convenience around that classifier.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::llm::api::{LlmApiMode, LlmRequestPayload, OutputFormat, ThinkingConfig};
use crate::llm::capabilities::WireDialect;
use crate::llm_config::{self, ProviderDef};
use crate::value::VmValue;

pub const TOOL_CONFORMANCE_SCHEMA_VERSION: u32 = 1;
pub const TOOL_PROBE_TOOL_NAME: &str = "echo_marker";
pub const DEFAULT_TOOL_PROBE_MARKER: &str = "harn_tool_probe_marker";

#[derive(Debug, Clone)]
pub struct ToolConformanceProbeOptions {
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    pub modes: Vec<ToolProbeMode>,
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
    pub tool_name: String,
    pub marker: String,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parser_errors: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
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
    for _ in 0..options.repeat.max(1) {
        for mode in &modes {
            cases.push(
                execute_live_probe_case(
                    &provider,
                    &model_id,
                    base_url.as_deref(),
                    *mode,
                    &options.marker,
                    options.timeout_secs,
                )
                .await,
            );
        }
    }
    report_from_cases(provider, model_id, base_url, options.marker, cases)
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
    let marker = marker.into();
    let response = serde_json::from_str::<Value>(raw).unwrap_or_else(|_| json!({ "content": raw }));
    let case = classify_tool_probe_response(mode, &response, &marker, None, None);
    report_from_cases(provider.into(), model.into(), None, marker, vec![case])
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
    marker: String,
    cases: Vec<ToolConformanceCase>,
) -> ToolConformanceReport {
    let summary = summarize_cases(&cases);
    ToolConformanceReport {
        schema_version: TOOL_CONFORMANCE_SCHEMA_VERSION,
        provider,
        model,
        base_url,
        tool_name: TOOL_PROBE_TOOL_NAME.to_string(),
        marker,
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
    let body = probe_request_body(provider, model, mode, marker);
    let client = if mode == ToolProbeMode::Streaming {
        crate::llm::shared_streaming_client().clone()
    } else {
        crate::llm::shared_blocking_client().clone()
    };
    let api_key = crate::llm::helpers::resolve_api_key(provider).unwrap_or_default();
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

fn probe_request_body(provider: &str, model: &str, mode: ToolProbeMode, marker: &str) -> Value {
    let payload = probe_request_payload(provider, model, mode, marker);
    provider_compatible_probe_request_body(&payload)
}

fn probe_request_payload(
    provider: &str,
    model: &str,
    mode: ToolProbeMode,
    marker: &str,
) -> LlmRequestPayload {
    let prompt = format!(
        "Call the {TOOL_PROBE_TOOL_NAME} tool exactly once with value {marker:?}. Do not answer in prose."
    );
    let native_tools =
        crate::llm::tools::vm_tools_to_native(&probe_tool_registry(), provider, model)
            .expect("tool probe registry is static and should convert to native tools");
    let tool_choice = if crate::llm::provider::provider_uses_ollama_messages(provider, model) {
        None
    } else {
        Some(json!({
            "type": "function",
            "function": {"name": TOOL_PROBE_TOOL_NAME}
        }))
    };
    LlmRequestPayload {
        provider: provider.to_string(),
        model: model.to_string(),
        region: None,
        api_key: String::new(),
        api_mode: LlmApiMode::ChatCompletions,
        fallback_chain: Vec::new(),
        route_fallbacks: Vec::new(),
        messages: vec![json!({"role": "user", "content": prompt})],
        system: None,
        max_tokens: 256,
        temperature: Some(0.0),
        top_p: None,
        top_k: None,
        logprobs: false,
        top_logprobs: None,
        stop: None,
        seed: None,
        frequency_penalty: None,
        presence_penalty: None,
        fast: false,
        output_format: OutputFormat::Text,
        response_format: None,
        json_schema: None,
        output_schema: None,
        schema_stream_abort: false,
        thinking: ThinkingConfig::Disabled,
        anthropic_beta_features: Vec::new(),
        vision: false,
        native_tools: Some(native_tools),
        provider_tools: Vec::new(),
        tool_choice,
        cache: false,
        prompt_cache_ttl: None,
        timeout: None,
        stream: mode == ToolProbeMode::Streaming,
        provider_overrides: None,
        previous_response_id: None,
        store: None,
        background: None,
        truncation: None,
        compact: None,
        include: None,
        max_tool_calls: None,
        prefill: None,
        session_id: None,
        reminder_lifecycle: Vec::new(),
        cli_llm_mock_scope: None,
    }
}

fn provider_compatible_probe_request_body(payload: &LlmRequestPayload) -> Value {
    match payload.provider.as_str() {
        "azure_openai" => {
            return crate::llm::providers::AzureOpenAiProvider::build_request_body(payload);
        }
        "bedrock" => {
            return crate::llm::providers::BedrockProvider::build_request_body(payload);
        }
        "vertex" => {
            return crate::llm::providers::VertexProvider::build_request_body(payload);
        }
        _ => {}
    }

    match crate::llm::capabilities::lookup(&payload.provider, &payload.model).message_wire_format {
        WireDialect::Anthropic => {
            crate::llm::providers::AnthropicProvider::build_request_body(payload)
        }
        WireDialect::Gemini => crate::llm::providers::GeminiProvider::build_request_body(payload),
        WireDialect::Ollama => crate::llm::providers::OllamaProvider::build_request_body(payload),
        WireDialect::OpenAiCompat => {
            crate::llm::providers::OpenAiCompatibleProvider::build_request_body(payload, false)
        }
    }
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
            for key in ["content", "response", "text"] {
                if let Some(text) = map.get(key).and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
            for child in map.values() {
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
mod tests {
    use super::*;

    #[test]
    fn probe_resolves_catalog_key_to_provider_wire_model() {
        let resolved = llm_config::resolve_model_info("baseten-glm-5.2");
        assert_eq!(resolved_probe_model_id(&resolved.id), "zai-org/GLM-5.2");
    }

    #[test]
    fn probe_request_body_uses_anthropic_tool_dialect() {
        let body = probe_request_body(
            "anthropic",
            "claude-sonnet-4-6",
            ToolProbeMode::NonStreaming,
            DEFAULT_TOOL_PROBE_MARKER,
        );

        assert_eq!(
            body["tools"][0]["name"], TOOL_PROBE_TOOL_NAME,
            "Anthropic tools use root-level names, not OpenAI function wrappers"
        );
        assert!(
            body["tools"][0].get("function").is_none(),
            "probe must not send OpenAI function-wrapper tools to Anthropic"
        );
        assert_eq!(
            body["tools"][0]["input_schema"]["properties"]["value"]["type"],
            "string"
        );
        assert_eq!(
            body["tool_choice"],
            json!({"type": "tool", "name": TOOL_PROBE_TOOL_NAME}),
            "Anthropic rejects OpenAI-shaped tool_choice objects"
        );
    }

    #[test]
    fn probe_request_body_preserves_openai_tool_dialect() {
        let body = probe_request_body(
            "openai",
            "gpt-5.4-mini",
            ToolProbeMode::NonStreaming,
            DEFAULT_TOOL_PROBE_MARKER,
        );

        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], TOOL_PROBE_TOOL_NAME);
        assert_eq!(
            body["tool_choice"],
            json!({"type": "function", "function": {"name": TOOL_PROBE_TOOL_NAME}})
        );
    }

    #[test]
    fn probe_request_body_maps_gemini_tool_choice_to_tool_config() {
        let body = probe_request_body(
            "gemini",
            "gemini-2.5-pro",
            ToolProbeMode::NonStreaming,
            DEFAULT_TOOL_PROBE_MARKER,
        );
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            TOOL_PROBE_TOOL_NAME
        );
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
        assert_eq!(
            body["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"],
            json!([TOOL_PROBE_TOOL_NAME])
        );
    }

    #[test]
    fn classify_openai_native_tool_call_as_pass() {
        let report = classify_tool_conformance_fixture(
            "local",
            "model",
            ToolProbeMode::NonStreaming,
            DEFAULT_TOOL_PROBE_MARKER,
            r#"{"choices":[{"message":{"tool_calls":[{"id":"call_1","type":"function","function":{"name":"echo_marker","arguments":"{\"value\":\"harn_tool_probe_marker\"}"}}]}}]}"#,
        );
        assert_eq!(report.tool_calling.native, ToolProbeStatus::Pass);
        assert_eq!(
            report.tool_calling.fallback_mode,
            ToolProbeFallbackMode::Native
        );
        assert_eq!(
            report.cases[0].classification,
            ToolProbeClassification::StructuredNativeToolCall
        );
    }

    #[test]
    fn classify_anthropic_tool_use_as_native_pass() {
        let report = classify_tool_conformance_fixture(
            "anthropic",
            "claude-sonnet-4-6",
            ToolProbeMode::NonStreaming,
            DEFAULT_TOOL_PROBE_MARKER,
            r#"{"content":[{"type":"tool_use","id":"toolu_1","name":"echo_marker","input":{"value":"harn_tool_probe_marker"}}],"stop_reason":"tool_use"}"#,
        );

        assert_eq!(report.tool_calling.native, ToolProbeStatus::Pass);
        assert_eq!(
            report.tool_calling.fallback_mode,
            ToolProbeFallbackMode::Native
        );
        assert_eq!(
            report.cases[0].classification,
            ToolProbeClassification::StructuredNativeToolCall
        );
    }

    #[test]
    fn classify_native_tool_call_with_text_call_in_name_as_pass() {
        let report = classify_tool_conformance_fixture(
            "zai",
            "glm-5",
            ToolProbeMode::NonStreaming,
            DEFAULT_TOOL_PROBE_MARKER,
            r#"{"choices":[{"message":{"tool_calls":[{"id":"call_1","type":"function","function":{"name":"echo_marker({ value: \"harn_tool_probe_marker\" })</arg_value>","arguments":"{}"}}]}}]}"#,
        );

        assert_eq!(report.tool_calling.native, ToolProbeStatus::Pass);
        assert_eq!(
            report.tool_calling.fallback_mode,
            ToolProbeFallbackMode::Native
        );
        assert_eq!(
            report.cases[0].classification,
            ToolProbeClassification::StructuredNativeToolCall
        );
    }

    #[test]
    fn classify_partial_text_call_in_native_name_as_malformed() {
        let report = classify_tool_conformance_fixture(
            "zai",
            "glm-5",
            ToolProbeMode::NonStreaming,
            DEFAULT_TOOL_PROBE_MARKER,
            r#"{"choices":[{"message":{"tool_calls":[{"id":"call_1","type":"function","function":{"name":"echo_marker({ value: <<EOF","arguments":"{"}}]}}]}"#,
        );

        assert_eq!(report.tool_calling.native, ToolProbeStatus::Fail);
        assert_eq!(
            report.cases[0].classification,
            ToolProbeClassification::MalformedJsonArguments
        );
    }

    #[test]
    fn classify_gemma_raw_json_tool_call_content_as_text_fallback() {
        let report = classify_tool_conformance_fixture(
            "ollama",
            "gemma4:26b",
            ToolProbeMode::NonStreaming,
            DEFAULT_TOOL_PROBE_MARKER,
            r#"{"message":{"content":"<tool_call>{\"name\":\"echo_marker\",\"arguments\":{\"value\":\"harn_tool_probe_marker\"}}</tool_call>"}}"#,
        );
        assert_eq!(report.tool_calling.native, ToolProbeStatus::Fail);
        assert_eq!(report.tool_calling.text, ToolProbeStatus::Pass);
        assert_eq!(
            report.tool_calling.fallback_mode,
            ToolProbeFallbackMode::Text
        );
        assert_eq!(
            report.cases[0].classification,
            ToolProbeClassification::ParseableHarnTextToolCall
        );
    }

    #[test]
    fn classify_qwen_call_colon_marker_as_text_fallback() {
        let report = classify_tool_conformance_fixture(
            "llamacpp",
            "qwen",
            ToolProbeMode::NonStreaming,
            DEFAULT_TOOL_PROBE_MARKER,
            r#"{"content":"call:echo_marker{ value: \"harn_tool_probe_marker\" }"}"#,
        );
        assert_eq!(report.tool_calling.text, ToolProbeStatus::Pass);
        assert_eq!(
            report.tool_calling.fallback_mode,
            ToolProbeFallbackMode::Text
        );
    }

    #[test]
    fn classify_prose_only_as_disabled() {
        let report = classify_tool_conformance_fixture(
            "ollama",
            "gemma4:26b",
            ToolProbeMode::NonStreaming,
            DEFAULT_TOOL_PROBE_MARKER,
            r#"{"message":{"content":"The comment has been added. I will now verify it."}}"#,
        );
        assert_eq!(
            report.tool_calling.fallback_mode,
            ToolProbeFallbackMode::Disabled
        );
        assert_eq!(
            report.cases[0].classification,
            ToolProbeClassification::ProseOnlyNonTool
        );
        assert_eq!(
            report.cases[0].failure_reason.as_deref(),
            Some("no_executable_tool_call")
        );
    }

    #[test]
    fn prose_only_response_does_not_satisfy_required_tool_probe() {
        let report = classify_tool_conformance_fixture(
            "anthropic",
            "claude-sonnet-4-6",
            ToolProbeMode::NonStreaming,
            DEFAULT_TOOL_PROBE_MARKER,
            r#"{"content":[{"type":"text","text":"I can call echo_marker with that value."}],"stop_reason":"end_turn"}"#,
        );

        assert_eq!(
            report.cases[0].classification,
            ToolProbeClassification::ProseOnlyNonTool
        );
        assert!(!report_satisfies_required_probe(&report, "tool_probe"));
        assert_eq!(
            report.tool_calling.fallback_mode,
            ToolProbeFallbackMode::Disabled
        );
    }

    #[test]
    fn aggregates_openai_streaming_tool_call_deltas() {
        let raw = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"echo_marker\",\"arguments\":\"{\\\"value\\\":\"}}]}}]}\n\
                   data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"harn_tool_probe_marker\\\"}\"}}]}}]}\n\
                   data: [DONE]\n";
        let response = aggregate_stream_text(raw, "local");
        let case = classify_tool_probe_response(
            ToolProbeMode::Streaming,
            &response,
            DEFAULT_TOOL_PROBE_MARKER,
            None,
            None,
        );
        assert!(case.ok, "{case:?}");
        assert_eq!(
            case.classification,
            ToolProbeClassification::StructuredNativeToolCall
        );
    }

    #[test]
    fn aggregates_anthropic_streaming_tool_use_deltas() {
        let raw = "event: message_start\n\
                   data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n\
                   event: content_block_start\n\
                   data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"echo_marker\",\"input\":{}}}\n\
                   event: content_block_delta\n\
                   data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"value\\\":\"}}\n\
                   event: content_block_delta\n\
                   data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"harn_tool_probe_marker\\\"}\"}}\n\
                   event: message_delta\n\
                   data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\
                   event: message_stop\n\
                   data: {\"type\":\"message_stop\"}\n";
        let response = aggregate_stream_text(raw, "anthropic");
        let case = classify_tool_probe_response(
            ToolProbeMode::Streaming,
            &response,
            DEFAULT_TOOL_PROBE_MARKER,
            None,
            None,
        );
        assert!(case.ok, "{case:?}");
        assert_eq!(
            case.classification,
            ToolProbeClassification::StructuredNativeToolCall
        );
    }

    #[test]
    fn report_satisfies_tool_probe_when_text_fallback_passes() {
        let report = classify_tool_conformance_fixture(
            "llamacpp",
            "qwen",
            ToolProbeMode::NonStreaming,
            DEFAULT_TOOL_PROBE_MARKER,
            r#"{"content":"echo_marker({ value: \"harn_tool_probe_marker\" })"}"#,
        );
        assert!(report_satisfies_required_probe(&report, "tool_probe"));
        assert!(!report_satisfies_required_probe(
            &report,
            "native_tool_probe"
        ));
    }

    #[test]
    fn summary_requires_every_repeated_native_case_to_pass() {
        let summary = summarize_cases(&[
            probe_case(
                ToolProbeMode::NonStreaming,
                true,
                ToolProbeClassification::StructuredNativeToolCall,
            ),
            probe_case(
                ToolProbeMode::NonStreaming,
                false,
                ToolProbeClassification::ProseOnlyNonTool,
            ),
        ]);
        assert_eq!(summary.native, ToolProbeStatus::Fail);
        assert_eq!(summary.fallback_mode, ToolProbeFallbackMode::Disabled);
    }

    #[test]
    fn summary_requires_every_repeated_text_case_to_pass() {
        let summary = summarize_cases(&[
            probe_case(
                ToolProbeMode::NonStreaming,
                true,
                ToolProbeClassification::ParseableHarnTextToolCall,
            ),
            probe_case(
                ToolProbeMode::NonStreaming,
                false,
                ToolProbeClassification::MalformedJsonArguments,
            ),
        ]);
        assert_eq!(summary.native, ToolProbeStatus::Fail);
        assert_eq!(summary.text, ToolProbeStatus::Fail);
        assert_eq!(summary.fallback_mode, ToolProbeFallbackMode::Disabled);
    }

    #[test]
    fn summary_preserves_nonstreaming_text_fallback_when_streaming_fails() {
        let summary = summarize_cases(&[
            probe_case(
                ToolProbeMode::NonStreaming,
                true,
                ToolProbeClassification::ParseableHarnTextToolCall,
            ),
            probe_case(
                ToolProbeMode::Streaming,
                false,
                ToolProbeClassification::ProseOnlyNonTool,
            ),
        ]);
        assert_eq!(summary.native, ToolProbeStatus::Fail);
        assert_eq!(summary.streaming_native, ToolProbeStatus::Fail);
        assert_eq!(summary.text, ToolProbeStatus::Pass);
        assert_eq!(summary.fallback_mode, ToolProbeFallbackMode::Text);
    }

    fn probe_case(
        mode: ToolProbeMode,
        ok: bool,
        classification: ToolProbeClassification,
    ) -> ToolConformanceCase {
        let native_tool_call_count =
            usize::from(classification == ToolProbeClassification::StructuredNativeToolCall);
        let text_tool_call_count =
            usize::from(classification == ToolProbeClassification::ParseableHarnTextToolCall);
        ToolConformanceCase {
            mode,
            ok,
            classification,
            fallback_mode: ToolProbeFallbackMode::Disabled,
            failure_reason: None,
            http_status: None,
            elapsed_ms: None,
            native_tool_call_count,
            text_tool_call_count,
            parser_errors: Vec::new(),
            protocol_violations: Vec::new(),
            content_sample: None,
        }
    }
}
