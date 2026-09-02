use serde_json::{json, Value};

use super::request_contract::probe_tool_contract;
use super::{
    probe_tool_registry, ToolConformanceRequestWarning, ToolProbeCase, ToolProbeFormat,
    ToolProbeMode, ToolProbeRequestProfile, TOOL_PROBE_TOOL_NAME,
};
use crate::llm::api::{LlmApiMode, LlmRequestPayload, OutputFormat};
use crate::llm_config;

#[path = "tool_conformance_request_validation.rs"]
mod validation;
use validation::request_validation_dialect;
#[cfg(test)]
pub(in crate::llm::tool_conformance) use validation::validate_probe_request_body;
pub(in crate::llm::tool_conformance) use validation::validate_probe_request_body_for_format;

const ANTHROPIC_THINKING_SIGNATURE: &str = "harn-scorecard-anthropic-thinking-signature";
const ANTHROPIC_REDACTED_THINKING_DATA: &str = "harn-scorecard-redacted-thinking-payload";
const GEMINI_THOUGHT_SIGNATURE: &str = "harn-scorecard-gemini-thinking-signature";
const DEFAULT_TOOL_PROBE_MAX_TOKENS: i64 = 256;
const TOOL_PROBE_VISIBLE_OUTPUT_HEADROOM: i64 = 768;
const TOOL_PROBE_MAX_TOKENS_CEILING: i64 = 32_768;

#[cfg(test)]
pub(super) fn probe_request_body(
    provider: &str,
    model: &str,
    mode: ToolProbeMode,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    marker: &str,
) -> Result<Value, String> {
    probe_request_body_for_format(
        provider,
        model,
        mode,
        ToolProbeFormat::Native,
        probe_case,
        request_profile,
        marker,
    )
}

pub(super) fn probe_request_body_for_format(
    provider: &str,
    model: &str,
    mode: ToolProbeMode,
    tool_format: ToolProbeFormat,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    marker: &str,
) -> Result<Value, String> {
    probe_request_body_with_warnings_for_format(
        provider,
        model,
        mode,
        tool_format,
        probe_case,
        request_profile,
        marker,
    )
    .map(|(body, _warnings)| body)
}

#[cfg(test)]
pub(super) fn probe_request_body_with_warnings(
    provider: &str,
    model: &str,
    mode: ToolProbeMode,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    marker: &str,
) -> Result<(Value, Vec<ToolConformanceRequestWarning>), String> {
    probe_request_body_with_warnings_for_format(
        provider,
        model,
        mode,
        ToolProbeFormat::Native,
        probe_case,
        request_profile,
        marker,
    )
}

pub(super) fn probe_request_body_with_warnings_for_format(
    provider: &str,
    model: &str,
    mode: ToolProbeMode,
    tool_format: ToolProbeFormat,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    marker: &str,
) -> Result<(Value, Vec<ToolConformanceRequestWarning>), String> {
    let payload = probe_request_payload_for_format(
        provider,
        model,
        mode,
        tool_format,
        probe_case,
        request_profile,
        marker,
    )?;
    let body = provider_compatible_probe_request_body(&payload);
    let warnings = request_body_warnings(&payload, &body);
    Ok((body, warnings))
}

#[cfg(test)]
pub(super) fn probe_request_payload(
    provider: &str,
    model: &str,
    mode: ToolProbeMode,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    marker: &str,
) -> Result<LlmRequestPayload, String> {
    probe_request_payload_for_format(
        provider,
        model,
        mode,
        ToolProbeFormat::Native,
        probe_case,
        request_profile,
        marker,
    )
}

