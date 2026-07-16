use serde_json::{json, Value};

use super::{
    probe_tool_registry, ToolConformanceRequestValidation, ToolConformanceRequestValidationStatus,
    ToolConformanceRequestWarning, ToolProbeCase, ToolProbeMode, ToolProbeRequestProfile,
    TOOL_PROBE_TOOL_NAME,
};
use crate::llm::api::{LlmApiMode, LlmRequestPayload, OutputFormat};
use crate::llm::capabilities::WireDialect;
use crate::llm_config;

const ANTHROPIC_THINKING_SIGNATURE: &str = "harn-scorecard-anthropic-thinking-signature";
const ANTHROPIC_REDACTED_THINKING_DATA: &str = "harn-scorecard-redacted-thinking-payload";
const GEMINI_THOUGHT_SIGNATURE: &str = "harn-scorecard-gemini-thinking-signature";

pub(super) fn probe_request_body(
    provider: &str,
    model: &str,
    mode: ToolProbeMode,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    marker: &str,
) -> Result<Value, String> {
    probe_request_body_with_warnings(provider, model, mode, probe_case, request_profile, marker)
        .map(|(body, _warnings)| body)
}

pub(super) fn probe_request_body_with_warnings(
    provider: &str,
    model: &str,
    mode: ToolProbeMode,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    marker: &str,
) -> Result<(Value, Vec<ToolConformanceRequestWarning>), String> {
    let payload =
        probe_request_payload(provider, model, mode, probe_case, request_profile, marker)?;
    let body = provider_compatible_probe_request_body(&payload);
    let warnings = request_body_warnings(&payload, &body);
    Ok((body, warnings))
}

pub(super) fn probe_request_payload(
    provider: &str,
    model: &str,
    mode: ToolProbeMode,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    marker: &str,
) -> Result<LlmRequestPayload, String> {
    let model_defaults = llm_config::model_params_for_route(provider, model);
    let default_float =
        |key: &str| -> Option<f64> { model_defaults.get(key).and_then(toml::Value::as_float) };
    let default_int =
        |key: &str| -> Option<i64> { model_defaults.get(key).and_then(toml::Value::as_integer) };
    let native_tools = if probe_case.request_uses_probe_tool() {
        Some(
            crate::llm::tools::vm_tools_to_native(&probe_tool_registry(), provider, model)
                .expect("tool probe registry is static and should convert to native tools"),
        )
    } else {
        None
    };
    let mut tool_choice = if probe_case.requires_probe_tool()
        && !crate::llm::provider::provider_uses_ollama_messages(provider, model)
    {
        Some(json!({
            "type": "function",
            "function": {"name": TOOL_PROBE_TOOL_NAME}
        }))
    } else {
        None
    };
    if request_profile == ToolProbeRequestProfile::ParameterEdges && tool_choice.is_some() {
        tool_choice = Some(json!("required"));
    }
    let caps = crate::llm::capabilities::lookup(provider, model);
    let thinking = crate::llm::helpers::resolve_catalog_thinking_config(
        &model_defaults,
        provider,
        model,
        &caps,
        true,
    )
    .map_err(|error| error.to_string())?;
    let mut payload = LlmRequestPayload {
        provider: provider.to_string(),
        model: model.to_string(),
        region: None,
        api_key: String::new(),
        api_mode: LlmApiMode::ChatCompletions,
        messages: Vec::new(),
        system: None,
        max_tokens: default_int("max_tokens").unwrap_or(256),
        temperature: Some(default_float("temperature").unwrap_or(0.0)),
        top_p: default_float("top_p"),
        top_k: default_int("top_k"),
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
        thinking,
        anthropic_beta_features: Vec::new(),
        vision: false,
        native_tools,
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
    };
    payload.messages = probe_messages(provider, probe_case, marker);
    apply_request_profile(&mut payload, request_profile);
    Ok(payload)
}

fn apply_request_profile(
    payload: &mut LlmRequestPayload,
    request_profile: ToolProbeRequestProfile,
) {
    match request_profile {
        ToolProbeRequestProfile::CatalogDefault => {}
        ToolProbeRequestProfile::ParameterEdges => {
            payload.max_tokens = 1;
            payload.temperature = Some(2.0);
            payload.top_p = Some(1.0);
            payload.top_k = Some(1);
        }
    }
}

