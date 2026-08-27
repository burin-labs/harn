//! Dry-run validation of provider tool-probe request bodies.
//!
//! Split out of `tool_conformance_request.rs`, which owns *building* a probe
//! request; this module owns *checking* one. The two stay in the same module
//! tree (`super`) so the probe constants and fixtures have exactly one
//! definition.
//!
//! Every wire family that Harn can build a body for needs an arm here.
//! `request_validation_dialect` names the family — including, for Gemini, WHICH
//! endpoint family — so a scorecard cannot validate an Interactions body
//! against `generateContent` pointers and pass vacuously.

use serde_json::Value;

use super::super::{
    ToolConformanceRequestValidation, ToolConformanceRequestValidationStatus, ToolProbeCase,
    ToolProbeFormat, ToolProbeRequestProfile, TOOL_PROBE_TOOL_NAME,
};
use super::{
    ANTHROPIC_REDACTED_THINKING_DATA, ANTHROPIC_THINKING_SIGNATURE, GEMINI_THOUGHT_SIGNATURE,
};

#[cfg(test)]
pub(in crate::llm::tool_conformance) fn validate_probe_request_body(
    provider: &str,
    model: &str,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    body: &Value,
) -> ToolConformanceRequestValidation {
    validate_probe_request_body_for_format(
        provider,
        model,
        ToolProbeFormat::Native,
        probe_case,
        request_profile,
        body,
    )
}

