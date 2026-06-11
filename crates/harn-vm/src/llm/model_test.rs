use std::time::Instant;

use serde::Serialize;

use super::api::{
    vm_call_llm_full_streaming, LlmApiMode, LlmCallOptions, LlmRoutePolicy, OutputFormat,
    ThinkingConfig,
};
use crate::value::{VmError, VmValue};

const SMOKE_TEST_MAX_TOKENS: i64 = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSmokeTestOptions {
    pub model: String,
    pub provider: Option<String>,
    pub prompt: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelSmokeTestResult {
    pub model_id: String,
    pub provider: String,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub estimated_cost_usd: f64,
}

pub async fn run_model_smoke_test(
    options: ModelSmokeTestOptions,
) -> Result<ModelSmokeTestResult, String> {
    super::provider::register_default_providers();

    let resolved = crate::llm_config::resolve_model_info(&options.model);
    let model_id = resolved.id;
    let provider = options
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(str::to_string)
        .unwrap_or(resolved.provider);
    let api_key = super::helpers::resolve_api_key(&provider).map_err(vm_error_message)?;

    if let Some(def) = crate::llm_config::provider_config(&provider) {
        if super::supports_model_readiness_probe(&def) {
            let readiness =
                super::probe_openai_compatible_model(&provider, &model_id, &api_key).await;
            if readiness.category == "model_missing" || readiness.category == "invalid_url" {
                return Err(readiness.message);
            }
        }
    }

    let opts = LlmCallOptions {
        provider: provider.clone(),
        model: model_id.clone(),
        api_key,
        api_mode: LlmApiMode::ChatCompletions,
        route_policy: LlmRoutePolicy::Manual,
        fallback_chain: Vec::new(),
        route_fallbacks: Vec::new(),
        routing_decision: None,
        routing_policy: None,
        region: None,
        session_id: None,
        reminders: None,
        reminder_lifecycle: Vec::new(),
        messages: vec![serde_json::json!({
            "role": "user",
            "content": options.prompt,
        })],
        system: None,
        transcript_summary: None,
        max_tokens: SMOKE_TEST_MAX_TOKENS,
        temperature: None,
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
        output_validation: None,
        schema_stream_abort: false,
        thinking: ThinkingConfig::Disabled,
        anthropic_beta_features: Vec::new(),
        vision: false,
        tools: None,
        native_tools: None,
        provider_tools: Vec::new(),
        tool_choice: None,
        tool_search: None,
        cache: false,
        timeout: None,
        idle_timeout: None,
        stream: true,
        provider_overrides: None,
        previous_response_id: None,
        store: None,
        background: None,
        truncation: None,
        compact: None,
        include: None,
        max_tool_calls: None,
        budget: None,
        prefill: None,
        structural_experiment: None,
        applied_structural_experiment: None,
    };

    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let started = Instant::now();
    let first_delta = tokio::spawn(async move { delta_rx.recv().await.map(|_| started.elapsed()) });
    let result = vm_call_llm_full_streaming(&opts, delta_tx)
        .await
        .map_err(vm_error_message);
    let latency_ms = duration_ms(started.elapsed());
    let first_token_ms = first_delta.await.ok().flatten().map(duration_ms);
    let result = result?;

    Ok(ModelSmokeTestResult {
        model_id: result.model.clone(),
        provider: result.provider.clone(),
        latency_ms,
        first_token_ms,
        input_tokens: result.input_tokens,
        output_tokens: result.output_tokens,
        estimated_cost_usd: super::calculate_cost_for_provider(
            &result.provider,
            &result.model,
            result.input_tokens,
            result.output_tokens,
        ),
    })
}

fn duration_ms(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn vm_error_message(error: VmError) -> String {
    match error {
        VmError::CategorizedError { message, .. } => message,
        VmError::Thrown(VmValue::String(message)) => message.to_string(),
        VmError::Thrown(VmValue::Dict(dict)) => dict
            .get("message")
            .map(VmValue::display)
            .unwrap_or_else(|| VmError::Thrown(VmValue::Dict(dict)).to_string()),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{run_model_smoke_test, ModelSmokeTestOptions};

    #[tokio::test]
    async fn mock_provider_smoke_test_reports_timing_tokens_and_cost() {
        crate::llm::reset_llm_state();
        let result = run_model_smoke_test(ModelSmokeTestOptions {
            model: "mock".to_string(),
            provider: Some("mock".to_string()),
            prompt: "ping".to_string(),
        })
        .await
        .expect("mock provider smoke test should not require network");

        assert_eq!(result.model_id, "mock");
        assert_eq!(result.provider, "mock");
        assert_eq!(result.input_tokens, 4);
        assert_eq!(result.output_tokens, 30);
        assert_eq!(result.estimated_cost_usd, 0.0);
        assert!(result.first_token_ms.is_some());
    }
}
