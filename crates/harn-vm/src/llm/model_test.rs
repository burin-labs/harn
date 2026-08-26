use std::time::Instant;

use serde::Serialize;

use super::api::{vm_call_llm_full_streaming, LlmCallOptions};
use super::usage::LlmUsage;
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
    /// `None` means the provider returned usage but the resolved route has no
    /// known price. It must stay distinct from an exact zero cost.
    pub estimated_cost_usd: Option<f64>,
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
    let api_key = super::resolve_api_key(&provider).map_err(vm_error_message)?;

    if let Some(def) = crate::llm_config::provider_config(&provider) {
        if super::supports_model_readiness_probe(&def) {
            let readiness = super::readiness::probe_provider_readiness_with_options(
                &provider,
                super::readiness::ProviderReadinessOptions {
                    requested_model: Some(&model_id),
                    base_url_override: None,
                    api_key_override: Some(&api_key),
                },
            )
            .await;
            if readiness_status_blocks_smoke_test(readiness.status) {
                return Err(readiness.message);
            }
        }
    }

    let opts = LlmCallOptions {
        provider: provider.clone(),
        model: model_id.clone(),
        api_key,
        messages: vec![serde_json::json!({
            "role": "user",
            "content": options.prompt,
        })],
        max_tokens: SMOKE_TEST_MAX_TOKENS,
        // A smoke test wants a bare, deterministic completion: no schema, no
        // thinking, plain text. Those all match `LlmCallOptions::default()`,
        // so only the routing + prompt + token cap need spelling out here.
        ..LlmCallOptions::default()
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
    let usage = result.usage();

    Ok(model_smoke_test_result(
        result.model,
        result.provider,
        latency_ms,
        first_token_ms,
        &usage,
    ))
}

fn model_smoke_test_result(
    model_id: String,
    provider: String,
    latency_ms: u64,
    first_token_ms: Option<u64>,
    usage: &LlmUsage,
) -> ModelSmokeTestResult {
    ModelSmokeTestResult {
        model_id,
        provider,
        latency_ms,
        first_token_ms,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        estimated_cost_usd: usage.cost_usd,
    }
}

fn readiness_status_blocks_smoke_test(status: super::readiness::ReadinessStatus) -> bool {
    matches!(
        status,
        super::readiness::ReadinessStatus::ModelMissing
            | super::readiness::ReadinessStatus::InvalidUrl
            | super::readiness::ReadinessStatus::ProviderMismatch
    )
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
    use super::{
        model_smoke_test_result, readiness_status_blocks_smoke_test, run_model_smoke_test,
        ModelSmokeTestOptions,
    };
    use crate::llm::readiness::ReadinessStatus;
    use crate::llm::usage::LlmUsage;

    #[test]
    fn smoke_test_blocks_provider_mismatch_before_generation() {
        assert!(readiness_status_blocks_smoke_test(
            ReadinessStatus::ProviderMismatch
        ));
        assert!(readiness_status_blocks_smoke_test(
            ReadinessStatus::ModelMissing
        ));
        assert!(!readiness_status_blocks_smoke_test(ReadinessStatus::Ok));
    }

    #[tokio::test]
    async fn mock_provider_smoke_test_reports_timing_and_tokens() {
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
        assert_eq!(result.estimated_cost_usd, None);
        assert!(result.first_token_ms.is_some());
    }

    #[test]
    fn smoke_test_preserves_an_exact_zero_cost_as_json_zero() {
        let mut usage = LlmUsage::known_zero_attempt();
        usage.cost_usd = Some(0.0);

        let result = model_smoke_test_result(
            "provider-model".to_string(),
            "provider".to_string(),
            10,
            Some(4),
            &usage,
        );

        assert_eq!(result.estimated_cost_usd, Some(0.0));
        let rendered = serde_json::to_value(result).expect("smoke-test result serializes");
        assert_eq!(rendered["estimated_cost_usd"], serde_json::json!(0.0));
    }

    #[test]
    fn smoke_test_preserves_an_unpriced_usage_as_null() {
        let mut usage = LlmUsage::known_zero_attempt();
        usage.input_tokens = 14;
        usage.output_tokens = 6;
        usage.cost_usd = None;
        usage.unpriced_calls = 1;

        let result = model_smoke_test_result(
            "provider-model".to_string(),
            "provider".to_string(),
            10,
            Some(4),
            &usage,
        );

        assert_eq!(result.estimated_cost_usd, None);
        let rendered = serde_json::to_value(result).expect("smoke-test result serializes");
        assert_eq!(rendered["estimated_cost_usd"], serde_json::Value::Null);
    }
}