fn probe_prompt(probe_case: ToolProbeCase, marker: &str) -> String {
    match probe_case {
        ToolProbeCase::SingleToolCall => format!(
            "Call the {TOOL_PROBE_TOOL_NAME} tool exactly once with value {marker:?}. Do not answer in prose."
        ),
        ToolProbeCase::ParallelToolCalls => {
            let first = format!("{marker}:first");
            let second = format!("{marker}:second");
            [
                format!("Call the {TOOL_PROBE_TOOL_NAME} tool exactly twice."),
                "The calls must be in the same assistant response.".to_string(),
                format!("The first call's value argument must exactly equal {first:?}."),
                format!("The second call's value argument must exactly equal {second:?}."),
                "Do not answer in prose.".to_string(),
            ]
            .join(" ")
        }
        ToolProbeCase::LargeStringArgument => format!(
            "Call the {TOOL_PROBE_TOOL_NAME} tool exactly once. The value argument must exactly equal this string, preserving newlines and escapes: {marker:?}. Do not answer in prose."
        ),
        ToolProbeCase::ToolResultFollowup => format!(
            "Call the {TOOL_PROBE_TOOL_NAME} tool exactly once with value {marker:?}. After the tool result arrives, reply with the exact text {marker:?} and do not call any tool again."
        ),
        ToolProbeCase::SignedThinkingToolResultFollowup => format!(
            "The previous assistant turn used signed thinking before calling {TOOL_PROBE_TOOL_NAME}. Continue after the tool result by replying with the exact text {marker:?} and do not call any tool again."
        ),
        ToolProbeCase::NoToolAnswerOrRefusal => format!(
            "Do not call any tool. Reply with the exact text {marker:?} and nothing else."
        ),
        ToolProbeCase::UnavailableToolRepair => format!(
            "The user requested a web_search tool, but no such tool is available in this session. Do not call any tool. Reply with the exact text {marker:?} and nothing else."
        ),
        ToolProbeCase::DoneSentinel => {
            format!("Do not call any tool. Emit the exact completion sentinel {marker:?}.")
        }
    }
}

fn probe_messages(provider: &str, probe_case: ToolProbeCase, marker: &str) -> Vec<Value> {
    if probe_case == ToolProbeCase::SignedThinkingToolResultFollowup {
        return signed_thinking_probe_messages(provider, marker);
    }
    if probe_case != ToolProbeCase::ToolResultFollowup {
        return vec![json!({"role": "user", "content": probe_prompt(probe_case, marker)})];
    }

    let tool_call_id = "call_harn_tool_probe_1";
    vec![
        json!({"role": "user", "content": probe_prompt(probe_case, marker)}),
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": tool_call_id,
                "type": "function",
                "function": {
                    "name": TOOL_PROBE_TOOL_NAME,
                    "arguments": json!({"value": marker}).to_string(),
                },
            }],
        }),
        json!({
            "role": "tool",
            "name": TOOL_PROBE_TOOL_NAME,
            "tool_call_id": tool_call_id,
            "content": json!({"value": marker}).to_string(),
        }),
    ]
}

fn signed_thinking_probe_messages(provider: &str, marker: &str) -> Vec<Value> {
    let tool_call_id = "call_harn_tool_probe_thinking_1";
    let prompt = probe_prompt(ToolProbeCase::SignedThinkingToolResultFollowup, marker);
    if provider == "gemini" || provider == "vertex" {
        return vec![
            json!({"role": "user", "content": prompt}),
            json!({
                "role": "assistant",
                "content": [{
                    "functionCall": {
                        "id": tool_call_id,
                        "name": TOOL_PROBE_TOOL_NAME,
                        "args": {"value": marker},
                    },
                    "thoughtSignature": GEMINI_THOUGHT_SIGNATURE,
                }],
            }),
            json!({
                "role": "tool",
                "name": TOOL_PROBE_TOOL_NAME,
                "tool_call_id": tool_call_id,
                "content": json!({"value": marker}).to_string(),
            }),
        ];
    }

    vec![
        json!({"role": "user", "content": prompt}),
        json!({
            "role": "assistant",
            "content": [
                {
                    "type": "thinking",
                    "thinking": "Need the probe tool before answering.",
                    "signature": ANTHROPIC_THINKING_SIGNATURE,
                },
                {
                    "type": "redacted_thinking",
                    "data": ANTHROPIC_REDACTED_THINKING_DATA,
                },
                {
                    "type": "tool_use",
                    "id": tool_call_id,
                    "name": TOOL_PROBE_TOOL_NAME,
                    "input": {"value": marker},
                },
            ],
        }),
        json!({
            "role": "tool_result",
            "tool_use_id": tool_call_id,
            "content": json!({"value": marker}).to_string(),
        }),
    ]
}