pub(super) fn probe_request_payload_for_format(
    provider: &str,
    model: &str,
    mode: ToolProbeMode,
    tool_format: ToolProbeFormat,
    probe_case: ToolProbeCase,
    request_profile: ToolProbeRequestProfile,
    marker: &str,
) -> Result<LlmRequestPayload, String> {
    if tool_format != ToolProbeFormat::Native
        && probe_case == ToolProbeCase::SignedThinkingToolResultFollowup
    {
        return Err(
            "signed-thinking tool-result replay is only defined for native tool format".to_string(),
        );
    }
    let model_defaults = llm_config::model_params_for_route(provider, model);
    let default_float =
        |key: &str| -> Option<f64> { model_defaults.get(key).and_then(toml::Value::as_float) };
    let default_int =
        |key: &str| -> Option<i64> { model_defaults.get(key).and_then(toml::Value::as_integer) };
    let native_tools =
        if tool_format == ToolProbeFormat::Native && probe_case.request_uses_probe_tool() {
            Some(
                crate::llm::tools::vm_tools_to_native(&probe_tool_registry(), provider, model)
                    .expect("tool probe registry is static and should convert to native tools"),
            )
        } else {
            None
        };
    let mut tool_choice = if tool_format == ToolProbeFormat::Native
        && probe_case.requires_probe_tool()
        && !crate::llm::provider::provider_uses_ollama_messages(provider, model)
    {
        if provider == "llamacpp" {
            // llama.cpp accepts only scalar tool-choice modes. The probe
            // exposes exactly one tool, so required preserves exact selection
            // without sending an object the server ignores.
            Some(json!("required"))
        } else {
            Some(json!({
                "type": "function",
                "function": {"name": TOOL_PROBE_TOOL_NAME}
            }))
        }
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
    let api_mode = crate::llm::api::effective_tool_api_mode(
        LlmApiMode::ChatCompletions,
        provider,
        &caps,
        &thinking,
        native_tools.as_ref().is_some_and(|tools| !tools.is_empty()),
    );
    let max_tokens = tool_probe_max_tokens(default_int("max_tokens"), &thinking);
    let mut payload = LlmRequestPayload {
        data_controls: crate::llm_config::DataPosture::Default,
        provider: provider.to_string(),
        model: model.to_string(),
        region: None,
        api_key: crate::llm::resolve_api_key(provider).unwrap_or_default(),
        api_mode,
        messages: Vec::new(),
        system: probe_tool_contract(tool_format)?,
        max_tokens,
        temperature: default_float("temperature"),
        top_p: default_float("top_p"),
        top_k: default_int("top_k"),
        logprobs: None,
        logit_bias: Vec::new(),
        min_p: None,
        repetition_penalty: None,
        prediction: None,
        verbosity: None,
        mirostat: None,
        stop: None,
        seed: None,
        frequency_penalty: None,
        presence_penalty: None,
        parallel_tool_calls: None,
        provider_contract_probe: None,
        fast: false,
        output_format: OutputFormat::Text,
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
        idle_timeout: None,
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
        mock_scope: None,
        done_sentinel: None,
        done_sentinel_form: None,
    };
    payload.messages = probe_messages(provider, tool_format, probe_case, marker);
    apply_request_profile(&mut payload, request_profile);
    Ok(payload)
}

pub(super) fn tool_probe_max_tokens(
    configured: Option<i64>,
    thinking: &crate::llm::api::ThinkingConfig,
) -> i64 {
    let configured = configured.unwrap_or(DEFAULT_TOOL_PROBE_MAX_TOKENS);
    let reasoning = i64::from(crate::llm::reasoning_policy::budget_for_thinking_config(
        thinking,
    ));
    if reasoning == 0 {
        return configured;
    }
    let reasoning_aware_floor = reasoning
        .saturating_add(TOOL_PROBE_VISIBLE_OUTPUT_HEADROOM)
        .min(TOOL_PROBE_MAX_TOKENS_CEILING);
    configured.max(reasoning_aware_floor)
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

fn probe_messages(
    provider: &str,
    tool_format: ToolProbeFormat,
    probe_case: ToolProbeCase,
    marker: &str,
) -> Vec<Value> {
    if probe_case == ToolProbeCase::SignedThinkingToolResultFollowup {
        return signed_thinking_probe_messages(provider, marker);
    }
    if probe_case != ToolProbeCase::ToolResultFollowup {
        return vec![json!({"role": "user", "content": probe_prompt(probe_case, marker)})];
    }

    let tool_call_id = "call_harn_tool_probe_1";
    if tool_format != ToolProbeFormat::Native {
        let call = match tool_format {
            ToolProbeFormat::Json => format!(
                "```tool\n{}\n```",
                json!({"name": TOOL_PROBE_TOOL_NAME, "args": {"value": marker}})
            ),
            ToolProbeFormat::Text => format!(
                "<tool_call>\n{TOOL_PROBE_TOOL_NAME}({{ value: {marker:?} }})\n</tool_call>"
            ),
            ToolProbeFormat::Native => unreachable!(),
        };
        return vec![
            json!({"role": "user", "content": probe_prompt(probe_case, marker)}),
            json!({"role": "assistant", "content": call}),
            json!({
                "role": "user",
                "content": format!(
                    "[result of {TOOL_PROBE_TOOL_NAME}]\n{}\n[end of {TOOL_PROBE_TOOL_NAME} result]\n",
                    json!({"value": marker})
                ),
            }),
        ];
    }
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
    let mut body = if payload.api_mode == LlmApiMode::Responses {
        crate::llm::providers::OpenAiResponsesProvider::build_request_body(payload)
    } else {
        match payload.provider.as_str() {
            "azure_openai" => {
                crate::llm::providers::AzureOpenAiProvider::build_request_body(payload)
            }
            "bedrock" => crate::llm::providers::BedrockProvider::build_request_body(payload),
            "vertex" => crate::llm::providers::VertexProvider::build_request_body(payload),
            _ => crate::llm::api::DialectContract::for_request(payload).build_request_body(payload),
        }
    };

    super::helpers::apply_stream_transport_fields(
        &payload.provider,
        &payload.model,
        payload.stream,
        &mut body,
    );
    body
}

fn request_body_warnings(
    payload: &LlmRequestPayload,
    body: &Value,
) -> Vec<ToolConformanceRequestWarning> {
    let caps = crate::llm::managed_supply::capabilities_for(&payload.provider, &payload.model);
    let dialect = request_validation_dialect(&payload.provider, &caps, body);
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
        "gemini_interactions" => {
            push_omitted_sampling_param(
                &mut omitted,
                payload.temperature.is_some(),
                body,
                "/generation_config/temperature",
                "temperature",
            );
            push_omitted_sampling_param(
                &mut omitted,
                payload.top_p.is_some(),
                body,
                "/generation_config/top_p",
                "top_p",
            );
            push_omitted_sampling_param(
                &mut omitted,
                payload.top_k.is_some(),
                body,
                "/generation_config/top_k",
                "top_k",
            );
            // Interactions' generation_config has no penalty fields at all, so
            // a request that asks for them is always reported as dropped rather
            // than failing the turn at the provider.
            push_omitted_sampling_param(
                &mut omitted,
                payload.frequency_penalty.is_some(),
                body,
                "/generation_config/frequency_penalty",
                "frequency_penalty",
            );
            push_omitted_sampling_param(
                &mut omitted,
                payload.presence_penalty.is_some(),
                body,
                "/generation_config/presence_penalty",
                "presence_penalty",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{ReasoningEffort, ThinkingConfig};
    use crate::llm::tool_conformance::ToolConformanceRequestValidationStatus;

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

    /// The dry-run audit has to be able to check BOTH Gemini endpoint families
    /// without spending anything, so a route on `gemini_interactions` must build
    /// an Interactions body and validate against Interactions pointers. If the
    /// audit still reported this route as the `gemini` dialect it would check
    /// `generateContent` pointers against an Interactions body and pass
    /// vacuously.
    #[test]
    fn signed_thinking_followup_audits_the_gemini_interactions_family() {
        let overrides: crate::llm::capabilities::CapabilitiesFile = toml::from_str(
            "[[provider.gemini]]\n\
             model_match = \"gemini-2.5-flash*\"\n\
             extends = true\n\
             live_endpoint_family = \"gemini_interactions\"\n",
        )
        .expect("override parses");
        let previous = crate::llm::capabilities::swap_user_overrides(Some(overrides));

        let body = probe_request_body(
            "gemini",
            "gemini-2.5-flash",
            ToolProbeMode::NonStreaming,
            super::super::ToolProbeCase::SignedThinkingToolResultFollowup,
            super::super::ToolProbeRequestProfile::CatalogDefault,
            "thinking:case",
        )
        .expect("Gemini Interactions signed-thinking request body");

        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], TOOL_PROBE_TOOL_NAME);
        assert!(
            body.get("contents").is_none(),
            "the generateContent envelope must not appear on this family"
        );
        assert_eq!(body["input"][1]["type"], "thought");
        assert_eq!(body["input"][1]["signature"], GEMINI_THOUGHT_SIGNATURE);
        assert_eq!(body["input"][2]["type"], "function_call");
        assert_eq!(body["input"][3]["type"], "function_result");

        let validation = validate_probe_request_body(
            "gemini",
            "gemini-2.5-flash",
            super::super::ToolProbeCase::SignedThinkingToolResultFollowup,
            super::super::ToolProbeRequestProfile::CatalogDefault,
            &body,
        );
        crate::llm::capabilities::swap_user_overrides(previous);
        assert_eq!(validation.dialect, "gemini_interactions");
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