pub(in crate::llm::tool_conformance) fn validate_probe_request_body_for_format(
    provider: &str,
    model: &str,
    tool_format: ToolProbeFormat,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    body: &Value,
) -> ToolConformanceRequestValidation {
    let caps = crate::llm::capabilities::lookup(provider, model);
    let dialect = request_validation_dialect(provider, &caps, body);
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
    if tool_format != ToolProbeFormat::Native {
        validate_text_channel_probe_request(body, &dialect, tool_format, &mut issues);
        validate_generation_parameter_ranges(body, &dialect, &mut issues);
        return ToolConformanceRequestValidation {
            dialect,
            status: if issues.is_empty() {
                ToolConformanceRequestValidationStatus::Pass
            } else {
                ToolConformanceRequestValidationStatus::Fail
            },
            warnings: Vec::new(),
            issues,
        };
    }
    match dialect.as_str() {
        "anthropic" => {
            validate_anthropic_probe_request(body, probe_case, request_profile, &mut issues);
        }
        "bedrock" => validate_bedrock_probe_request(body, probe_case, &mut issues),
        "gemini" | "vertex" => {
            validate_gemini_probe_request(body, probe_case, request_profile, &mut issues);
        }
        "gemini_interactions" => {
            validate_gemini_interactions_probe_request(
                body,
                probe_case,
                request_profile,
                &mut issues,
            );
        }
        "ollama" => validate_ollama_probe_request(body, probe_case, &mut issues),
        "openai_compat" => {
            validate_openai_compat_probe_request(body, probe_case, &caps, &mut issues);
        }
        "openai_responses" => {
            validate_openai_responses_probe_request(body, probe_case, request_profile, &mut issues);
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

fn validate_text_channel_probe_request(
    body: &Value,
    dialect: &str,
    tool_format: ToolProbeFormat,
    issues: &mut Vec<String>,
) {
    match dialect {
        "gemini" | "vertex" => require_array(body, "/contents", issues),
        "gemini_interactions" => require_array(body, "/input", issues),
        _ => require_array(body, "/messages", issues),
    }
    for pointer in ["/tools", "/tool_choice", "/toolConfig", "/tool_config"] {
        reject_present(body, pointer, "text-channel tool probe", issues);
    }
    let body_text = body.to_string();
    let format_marker = match tool_format {
        ToolProbeFormat::Json => "```tool",
        ToolProbeFormat::Text => "<tool_call>",
        ToolProbeFormat::Native => unreachable!(),
    };
    if !body_text.contains(TOOL_PROBE_TOOL_NAME) || !body_text.contains(format_marker) {
        issues.push(format!(
            "text-channel request is missing the {tool_format:?} echo_marker contract"
        ));
    }
}

pub(super) fn request_validation_dialect(
    provider: &str,
    caps: &crate::llm::capabilities::Capabilities,
    body: &Value,
) -> String {
    if provider == "bedrock" {
        return "bedrock".to_string();
    }
    if provider == "vertex" {
        return "vertex".to_string();
    }
    if provider == "openai" && caps.responses_api && body.get("input").is_some() {
        return "openai_responses".to_string();
    }
    match caps.message_wire_format {
        crate::llm::capabilities::WireDialect::Anthropic => "anthropic".to_string(),
        // The Gemini dialect serves two live endpoint families with completely
        // different bodies, so the audit dialect has to name the family — a
        // scorecard that reported both as `gemini` would validate an
        // Interactions body against `generateContent` pointers and pass.
        crate::llm::capabilities::WireDialect::Gemini => match caps.live_endpoint_family {
            Some(crate::llm::capabilities::LiveEndpointFamily::GeminiInteractions) => {
                "gemini_interactions".to_string()
            }
            _ => "gemini".to_string(),
        },
        crate::llm::capabilities::WireDialect::Ollama => "ollama".to_string(),
        crate::llm::capabilities::WireDialect::OpenAiCompat => "openai_compat".to_string(),
    }
}

fn validate_openai_responses_probe_request(
    body: &Value,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    issues: &mut Vec<String>,
) {
    require_array(body, "/input", issues);
    reject_present(body, "/messages", "OpenAI Responses request", issues);
    if !probe_case.request_uses_probe_tool() {
        reject_present(body, "/tools", "OpenAI Responses no-tool request", issues);
        return;
    }
    require_string_eq(
        body,
        "/tools/0/type",
        "function",
        "OpenAI Responses tool type",
        issues,
    );
    require_string_eq(
        body,
        "/tools/0/name",
        TOOL_PROBE_TOOL_NAME,
        "OpenAI Responses tool name",
        issues,
    );
    if probe_case.requires_probe_tool() {
        if request_profile == ToolProbeRequestProfile::ParameterEdges {
            require_string_eq(
                body,
                "/tool_choice",
                "required",
                "OpenAI Responses required tool choice",
                issues,
            );
        } else {
            require_string_eq(
                body,
                "/tool_choice/name",
                TOOL_PROBE_TOOL_NAME,
                "OpenAI Responses tool choice",
                issues,
            );
        }
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

/// Dry-run validation of a Gemini **Interactions** probe body.
///
/// Deliberately a separate function from [`validate_gemini_probe_request`]
/// rather than a branch inside it: every pointer differs (`input` vs
/// `contents`, flat `tools[]` vs `tools[0].functionDeclarations[]`,
/// `generation_config.tool_choice` vs `toolConfig.functionCallingConfig`), so
/// sharing one body would mean a validator that silently skips half its
/// assertions on whichever family it was not written for.
fn validate_gemini_interactions_probe_request(
    body: &Value,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    issues: &mut Vec<String>,
) {
    require_array(body, "/input", issues);
    // `generateContent` pointers must never appear on this family.
    for pointer in [
        "/contents",
        "/generationConfig",
        "/toolConfig",
        "/tool_choice",
    ] {
        reject_present(body, pointer, "Gemini Interactions request", issues);
    }
    if probe_case == ToolProbeCase::SignedThinkingToolResultFollowup {
        require_string_eq(
            body,
            "/tools/0/name",
            TOOL_PROBE_TOOL_NAME,
            "Gemini Interactions function tool name",
            issues,
        );
        validate_gemini_interactions_signed_thinking_history(body, issues);
        return;
    }
    if !probe_case.request_uses_probe_tool() {
        reject_present(
            body,
            "/tools",
            "Gemini Interactions no-tool request",
            issues,
        );
        reject_present(
            body,
            "/generation_config/tool_choice",
            "Gemini Interactions no-tool request",
            issues,
        );
        return;
    }
    require_string_eq(
        body,
        "/tools/0/type",
        "function",
        "Gemini Interactions tool type",
        issues,
    );
    require_string_eq(
        body,
        "/tools/0/name",
        TOOL_PROBE_TOOL_NAME,
        "Gemini Interactions function tool name",
        issues,
    );
    require_string_eq(
        body,
        "/tools/0/parameters/properties/value/type",
        "string",
        "Gemini Interactions function tool value type",
        issues,
    );
    if probe_case.requires_probe_tool() {
        match request_profile {
            ToolProbeRequestProfile::CatalogDefault => {
                require_string_eq(
                    body,
                    "/generation_config/tool_choice/allowed_tools/mode",
                    "any",
                    "Gemini Interactions allowed tool mode",
                    issues,
                );
                require_string_eq(
                    body,
                    "/generation_config/tool_choice/allowed_tools/tools/0",
                    TOOL_PROBE_TOOL_NAME,
                    "Gemini Interactions allowed tool name",
                    issues,
                );
            }
            ToolProbeRequestProfile::ParameterEdges => require_string_eq(
                body,
                "/generation_config/tool_choice",
                "any",
                "Gemini Interactions parameter-edge tool_choice",
                issues,
            ),
        }
    } else {
        reject_present(
            body,
            "/generation_config/tool_choice",
            "Gemini Interactions tool-result-followup request",
            issues,
        );
    }
}

/// The signed-thinking replay contract on Interactions: the opaque signature
/// rides its own `thought` step ahead of the call it authorizes, and the tool
/// result comes back as a `function_result` step keyed by `call_id`. Dropping
/// the `thought` step makes the provider reject the follow-up outright, so it
/// is asserted positionally.
fn validate_gemini_interactions_signed_thinking_history(body: &Value, issues: &mut Vec<String>) {
    require_string_eq(
        body,
        "/input/1/type",
        "thought",
        "Gemini Interactions thought step",
        issues,
    );
    require_string_eq(
        body,
        "/input/1/signature",
        GEMINI_THOUGHT_SIGNATURE,
        "Gemini Interactions thought signature",
        issues,
    );
    require_string_eq(
        body,
        "/input/2/type",
        "function_call",
        "Gemini Interactions function_call step",
        issues,
    );
    require_string_eq(
        body,
        "/input/2/name",
        TOOL_PROBE_TOOL_NAME,
        "Gemini Interactions function_call name",
        issues,
    );
    require_string_eq(
        body,
        "/input/3/type",
        "function_result",
        "Gemini Interactions function_result step",
        issues,
    );
    require_string_eq(
        body,
        "/input/3/name",
        TOOL_PROBE_TOOL_NAME,
        "Gemini Interactions function_result name",
        issues,
    );
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
        "gemini_interactions" => {
            require_optional_number_range(body, "/generation_config/temperature", 0.0, 2.0, issues);
            require_optional_number_range(body, "/generation_config/top_p", 0.0, 1.0, issues);
            require_optional_integer_min(body, "/generation_config/top_k", 1, issues);
            require_optional_integer_min(body, "/generation_config/max_output_tokens", 1, issues);
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
            require_optional_integer_min(body, "/max_output_tokens", 1, issues);
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