fn provider_compatible_probe_request_body(payload: &LlmRequestPayload) -> Value {
    match payload.provider.as_str() {
        "azure_openai" => {
            return crate::llm::providers::AzureOpenAiProvider::build_request_body(payload);
        }
        "bedrock" => return crate::llm::providers::BedrockProvider::build_request_body(payload),
        "vertex" => return crate::llm::providers::VertexProvider::build_request_body(payload),
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

fn request_body_warnings(
    payload: &LlmRequestPayload,
    body: &Value,
) -> Vec<ToolConformanceRequestWarning> {
    let caps = crate::llm::capabilities::lookup(&payload.provider, &payload.model);
    let dialect = request_validation_dialect(&payload.provider, &caps);
    let mut omitted = Vec::new();

    match dialect.as_str() {
        "bedrock" => {
            push_omitted_sampling_param(
                &mut omitted,
                payload.temperature.is_some(),
                body,
                "/inferenceConfig/temperature",
                "temperature",
            );
            push_omitted_sampling_param(
                &mut omitted,
                payload.top_p.is_some(),
                body,
                "/inferenceConfig/topP",
                "top_p",
            );
            push_omitted_sampling_param(
                &mut omitted,
                payload.top_k.is_some(),
                body,
                "/inferenceConfig/topK",
                "top_k",
            );
        }
        "gemini" | "vertex" => {
            push_omitted_sampling_param(
                &mut omitted,
                payload.temperature.is_some(),
                body,
                "/generationConfig/temperature",
                "temperature",
            );
            push_omitted_sampling_param(
                &mut omitted,
                payload.top_p.is_some(),
                body,
                "/generationConfig/topP",
                "top_p",
            );
            push_omitted_sampling_param(
                &mut omitted,
                payload.top_k.is_some(),
                body,
                "/generationConfig/topK",
                "top_k",
            );
        }
        "ollama" => {
            push_omitted_sampling_param(
                &mut omitted,
                payload.temperature.is_some(),
                body,
                "/options/temperature",
                "temperature",
            );
            push_omitted_sampling_param(
                &mut omitted,
                payload.top_p.is_some(),
                body,
                "/options/top_p",
                "top_p",
            );
            push_omitted_sampling_param(
                &mut omitted,
                payload.top_k.is_some(),
                body,
                "/options/top_k",
                "top_k",
            );
        }
        _ => {
            push_omitted_sampling_param(
                &mut omitted,
                payload.temperature.is_some(),
                body,
                "/temperature",
                "temperature",
            );
            push_omitted_sampling_param(
                &mut omitted,
                payload.top_p.is_some(),
                body,
                "/top_p",
                "top_p",
            );
            push_omitted_sampling_param(
                &mut omitted,
                payload.top_k.is_some(),
                body,
                "/top_k",
                "top_k",
            );
        }
    }

    if omitted.is_empty() {
        Vec::new()
    } else {
        vec![ToolConformanceRequestWarning::SamplingParamsOmitted {
            dialect,
            params: omitted.into_iter().map(str::to_string).collect(),
        }]
    }
}

fn push_omitted_sampling_param(
    omitted: &mut Vec<&'static str>,
    payload_supplied: bool,
    body: &Value,
    pointer: &str,
    name: &'static str,
) {
    if payload_supplied && body.pointer(pointer).is_none() {
        omitted.push(name);
    }
}

pub(super) fn validate_probe_request_body(
    provider: &str,
    model: &str,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    body: &Value,
) -> ToolConformanceRequestValidation {
    let caps = crate::llm::capabilities::lookup(provider, model);
    let dialect = request_validation_dialect(provider, &caps);
    if probe_case == ToolProbeCase::SignedThinkingToolResultFollowup
        && !crate::llm::tool_scorecard::signed_thinking_tool_history_supported(provider, model)
    {
        return ToolConformanceRequestValidation {
            dialect,
            status: ToolConformanceRequestValidationStatus::NotApplicable,
            warnings: Vec::new(),
            issues: vec![format!(
                "signed thinking replay request is not applicable to {provider}:{model}; route has no signed-thinking tool-history surface"
            )],
        };
    }
    let mut issues = Vec::new();
    match dialect.as_str() {
        "anthropic" => {
            validate_anthropic_probe_request(body, probe_case, request_profile, &mut issues);
        }
        "bedrock" => validate_bedrock_probe_request(body, probe_case, &mut issues),
        "gemini" | "vertex" => {
            validate_gemini_probe_request(body, probe_case, request_profile, &mut issues);
        }
        "ollama" => validate_ollama_probe_request(body, probe_case, &mut issues),
        "openai_compat" => {
            validate_openai_compat_probe_request(body, probe_case, &caps, &mut issues);
        }
        _ => issues.push(format!("unsupported validation dialect `{dialect}`")),
    }
    validate_generation_parameter_ranges(body, &dialect, &mut issues);
    ToolConformanceRequestValidation {
        dialect,
        status: if issues.is_empty() {
            ToolConformanceRequestValidationStatus::Pass
        } else {
            ToolConformanceRequestValidationStatus::Fail
        },
        warnings: Vec::new(),
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
    probe_case: ToolProbeCase,
    caps: &crate::llm::capabilities::Capabilities,
    issues: &mut Vec<String>,
) {
    require_array(body, "/messages", issues);
    if probe_case == ToolProbeCase::SignedThinkingToolResultFollowup {
        issues.push(
            "signed thinking replay request is not defined for OpenAI-compatible dialects"
                .to_string(),
        );
        return;
    }
    if !probe_case.request_uses_probe_tool() {
        reject_present(body, "/tools", "OpenAI-compatible no-tool request", issues);
        reject_present(
            body,
            "/tool_choice",
            "OpenAI-compatible no-tool request",
            issues,
        );
        return;
    }
    require_openai_function_tool(body, "/tools/0", issues);
    if probe_case.requires_probe_tool() {
        validate_openai_compat_tool_choice(body, caps, issues);
    } else {
        reject_present(
            body,
            "/tool_choice",
            "OpenAI-compatible tool-result-followup request",
            issues,
        );
    }
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
        if caps.allowed_tool_choice_modes.is_empty()
            || caps
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

fn validate_anthropic_probe_request(
    body: &Value,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    issues: &mut Vec<String>,
) {
    require_array(body, "/messages", issues);
    if !probe_case.request_uses_probe_tool() {
        reject_present(body, "/tools", "Anthropic no-tool request", issues);
        reject_present(body, "/tool_choice", "Anthropic no-tool request", issues);
        return;
    }
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
    if probe_case == ToolProbeCase::SignedThinkingToolResultFollowup {
        validate_anthropic_signed_thinking_history(body, issues);
        reject_present(
            body,
            "/tool_choice",
            "Anthropic signed-thinking follow-up request",
            issues,
        );
        return;
    }
    if probe_case.requires_probe_tool() {
        match request_profile {
            ToolProbeRequestProfile::CatalogDefault => {
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
            ToolProbeRequestProfile::ParameterEdges => {
                require_string_eq(
                    body,
                    "/tool_choice/type",
                    "any",
                    "Anthropic parameter-edge tool_choice.type",
                    issues,
                );
            }
        }
    } else {
        reject_present(
            body,
            "/tool_choice",
            "Anthropic tool-result-followup request",
            issues,
        );
    }
}

fn validate_gemini_probe_request(
    body: &Value,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    issues: &mut Vec<String>,
) {
    require_array(body, "/contents", issues);
    if probe_case == ToolProbeCase::SignedThinkingToolResultFollowup {
        require_string_eq(
            body,
            "/tools/0/functionDeclarations/0/name",
            TOOL_PROBE_TOOL_NAME,
            "Gemini function declaration name",
            issues,
        );
        validate_gemini_signed_thinking_history(body, issues);
        reject_present(
            body,
            "/toolConfig",
            "Gemini signed-thinking follow-up request",
            issues,
        );
        reject_present(body, "/tool_choice", "Gemini request", issues);
        return;
    }
    if !probe_case.request_uses_probe_tool() {
        reject_present(body, "/tools", "Gemini no-tool request", issues);
        reject_present(body, "/toolConfig", "Gemini no-tool request", issues);
        return;
    }
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
    if probe_case.requires_probe_tool() {
        require_string_eq(
            body,
            "/toolConfig/functionCallingConfig/mode",
            "ANY",
            "Gemini toolConfig mode",
            issues,
        );
    }
    if probe_case.requires_probe_tool()
        && request_profile == ToolProbeRequestProfile::CatalogDefault
    {
        require_array_contains_string(
            body,
            "/toolConfig/functionCallingConfig/allowedFunctionNames",
            TOOL_PROBE_TOOL_NAME,
            "Gemini allowedFunctionNames",
            issues,
        );
    } else if !probe_case.requires_probe_tool() {
        reject_present(
            body,
            "/toolConfig",
            "Gemini tool-result-followup request",
            issues,
        );
    }
    reject_present(body, "/tool_choice", "Gemini request", issues);
}

fn validate_bedrock_probe_request(
    body: &Value,
    probe_case: ToolProbeCase,
    issues: &mut Vec<String>,
) {
    require_array(body, "/messages", issues);
    if probe_case == ToolProbeCase::SignedThinkingToolResultFollowup {
        issues
            .push("signed thinking replay request is not defined for Bedrock Converse".to_string());
        return;
    }
    if !probe_case.request_uses_probe_tool() {
        reject_present(body, "/toolConfig", "Bedrock no-tool request", issues);
        return;
    }
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

fn validate_ollama_probe_request(
    body: &Value,
    probe_case: ToolProbeCase,
    issues: &mut Vec<String>,
) {
    require_array(body, "/messages", issues);
    if probe_case == ToolProbeCase::SignedThinkingToolResultFollowup {
        issues
            .push("signed thinking replay request is not defined for Ollama dialects".to_string());
        return;
    }
    if !probe_case.request_uses_probe_tool() {
        reject_present(body, "/tools", "Ollama no-tool request", issues);
        reject_present(body, "/tool_choice", "Ollama no-tool request", issues);
        return;
    }
    require_openai_function_tool(body, "/tools/0", issues);
    reject_present(body, "/tool_choice", "Ollama request", issues);
}

fn validate_anthropic_signed_thinking_history(body: &Value, issues: &mut Vec<String>) {
    require_string_eq(
        body,
        "/messages/1/role",
        "assistant",
        "Anthropic signed-thinking assistant role",
        issues,
    );
    require_string_eq(
        body,
        "/messages/1/content/0/type",
        "thinking",
        "Anthropic thinking block type",
        issues,
    );
    require_string_eq(
        body,
        "/messages/1/content/0/signature",
        ANTHROPIC_THINKING_SIGNATURE,
        "Anthropic thinking signature",
        issues,
    );
    require_string_eq(
        body,
        "/messages/1/content/1/type",
        "redacted_thinking",
        "Anthropic redacted thinking block type",
        issues,
    );
    require_string_eq(
        body,
        "/messages/1/content/1/data",
        ANTHROPIC_REDACTED_THINKING_DATA,
        "Anthropic redacted thinking data",
        issues,
    );
    require_string_eq(
        body,
        "/messages/1/content/2/type",
        "tool_use",
        "Anthropic signed-thinking tool_use type",
        issues,
    );
    require_string_eq(
        body,
        "/messages/1/content/2/name",
        TOOL_PROBE_TOOL_NAME,
        "Anthropic signed-thinking tool_use name",
        issues,
    );
    require_string_eq(
        body,
        "/messages/2/role",
        "user",
        "Anthropic signed-thinking tool_result role",
        issues,
    );
    require_string_eq(
        body,
        "/messages/2/content/0/type",
        "tool_result",
        "Anthropic signed-thinking tool_result type",
        issues,
    );
    let tool_use_id = body
        .pointer("/messages/1/content/2/id")
        .and_then(Value::as_str);
    let tool_result_id = body
        .pointer("/messages/2/content/0/tool_use_id")
        .and_then(Value::as_str);
    if tool_use_id.is_none() || tool_use_id != tool_result_id {
        issues.push(format!(
            "Anthropic signed-thinking tool_result id must match tool_use id, got use={tool_use_id:?} result={tool_result_id:?}"
        ));
    }
}

fn validate_gemini_signed_thinking_history(body: &Value, issues: &mut Vec<String>) {
    require_string_eq(
        body,
        "/contents/1/role",
        "model",
        "Gemini signed-thinking model role",
        issues,
    );
    require_string_eq(
        body,
        "/contents/1/parts/0/thoughtSignature",
        GEMINI_THOUGHT_SIGNATURE,
        "Gemini thoughtSignature",
        issues,
    );
    require_string_eq(
        body,
        "/contents/1/parts/0/functionCall/name",
        TOOL_PROBE_TOOL_NAME,
        "Gemini signed-thinking functionCall name",
        issues,
    );
    require_string_eq(
        body,
        "/contents/2/role",
        "user",
        "Gemini signed-thinking functionResponse role",
        issues,
    );
    require_string_eq(
        body,
        "/contents/2/parts/0/functionResponse/name",
        TOOL_PROBE_TOOL_NAME,
        "Gemini signed-thinking functionResponse name",
        issues,
    );
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

fn validate_generation_parameter_ranges(body: &Value, dialect: &str, issues: &mut Vec<String>) {
    match dialect {
        "gemini" | "vertex" => {
            require_optional_number_range(body, "/generationConfig/temperature", 0.0, 2.0, issues);
            require_optional_number_range(body, "/generationConfig/topP", 0.0, 1.0, issues);
            require_optional_integer_min(body, "/generationConfig/topK", 1, issues);
            require_optional_integer_min(body, "/generationConfig/maxOutputTokens", 1, issues);
        }
        "bedrock" => {
            require_optional_number_range(body, "/inferenceConfig/temperature", 0.0, 2.0, issues);
            require_optional_number_range(body, "/inferenceConfig/topP", 0.0, 1.0, issues);
            require_optional_integer_min(body, "/inferenceConfig/maxTokens", 1, issues);
        }
        "ollama" => {
            require_optional_number_range(body, "/temperature", 0.0, 2.0, issues);
            require_optional_number_range(body, "/top_p", 0.0, 1.0, issues);
            require_optional_integer_min(body, "/max_tokens", 1, issues);
            require_optional_integer_min(body, "/options/num_predict", 1, issues);
        }
        _ => {
            require_optional_number_range(body, "/temperature", 0.0, 2.0, issues);
            require_optional_number_range(body, "/top_p", 0.0, 1.0, issues);
            require_optional_integer_min(body, "/top_k", 1, issues);
            require_optional_integer_min(body, "/max_tokens", 1, issues);
            require_optional_integer_min(body, "/max_completion_tokens", 1, issues);
        }
    }
}

fn require_optional_number_range(
    body: &Value,
    pointer: &str,
    min: f64,
    max: f64,
    issues: &mut Vec<String>,
) {
    let Some(value) = body.pointer(pointer) else {
        return;
    };
    let Some(number) = value.as_f64() else {
        issues.push(format!("{pointer} must be a number when present"));
        return;
    };
    if !number.is_finite() || number < min || number > max {
        issues.push(format!(
            "{pointer} must be finite and within [{min}, {max}], got {number}"
        ));
    }
}

fn require_optional_integer_min(body: &Value, pointer: &str, min: i64, issues: &mut Vec<String>) {
    let Some(value) = body.pointer(pointer) else {
        return;
    };
    let Some(number) = value.as_i64() else {
        issues.push(format!("{pointer} must be an integer when present"));
        return;
    };
    if number < min {
        issues.push(format!("{pointer} must be >= {min}, got {number}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{ReasoningEffort, ThinkingConfig};

    #[test]
    fn gpt_oss_payload_and_body_inherit_logical_generation_defaults() {
        let _guard = crate::llm::env_guard();
        llm_config::clear_user_overrides();
        crate::agent_sessions::reset_session_store();
        let session_id = crate::agent_sessions::open_or_create(Some(
            "tool-probe-catalog-reasoning-default".to_string(),
        ));
        crate::agent_sessions::set_pinned_reasoning_policy(&session_id, Some("off".to_string()))
            .expect("pin ambient reasoning policy");
        let session_guard = crate::agent_sessions::enter_current_session(session_id);
        let payload = probe_request_payload(
            "fireworks",
            "accounts/fireworks/models/gpt-oss-120b",
            ToolProbeMode::NonStreaming,
            super::super::ToolProbeCase::SingleToolCall,
            super::super::ToolProbeRequestProfile::CatalogDefault,
            super::super::DEFAULT_TOOL_PROBE_MARKER,
        )
        .expect("GPT-OSS probe payload");
        drop(session_guard);
        crate::agent_sessions::reset_session_store();

        assert_eq!(payload.temperature, Some(1.0));
        assert_eq!(payload.top_p, Some(1.0));
        assert_eq!(
            payload.thinking,
            ThinkingConfig::Effort {
                level: ReasoningEffort::High
            }
        );
        let body = provider_compatible_probe_request_body(&payload);
        assert_eq!(body["temperature"], 1.0);
        assert_eq!(body["top_p"], 1.0);
        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn request_warnings_record_anthropic_sampling_param_omission() {
        let (body, warnings) = probe_request_body_with_warnings(
            "anthropic",
            "claude-opus-4-7",
            ToolProbeMode::NonStreaming,
            super::super::ToolProbeCase::SingleToolCall,
            super::super::ToolProbeRequestProfile::ParameterEdges,
            super::super::DEFAULT_TOOL_PROBE_MARKER,
        )
        .expect("Anthropic request body");

        assert!(body.get("temperature").is_none(), "{body}");
        assert!(body.get("top_p").is_none(), "{body}");
        assert!(body.get("top_k").is_none(), "{body}");
        assert_eq!(
            warnings,
            vec![ToolConformanceRequestWarning::SamplingParamsOmitted {
                dialect: "anthropic".to_string(),
                params: vec![
                    "temperature".to_string(),
                    "top_p".to_string(),
                    "top_k".to_string(),
                ],
            }]
        );
    }

    #[test]
    fn tool_result_followup_renders_anthropic_adjacent_tool_result() {
        let body = probe_request_body(
            "anthropic",
            "claude-3-5-haiku-20241022",
            ToolProbeMode::NonStreaming,
            super::super::ToolProbeCase::ToolResultFollowup,
            super::super::ToolProbeRequestProfile::CatalogDefault,
            "tool_result_followup:case",
        )
        .expect("Anthropic follow-up request body");

        assert_eq!(body["tools"][0]["name"], TOOL_PROBE_TOOL_NAME);
        assert!(body.get("tool_choice").is_none());
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(
            body["messages"][1]["content"][0]["type"],
            serde_json::json!("tool_use")
        );
        assert_eq!(body["messages"][2]["role"], "user");
        assert_eq!(
            body["messages"][2]["content"][0]["type"],
            serde_json::json!("tool_result")
        );
        assert_eq!(
            body["messages"][2]["content"][0]["tool_use_id"],
            body["messages"][1]["content"][0]["id"]
        );

        let validation = validate_probe_request_body(
            "anthropic",
            "claude-3-5-haiku-20241022",
            super::super::ToolProbeCase::ToolResultFollowup,
            super::super::ToolProbeRequestProfile::CatalogDefault,
            &body,
        );
        assert_eq!(
            validation.status,
            ToolConformanceRequestValidationStatus::Pass,
            "{:?}",
            validation.issues
        );
    }

    #[test]
    fn signed_thinking_followup_preserves_anthropic_replay_blocks() {
        let body = probe_request_body(
            "anthropic",
            "claude-sonnet-5",
            ToolProbeMode::NonStreaming,
            super::super::ToolProbeCase::SignedThinkingToolResultFollowup,
            super::super::ToolProbeRequestProfile::CatalogDefault,
            "thinking:case",
        )
        .expect("Anthropic signed-thinking request body");

        assert_eq!(body["tools"][0]["name"], TOOL_PROBE_TOOL_NAME);
        assert!(body.get("tool_choice").is_none());
        assert_eq!(body["messages"][1]["content"][0]["type"], "thinking");
        assert_eq!(
            body["messages"][1]["content"][0]["signature"],
            ANTHROPIC_THINKING_SIGNATURE
        );
        assert_eq!(
            body["messages"][1]["content"][1]["type"],
            "redacted_thinking"
        );
        assert_eq!(
            body["messages"][1]["content"][1]["data"],
            ANTHROPIC_REDACTED_THINKING_DATA
        );
        assert_eq!(body["messages"][1]["content"][2]["type"], "tool_use");
        assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(
            body["messages"][2]["content"][0]["tool_use_id"],
            body["messages"][1]["content"][2]["id"]
        );

        let validation = validate_probe_request_body(
            "anthropic",
            "claude-sonnet-5",
            super::super::ToolProbeCase::SignedThinkingToolResultFollowup,
            super::super::ToolProbeRequestProfile::CatalogDefault,
            &body,
        );
        assert_eq!(
            validation.status,
            ToolConformanceRequestValidationStatus::Pass,
            "{:?}",
            validation.issues
        );
    }

    #[test]
    fn signed_thinking_followup_preserves_gemini_thought_signature() {
        let body = probe_request_body(
            "gemini",
            "gemini-2.5-flash",
            ToolProbeMode::NonStreaming,
            super::super::ToolProbeCase::SignedThinkingToolResultFollowup,
            super::super::ToolProbeRequestProfile::CatalogDefault,
            "thinking:case",
        )
        .expect("Gemini signed-thinking request body");

        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            TOOL_PROBE_TOOL_NAME
        );
        assert!(body.get("toolConfig").is_none());
        assert_eq!(
            body["contents"][1]["parts"][0]["thoughtSignature"],
            GEMINI_THOUGHT_SIGNATURE
        );
        assert_eq!(
            body["contents"][1]["parts"][0]["functionCall"]["name"],
            TOOL_PROBE_TOOL_NAME
        );
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["name"],
            TOOL_PROBE_TOOL_NAME
        );

        let validation = validate_probe_request_body(
            "gemini",
            "gemini-2.5-flash",
            super::super::ToolProbeCase::SignedThinkingToolResultFollowup,
            super::super::ToolProbeRequestProfile::CatalogDefault,
            &body,
        );
        assert_eq!(
            validation.status,
            ToolConformanceRequestValidationStatus::Pass,
            "{:?}",
            validation.issues
        );
    }

    #[test]
    fn tool_result_followup_renders_openai_tool_history_without_tool_choice() {
        let body = probe_request_body(
            "openai",
            "gpt-5.4-mini",
            ToolProbeMode::NonStreaming,
            super::super::ToolProbeCase::ToolResultFollowup,
            super::super::ToolProbeRequestProfile::CatalogDefault,
            "tool_result_followup:case",
        )
        .expect("OpenAI follow-up request body");

        assert_eq!(body["tools"][0]["function"]["name"], TOOL_PROBE_TOOL_NAME);
        assert!(body.get("tool_choice").is_none());
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(
            body["messages"][1]["tool_calls"][0]["id"],
            "call_harn_tool_probe_1"
        );
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(
            body["messages"][2]["tool_call_id"],
            "call_harn_tool_probe_1"
        );

        let validation = validate_probe_request_body(
            "openai",
            "gpt-5.4-mini",
            super::super::ToolProbeCase::ToolResultFollowup,
            super::super::ToolProbeRequestProfile::CatalogDefault,
            &body,
        );
        assert_eq!(
            validation.status,
            ToolConformanceRequestValidationStatus::Pass,
            "{:?}",
            validation.issues
        );
    }

    #[test]
    fn tool_result_followup_renders_gemini_function_response_without_forcing_tool() {
        let body = probe_request_body(
            "gemini",
            "gemini-2.5-flash",
            ToolProbeMode::NonStreaming,
            super::super::ToolProbeCase::ToolResultFollowup,
            super::super::ToolProbeRequestProfile::CatalogDefault,
            "tool_result_followup:case",
        )
        .expect("Gemini follow-up request body");

        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            TOOL_PROBE_TOOL_NAME
        );
        assert!(body.get("toolConfig").is_none());
        assert_eq!(body["contents"][1]["role"], "model");
        let function_call = body["contents"][1]["parts"]
            .as_array()
            .expect("Gemini model contents parts")
            .iter()
            .find_map(|part| part.get("functionCall"))
            .expect("Gemini model history includes functionCall");
        assert_eq!(function_call["name"], TOOL_PROBE_TOOL_NAME);
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["name"],
            TOOL_PROBE_TOOL_NAME
        );

        let validation = validate_probe_request_body(
            "gemini",
            "gemini-2.5-flash",
            super::super::ToolProbeCase::ToolResultFollowup,
            super::super::ToolProbeRequestProfile::CatalogDefault,
            &body,
        );
        assert_eq!(
            validation.status,
            ToolConformanceRequestValidationStatus::Pass,
            "{:?}",
            validation.issues
        );
    }
}
